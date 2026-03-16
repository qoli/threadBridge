use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tracing::{debug, info, warn};

const APP_SERVER_CLIENT_NAME: &str = "threadbridge";
const APP_SERVER_CLIENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const APP_SERVER_READY_PROMPT_PREFIX: &str =
    "Follow the workspace AGENTS.md, including any threadBridge-managed runtime appendix.";

#[derive(Debug, Clone, Serialize)]
pub struct CodexWorkspace {
    pub working_directory: PathBuf,
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
    TurnStarted,
    #[serde(rename = "turn.completed")]
    TurnCompleted { usage: Option<Value> },
    #[serde(rename = "turn.failed")]
    TurnFailed { error: Value },
    #[serde(rename = "error")]
    Error { message: String },
    #[serde(rename = "item.started")]
    ItemStarted { item: Value },
    #[serde(rename = "item.updated")]
    ItemUpdated { item: Value },
    #[serde(rename = "item.completed")]
    ItemCompleted { item: Value },
}

#[derive(Debug, Clone, Serialize)]
pub struct CodexRunResult {
    pub final_response: String,
    pub selected_factory: String,
    pub thread_id: String,
    pub thread_id_changed: bool,
}

#[derive(Debug, Clone)]
pub struct CodexThreadBinding {
    pub thread_id: String,
    pub cwd: String,
}

#[derive(Debug)]
struct AppServerClient {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<tokio::process::ChildStdout>,
    next_request_id: i64,
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
    },
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
    async fn start(workspace: &Path) -> Result<Self> {
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
            child,
            stdin,
            stdout: BufReader::new(stdout),
            next_request_id: 0,
        };
        client.initialize().await?;
        Ok(client)
    }

    async fn initialize(&mut self) -> Result<()> {
        let params = json!({
            "clientInfo": {
                "name": APP_SERVER_CLIENT_NAME,
                "title": null,
                "version": APP_SERVER_CLIENT_VERSION,
            },
            "capabilities": {
                "experimental_api": true,
            }
        });
        let _ = self.request_simple("initialize", params).await?;
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
        self.stdin
            .write_all(text.as_bytes())
            .await
            .context("failed writing to app-server stdin")?;
        self.stdin
            .write_all(b"\n")
            .await
            .context("failed writing newline to app-server stdin")?;
        self.stdin
            .flush()
            .await
            .context("failed flushing app-server stdin")
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

    async fn read_message(&mut self) -> Result<RpcMessage> {
        let mut line = String::new();
        let bytes = self
            .stdout
            .read_line(&mut line)
            .await
            .context("failed to read app-server stdout")?;
        if bytes == 0 {
            let status = self.child.wait().await.ok();
            bail!("codex app-server exited unexpectedly: {status:?}");
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
                return Ok(RpcMessage::Request { id, method });
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

        Err(anyhow!("unknown app-server message shape: {raw}"))
    }
}

impl CodexRunner {
    pub fn new(model: Option<String>) -> Self {
        Self { model }
    }

    fn build_prompt_text(prompt: &str) -> String {
        if prompt.trim().is_empty() {
            APP_SERVER_READY_PROMPT_PREFIX.to_owned()
        } else {
            format!("{APP_SERVER_READY_PROMPT_PREFIX}\n\n{prompt}")
        }
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
                    "text": Self::build_prompt_text(&texts.join("\n\n")),
                    "text_elements": [],
                }),
            );
        }

        payload
    }

    fn build_thread_start_params(&self, workspace: &Path) -> Value {
        json!({
            "model": self.model,
            "cwd": workspace.display().to_string(),
            "approvalPolicy": "never",
            "sandbox": "danger-full-access",
            "experimentalRawEvents": false,
            "persistExtendedHistory": false,
        })
    }

    fn build_thread_resume_params(thread_id: &str) -> Value {
        json!({
            "threadId": thread_id,
            "persistExtendedHistory": false,
        })
    }

    fn build_thread_read_params(thread_id: &str) -> Value {
        json!({
            "threadId": thread_id,
            "includeTurns": false,
        })
    }

    fn build_turn_start_params(thread_id: &str, input: &[CodexInputItem]) -> Value {
        json!({
            "threadId": thread_id,
            "input": Self::normalize_input(input),
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
        Ok(CodexThreadBinding { thread_id, cwd })
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
        let mut client = AppServerClient::start(&workspace.working_directory).await?;
        let result = client
            .request_simple(
                "thread/start",
                self.build_thread_start_params(&workspace.working_directory),
            )
            .await?;
        let binding = Self::parse_binding(&result)?;
        Self::ensure_workspace_cwd(workspace, &binding)?;
        Ok(binding)
    }

    pub async fn reconnect_session(
        &self,
        workspace: &CodexWorkspace,
        existing_thread_id: &str,
    ) -> Result<()> {
        let mut client = AppServerClient::start(&workspace.working_directory).await?;
        let result = client
            .request_simple(
                "thread/read",
                Self::build_thread_read_params(existing_thread_id),
            )
            .await?;
        let binding = Self::parse_binding(&result)?;
        if binding.thread_id != existing_thread_id {
            bail!(
                "Codex thread continuity changed: expected {}, got {}",
                existing_thread_id,
                binding.thread_id
            );
        }
        Self::ensure_workspace_cwd(workspace, &binding)
    }

    pub async fn run_with_events<F, Fut>(
        &self,
        workspace: &CodexWorkspace,
        existing_thread_id: Option<&str>,
        input: Vec<CodexInputItem>,
        mut on_event: F,
    ) -> Result<CodexRunResult>
    where
        F: FnMut(CodexThreadEvent) -> Fut,
        Fut: Future<Output = ()>,
    {
        let mut client = AppServerClient::start(&workspace.working_directory).await?;
        let (binding, selected_factory) = match existing_thread_id {
            Some(thread_id) => {
                let result = client
                    .request_simple("thread/resume", Self::build_thread_resume_params(thread_id))
                    .await?;
                let binding = Self::parse_binding(&result)?;
                if binding.thread_id != thread_id {
                    bail!(
                        "Codex thread continuity changed: expected {}, got {}",
                        thread_id,
                        binding.thread_id
                    );
                }
                (binding, "resumeThread")
            }
            None => {
                let result = client
                    .request_simple(
                        "thread/start",
                        self.build_thread_start_params(&workspace.working_directory),
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

        let request_id = client
            .send_request(
                "turn/start",
                Self::build_turn_start_params(&binding.thread_id, &input),
            )
            .await?;

        let mut request_acked = false;
        let mut turn_completed = false;
        let mut final_response = String::new();
        let mut latest_agent_message_by_id: HashMap<String, String> = HashMap::new();

        while !(request_acked && turn_completed) {
            match client.read_message().await? {
                RpcMessage::Response { id, .. } if id == request_id => {
                    request_acked = true;
                }
                RpcMessage::Error { id, message, data } if id == request_id => {
                    let details = data.map(|value| value.to_string()).unwrap_or_default();
                    if details.is_empty() {
                        bail!("turn/start failed: {message}");
                    }
                    bail!("turn/start failed: {message} ({details})");
                }
                RpcMessage::Notification { method, params } => {
                    if let Some(event) = Self::map_notification(
                        &method,
                        params.unwrap_or(Value::Null),
                        &mut latest_agent_message_by_id,
                    )? {
                        match &event {
                            CodexThreadEvent::ItemStarted { item } => {
                                log_item_event("started", item)
                            }
                            CodexThreadEvent::ItemCompleted { item } => {
                                log_item_event("completed", item);
                                if item.get("type").and_then(Value::as_str) == Some("agent_message")
                                {
                                    if let Some(text) = item.get("text").and_then(Value::as_str) {
                                        final_response = text.to_owned();
                                    }
                                }
                            }
                            CodexThreadEvent::ItemUpdated { item } => {
                                if item.get("type").and_then(Value::as_str) == Some("agent_message")
                                {
                                    if let Some(text) = item.get("text").and_then(Value::as_str) {
                                        final_response = text.to_owned();
                                    }
                                }
                            }
                            CodexThreadEvent::TurnCompleted { .. } => {
                                turn_completed = true;
                            }
                            CodexThreadEvent::TurnFailed { .. } => {
                                turn_completed = true;
                            }
                            CodexThreadEvent::Error { .. }
                            | CodexThreadEvent::TurnStarted
                            | CodexThreadEvent::ThreadStarted { .. } => {}
                        }
                        on_event(event).await;
                    }
                }
                RpcMessage::Request { id, method, .. } => {
                    client.reject_server_request(id, &method).await?;
                }
                RpcMessage::Response { .. } | RpcMessage::Error { .. } => {}
            }
        }

        Ok(CodexRunResult {
            final_response,
            selected_factory: selected_factory.to_owned(),
            thread_id_changed: existing_thread_id.is_some_and(|id| id != binding.thread_id),
            thread_id: binding.thread_id,
        })
    }

    fn map_notification(
        method: &str,
        params: Value,
        latest_agent_message_by_id: &mut HashMap<String, String>,
    ) -> Result<Option<CodexThreadEvent>> {
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
            "turn/started" => Ok(Some(CodexThreadEvent::TurnStarted)),
            "turn/completed" => {
                let turn = params.get("turn").cloned().unwrap_or(Value::Null);
                if turn.get("status").and_then(Value::as_str) == Some("failed") {
                    Ok(Some(CodexThreadEvent::TurnFailed {
                        error: turn.get("error").cloned().unwrap_or(Value::Null),
                    }))
                } else {
                    Ok(Some(CodexThreadEvent::TurnCompleted {
                        usage: turn.get("usage").cloned(),
                    }))
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
                Ok(Some(CodexThreadEvent::ItemCompleted { item }))
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
                    item: json!({
                        "type": "agent_message",
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
        self.run_with_events(workspace, existing_thread_id, input, |_| async {})
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
        let result = self
            .run_with_events(workspace, Some(locked_thread_id), input, on_event)
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
        self.run_locked_with_events(
            workspace,
            locked_thread_id,
            vec![CodexInputItem::Text {
                text: prompt.to_owned(),
            }],
            on_event,
        )
        .await
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
        APP_SERVER_READY_PROMPT_PREFIX, CodexInputItem, CodexRunner, CodexWorkspace, Value, json,
        normalize_item,
    };
    use std::path::PathBuf;

    fn workspace() -> CodexWorkspace {
        CodexWorkspace {
            working_directory: PathBuf::from("/tmp/workspace"),
        }
    }

    #[test]
    fn normalize_input_inserts_workspace_instruction_prefix() {
        let payload = CodexRunner::normalize_input(&[CodexInputItem::Text {
            text: "hello".to_owned(),
        }]);
        assert_eq!(payload.len(), 1);
        assert_eq!(payload[0]["type"], "text");
        assert_eq!(payload[0]["text_elements"], json!([]));
        let text = payload[0]["text"].as_str().unwrap();
        assert!(text.contains(APP_SERVER_READY_PROMPT_PREFIX));
        assert!(text.ends_with("hello"));
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
    fn thread_start_params_use_non_interactive_policy() {
        let runner = CodexRunner::new(Some("gpt-test".to_owned()));
        let params = runner.build_thread_start_params(&workspace().working_directory);
        assert_eq!(params["approvalPolicy"], "never");
        assert_eq!(params["sandbox"], "danger-full-access");
        assert_eq!(params["cwd"], "/tmp/workspace");
        assert_eq!(params["model"], "gpt-test");
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
    fn locked_thread_id_rejects_thread_drift() {
        let runner = CodexRunner::new(None);
        let result = runner.ensure_locked_thread_id(
            "thread-123",
            super::CodexRunResult {
                final_response: "ok".to_owned(),
                selected_factory: "resumeThread".to_owned(),
                thread_id: "thread-999".to_owned(),
                thread_id_changed: true,
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
            }
        }))
        .unwrap();
        assert_eq!(binding.thread_id, "thr_123");
        assert_eq!(binding.cwd, "/tmp/workspace");
    }

    #[test]
    fn map_agent_message_delta_emits_item_updated() {
        let mut latest = std::collections::HashMap::new();
        let event = CodexRunner::map_notification(
            "item/agentMessage/delta",
            json!({
                "itemId": "msg_1",
                "delta": "Hello"
            }),
            &mut latest,
        )
        .unwrap()
        .unwrap();

        match event {
            super::CodexThreadEvent::ItemUpdated { item } => {
                assert_eq!(item["type"], "agent_message");
                assert_eq!(item["text"], "Hello");
            }
            other => panic!("unexpected event: {other:?}"),
        }
    }

    #[test]
    fn normalize_item_handles_non_object_values() {
        let normalized = normalize_item(Value::String("oops".to_owned()));
        assert_eq!(normalized, Value::String("oops".to_owned()));
    }
}
