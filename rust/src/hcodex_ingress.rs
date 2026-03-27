use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};

use anyhow::{Context, Result, anyhow};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, mpsc};
use tokio::task::AbortHandle;
use tokio_tungstenite::accept_hdr_async;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
use tracing::{debug, info, warn};

use crate::app_server_observer::AppServerMirrorObserverManager;
use crate::app_server_runtime::{
    WorkspaceRuntimeState, consume_hcodex_launch_ticket, read_workspace_runtime_state_file,
    write_workspace_runtime_state_file,
};
use crate::collaboration_mode::CollaborationMode;
#[cfg(test)]
use crate::process_transcript::workspace_item_diagnostic;
use crate::repository::ThreadRepository;
use crate::runtime_interaction::RuntimeInteractionSender;
use crate::workspace_status::{
    ObserverAttachMode, record_hcodex_ingress_connected, record_hcodex_ingress_disconnected,
    record_hcodex_ingress_turn_started,
};

const INGRESS_HEALTH_PATH: &str = "/healthz";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrackedRequestMethod {
    ThreadResume,
    ThreadStart,
    TurnStart,
}

#[derive(Debug, Clone)]
pub struct HcodexIngressManager {
    repository: ThreadRepository,
    inner: Arc<Mutex<HashMap<String, WorkspaceHcodexIngress>>>,
    daemon_request_channels: Arc<Mutex<HashMap<String, mpsc::UnboundedSender<WsMessage>>>>,
    observer_runtime: AppServerMirrorObserverManager,
}

#[derive(Debug, Clone)]
struct WorkspaceHcodexIngress {
    workspace_path: PathBuf,
    daemon_ws_url: String,
    observer_ws_url: String,
    proxy_base_ws_url: String,
    abort_handle: AbortHandle,
}

impl HcodexIngressManager {
    pub fn new(repository: ThreadRepository) -> Self {
        let turn_modes = Arc::new(Mutex::new(HashMap::new()));
        Self {
            observer_runtime: AppServerMirrorObserverManager::new(turn_modes),
            repository,
            inner: Arc::new(Mutex::new(HashMap::new())),
            daemon_request_channels: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn configure_interaction_sender(&self, sender: RuntimeInteractionSender) {
        self.observer_runtime
            .set_interaction_sender(Some(sender))
            .await;
    }

    pub async fn submit_request_user_input_response<T: serde::Serialize>(
        &self,
        thread_key: &str,
        request_id: i64,
        response: &T,
    ) -> Result<()> {
        let payload = serde_json::to_string(&json!({
            "id": request_id,
            "result": serde_json::to_value(response)?,
        }))?;
        let channel = self
            .daemon_request_channels
            .lock()
            .await
            .get(thread_key)
            .cloned()
            .context("no live hcodex ingress channel for thread")?;
        channel
            .send(WsMessage::Text(payload))
            .map_err(|_| anyhow!("failed to forward response into live hcodex ingress"))?;
        Ok(())
    }

    pub async fn ensure_workspace_ingress(
        &self,
        workspace_path: &Path,
        daemon_ws_url: &str,
        observer_ws_url: &str,
    ) -> Result<WorkspaceRuntimeState> {
        let key = canonical_workspace_key(workspace_path)?;
        let mut inner = self.inner.lock().await;
        if let Some(existing) = inner.get(&key).cloned() {
            if should_reuse_existing_ingress(
                &existing.daemon_ws_url,
                daemon_ws_url,
                &existing.observer_ws_url,
                observer_ws_url,
                hcodex_ingress_endpoint_is_live(&existing.proxy_base_ws_url).await,
            ) {
                let existing_runtime_state =
                    read_workspace_runtime_state_file(&existing.workspace_path)
                        .await
                        .ok()
                        .flatten();
                let state = WorkspaceRuntimeState {
                    schema_version: 3,
                    workspace_cwd: existing.workspace_path.display().to_string(),
                    daemon_ws_url: existing.daemon_ws_url.clone(),
                    worker_ws_url: existing_runtime_state
                        .as_ref()
                        .and_then(|state| state.worker_ws_url.clone()),
                    worker_pid: existing_runtime_state.and_then(|state| state.worker_pid),
                    hcodex_ws_url: Some(existing.proxy_base_ws_url.clone()),
                };
                info!(
                    event = "hcodex_ingress.reuse",
                    workspace = %existing.workspace_path.display(),
                    daemon_ws_url = %existing.daemon_ws_url,
                    observer_ws_url = %existing.observer_ws_url,
                    proxy_base_ws_url = %existing.proxy_base_ws_url,
                    "reusing workspace hcodex ingress"
                );
                drop(inner);
                write_workspace_runtime_state_file(&existing.workspace_path, &state).await?;
                return Ok(state);
            }

            info!(
                event = "hcodex_ingress.rebuild",
                workspace = %existing.workspace_path.display(),
                previous_daemon_ws_url = %existing.daemon_ws_url,
                requested_daemon_ws_url = %daemon_ws_url,
                previous_observer_ws_url = %existing.observer_ws_url,
                requested_observer_ws_url = %observer_ws_url,
                previous_proxy_base_ws_url = %existing.proxy_base_ws_url,
                "rebuilding workspace hcodex ingress"
            );
            existing.abort_handle.abort();
            inner.remove(&key);
        }

        let workspace_path = workspace_path
            .canonicalize()
            .unwrap_or_else(|_| workspace_path.to_path_buf());
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .context("failed to bind hcodex ingress listener")?;
        let local_addr = listener
            .local_addr()
            .context("failed to read hcodex ingress listener addr")?;
        let proxy_base_ws_url = format!("ws://127.0.0.1:{}", local_addr.port());
        let daemon_ws_url = daemon_ws_url.to_owned();
        let observer_ws_url = observer_ws_url.to_owned();
        let repository = self.repository.clone();
        let daemon_request_channels = self.daemon_request_channels.clone();
        let observer_runtime = self.observer_runtime.clone();
        let workspace_for_task = workspace_path.clone();
        let daemon_ws_url_for_task = daemon_ws_url.clone();
        let observer_ws_url_for_task = observer_ws_url.clone();

        let listener_task = tokio::spawn(async move {
            if let Err(error) = run_ingress_listener(
                listener,
                repository,
                daemon_request_channels,
                observer_runtime,
                workspace_for_task,
                daemon_ws_url_for_task,
                observer_ws_url_for_task,
            )
            .await
            {
                warn!(event = "hcodex_ingress.listener.failed", error = %error);
            }
        });
        let abort_handle = listener_task.abort_handle();

        let runtime = WorkspaceHcodexIngress {
            workspace_path: workspace_path.clone(),
            daemon_ws_url: daemon_ws_url.to_owned(),
            observer_ws_url: observer_ws_url.to_owned(),
            proxy_base_ws_url: proxy_base_ws_url.clone(),
            abort_handle,
        };
        let existing_runtime_state = read_workspace_runtime_state_file(&workspace_path)
            .await
            .ok()
            .flatten();
        let state = WorkspaceRuntimeState {
            schema_version: 3,
            workspace_cwd: workspace_path.display().to_string(),
            daemon_ws_url: runtime.daemon_ws_url.clone(),
            worker_ws_url: existing_runtime_state
                .as_ref()
                .and_then(|state| state.worker_ws_url.clone()),
            worker_pid: existing_runtime_state.and_then(|state| state.worker_pid),
            hcodex_ws_url: Some(runtime.proxy_base_ws_url.clone()),
        };
        info!(
            event = "hcodex_ingress.spawned",
            workspace = %workspace_path.display(),
            daemon_ws_url = %runtime.daemon_ws_url,
            observer_ws_url = %runtime.observer_ws_url,
            proxy_base_ws_url = %runtime.proxy_base_ws_url,
            "spawned workspace hcodex ingress"
        );
        inner.insert(key, runtime);
        drop(inner);
        write_workspace_runtime_state_file(&workspace_path, &state).await?;
        Ok(state)
    }
}

pub async fn hcodex_ingress_endpoint_is_live(url: &str) -> bool {
    let probe_url = format!("{url}{INGRESS_HEALTH_PATH}");
    connect_async(&probe_url).await.is_ok()
}

fn should_reuse_existing_ingress(
    existing_daemon_ws_url: &str,
    requested_daemon_ws_url: &str,
    existing_observer_ws_url: &str,
    requested_observer_ws_url: &str,
    proxy_alive: bool,
) -> bool {
    existing_daemon_ws_url == requested_daemon_ws_url
        && existing_observer_ws_url == requested_observer_ws_url
        && proxy_alive
}

async fn run_ingress_listener(
    listener: TcpListener,
    repository: ThreadRepository,
    daemon_request_channels: Arc<Mutex<HashMap<String, mpsc::UnboundedSender<WsMessage>>>>,
    observer_runtime: AppServerMirrorObserverManager,
    workspace_path: PathBuf,
    daemon_ws_url: String,
    observer_ws_url: String,
) -> Result<()> {
    loop {
        let (stream, remote_addr) = listener.accept().await?;
        let repository = repository.clone();
        let daemon_request_channels = daemon_request_channels.clone();
        let observer_runtime = observer_runtime.clone();
        let workspace_path = workspace_path.clone();
        let daemon_ws_url = daemon_ws_url.clone();
        let observer_ws_url = observer_ws_url.clone();
        tokio::spawn(async move {
            if let Err(error) = handle_ingress_connection(
                stream,
                remote_addr,
                repository,
                daemon_request_channels,
                observer_runtime,
                workspace_path,
                daemon_ws_url,
                observer_ws_url,
            )
            .await
            {
                warn!(event = "hcodex_ingress.connection.failed", error = %error);
            }
        });
    }
}

async fn handle_ingress_connection(
    stream: TcpStream,
    remote_addr: SocketAddr,
    repository: ThreadRepository,
    daemon_request_channels: Arc<Mutex<HashMap<String, mpsc::UnboundedSender<WsMessage>>>>,
    observer_runtime: AppServerMirrorObserverManager,
    workspace_path: PathBuf,
    daemon_ws_url: String,
    observer_ws_url: String,
) -> Result<()> {
    let captured_path = Arc::new(StdMutex::new(None::<(String, Option<String>)>));
    let captured_path_clone = captured_path.clone();
    let client_ws = accept_hdr_async(stream, move |request: &Request, response: Response| {
        *captured_path_clone.lock().expect("capture request path") = Some((
            request.uri().path().to_owned(),
            request.uri().query().map(str::to_owned),
        ));
        Ok(response)
    })
    .await
    .context("failed to accept hcodex ingress websocket")?;
    let (path, query) = captured_path
        .lock()
        .expect("read request path")
        .clone()
        .unwrap_or_default();
    if path == INGRESS_HEALTH_PATH {
        let mut client_ws = client_ws;
        let _ = client_ws.close(None).await;
        return Ok(());
    }
    let thread_key =
        resolve_thread_key_from_ticket(&workspace_path, &path, query.as_deref()).await?;
    let (mut daemon_ws, _) = connect_async(&daemon_ws_url).await.with_context(|| {
        format!("failed to connect hcodex ingress to daemon at {daemon_ws_url}")
    })?;
    let mut client_ws = client_ws;
    let (injected_tx, mut injected_rx) = mpsc::unbounded_channel();
    daemon_request_channels
        .lock()
        .await
        .insert(thread_key.clone(), injected_tx);
    let mut tracked_request_method_by_id: HashMap<i64, TrackedRequestMethod> = HashMap::new();
    let mut tracked_turn_mode_by_request_id: HashMap<i64, CollaborationMode> = HashMap::new();
    let mut current_session_id: Option<String> = None;

    info!(
        event = "hcodex_ingress.connection.accepted",
        remote_addr = %remote_addr,
        workspace = %workspace_path.display(),
        thread_key = %thread_key,
        daemon_ws_url = %daemon_ws_url,
    );

    loop {
        tokio::select! {
            client_message = client_ws.next() => {
                let Some(client_message) = client_message else {
                    break;
                };
                let client_message = client_message.context("failed to read hcodex client websocket message")?;
                maybe_track_turn_mode_request(&client_message, &mut tracked_turn_mode_by_request_id)?;
                if let Some((request_id, method)) = track_client_request(&client_message)? {
                    tracked_request_method_by_id.insert(request_id, method);
                }
                if matches!(client_message, WsMessage::Close(_)) {
                    let _ = daemon_ws.send(client_message).await;
                    break;
                }
                daemon_ws
                    .send(client_message)
                    .await
                    .context("failed to forward hcodex client message to daemon")?;
            }
            daemon_message = daemon_ws.next() => {
                let Some(daemon_message) = daemon_message else {
                    break;
                };
                let daemon_message = daemon_message.context("failed to read daemon websocket message")?;
                if let Some(session_id) = maybe_track_server_response(
                    &repository,
                    &observer_runtime,
                    &thread_key,
                    &workspace_path,
                    &observer_ws_url,
                    &daemon_message,
                    &mut tracked_request_method_by_id,
                    &mut tracked_turn_mode_by_request_id,
                )
                .await? {
                    current_session_id = Some(session_id);
                }
                if matches!(daemon_message, WsMessage::Close(_)) {
                    let _ = client_ws.send(daemon_message).await;
                    break;
                }
                client_ws
                    .send(daemon_message)
                    .await
                    .context("failed to forward daemon websocket message to hcodex client")?;
            }
            injected_message = injected_rx.recv() => {
                let Some(injected_message) = injected_message else {
                    break;
                };
                daemon_ws
                    .send(injected_message)
                    .await
                    .context("failed to forward injected message to daemon")?;
            }
        }
    }

    daemon_request_channels.lock().await.remove(&thread_key);
    observer_runtime
        .stop_thread_observer(&workspace_path, &thread_key)
        .await;
    record_hcodex_ingress_disconnected(&workspace_path, &thread_key, current_session_id.as_deref())
        .await?;
    debug!(
        event = "hcodex_ingress.connection.closed",
        workspace = %workspace_path.display(),
        thread_key = %thread_key,
    );
    Ok(())
}

async fn resolve_thread_key_from_ticket(
    workspace_path: &Path,
    path: &str,
    query: Option<&str>,
) -> Result<String> {
    if path != "/" {
        return Err(anyhow!("hcodex ingress path must be /"));
    }
    let Some(query) = query else {
        return Err(anyhow!("hcodex ingress launch is missing launch_ticket"));
    };
    let ticket = query
        .split('&')
        .find_map(|part| {
            part.split_once('=')
                .filter(|(key, _)| *key == "launch_ticket")
        })
        .map(|(_, value)| value)
        .filter(|value| !value.trim().is_empty())
        .context("hcodex ingress launch is missing launch_ticket")?;
    // launch_ticket is one-shot. If a later local reconnect reaches this path
    // again, the bridge has already lost the original upstream session.
    let ticket = consume_hcodex_launch_ticket(workspace_path, ticket)
        .await?
        .context("hcodex ingress launch ticket is invalid or already used")?;
    Ok(ticket.thread_key)
}

fn track_client_request(message: &WsMessage) -> Result<Option<(i64, TrackedRequestMethod)>> {
    let text = match message {
        WsMessage::Text(text) => text.as_str(),
        WsMessage::Binary(bytes) => {
            std::str::from_utf8(bytes).context("invalid utf8 client frame")?
        }
        _ => return Ok(None),
    };
    let payload: Value =
        serde_json::from_str(text).context("invalid hcodex ingress client json")?;
    let Some(method) = payload.get("method").and_then(Value::as_str) else {
        return Ok(None);
    };
    if !matches!(method, "thread/resume" | "thread/start" | "turn/start") {
        return Ok(None);
    }
    let Some(request_id) = payload.get("id").and_then(Value::as_i64) else {
        return Ok(None);
    };
    let method = match method {
        "thread/resume" => TrackedRequestMethod::ThreadResume,
        "thread/start" => TrackedRequestMethod::ThreadStart,
        "turn/start" => TrackedRequestMethod::TurnStart,
        _ => return Ok(None),
    };
    Ok(Some((request_id, method)))
}

async fn maybe_track_server_response(
    repository: &ThreadRepository,
    observer_runtime: &AppServerMirrorObserverManager,
    thread_key: &str,
    workspace_path: &Path,
    observer_ws_url: &str,
    message: &WsMessage,
    tracked_request_method_by_id: &mut HashMap<i64, TrackedRequestMethod>,
    tracked_turn_mode_by_request_id: &mut HashMap<i64, CollaborationMode>,
) -> Result<Option<String>> {
    let text = match message {
        WsMessage::Text(text) => text.as_str(),
        WsMessage::Binary(bytes) => {
            std::str::from_utf8(bytes).context("invalid utf8 daemon frame")?
        }
        _ => return Ok(None),
    };
    let payload: Value = match serde_json::from_str(text) {
        Ok(payload) => payload,
        Err(_) => return Ok(None),
    };
    let Some(response_id) = payload.get("id").and_then(Value::as_i64) else {
        return Ok(None);
    };
    let Some(method) = tracked_request_method_by_id.remove(&response_id) else {
        return Ok(None);
    };
    if payload.get("error").is_some() {
        debug!(event = "hcodex_ingress.request.failed", thread_key = %thread_key, method = ?method);
        return Ok(None);
    }
    let Some(result) = payload.get("result") else {
        return Ok(None);
    };
    if method == TrackedRequestMethod::TurnStart {
        if let Some(turn_id) = result
            .get("turn")
            .and_then(|turn| turn.get("id"))
            .and_then(Value::as_str)
        {
            if let Some(record) = repository.find_active_thread_by_key(thread_key).await?
                && let Some(binding) = repository.read_session_binding(&record).await?
                && let Some(session_id) = binding.tui_active_codex_thread_id.as_deref()
            {
                record_hcodex_ingress_turn_started(workspace_path, session_id, Some(turn_id))
                    .await?;
            }
            if let Some(mode) = tracked_turn_mode_by_request_id.remove(&response_id) {
                observer_runtime.record_turn_mode(turn_id, mode).await;
            }
        }
        return Ok(None);
    }
    let Some(thread_id) = result
        .get("thread")
        .and_then(|thread| thread.get("id"))
        .and_then(Value::as_str)
    else {
        return Ok(None);
    };
    let attach_mode = match method {
        TrackedRequestMethod::ThreadStart | TrackedRequestMethod::ThreadResume => {
            ObserverAttachMode::WorkerObserve
        }
        TrackedRequestMethod::TurnStart => return Ok(None),
    };
    repository
        .set_tui_active_session_for_thread_key(thread_key, thread_id.to_owned())
        .await?;
    observer_runtime
        .ensure_thread_observer(workspace_path, observer_ws_url, thread_key, thread_id)
        .await?;
    record_hcodex_ingress_connected(workspace_path, thread_key, thread_id, attach_mode).await?;
    info!(
        event = "hcodex_ingress.session_tracked",
        thread_key = %thread_key,
        method = ?method,
        tui_active_codex_thread_id = %thread_id,
        attach_mode = attach_mode.as_str(),
    );
    Ok(Some(thread_id.to_owned()))
}

fn maybe_track_turn_mode_request(
    message: &WsMessage,
    tracked_turn_mode_by_request_id: &mut HashMap<i64, CollaborationMode>,
) -> Result<()> {
    let text = match message {
        WsMessage::Text(text) => text.as_str(),
        WsMessage::Binary(bytes) => {
            std::str::from_utf8(bytes).context("invalid utf8 client frame")?
        }
        _ => return Ok(()),
    };
    let payload: Value = match serde_json::from_str(text) {
        Ok(payload) => payload,
        Err(_) => return Ok(()),
    };
    if payload.get("method").and_then(Value::as_str) != Some("turn/start") {
        return Ok(());
    }
    let Some(request_id) = payload.get("id").and_then(Value::as_i64) else {
        return Ok(());
    };
    let mode = payload
        .get("params")
        .and_then(|params| params.get("collaborationMode"))
        .and_then(CollaborationMode::from_wire_value)
        .unwrap_or(CollaborationMode::Default);
    tracked_turn_mode_by_request_id.insert(request_id, mode);
    Ok(())
}

#[cfg(test)]
fn should_reset_assistant_preview_segment(message: &WsMessage) -> Result<bool> {
    let Some(diagnostic) = workspace_item_diagnostic(message)? else {
        return Ok(false);
    };
    let is_item_lifecycle = matches!(
        diagnostic.method.as_str(),
        "item.started"
            | "item/started"
            | "item.completed"
            | "item/completed"
            | "item.updated"
            | "item/updated"
    );
    if !is_item_lifecycle {
        return Ok(false);
    }
    Ok(matches!(
        diagnostic.item_type.as_str(),
        "command_execution"
            | "commandExecution"
            | "mcp_tool_call"
            | "mcpToolCall"
            | "web_search"
            | "webSearch"
            | "todo_list"
            | "plan"
    ))
}

#[cfg(test)]
fn maybe_extract_plan_delta(message: &WsMessage) -> Result<Option<(String, String)>> {
    let text = match message {
        WsMessage::Text(text) => text.as_str(),
        WsMessage::Binary(bytes) => {
            std::str::from_utf8(bytes).context("invalid utf8 daemon frame")?
        }
        _ => return Ok(None),
    };
    let payload: Value = match serde_json::from_str(text) {
        Ok(payload) => payload,
        Err(_) => return Ok(None),
    };
    if payload.get("method").and_then(Value::as_str) != Some("item/plan/delta") {
        return Ok(None);
    }
    let item_id = payload
        .get("params")
        .and_then(|params| params.get("itemId"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let delta = payload
        .get("params")
        .and_then(|params| params.get("delta"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    Ok(item_id.zip(delta))
}

#[cfg(test)]
fn maybe_extract_completed_plan_text(message: &WsMessage) -> Result<Option<String>> {
    let text = match message {
        WsMessage::Text(text) => text.as_str(),
        WsMessage::Binary(bytes) => {
            std::str::from_utf8(bytes).context("invalid utf8 daemon frame")?
        }
        _ => return Ok(None),
    };
    let payload: Value = match serde_json::from_str(text) {
        Ok(payload) => payload,
        Err(_) => return Ok(None),
    };
    if payload.get("method").and_then(Value::as_str) != Some("item/completed") {
        return Ok(None);
    }
    let item = payload.get("params").and_then(|params| params.get("item"));
    if item
        .and_then(|item| item.get("type"))
        .and_then(Value::as_str)
        != Some("plan")
    {
        return Ok(None);
    }
    Ok(item
        .and_then(|item| item.get("text"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned))
}

fn canonical_workspace_key(workspace_path: &Path) -> Result<String> {
    Ok(workspace_path
        .canonicalize()
        .unwrap_or_else(|_| workspace_path.to_path_buf())
        .display()
        .to_string())
}

#[cfg(test)]
mod tests {
    use super::{
        TrackedRequestMethod, maybe_extract_completed_plan_text, maybe_extract_plan_delta,
        should_reset_assistant_preview_segment, should_reuse_existing_ingress,
        track_client_request,
    };
    use serde_json::json;
    use tokio_tungstenite::tungstenite::Message as WsMessage;

    #[test]
    fn track_client_request_marks_turn_start_requests() {
        let message = WsMessage::Text(
            r#"{"jsonrpc":"2.0","id":42,"method":"turn/start","params":{"threadId":"thr_1","input":[]}}"#
                .into(),
        );
        let tracked = track_client_request(&message).expect("parse ok");
        assert_eq!(tracked, Some((42, TrackedRequestMethod::TurnStart)));
    }

    #[test]
    fn existing_proxy_is_not_reused_when_daemon_url_changes() {
        assert!(!should_reuse_existing_ingress(
            "ws://127.0.0.1:40111",
            "ws://127.0.0.1:40112",
            "ws://127.0.0.1:40211",
            "ws://127.0.0.1:40211",
            true
        ));
        assert!(!should_reuse_existing_ingress(
            "ws://127.0.0.1:40111",
            "ws://127.0.0.1:40111",
            "ws://127.0.0.1:40211",
            "ws://127.0.0.1:40211",
            false
        ));
        assert!(!should_reuse_existing_ingress(
            "ws://127.0.0.1:40111",
            "ws://127.0.0.1:40111",
            "ws://127.0.0.1:40211",
            "ws://127.0.0.1:40212",
            true
        ));
        assert!(should_reuse_existing_ingress(
            "ws://127.0.0.1:40111",
            "ws://127.0.0.1:40111",
            "ws://127.0.0.1:40211",
            "ws://127.0.0.1:40211",
            true
        ));
    }

    #[test]
    fn tool_items_reset_assistant_preview_segment() {
        let message = WsMessage::Text(
            json!({
                "method": "item/started",
                "params": {
                    "item": {
                        "type": "commandExecution",
                        "command": "git log -2"
                    }
                }
            })
            .to_string()
            .into(),
        );
        assert!(should_reset_assistant_preview_segment(&message).unwrap());
    }

    #[test]
    fn agent_message_items_do_not_reset_assistant_preview_segment() {
        let message = WsMessage::Text(
            json!({
                "method": "item/completed",
                "params": {
                    "item": {
                        "type": "agent_message",
                        "text": "我先查一下"
                    }
                }
            })
            .to_string()
            .into(),
        );
        assert!(!should_reset_assistant_preview_segment(&message).unwrap());
    }

    #[test]
    fn plan_delta_is_extracted_from_workspace_message() {
        let message = WsMessage::Text(
            json!({
                "method": "item/plan/delta",
                "params": {
                    "itemId": "plan_1",
                    "delta": "# Plan\n"
                }
            })
            .to_string()
            .into(),
        );
        let extracted = maybe_extract_plan_delta(&message).unwrap();
        assert_eq!(
            extracted,
            Some(("plan_1".to_owned(), "# Plan\n".to_owned()))
        );
    }

    #[test]
    fn completed_plan_text_is_extracted_from_workspace_message() {
        let message = WsMessage::Text(
            json!({
                "method": "item/completed",
                "params": {
                    "item": {
                        "type": "plan",
                        "text": "# Final plan\n- one"
                    }
                }
            })
            .to_string()
            .into(),
        );
        let extracted = maybe_extract_completed_plan_text(&message).unwrap();
        assert_eq!(extracted.as_deref(), Some("# Final plan\n- one"));
    }
}
