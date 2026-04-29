use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Context, Result, anyhow, bail};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::oneshot;
use tokio_tungstenite::MaybeTlsStream;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tracing::{debug, info, warn};

use crate::approval::{
    CommandExecutionRequestApprovalParams, FileChangeRequestApprovalParams,
    PermissionsRequestApprovalParams,
};
use crate::collaboration_mode::CollaborationMode;
use crate::execution_mode::{ExecutionMode, SessionExecutionSnapshot};
use crate::interactive::{
    ServerRequestResolvedNotification, ToolRequestUserInputParams, ToolRequestUserInputResponse,
};

const APP_SERVER_CLIENT_NAME: &str = "threadbridge";
const APP_SERVER_CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const WORKSPACE_READY_PROMPT: &str = "Read and follow the workspace AGENTS.md if present, then reply with exactly READY. Do not ask follow-up questions. Do not run tools.";
pub(crate) const COLLABORATION_MODE_UNAVAILABLE_PREFIX: &str = "collaboration mode unavailable:";

#[derive(Debug, Clone, Serialize)]
pub struct CodexWorkspace {
    pub working_directory: PathBuf,
    pub app_server_url: Option<String>,
}

#[derive(Debug, Clone)]
pub enum CodexInputItem {
    Text { text: String },
    LocalImage { path: String },
}

#[derive(Debug, Clone)]
pub struct CodexRunner {
    pub model: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum CodexThreadEvent {
    #[serde(rename = "thread.started")]
    ThreadStarted { thread_id: String },
    #[serde(rename = "turn.started")]
    TurnStarted { turn_id: Option<String> },
    #[serde(rename = "turn.completed")]
    TurnCompleted {
        turn_id: Option<String>,
        usage: Option<Value>,
    },
    #[serde(rename = "turn.interrupted")]
    TurnInterrupted {
        turn_id: Option<String>,
        usage: Option<Value>,
    },
    #[serde(rename = "turn.failed")]
    TurnFailed {
        turn_id: Option<String>,
        error: Value,
    },
    #[serde(rename = "error")]
    Error { message: String },
    #[serde(rename = "item.started")]
    ItemStarted {
        turn_id: Option<String>,
        item: Value,
    },
    #[serde(rename = "item.updated")]
    ItemUpdated {
        turn_id: Option<String>,
        item: Value,
    },
    #[serde(rename = "item.completed")]
    ItemCompleted {
        turn_id: Option<String>,
        item: Value,
    },
}

#[derive(Debug, Clone, Serialize)]
pub struct CodexRunResult {
    pub final_response: String,
    pub final_plan_text: Option<String>,
    pub turn_outcome: CodexTurnOutcome,
    pub selected_factory: String,
    pub thread_id: String,
    pub thread_id_changed: bool,
    pub execution: SessionExecutionSnapshot,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendThreadRunState {
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "isBusy")]
    pub is_busy: bool,
    #[serde(rename = "activeTurnId")]
    pub active_turn_id: Option<String>,
    pub interruptible: bool,
    pub phase: Option<String>,
    #[serde(rename = "lastTransitionAt")]
    pub last_transition_at: Option<String>,
}

pub fn ensure_thread_run_state_idle(
    thread_id: &str,
    run_state: &BackendThreadRunState,
) -> Result<()> {
    if !run_state.is_busy {
        return Ok(());
    }
    let active_turn_id = run_state.active_turn_id.as_deref().unwrap_or("unknown");
    let phase = run_state.phase.as_deref().unwrap_or("unknown");
    bail!(
        "saved Codex session `{thread_id}` resumed, but worker still reports active turn `{active_turn_id}` in phase `{phase}`"
    );
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CodexTurnOutcome {
    Completed,
    Interrupted,
    Failed,
}

#[derive(Debug, Clone)]
pub struct CodexThreadBinding {
    pub thread_id: String,
    pub cwd: String,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub execution: SessionExecutionSnapshot,
}

#[derive(Debug)]
struct AppServerClient {
    transport: AppServerTransport,
    next_request_id: i64,
}

#[derive(Debug, Clone)]
struct TurnRunResult {
    final_response: String,
    final_plan_text: Option<String>,
    outcome: CodexTurnOutcome,
}

#[derive(Debug)]
enum AppServerTransport {
    Stdio {
        child: Child,
        stdin: ChildStdin,
        stdout: BufReader<tokio::process::ChildStdout>,
    },
    WebSocket {
        stream: WebSocketStream<MaybeTlsStream<TcpStream>>,
    },
}

#[derive(Debug)]
enum RpcMessage {
    Response {
        id: i64,
        result: Value,
    },
    Error {
        id: i64,
        message: String,
        data: Option<Value>,
    },
    Notification {
        method: String,
        params: Option<Value>,
    },
    Request {
        id: i64,
        method: String,
        params: Option<Value>,
    },
}

#[derive(Debug, Clone)]
pub enum CodexServerRequest {
    CommandExecutionRequestApproval {
        request_id: i64,
        params: CommandExecutionRequestApprovalParams,
    },
    FileChangeRequestApproval {
        request_id: i64,
        params: FileChangeRequestApprovalParams,
    },
    PermissionsRequestApproval {
        request_id: i64,
        params: PermissionsRequestApprovalParams,
    },
    RequestUserInput {
        request_id: i64,
        params: ToolRequestUserInputParams,
    },
}

#[derive(Debug, Clone)]
pub enum CodexServerNotification {
    ServerRequestResolved(ServerRequestResolvedNotification),
}

fn log_item_event(lifecycle: &str, item: &Value) {
    let Some(item_type) = item.get("type").and_then(Value::as_str) else {
        return;
    };

    match item_type {
        "command_execution" | "mcp_tool_call" | "web_search" => {
            info!(
                event = "codex.item",
                lifecycle,
                item_type,
                item = %item,
                "codex tool activity"
            );
        }
        _ => {}
    }
}

impl AppServerClient {
    fn initialize_params() -> Value {
        json!({
            "clientInfo": {
                "name": APP_SERVER_CLIENT_NAME,
                "title": null,
                "version": APP_SERVER_CLIENT_VERSION,
            },
            "capabilities": {
                "experimentalApi": true,
            }
        })
    }

    async fn start(workspace: &CodexWorkspace) -> Result<Self> {
        if let Some(app_server_url) = workspace.app_server_url.as_deref() {
            return Self::start_websocket(app_server_url).await;
        }
        Self::start_stdio(&workspace.working_directory).await
    }

    async fn start_stdio(workspace: &Path) -> Result<Self> {
        let mut child = Command::new("codex")
            .args(["app-server", "--listen", "stdio://"])
            .current_dir(workspace)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .context("failed to spawn codex app-server")?;

        let stdin = child.stdin.take().context("missing app-server stdin")?;
        let stdout = child.stdout.take().context("missing app-server stdout")?;
        if let Some(stderr) = child.stderr.take() {
            let mut stderr_lines = BufReader::new(stderr).lines();
            tokio::spawn(async move {
                while let Ok(Some(line)) = stderr_lines.next_line().await {
                    debug!(event = "codex.app_server.stderr", line = %line);
                }
            });
        }

        let mut client = Self {
            transport: AppServerTransport::Stdio {
                child,
                stdin,
                stdout: BufReader::new(stdout),
            },
            next_request_id: 0,
        };
        client.initialize().await?;
        Ok(client)
    }

    async fn start_websocket(app_server_url: &str) -> Result<Self> {
        let (stream, _) = connect_async(app_server_url).await.with_context(|| {
            format!("failed to connect to shared app-server at {app_server_url}")
        })?;
        let mut client = Self {
            transport: AppServerTransport::WebSocket { stream },
            next_request_id: 0,
        };
        client.initialize().await?;
        Ok(client)
    }

    async fn initialize(&mut self) -> Result<()> {
        let _ = self
            .request_simple("initialize", Self::initialize_params())
            .await?;
        self.send_notification("initialized", None).await?;
        Ok(())
    }

    async fn send_notification(&mut self, method: &str, params: Option<Value>) -> Result<()> {
        let payload = match params {
            Some(params) => json!({
                "method": method,
                "params": params,
            }),
            None => json!({
                "method": method,
            }),
        };
        self.send_payload(payload).await
    }

    async fn request_simple(&mut self, method: &str, params: Value) -> Result<Value> {
        let request_id = self.send_request(method, params).await?;
        loop {
            match self.read_message().await? {
                RpcMessage::Response { id, result } if id == request_id => return Ok(result),
                RpcMessage::Error { id, message, data } if id == request_id => {
                    let details = data.map(|value| value.to_string()).unwrap_or_default();
                    if details.is_empty() {
                        bail!("{method} failed: {message}");
                    }
                    bail!("{method} failed: {message} ({details})");
                }
                RpcMessage::Notification { method, params } => {
                    let params_for_log = params.clone().unwrap_or(Value::Null);
                    debug!(
                        event = "codex.app_server.notification.ignored",
                        method,
                        params = %params_for_log
                    );
                }
                RpcMessage::Request { id, method, .. } => {
                    self.reject_server_request(id, &method).await?;
                }
                RpcMessage::Response { .. } | RpcMessage::Error { .. } => {}
            }
        }
    }

    async fn send_request(&mut self, method: &str, params: Value) -> Result<i64> {
        let request_id = self.next_request_id;
        self.next_request_id += 1;
        self.send_payload(json!({
            "id": request_id,
            "method": method,
            "params": params,
            "trace": null,
        }))
        .await?;
        Ok(request_id)
    }

    async fn send_payload(&mut self, payload: Value) -> Result<()> {
        let text = serde_json::to_string(&payload)?;
        match &mut self.transport {
            AppServerTransport::Stdio { stdin, .. } => {
                stdin
                    .write_all(text.as_bytes())
                    .await
                    .context("failed writing to app-server stdin")?;
                stdin
                    .write_all(b"\n")
                    .await
                    .context("failed writing newline to app-server stdin")?;
                stdin
                    .flush()
                    .await
                    .context("failed flushing app-server stdin")
            }
            AppServerTransport::WebSocket { stream } => stream
                .send(WsMessage::Text(text))
                .await
                .context("failed sending app-server websocket payload"),
        }
    }

    async fn reject_server_request(&mut self, request_id: i64, method: &str) -> Result<()> {
        warn!(
            event = "codex.app_server.request.rejected",
            request_id, method, "rejecting unsupported app-server server request"
        );
        self.send_payload(json!({
            "id": request_id,
            "error": {
                "code": -32601,
                "message": format!("threadbridge does not support app-server server request `{method}`"),
            }
        }))
        .await
    }

    async fn send_server_request_response<T: Serialize>(
        &mut self,
        request_id: i64,
        result: &T,
    ) -> Result<()> {
        self.send_payload(json!({
            "id": request_id,
            "result": serde_json::to_value(result)?,
        }))
        .await
    }

    async fn read_message(&mut self) -> Result<RpcMessage> {
        loop {
            let mut line = String::new();
            match &mut self.transport {
                AppServerTransport::Stdio { child, stdout, .. } => {
                    let bytes = stdout
                        .read_line(&mut line)
                        .await
                        .context("failed to read app-server stdout")?;
                    if bytes == 0 {
                        let status = child.wait().await.ok();
                        bail!("codex app-server exited unexpectedly: {status:?}");
                    }
                }
                AppServerTransport::WebSocket { stream } => {
                    let Some(message) = stream.next().await else {
                        bail!("shared codex app-server websocket closed unexpectedly");
                    };
                    match message.context("failed to read app-server websocket message")? {
                        WsMessage::Text(text) => line = text,
                        WsMessage::Binary(bytes) => {
                            line =
                                String::from_utf8(bytes).context("invalid utf8 websocket frame")?
                        }
                        WsMessage::Ping(payload) => {
                            stream
                                .send(WsMessage::Pong(payload))
                                .await
                                .context("failed responding to websocket ping")?;
                            continue;
                        }
                        WsMessage::Pong(_) | WsMessage::Frame(_) => continue,
                        WsMessage::Close(frame) => {
                            bail!("shared codex app-server websocket closed: {frame:?}");
                        }
                    }
                }
            }

            let raw: Value = serde_json::from_str(&line)
                .with_context(|| format!("invalid app-server json: {line}"))?;

            if raw.get("method").is_some() {
                let method = raw
                    .get("method")
                    .and_then(Value::as_str)
                    .context("app-server message missing method")?
                    .to_owned();
                if let Some(id) = raw.get("id").and_then(Value::as_i64) {
                    return Ok(RpcMessage::Request {
                        id,
                        method,
                        params: raw.get("params").cloned(),
                    });
                }
                let params = raw.get("params").cloned();
                return Ok(RpcMessage::Notification { method, params });
            }

            let id = raw
                .get("id")
                .and_then(Value::as_i64)
                .context("app-server response missing numeric id")?;
            if let Some(result) = raw.get("result") {
                return Ok(RpcMessage::Response {
                    id,
                    result: result.clone(),
                });
            }
            if let Some(error) = raw.get("error") {
                return Ok(RpcMessage::Error {
                    id,
                    message: error
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown app-server error")
                        .to_owned(),
                    data: error.get("data").cloned(),
                });
            }

            return Err(anyhow!("unknown app-server message shape: {raw}"));
        }
    }
}

impl CodexRunner {
    pub fn new(model: Option<String>) -> Self {
        Self { model }
    }

    fn normalize_input(input: &[CodexInputItem]) -> Vec<Value> {
        let mut texts = Vec::new();
        let mut payload = Vec::new();

        for item in input {
            match item {
                CodexInputItem::Text { text } => texts.push(text.clone()),
                CodexInputItem::LocalImage { path } => {
                    payload.push(json!({
                        "type": "localImage",
                        "path": path,
                    }));
                }
            }
        }

        if !texts.is_empty() {
            payload.insert(
                0,
                json!({
                    "type": "text",
                    "text": texts.join("\n\n"),
                    "text_elements": [],
                }),
            );
        }

        payload
    }

    fn build_thread_start_params(&self, workspace: &Path, execution_mode: ExecutionMode) -> Value {
        json!({
            "model": self.model,
            "cwd": workspace.display().to_string(),
            "approvalPolicy": execution_mode.approval_policy(),
            "sandbox": execution_mode.sandbox_mode(),
            "experimentalRawEvents": false,
            "persistExtendedHistory": false,
        })
    }

    fn build_thread_resume_params(thread_id: &str, execution_mode: Option<ExecutionMode>) -> Value {
        let mut params = json!({
            "threadId": thread_id,
            "persistExtendedHistory": false,
        });
        if let Some(execution_mode) = execution_mode {
            params["approvalPolicy"] = Value::String(execution_mode.approval_policy().to_owned());
            params["sandbox"] = Value::String(execution_mode.sandbox_mode().to_owned());
        }
        params
    }

    fn build_thread_read_params(thread_id: &str) -> Value {
        json!({
            "threadId": thread_id,
            "includeTurns": false,
        })
    }

    async fn resume_thread_on_client(
        client: &mut AppServerClient,
        thread_id: &str,
        execution_mode: Option<ExecutionMode>,
    ) -> Result<CodexThreadBinding> {
        let result = client
            .request_simple(
                "thread/resume",
                Self::build_thread_resume_params(thread_id, execution_mode),
            )
            .await?;
        let binding = Self::parse_binding(&result)?;
        if binding.thread_id != thread_id {
            bail!(
                "Codex thread continuity changed: expected {}, got {}",
                thread_id,
                binding.thread_id
            );
        }
        Ok(binding)
    }

    async fn read_thread_on_client(
        client: &mut AppServerClient,
        thread_id: &str,
    ) -> Result<CodexThreadBinding> {
        let result = client
            .request_simple("thread/read", Self::build_thread_read_params(thread_id))
            .await?;
        let binding = Self::parse_binding(&result)?;
        if binding.thread_id != thread_id {
            bail!(
                "Codex thread continuity changed: expected {}, got {}",
                thread_id,
                binding.thread_id
            );
        }
        Ok(binding)
    }

    fn build_turn_start_params(
        thread_id: &str,
        input: &[CodexInputItem],
        collaboration_mode: Option<Value>,
    ) -> Value {
        let mut params = json!({
            "threadId": thread_id,
            "input": Self::normalize_input(input),
        });
        if let Some(collaboration_mode) = collaboration_mode {
            params["collaborationMode"] = collaboration_mode;
        }
        params
    }

    fn build_turn_interrupt_params(thread_id: &str, turn_id: &str) -> Value {
        json!({
            "threadId": thread_id,
            "turnId": turn_id,
        })
    }

    fn build_turn_steer_params(
        thread_id: &str,
        expected_turn_id: &str,
        input: &[CodexInputItem],
    ) -> Value {
        json!({
            "threadId": thread_id,
            "input": Self::normalize_input(input),
            "expectedTurnId": expected_turn_id,
        })
    }

    fn parse_binding(result: &Value) -> Result<CodexThreadBinding> {
        let thread = result
            .get("thread")
            .context("app-server result missing thread")?;
        let thread_id = thread
            .get("id")
            .and_then(Value::as_str)
            .context("app-server thread missing id")?
            .to_owned();
        let cwd = thread
            .get("cwd")
            .or_else(|| result.get("cwd"))
            .and_then(Value::as_str)
            .context("app-server thread missing cwd")?
            .to_owned();
        Ok(CodexThreadBinding {
            thread_id,
            cwd,
            model: result
                .get("model")
                .and_then(Value::as_str)
                .map(str::to_owned),
            reasoning_effort: result
                .get("reasoningEffort")
                .or_else(|| result.get("reasoning_effort"))
                .and_then(Value::as_str)
                .map(str::to_owned),
            execution: SessionExecutionSnapshot::from_thread_result(result),
        })
    }

    fn build_collaboration_mode_payload(
        mode: CollaborationMode,
        model: String,
        reasoning_effort: Option<String>,
    ) -> Value {
        json!({
            "mode": mode.as_str(),
            "settings": {
                "model": model,
                "reasoning_effort": reasoning_effort,
                "developer_instructions": Value::Null,
            },
        })
    }

    fn resolve_collaboration_model(
        &self,
        binding: &CodexThreadBinding,
        selected: Option<&Value>,
        collaboration_mode: CollaborationMode,
    ) -> Result<String> {
        selected
            .and_then(|value| value.get("model"))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| binding.model.clone())
            .or_else(|| self.model.clone())
            .context("session model is unavailable")
            .with_context(|| {
                format!(
                    "{COLLABORATION_MODE_UNAVAILABLE_PREFIX} missing session model for {}",
                    collaboration_mode.as_str()
                )
            })
    }

    fn resolve_default_collaboration_mode_payload(
        &self,
        binding: &CodexThreadBinding,
    ) -> Result<Value> {
        let model = self.resolve_collaboration_model(binding, None, CollaborationMode::Default)?;
        Ok(Self::build_collaboration_mode_payload(
            CollaborationMode::Default,
            model,
            binding.reasoning_effort.clone(),
        ))
    }

    async fn resolve_collaboration_mode_payload(
        &self,
        client: &mut AppServerClient,
        binding: &CodexThreadBinding,
        collaboration_mode: Option<CollaborationMode>,
    ) -> Result<Option<Value>> {
        if !Self::requires_collaboration_mode_payload(collaboration_mode) {
            return Ok(None);
        }
        let Some(collaboration_mode) = collaboration_mode else {
            return Ok(None);
        };
        if matches!(collaboration_mode, CollaborationMode::Default) {
            return self
                .resolve_default_collaboration_mode_payload(binding)
                .map(Some);
        }
        let masks = client
            .request_simple("collaborationMode/list", json!({}))
            .await
            .with_context(|| {
                format!(
                    "{COLLABORATION_MODE_UNAVAILABLE_PREFIX} failed to fetch collaboration modes"
                )
            })?;
        let items = masks
            .get("data")
            .and_then(Value::as_array)
            .context("collaborationMode/list result missing data array")
            .with_context(|| {
                format!("{COLLABORATION_MODE_UNAVAILABLE_PREFIX} invalid collaboration mode list")
            })?;
        let selected = items
            .iter()
            .find(|mask| {
                mask.get("mode").and_then(Value::as_str) == Some(collaboration_mode.as_str())
            })
            .context("selected collaboration mode is unavailable")
            .with_context(|| {
                format!(
                    "{COLLABORATION_MODE_UNAVAILABLE_PREFIX} {}",
                    collaboration_mode.as_str()
                )
            })?;
        let model =
            self.resolve_collaboration_model(binding, Some(selected), collaboration_mode)?;
        let reasoning_effort = selected
            .get("reasoning_effort")
            .and_then(|value| (!value.is_null()).then_some(value))
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| binding.reasoning_effort.clone());

        Ok(Some(Self::build_collaboration_mode_payload(
            collaboration_mode,
            model,
            reasoning_effort,
        )))
    }

    fn requires_collaboration_mode_payload(collaboration_mode: Option<CollaborationMode>) -> bool {
        collaboration_mode.is_some()
    }

    fn ensure_workspace_cwd(
        workspace: &CodexWorkspace,
        binding: &CodexThreadBinding,
    ) -> Result<()> {
        let actual = workspace
            .working_directory
            .canonicalize()
            .unwrap_or_else(|_| workspace.working_directory.clone());
        let expected = PathBuf::from(&binding.cwd)
            .canonicalize()
            .unwrap_or_else(|_| PathBuf::from(&binding.cwd));
        if actual != expected {
            bail!(
                "Codex thread cwd mismatch: expected {}, got {}",
                actual.display(),
                expected.display()
            );
        }
        Ok(())
    }

    pub async fn start_thread(&self, workspace: &CodexWorkspace) -> Result<CodexThreadBinding> {
        self.start_thread_with_mode(workspace, ExecutionMode::FullAuto)
            .await
    }

    pub async fn start_thread_with_mode(
        &self,
        workspace: &CodexWorkspace,
        execution_mode: ExecutionMode,
    ) -> Result<CodexThreadBinding> {
        let mut client = AppServerClient::start(workspace).await?;
        let result = client
            .request_simple(
                "thread/start",
                self.build_thread_start_params(&workspace.working_directory, execution_mode),
            )
            .await?;
        let binding = Self::parse_binding(&result)?;
        Self::ensure_workspace_cwd(workspace, &binding)?;
        let ready = self
            .run_turn_on_client(
                &mut client,
                &binding,
                vec![CodexInputItem::Text {
                    text: WORKSPACE_READY_PROMPT.to_owned(),
                }],
                None,
                |_| async {},
                |_| async { Ok(None) },
            )
            .await?;
        self.ensure_ready_response(&ready.final_response, "workspace initialization")?;
        Ok(binding)
    }

    pub async fn resume_session(
        &self,
        workspace: &CodexWorkspace,
        existing_thread_id: &str,
        execution_mode: Option<ExecutionMode>,
    ) -> Result<CodexThreadBinding> {
        let mut client = AppServerClient::start(workspace).await?;
        let binding =
            Self::resume_thread_on_client(&mut client, existing_thread_id, execution_mode).await?;
        Self::ensure_workspace_cwd(workspace, &binding)?;
        Ok(binding)
    }

    pub async fn read_session_binding(
        &self,
        workspace: &CodexWorkspace,
        existing_thread_id: &str,
    ) -> Result<CodexThreadBinding> {
        let mut client = AppServerClient::start(workspace).await?;
        let binding = Self::read_thread_on_client(&mut client, existing_thread_id).await?;
        Self::ensure_workspace_cwd(workspace, &binding)?;
        Ok(binding)
    }

    pub async fn run_with_events<F, Fut>(
        &self,
        workspace: &CodexWorkspace,
        existing_thread_id: Option<&str>,
        execution_mode: Option<ExecutionMode>,
        input: Vec<CodexInputItem>,
        on_event: F,
    ) -> Result<CodexRunResult>
    where
        F: FnMut(CodexThreadEvent) -> Fut,
        Fut: Future<Output = ()>,
    {
        self.run_with_events_and_server_requests(
            workspace,
            existing_thread_id,
            execution_mode,
            None,
            input,
            on_event,
            |request| async move {
                match request {
                    CodexServerRequest::CommandExecutionRequestApproval { .. }
                    | CodexServerRequest::FileChangeRequestApproval { .. }
                    | CodexServerRequest::PermissionsRequestApproval { .. }
                    | CodexServerRequest::RequestUserInput { .. } => Ok(None),
                }
            },
        )
        .await
    }

    pub(crate) async fn run_with_events_and_server_requests<F, Fut, H, Hf>(
        &self,
        workspace: &CodexWorkspace,
        existing_thread_id: Option<&str>,
        execution_mode: Option<ExecutionMode>,
        collaboration_mode: Option<CollaborationMode>,
        input: Vec<CodexInputItem>,
        mut on_event: F,
        on_server_request: H,
    ) -> Result<CodexRunResult>
    where
        F: FnMut(CodexThreadEvent) -> Fut,
        Fut: Future<Output = ()>,
        H: FnMut(CodexServerRequest) -> Hf,
        Hf: Future<Output = Result<Option<Value>>>,
    {
        let mut client = AppServerClient::start(workspace).await?;
        let (binding, selected_factory) = match existing_thread_id {
            Some(thread_id) => {
                let binding =
                    Self::resume_thread_on_client(&mut client, thread_id, execution_mode).await?;
                (binding, "resumeThread")
            }
            None => {
                let result = client
                    .request_simple(
                        "thread/start",
                        self.build_thread_start_params(
                            &workspace.working_directory,
                            execution_mode.unwrap_or_default(),
                        ),
                    )
                    .await?;
                (Self::parse_binding(&result)?, "startThread")
            }
        };

        Self::ensure_workspace_cwd(workspace, &binding)?;
        on_event(CodexThreadEvent::ThreadStarted {
            thread_id: binding.thread_id.clone(),
        })
        .await;
        let turn_result = self
            .run_turn_on_client(
                &mut client,
                &binding,
                input,
                collaboration_mode,
                on_event,
                on_server_request,
            )
            .await?;

        Ok(CodexRunResult {
            final_response: turn_result.final_response,
            final_plan_text: turn_result.final_plan_text,
            turn_outcome: turn_result.outcome,
            selected_factory: selected_factory.to_owned(),
            thread_id_changed: existing_thread_id.is_some_and(|id| id != binding.thread_id),
            thread_id: binding.thread_id,
            execution: binding.execution,
        })
    }

    pub(crate) fn map_notification(
        method: &str,
        params: Value,
        latest_agent_message_by_id: &mut HashMap<String, String>,
        latest_plan_by_id: &mut HashMap<String, String>,
    ) -> Result<Option<CodexThreadEvent>> {
        let turn_id = || {
            params
                .get("turnId")
                .and_then(Value::as_str)
                .map(str::to_owned)
        };
        match method {
            "thread/started" => {
                let thread_id = params
                    .get("thread")
                    .and_then(|value| value.get("id"))
                    .and_then(Value::as_str)
                    .context("thread/started missing thread id")?
                    .to_owned();
                Ok(Some(CodexThreadEvent::ThreadStarted { thread_id }))
            }
            "turn/started" => Ok(Some(CodexThreadEvent::TurnStarted {
                turn_id: params
                    .get("turn")
                    .and_then(|value| value.get("id"))
                    .and_then(Value::as_str)
                    .map(str::to_owned),
            })),
            "turn/completed" => {
                let turn = params.get("turn").cloned().unwrap_or(Value::Null);
                let turn_id = turn.get("id").and_then(Value::as_str).map(str::to_owned);
                match turn.get("status").and_then(Value::as_str) {
                    Some("failed") => Ok(Some(CodexThreadEvent::TurnFailed {
                        turn_id,
                        error: turn.get("error").cloned().unwrap_or(Value::Null),
                    })),
                    Some("interrupted") => Ok(Some(CodexThreadEvent::TurnInterrupted {
                        turn_id,
                        usage: turn.get("usage").cloned(),
                    })),
                    _ => Ok(Some(CodexThreadEvent::TurnCompleted {
                        turn_id,
                        usage: turn.get("usage").cloned(),
                    })),
                }
            }
            "error" => Ok(Some(CodexThreadEvent::Error {
                message: params
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown app-server error")
                    .to_owned(),
            })),
            "item/started" => Ok(Some(CodexThreadEvent::ItemStarted {
                turn_id: turn_id(),
                item: normalize_item(params.get("item").cloned().unwrap_or(Value::Null)),
            })),
            "item/completed" => {
                let item = normalize_item(params.get("item").cloned().unwrap_or(Value::Null));
                if item.get("type").and_then(Value::as_str) == Some("agent_message") {
                    if let (Some(item_id), Some(text)) = (
                        item.get("id").and_then(Value::as_str),
                        item.get("text").and_then(Value::as_str),
                    ) {
                        latest_agent_message_by_id.insert(item_id.to_owned(), text.to_owned());
                    }
                }
                Ok(Some(CodexThreadEvent::ItemCompleted {
                    turn_id: turn_id(),
                    item,
                }))
            }
            "item/agentMessage/delta" => {
                let item_id = params
                    .get("itemId")
                    .and_then(Value::as_str)
                    .context("item/agentMessage/delta missing itemId")?;
                let delta = params
                    .get("delta")
                    .and_then(Value::as_str)
                    .context("item/agentMessage/delta missing delta")?;
                let entry = latest_agent_message_by_id
                    .entry(item_id.to_owned())
                    .or_default();
                entry.push_str(delta);
                Ok(Some(CodexThreadEvent::ItemUpdated {
                    turn_id: turn_id(),
                    item: json!({
                        "type": "agent_message",
                        "id": item_id,
                        "text": entry,
                    }),
                }))
            }
            "item/plan/delta" => {
                let item_id = params
                    .get("itemId")
                    .and_then(Value::as_str)
                    .context("item/plan/delta missing itemId")?;
                let delta = params
                    .get("delta")
                    .and_then(Value::as_str)
                    .context("item/plan/delta missing delta")?;
                let entry = latest_plan_by_id.entry(item_id.to_owned()).or_default();
                entry.push_str(delta);
                Ok(Some(CodexThreadEvent::ItemUpdated {
                    turn_id: turn_id(),
                    item: json!({
                        "type": "plan",
                        "id": item_id,
                        "text": entry,
                    }),
                }))
            }
            _ => Ok(None),
        }
    }

    pub async fn run(
        &self,
        workspace: &CodexWorkspace,
        existing_thread_id: Option<&str>,
        input: Vec<CodexInputItem>,
    ) -> Result<CodexRunResult> {
        self.run_with_events(workspace, existing_thread_id, None, input, |_| async {})
            .await
    }

    pub async fn run_locked(
        &self,
        workspace: &CodexWorkspace,
        locked_thread_id: &str,
        input: Vec<CodexInputItem>,
    ) -> Result<CodexRunResult> {
        let result = self.run(workspace, Some(locked_thread_id), input).await?;
        self.ensure_locked_thread_id(locked_thread_id, result)
    }

    pub async fn run_locked_with_events<F, Fut>(
        &self,
        workspace: &CodexWorkspace,
        locked_thread_id: &str,
        input: Vec<CodexInputItem>,
        on_event: F,
    ) -> Result<CodexRunResult>
    where
        F: FnMut(CodexThreadEvent) -> Fut,
        Fut: Future<Output = ()>,
    {
        self.run_locked_with_events_and_mode(workspace, locked_thread_id, None, input, on_event)
            .await
    }

    pub async fn run_locked_with_events_and_mode<F, Fut>(
        &self,
        workspace: &CodexWorkspace,
        locked_thread_id: &str,
        execution_mode: Option<ExecutionMode>,
        input: Vec<CodexInputItem>,
        on_event: F,
    ) -> Result<CodexRunResult>
    where
        F: FnMut(CodexThreadEvent) -> Fut,
        Fut: Future<Output = ()>,
    {
        let result = self
            .run_with_events(
                workspace,
                Some(locked_thread_id),
                execution_mode,
                input,
                on_event,
            )
            .await?;
        self.ensure_locked_thread_id(locked_thread_id, result)
    }

    pub async fn run_prompt(
        &self,
        workspace: &CodexWorkspace,
        existing_thread_id: Option<&str>,
        prompt: &str,
    ) -> Result<CodexRunResult> {
        self.run(
            workspace,
            existing_thread_id,
            vec![CodexInputItem::Text {
                text: prompt.to_owned(),
            }],
        )
        .await
    }

    pub async fn run_locked_prompt(
        &self,
        workspace: &CodexWorkspace,
        locked_thread_id: &str,
        prompt: &str,
    ) -> Result<CodexRunResult> {
        self.run_locked(
            workspace,
            locked_thread_id,
            vec![CodexInputItem::Text {
                text: prompt.to_owned(),
            }],
        )
        .await
    }

    pub async fn run_locked_prompt_with_events<F, Fut>(
        &self,
        workspace: &CodexWorkspace,
        locked_thread_id: &str,
        prompt: &str,
        on_event: F,
    ) -> Result<CodexRunResult>
    where
        F: FnMut(CodexThreadEvent) -> Fut,
        Fut: Future<Output = ()>,
    {
        self.run_locked_prompt_with_events_and_mode(
            workspace,
            locked_thread_id,
            None,
            prompt,
            on_event,
        )
        .await
    }

    pub async fn run_locked_prompt_with_events_and_mode<F, Fut>(
        &self,
        workspace: &CodexWorkspace,
        locked_thread_id: &str,
        execution_mode: Option<ExecutionMode>,
        prompt: &str,
        on_event: F,
    ) -> Result<CodexRunResult>
    where
        F: FnMut(CodexThreadEvent) -> Fut,
        Fut: Future<Output = ()>,
    {
        self.run_locked_with_events_and_mode(
            workspace,
            locked_thread_id,
            execution_mode,
            vec![CodexInputItem::Text {
                text: prompt.to_owned(),
            }],
            on_event,
        )
        .await
    }

    pub(crate) async fn run_locked_prompt_with_events_mode_and_requests<F, Fut, H, Hf>(
        &self,
        workspace: &CodexWorkspace,
        locked_thread_id: &str,
        execution_mode: Option<ExecutionMode>,
        collaboration_mode: Option<CollaborationMode>,
        prompt: &str,
        on_event: F,
        on_server_request: H,
    ) -> Result<CodexRunResult>
    where
        F: FnMut(CodexThreadEvent) -> Fut,
        Fut: Future<Output = ()>,
        H: FnMut(CodexServerRequest) -> Hf,
        Hf: Future<Output = Result<Option<Value>>>,
    {
        let result = self
            .run_with_events_and_server_requests(
                workspace,
                Some(locked_thread_id),
                execution_mode,
                collaboration_mode,
                vec![CodexInputItem::Text {
                    text: prompt.to_owned(),
                }],
                on_event,
                on_server_request,
            )
            .await?;
        self.ensure_locked_thread_id(locked_thread_id, result)
    }

    pub async fn generate_thread_title_from_session(
        &self,
        workspace: &CodexWorkspace,
        existing_thread_id: &str,
    ) -> Result<CodexRunResult> {
        let prompt = [
            "Generate a concise Telegram thread title for our conversation so far.",
            "Rules:",
            "- Return only the title.",
            "- Use the same language as the conversation when possible.",
            "- Keep it under 48 characters.",
            "- No quotes, no markdown, no emojis unless the conversation clearly needs one.",
        ]
        .join("\n");
        self.run_locked_prompt(workspace, existing_thread_id, &prompt)
            .await
    }

    pub async fn interrupt_turn(
        &self,
        workspace: &CodexWorkspace,
        thread_id: &str,
        turn_id: &str,
    ) -> Result<()> {
        let mut client = AppServerClient::start(workspace).await?;
        let _ = client
            .request_simple(
                "turn/interrupt",
                Self::build_turn_interrupt_params(thread_id, turn_id),
            )
            .await?;
        Ok(())
    }

    pub async fn steer_turn(
        &self,
        workspace: &CodexWorkspace,
        thread_id: &str,
        expected_turn_id: &str,
        input: Vec<CodexInputItem>,
    ) -> Result<String> {
        let mut client = AppServerClient::start(workspace).await?;
        let result = client
            .request_simple(
                "turn/steer",
                Self::build_turn_steer_params(thread_id, expected_turn_id, &input),
            )
            .await?;
        result
            .get("turnId")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .context("turn/steer result missing turnId")
    }

    pub async fn read_thread_run_state(
        &self,
        workspace: &CodexWorkspace,
        thread_id: &str,
    ) -> Result<BackendThreadRunState> {
        let mut client = AppServerClient::start(workspace).await?;
        let result = client
            .request_simple(
                "threadbridge/getThreadRunState",
                json!({
                    "threadId": thread_id,
                }),
            )
            .await?;
        serde_json::from_value(result).context("invalid threadbridge/getThreadRunState result")
    }

    pub async fn respond_server_request(
        &self,
        workspace: &CodexWorkspace,
        thread_id: &str,
        request_id: i64,
        response: &Value,
    ) -> Result<()> {
        let mut client = AppServerClient::start(workspace).await?;
        let _ = client
            .request_simple(
                "threadbridge/respondServerRequest",
                json!({
                    "threadId": thread_id,
                    "requestId": request_id,
                    "response": response,
                }),
            )
            .await?;
        Ok(())
    }

    pub async fn respond_request_user_input(
        &self,
        workspace: &CodexWorkspace,
        thread_id: &str,
        request_id: i64,
        response: &ToolRequestUserInputResponse,
    ) -> Result<()> {
        self.respond_server_request(
            workspace,
            thread_id,
            request_id,
            &serde_json::to_value(response)?,
        )
        .await
    }

    fn ensure_locked_thread_id(
        &self,
        locked_thread_id: &str,
        result: CodexRunResult,
    ) -> Result<CodexRunResult> {
        if result.thread_id != locked_thread_id || result.thread_id_changed {
            bail!(
                "Codex session continuity changed: expected thread {}, got {}",
                locked_thread_id,
                result.thread_id
            );
        }
        Ok(result)
    }

    async fn run_turn_on_client<F, Fut, H, Hf>(
        &self,
        client: &mut AppServerClient,
        binding: &CodexThreadBinding,
        input: Vec<CodexInputItem>,
        collaboration_mode: Option<CollaborationMode>,
        mut on_event: F,
        mut on_server_request: H,
    ) -> Result<TurnRunResult>
    where
        F: FnMut(CodexThreadEvent) -> Fut,
        Fut: Future<Output = ()>,
        H: FnMut(CodexServerRequest) -> Hf,
        Hf: Future<Output = Result<Option<Value>>>,
    {
        let collaboration_mode = self
            .resolve_collaboration_mode_payload(client, binding, collaboration_mode)
            .await?;
        let request_id = client
            .send_request(
                "turn/start",
                Self::build_turn_start_params(&binding.thread_id, &input, collaboration_mode),
            )
            .await?;

        let mut request_acked = false;
        let mut turn_completed = false;
        let mut turn_outcome = CodexTurnOutcome::Completed;
        let mut final_response = String::new();
        let mut latest_agent_message_by_id: HashMap<String, String> = HashMap::new();
        let mut latest_plan_by_id: HashMap<String, String> = HashMap::new();
        let mut final_plan_text: Option<String> = None;

        while !(request_acked && turn_completed) {
            match client.read_message().await? {
                RpcMessage::Response { id, result } if id == request_id => {
                    request_acked = true;
                    let turn_id = result
                        .get("turn")
                        .and_then(|turn| turn.get("id"))
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                    on_event(CodexThreadEvent::TurnStarted { turn_id }).await;
                }
                RpcMessage::Error { id, message, data } if id == request_id => {
                    let details = data.map(|value| value.to_string()).unwrap_or_default();
                    if details.is_empty() {
                        bail!("turn/start failed: {message}");
                    }
                    bail!("turn/start failed: {message} ({details})");
                }
                RpcMessage::Notification { method, params } => {
                    if let Some(notification) = Self::map_server_notification(
                        &method,
                        params.clone().unwrap_or(Value::Null),
                    ) {
                        match notification {
                            CodexServerNotification::ServerRequestResolved(resolved) => {
                                debug!(
                                    event = "codex.app_server.request.resolved",
                                    thread_id = %resolved.thread_id,
                                    request_id = %resolved.request_id,
                                );
                            }
                        }
                    }
                    if let Some(event) = Self::map_notification(
                        &method,
                        params.unwrap_or(Value::Null),
                        &mut latest_agent_message_by_id,
                        &mut latest_plan_by_id,
                    )? {
                        match &event {
                            CodexThreadEvent::ItemStarted { item, .. } => {
                                log_item_event("started", item)
                            }
                            CodexThreadEvent::ItemCompleted { item, .. } => {
                                log_item_event("completed", item);
                                if item.get("type").and_then(Value::as_str) == Some("agent_message")
                                {
                                    if let Some(text) = item.get("text").and_then(Value::as_str) {
                                        final_response = text.to_owned();
                                    }
                                } else if item.get("type").and_then(Value::as_str) == Some("plan") {
                                    final_plan_text = item
                                        .get("text")
                                        .and_then(Value::as_str)
                                        .map(str::trim)
                                        .filter(|text| !text.is_empty())
                                        .map(str::to_owned);
                                }
                            }
                            CodexThreadEvent::ItemUpdated { item, .. } => {
                                if item.get("type").and_then(Value::as_str) == Some("agent_message")
                                {
                                    if let Some(text) = item.get("text").and_then(Value::as_str) {
                                        final_response = text.to_owned();
                                    }
                                }
                            }
                            CodexThreadEvent::TurnCompleted { .. } => {
                                turn_completed = true;
                                turn_outcome = CodexTurnOutcome::Completed;
                            }
                            CodexThreadEvent::TurnInterrupted { .. } => {
                                turn_completed = true;
                                turn_outcome = CodexTurnOutcome::Interrupted;
                            }
                            CodexThreadEvent::TurnFailed { error, .. } => {
                                turn_completed = true;
                                turn_outcome = CodexTurnOutcome::Failed;
                                if !error.is_null() {
                                    final_response = error.to_string();
                                }
                            }
                            CodexThreadEvent::Error { .. }
                            | CodexThreadEvent::TurnStarted { .. }
                            | CodexThreadEvent::ThreadStarted { .. } => {}
                        }
                        on_event(event).await;
                    }
                }
                RpcMessage::Request { id, method, params } => match method.as_str() {
                    "item/commandExecution/requestApproval" => {
                        let params: CommandExecutionRequestApprovalParams = serde_json::from_value(
                            params.unwrap_or(Value::Null),
                        )
                        .with_context(|| {
                            "invalid item/commandExecution/requestApproval params".to_owned()
                        })?;
                        if let Some(response) =
                            on_server_request(CodexServerRequest::CommandExecutionRequestApproval {
                                request_id: id,
                                params,
                            })
                            .await?
                        {
                            client.send_server_request_response(id, &response).await?;
                        } else {
                            client.reject_server_request(id, &method).await?;
                        }
                    }
                    "item/fileChange/requestApproval" => {
                        let params: FileChangeRequestApprovalParams =
                            serde_json::from_value(params.unwrap_or(Value::Null)).with_context(
                                || "invalid item/fileChange/requestApproval params".to_owned(),
                            )?;
                        if let Some(response) =
                            on_server_request(CodexServerRequest::FileChangeRequestApproval {
                                request_id: id,
                                params,
                            })
                            .await?
                        {
                            client.send_server_request_response(id, &response).await?;
                        } else {
                            client.reject_server_request(id, &method).await?;
                        }
                    }
                    "item/permissions/requestApproval" => {
                        let params: PermissionsRequestApprovalParams =
                            serde_json::from_value(params.unwrap_or(Value::Null)).with_context(
                                || "invalid item/permissions/requestApproval params".to_owned(),
                            )?;
                        if let Some(response) =
                            on_server_request(CodexServerRequest::PermissionsRequestApproval {
                                request_id: id,
                                params,
                            })
                            .await?
                        {
                            client.send_server_request_response(id, &response).await?;
                        } else {
                            client.reject_server_request(id, &method).await?;
                        }
                    }
                    "item/tool/requestUserInput" => {
                        let params: ToolRequestUserInputParams = serde_json::from_value(
                            params.unwrap_or(Value::Null),
                        )
                        .with_context(|| "invalid item/tool/requestUserInput params".to_owned())?;
                        if let Some(response) =
                            on_server_request(CodexServerRequest::RequestUserInput {
                                request_id: id,
                                params,
                            })
                            .await?
                        {
                            client.send_server_request_response(id, &response).await?;
                        } else {
                            client.reject_server_request(id, &method).await?;
                        }
                    }
                    _ => {
                        client.reject_server_request(id, &method).await?;
                    }
                },
                RpcMessage::Response { .. } | RpcMessage::Error { .. } => {}
            }
        }

        Ok(TurnRunResult {
            final_response,
            final_plan_text,
            outcome: turn_outcome,
        })
    }

    pub(crate) fn map_server_notification(
        method: &str,
        params: Value,
    ) -> Option<CodexServerNotification> {
        match method {
            "serverRequest/resolved" => serde_json::from_value(params)
                .ok()
                .map(CodexServerNotification::ServerRequestResolved),
            _ => None,
        }
    }

    fn ensure_ready_response(&self, response: &str, context: &str) -> Result<()> {
        if response.trim() != "READY" {
            bail!("{context} did not return READY: {}", response.trim());
        }
        Ok(())
    }
}

pub(crate) async fn observe_thread_with_handlers<F, Fut, H, Hf, N, Nf>(
    app_server_url: &str,
    thread_id: &str,
    mut on_event: F,
    mut on_server_request: H,
    mut on_server_notification: N,
    mut shutdown_rx: Option<oneshot::Receiver<()>>,
) -> Result<()>
where
    F: FnMut(CodexThreadEvent) -> Fut,
    Fut: Future<Output = Result<()>>,
    H: FnMut(CodexServerRequest) -> Hf,
    Hf: Future<Output = Result<()>>,
    N: FnMut(CodexServerNotification) -> Nf,
    Nf: Future<Output = Result<()>>,
{
    let mut client = AppServerClient::start_websocket(app_server_url).await?;
    let result = client
        .request_simple(
            "threadbridge/subscribeThread",
            json!({
                "threadId": thread_id,
            }),
        )
        .await?;
    let binding = CodexRunner::parse_binding(&result)?;
    if binding.thread_id != thread_id {
        bail!(
            "Codex thread continuity changed while attaching observer: expected {}, got {}",
            thread_id,
            binding.thread_id
        );
    }
    on_event(CodexThreadEvent::ThreadStarted {
        thread_id: binding.thread_id.clone(),
    })
    .await?;

    let mut latest_agent_message_by_id: HashMap<String, String> = HashMap::new();
    let mut latest_plan_by_id: HashMap<String, String> = HashMap::new();
    loop {
        let message = if let Some(rx) = shutdown_rx.as_mut() {
            tokio::select! {
                _ = rx => {
                    if let Err(error) = request_observer_unsubscribe(&mut client, thread_id).await {
                        warn!(
                            event = "codex.observe_thread.unsubscribe_failed",
                            thread_id = %thread_id,
                            error = %error,
                        );
                    }
                    return Ok(());
                }
                message = client.read_message() => message?,
            }
        } else {
            client.read_message().await?
        };

        match message {
            RpcMessage::Notification { method, params } => {
                if let Some(notification) = CodexRunner::map_server_notification(
                    &method,
                    params.clone().unwrap_or(Value::Null),
                ) {
                    on_server_notification(notification).await?;
                }
                if let Some(event) = CodexRunner::map_notification(
                    &method,
                    params.unwrap_or(Value::Null),
                    &mut latest_agent_message_by_id,
                    &mut latest_plan_by_id,
                )? {
                    on_event(event).await?;
                }
            }
            RpcMessage::Request { id, method, params } => {
                let params = params.unwrap_or(Value::Null);
                let request = match method.as_str() {
                    "item/commandExecution/requestApproval" => {
                        Some(CodexServerRequest::CommandExecutionRequestApproval {
                            request_id: id,
                            params: serde_json::from_value(params).with_context(|| {
                                "invalid item/commandExecution/requestApproval params".to_owned()
                            })?,
                        })
                    }
                    "item/fileChange/requestApproval" => {
                        Some(CodexServerRequest::FileChangeRequestApproval {
                            request_id: id,
                            params: serde_json::from_value(params).with_context(|| {
                                "invalid item/fileChange/requestApproval params".to_owned()
                            })?,
                        })
                    }
                    "item/permissions/requestApproval" => {
                        Some(CodexServerRequest::PermissionsRequestApproval {
                            request_id: id,
                            params: serde_json::from_value(params).with_context(|| {
                                "invalid item/permissions/requestApproval params".to_owned()
                            })?,
                        })
                    }
                    "item/tool/requestUserInput" => Some(CodexServerRequest::RequestUserInput {
                        request_id: id,
                        params: serde_json::from_value(params).with_context(|| {
                            "invalid item/tool/requestUserInput params".to_owned()
                        })?,
                    }),
                    _ => None,
                };
                if let Some(request) = request {
                    on_server_request(request).await?;
                }
            }
            RpcMessage::Response { .. } | RpcMessage::Error { .. } => {}
        }
    }
}

async fn request_observer_unsubscribe(client: &mut AppServerClient, thread_id: &str) -> Result<()> {
    let _ = client
        .request_simple(
            "threadbridge/unsubscribeThread",
            json!({
                "threadId": thread_id,
            }),
        )
        .await?;
    Ok(())
}

fn normalize_item(item: Value) -> Value {
    let Some(item_type) = item
        .get("type")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
    else {
        return item;
    };
    let normalized_type = match item_type.as_str() {
        "userMessage" => "user_message",
        "agentMessage" => "agent_message",
        "commandExecution" => "command_execution",
        "fileChange" => "file_change",
        "mcpToolCall" => "mcp_tool_call",
        "dynamicToolCall" => "dynamic_tool_call",
        "collabAgentToolCall" => "collab_agent_tool_call",
        "webSearch" => "web_search",
        "imageView" => "image_view",
        "imageGeneration" => "image_generation",
        "enteredReviewMode" => "entered_review_mode",
        "exitedReviewMode" => "exited_review_mode",
        "contextCompaction" => "context_compaction",
        _ => item_type.as_str(),
    };

    let mut object = match item {
        Value::Object(object) => object,
        other => return other,
    };
    object.insert("type".to_owned(), Value::String(normalized_type.to_owned()));
    Value::Object(object)
}

#[cfg(test)]
mod tests {
    use super::{
        AppServerClient, CodexInputItem, CodexRunner, CodexThreadBinding, CodexThreadEvent,
        CodexWorkspace, Value, json, normalize_item,
    };
    use crate::collaboration_mode::CollaborationMode;
    use crate::execution_mode::{ExecutionMode, SessionExecutionSnapshot};
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use tokio::net::TcpListener;
    use tokio_tungstenite::accept_async;
    use tokio_tungstenite::tungstenite::Message as WsMessage;

    fn workspace() -> CodexWorkspace {
        CodexWorkspace {
            working_directory: PathBuf::from("/tmp/workspace"),
            app_server_url: None,
        }
    }

    async fn start_mock_app_server(
        workspace: PathBuf,
        thread_method: &'static str,
    ) -> anyhow::Result<(String, Arc<Mutex<Vec<String>>>)> {
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let seen_methods = Arc::new(Mutex::new(Vec::new()));
        tokio::spawn({
            let seen_methods = seen_methods.clone();
            async move {
                loop {
                    let Ok((stream, _)) = listener.accept().await else {
                        break;
                    };
                    let seen_methods = seen_methods.clone();
                    let workspace = workspace.clone();
                    tokio::spawn(async move {
                        let Ok(mut ws) = accept_async(stream).await else {
                            return;
                        };
                        while let Some(message) = futures_util::StreamExt::next(&mut ws).await {
                            let Ok(message) = message else {
                                break;
                            };
                            let WsMessage::Text(text) = message else {
                                continue;
                            };
                            let Ok(payload) = serde_json::from_str::<serde_json::Value>(&text)
                            else {
                                continue;
                            };
                            let Some(method) = payload
                                .get("method")
                                .and_then(serde_json::Value::as_str)
                                .map(str::to_owned)
                            else {
                                continue;
                            };
                            seen_methods.lock().unwrap().push(method.clone());
                            if payload.get("id").is_none() {
                                continue;
                            }
                            let id = payload["id"].as_i64().unwrap();
                            let response = match method.as_str() {
                                "initialize" => json!({
                                    "id": id,
                                    "result": { "protocolVersion": "2" },
                                }),
                                value if value == thread_method => json!({
                                    "id": id,
                                    "result": {
                                        "thread": {
                                            "id": "thr_resume",
                                            "cwd": workspace.display().to_string(),
                                        },
                                        "cwd": workspace.display().to_string(),
                                        "model": "gpt-test",
                                        "reasoningEffort": "medium",
                                        "approvalPolicy": "on-request",
                                        "sandbox": "workspace-write",
                                    },
                                }),
                                _ => json!({
                                    "id": id,
                                    "error": {
                                        "message": format!("unsupported method: {method}"),
                                    },
                                }),
                            };
                            let _ = futures_util::SinkExt::send(
                                &mut ws,
                                WsMessage::Text(response.to_string().into()),
                            )
                            .await;
                        }
                    });
                }
            }
        });
        Ok((format!("ws://127.0.0.1:{}", addr.port()), seen_methods))
    }

    #[test]
    fn normalize_input_keeps_text_unchanged() {
        let payload = CodexRunner::normalize_input(&[CodexInputItem::Text {
            text: "hello".to_owned(),
        }]);
        assert_eq!(payload.len(), 1);
        assert_eq!(payload[0]["type"], "text");
        assert_eq!(payload[0]["text_elements"], json!([]));
        assert_eq!(payload[0]["text"], "hello");
    }

    #[test]
    fn normalize_input_keeps_local_images() {
        let payload = CodexRunner::normalize_input(&[
            CodexInputItem::Text {
                text: "analyze".to_owned(),
            },
            CodexInputItem::LocalImage {
                path: "/tmp/example.png".to_owned(),
            },
        ]);
        assert_eq!(
            payload[1],
            json!({"type": "localImage", "path": "/tmp/example.png"})
        );
    }

    #[test]
    fn thread_start_params_use_full_auto_policy() {
        let runner = CodexRunner::new(Some("gpt-test".to_owned()));
        let params = runner
            .build_thread_start_params(&workspace().working_directory, ExecutionMode::FullAuto);
        assert_eq!(params["approvalPolicy"], "on-request");
        assert_eq!(params["sandbox"], "workspace-write");
        assert_eq!(params["cwd"], "/tmp/workspace");
        assert_eq!(params["model"], "gpt-test");
    }

    #[test]
    fn thread_start_params_use_yolo_policy() {
        let runner = CodexRunner::new(None);
        let params =
            runner.build_thread_start_params(&workspace().working_directory, ExecutionMode::Yolo);
        assert_eq!(params["approvalPolicy"], "never");
        assert_eq!(params["sandbox"], "danger-full-access");
    }

    #[tokio::test]
    async fn resume_session_uses_thread_resume() {
        let workspace_dir = PathBuf::from("/tmp/workspace");
        let (app_server_url, seen_methods) =
            start_mock_app_server(workspace_dir.clone(), "thread/resume")
                .await
                .unwrap();
        let runner = CodexRunner::new(None);
        let binding = runner
            .resume_session(
                &CodexWorkspace {
                    working_directory: workspace_dir.clone(),
                    app_server_url: Some(app_server_url),
                },
                "thr_resume",
                Some(ExecutionMode::FullAuto),
            )
            .await
            .unwrap();

        assert_eq!(binding.thread_id, "thr_resume");
        assert_eq!(binding.cwd, workspace_dir.display().to_string());
        assert_eq!(
            seen_methods.lock().unwrap().as_slice(),
            &["initialize", "initialized", "thread/resume"]
        );
    }

    #[tokio::test]
    async fn read_session_binding_uses_thread_read() {
        let workspace_dir = PathBuf::from("/tmp/workspace");
        let (app_server_url, seen_methods) =
            start_mock_app_server(workspace_dir.clone(), "thread/read")
                .await
                .unwrap();
        let runner = CodexRunner::new(None);
        let binding = runner
            .read_session_binding(
                &CodexWorkspace {
                    working_directory: workspace_dir.clone(),
                    app_server_url: Some(app_server_url),
                },
                "thr_resume",
            )
            .await
            .unwrap();

        assert_eq!(binding.thread_id, "thr_resume");
        assert_eq!(
            seen_methods.lock().unwrap().as_slice(),
            &["initialize", "initialized", "thread/read"]
        );
    }

    #[test]
    fn turn_start_params_keep_collaboration_mode_object() {
        let params = CodexRunner::build_turn_start_params(
            "thr_123",
            &[CodexInputItem::Text {
                text: "hello".to_owned(),
            }],
            Some(json!({
                "mode": "plan",
                "settings": {
                    "model": "gpt-test",
                    "reasoning_effort": "medium",
                    "developer_instructions": null,
                }
            })),
        );
        assert_eq!(params["collaborationMode"]["mode"], "plan");
        assert_eq!(
            params["collaborationMode"]["settings"]["reasoning_effort"],
            "medium"
        );
    }

    #[test]
    fn turn_interrupt_params_include_thread_and_turn_ids() {
        let params = CodexRunner::build_turn_interrupt_params("thr_123", "turn_456");
        assert_eq!(params["threadId"], "thr_123");
        assert_eq!(params["turnId"], "turn_456");
    }

    #[test]
    fn turn_steer_params_include_thread_expected_turn_and_input() {
        let params = CodexRunner::build_turn_steer_params(
            "thr_123",
            "turn_456",
            &[CodexInputItem::Text {
                text: "extra context".to_owned(),
            }],
        );
        assert_eq!(params["threadId"], "thr_123");
        assert_eq!(params["expectedTurnId"], "turn_456");
        assert_eq!(params["input"][0]["type"], "text");
        assert_eq!(params["input"][0]["text"], "extra context");
    }

    #[test]
    fn initialize_params_enable_experimental_api_with_camel_case() {
        let params = AppServerClient::initialize_params();
        assert_eq!(params["capabilities"]["experimentalApi"], true);
        assert!(params["capabilities"].get("experimental_api").is_none());
    }

    #[test]
    fn default_mode_requires_collaboration_mode_payload() {
        assert!(CodexRunner::requires_collaboration_mode_payload(Some(
            CollaborationMode::Default,
        )));
        assert!(!CodexRunner::requires_collaboration_mode_payload(None));
    }

    #[test]
    fn plan_mode_requires_collaboration_mode_payload() {
        assert!(CodexRunner::requires_collaboration_mode_payload(Some(
            CollaborationMode::Plan,
        )));
    }

    #[test]
    fn default_collaboration_payload_uses_binding_state() {
        let runner = CodexRunner::new(Some("gpt-runner".to_owned()));
        let payload = runner
            .resolve_default_collaboration_mode_payload(&CodexThreadBinding {
                thread_id: "thr_123".to_owned(),
                cwd: "/tmp/workspace".to_owned(),
                model: Some("gpt-session".to_owned()),
                reasoning_effort: Some("high".to_owned()),
                execution: SessionExecutionSnapshot::from_mode(ExecutionMode::FullAuto),
            })
            .unwrap();
        assert_eq!(payload["mode"], "default");
        assert_eq!(payload["settings"]["model"], "gpt-session");
        assert_eq!(payload["settings"]["reasoning_effort"], "high");
        assert!(payload["settings"]["developer_instructions"].is_null());
    }

    #[test]
    fn default_collaboration_payload_falls_back_to_runner_model() {
        let runner = CodexRunner::new(Some("gpt-runner".to_owned()));
        let payload = runner
            .resolve_default_collaboration_mode_payload(&CodexThreadBinding {
                thread_id: "thr_123".to_owned(),
                cwd: "/tmp/workspace".to_owned(),
                model: None,
                reasoning_effort: None,
                execution: SessionExecutionSnapshot::from_mode(ExecutionMode::FullAuto),
            })
            .unwrap();
        assert_eq!(payload["settings"]["model"], "gpt-runner");
        assert!(payload["settings"]["reasoning_effort"].is_null());
    }

    #[test]
    fn normalize_item_converts_known_types_to_snake_case() {
        let normalized = normalize_item(json!({
            "type": "commandExecution",
            "id": "cmd_1",
            "command": "ls"
        }));
        assert_eq!(normalized["type"], "command_execution");
        assert_eq!(normalized["command"], "ls");
    }

    #[test]
    fn normalize_item_leaves_unknown_type_unchanged() {
        let normalized = normalize_item(json!({
            "type": "reasoning",
            "id": "r_1"
        }));
        assert_eq!(normalized["type"], "reasoning");
    }

    #[test]
    fn turn_started_notification_carries_turn_id_when_present() {
        let event = CodexRunner::map_notification(
            "turn/started",
            json!({
                "turn": {
                    "id": "turn_123",
                }
            }),
            &mut std::collections::HashMap::new(),
            &mut std::collections::HashMap::new(),
        )
        .unwrap();
        assert!(matches!(
            event,
            Some(CodexThreadEvent::TurnStarted {
                turn_id: Some(ref turn_id)
            }) if turn_id == "turn_123"
        ));
    }

    #[test]
    fn interrupted_turn_completed_notification_maps_to_interrupted_event() {
        let event = CodexRunner::map_notification(
            "turn/completed",
            json!({
                "turn": {
                    "id": "turn_123",
                    "status": "interrupted",
                    "usage": {
                        "totalTokens": 12
                    }
                }
            }),
            &mut std::collections::HashMap::new(),
            &mut std::collections::HashMap::new(),
        )
        .unwrap();
        assert!(matches!(
            event,
            Some(CodexThreadEvent::TurnInterrupted {
                turn_id: Some(ref turn_id),
                ..
            }) if turn_id == "turn_123"
        ));
    }

    #[test]
    fn locked_thread_id_rejects_thread_drift() {
        let runner = CodexRunner::new(None);
        let result = runner.ensure_locked_thread_id(
            "thread-123",
            super::CodexRunResult {
                final_response: "ok".to_owned(),
                final_plan_text: None,
                turn_outcome: super::CodexTurnOutcome::Completed,
                selected_factory: "resumeThread".to_owned(),
                thread_id: "thread-999".to_owned(),
                thread_id_changed: true,
                execution: SessionExecutionSnapshot::from_mode(ExecutionMode::FullAuto),
            },
        );
        assert!(result.is_err());
    }

    #[test]
    fn parse_binding_reads_thread_id_and_cwd() {
        let binding = CodexRunner::parse_binding(&json!({
            "thread": {
                "id": "thr_123",
                "cwd": "/tmp/workspace"
            },
            "model": "gpt-test",
            "reasoningEffort": "high"
        }))
        .unwrap();
        assert_eq!(binding.thread_id, "thr_123");
        assert_eq!(binding.cwd, "/tmp/workspace");
        assert_eq!(binding.model.as_deref(), Some("gpt-test"));
        assert_eq!(binding.reasoning_effort.as_deref(), Some("high"));
        assert_eq!(binding.execution.execution_mode, None);
    }

    #[test]
    fn ensure_thread_run_state_idle_accepts_idle_state() {
        let run_state = super::BackendThreadRunState {
            thread_id: "thr_123".to_owned(),
            is_busy: false,
            active_turn_id: None,
            interruptible: false,
            phase: Some("idle".to_owned()),
            last_transition_at: None,
        };
        assert!(super::ensure_thread_run_state_idle("thr_123", &run_state).is_ok());
    }

    #[test]
    fn ensure_thread_run_state_idle_rejects_busy_state() {
        let run_state = super::BackendThreadRunState {
            thread_id: "thr_123".to_owned(),
            is_busy: true,
            active_turn_id: Some("turn_456".to_owned()),
            interruptible: false,
            phase: Some("turn_interrupt_requested".to_owned()),
            last_transition_at: None,
        };
        let error = super::ensure_thread_run_state_idle("thr_123", &run_state)
            .unwrap_err()
            .to_string();
        assert!(error.contains("thr_123"));
        assert!(error.contains("turn_456"));
        assert!(error.contains("turn_interrupt_requested"));
    }

    #[test]
    fn map_agent_message_delta_emits_item_updated() {
        let mut latest = std::collections::HashMap::new();
        let event = CodexRunner::map_notification(
            "item/agentMessage/delta",
            json!({
                "turnId": "turn-1",
                "itemId": "msg_1",
                "delta": "Hello"
            }),
            &mut latest,
            &mut std::collections::HashMap::new(),
        )
        .unwrap()
        .unwrap();

        match event {
            super::CodexThreadEvent::ItemUpdated { turn_id, item } => {
                assert_eq!(turn_id.as_deref(), Some("turn-1"));
                assert_eq!(item["type"], "agent_message");
                assert_eq!(item["text"], "Hello");
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn map_item_completed_preserves_turn_id() {
        let event = CodexRunner::map_notification(
            "item/completed",
            json!({
                "turnId": "turn-3",
                "item": {
                    "type": "agentMessage",
                    "id": "msg_1",
                    "text": "done"
                }
            }),
            &mut std::collections::HashMap::new(),
            &mut std::collections::HashMap::new(),
        )
        .unwrap()
        .unwrap();

        match event {
            super::CodexThreadEvent::ItemCompleted { turn_id, item } => {
                assert_eq!(turn_id.as_deref(), Some("turn-3"));
                assert_eq!(item["type"], "agent_message");
                assert_eq!(item["text"], "done");
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn normalize_item_handles_non_object_values() {
        let normalized = normalize_item(Value::String("oops".to_owned()));
        assert_eq!(normalized, Value::String("oops".to_owned()));
    }

    #[test]
    fn map_plan_delta_emits_item_updated() {
        let event = CodexRunner::map_notification(
            "item/plan/delta",
            json!({
                "turnId": "turn-2",
                "itemId": "plan_1",
                "delta": "# Plan\n"
            }),
            &mut std::collections::HashMap::new(),
            &mut std::collections::HashMap::new(),
        )
        .unwrap()
        .unwrap();

        match event {
            super::CodexThreadEvent::ItemUpdated { turn_id, item } => {
                assert_eq!(turn_id.as_deref(), Some("turn-2"));
                assert_eq!(item["type"], "plan");
                assert_eq!(item["text"], "# Plan\n");
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }
}
