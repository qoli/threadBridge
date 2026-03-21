use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};

use anyhow::{Context, Result, anyhow};
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;
use tokio::task::AbortHandle;
use tokio_tungstenite::accept_hdr_async;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
use tracing::{debug, info, warn};

use crate::app_server_runtime::{WorkspaceRuntimeState, write_workspace_runtime_state_file};
use crate::repository::ThreadRepository;
use crate::workspace_status::{
    record_tui_proxy_completed, record_tui_proxy_connected, record_tui_proxy_disconnected,
    record_tui_proxy_prompt,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TrackedRequestMethod {
    ThreadResume,
    ThreadStart,
    TurnStart,
}

#[derive(Debug, Clone)]
pub struct TuiProxyManager {
    repository: ThreadRepository,
    inner: Arc<Mutex<HashMap<String, WorkspaceTuiProxy>>>,
}

#[derive(Debug, Clone)]
struct WorkspaceTuiProxy {
    workspace_path: PathBuf,
    daemon_ws_url: String,
    proxy_base_ws_url: String,
    abort_handle: AbortHandle,
}

impl TuiProxyManager {
    pub fn new(repository: ThreadRepository) -> Self {
        Self {
            repository,
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn ensure_workspace_proxy(
        &self,
        workspace_path: &Path,
        daemon_ws_url: &str,
    ) -> Result<WorkspaceRuntimeState> {
        let key = canonical_workspace_key(workspace_path)?;
        let mut inner = self.inner.lock().await;
        if let Some(existing) = inner.get(&key).cloned() {
            if should_reuse_existing_proxy(
                &existing.daemon_ws_url,
                daemon_ws_url,
                proxy_endpoint_is_live(&existing.proxy_base_ws_url).await,
            ) {
                let state = WorkspaceRuntimeState {
                    schema_version: 1,
                    workspace_cwd: existing.workspace_path.display().to_string(),
                    daemon_ws_url: existing.daemon_ws_url.clone(),
                    tui_proxy_base_ws_url: Some(existing.proxy_base_ws_url.clone()),
                };
                info!(
                    event = "tui_proxy.reuse",
                    workspace = %existing.workspace_path.display(),
                    daemon_ws_url = %existing.daemon_ws_url,
                    proxy_base_ws_url = %existing.proxy_base_ws_url,
                    "reusing workspace TUI proxy"
                );
                drop(inner);
                write_workspace_runtime_state_file(&existing.workspace_path, &state).await?;
                return Ok(state);
            }

            info!(
                event = "tui_proxy.rebuild",
                workspace = %existing.workspace_path.display(),
                previous_daemon_ws_url = %existing.daemon_ws_url,
                requested_daemon_ws_url = %daemon_ws_url,
                previous_proxy_base_ws_url = %existing.proxy_base_ws_url,
                "rebuilding workspace TUI proxy"
            );
            existing.abort_handle.abort();
            inner.remove(&key);
        }

        let workspace_path = workspace_path
            .canonicalize()
            .unwrap_or_else(|_| workspace_path.to_path_buf());
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .context("failed to bind TUI proxy listener")?;
        let local_addr = listener
            .local_addr()
            .context("failed to read TUI proxy listener addr")?;
        let proxy_base_ws_url = format!("ws://127.0.0.1:{}", local_addr.port());
        let daemon_ws_url = daemon_ws_url.to_owned();
        let repository = self.repository.clone();
        let workspace_for_task = workspace_path.clone();
        let daemon_ws_url_for_task = daemon_ws_url.clone();

        let listener_task = tokio::spawn(async move {
            if let Err(error) = run_proxy_listener(
                listener,
                repository,
                workspace_for_task,
                daemon_ws_url_for_task,
            )
            .await
            {
                warn!(event = "tui_proxy.listener.failed", error = %error);
            }
        });
        let abort_handle = listener_task.abort_handle();

        let runtime = WorkspaceTuiProxy {
            workspace_path: workspace_path.clone(),
            daemon_ws_url: daemon_ws_url.to_owned(),
            proxy_base_ws_url: proxy_base_ws_url.clone(),
            abort_handle,
        };
        let state = WorkspaceRuntimeState {
            schema_version: 1,
            workspace_cwd: workspace_path.display().to_string(),
            daemon_ws_url: runtime.daemon_ws_url.clone(),
            tui_proxy_base_ws_url: Some(runtime.proxy_base_ws_url.clone()),
        };
        info!(
            event = "tui_proxy.spawned",
            workspace = %workspace_path.display(),
            daemon_ws_url = %runtime.daemon_ws_url,
            proxy_base_ws_url = %runtime.proxy_base_ws_url,
            "spawned workspace TUI proxy"
        );
        inner.insert(key, runtime);
        drop(inner);
        write_workspace_runtime_state_file(&workspace_path, &state).await?;
        Ok(state)
    }
}

async fn proxy_endpoint_is_live(url: &str) -> bool {
    let Some(socket_addr) = url.strip_prefix("ws://") else {
        return false;
    };
    TcpStream::connect(socket_addr).await.is_ok()
}

fn should_reuse_existing_proxy(
    existing_daemon_ws_url: &str,
    requested_daemon_ws_url: &str,
    proxy_alive: bool,
) -> bool {
    existing_daemon_ws_url == requested_daemon_ws_url && proxy_alive
}

async fn run_proxy_listener(
    listener: TcpListener,
    repository: ThreadRepository,
    workspace_path: PathBuf,
    daemon_ws_url: String,
) -> Result<()> {
    loop {
        let (stream, remote_addr) = listener.accept().await?;
        let repository = repository.clone();
        let workspace_path = workspace_path.clone();
        let daemon_ws_url = daemon_ws_url.clone();
        tokio::spawn(async move {
            if let Err(error) = handle_proxy_connection(
                stream,
                remote_addr,
                repository,
                workspace_path,
                daemon_ws_url,
            )
            .await
            {
                warn!(event = "tui_proxy.connection.failed", error = %error);
            }
        });
    }
}

async fn handle_proxy_connection(
    stream: TcpStream,
    remote_addr: SocketAddr,
    repository: ThreadRepository,
    workspace_path: PathBuf,
    daemon_ws_url: String,
) -> Result<()> {
    let captured_path = Arc::new(StdMutex::new(None::<String>));
    let captured_path_clone = captured_path.clone();
    let client_ws = accept_hdr_async(stream, move |request: &Request, response: Response| {
        *captured_path_clone.lock().expect("capture request path") =
            Some(request.uri().path().to_owned());
        Ok(response)
    })
    .await
    .context("failed to accept TUI proxy websocket")?;
    let path = captured_path
        .lock()
        .expect("read request path")
        .clone()
        .unwrap_or_default();
    let thread_key = thread_key_from_path(&path)?;
    let (mut daemon_ws, _) = connect_async(&daemon_ws_url)
        .await
        .with_context(|| format!("failed to connect TUI proxy to daemon at {daemon_ws_url}"))?;
    let mut client_ws = client_ws;
    let mut tracked_request_method_by_id: HashMap<i64, TrackedRequestMethod> = HashMap::new();
    let mut current_session_id: Option<String> = None;
    let mut local_turn_ids: HashSet<String> = HashSet::new();
    let mut latest_assistant_message = String::new();

    info!(
        event = "tui_proxy.connection.accepted",
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
                let client_message = client_message.context("failed to read TUI client websocket message")?;
                maybe_track_client_turn_start(&workspace_path, current_session_id.as_deref(), &client_message).await?;
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
                    .context("failed to forward TUI client message to daemon")?;
            }
            daemon_message = daemon_ws.next() => {
                let Some(daemon_message) = daemon_message else {
                    break;
                };
                let daemon_message = daemon_message.context("failed to read daemon websocket message")?;
                maybe_track_server_message(
                    &repository,
                    &thread_key,
                    &workspace_path,
                    &daemon_message,
                    &mut tracked_request_method_by_id,
                    &mut current_session_id,
                    &mut local_turn_ids,
                    &mut latest_assistant_message,
                ).await?;
                if matches!(daemon_message, WsMessage::Close(_)) {
                    let _ = client_ws.send(daemon_message).await;
                    break;
                }
                client_ws
                    .send(daemon_message)
                    .await
                    .context("failed to forward daemon websocket message to TUI client")?;
            }
        }
    }

    record_tui_proxy_disconnected(&workspace_path, &thread_key, current_session_id.as_deref())
        .await?;
    debug!(
        event = "tui_proxy.connection.closed",
        workspace = %workspace_path.display(),
        thread_key = %thread_key,
    );
    Ok(())
}

fn thread_key_from_path(path: &str) -> Result<String> {
    let prefix = "/thread/";
    let remainder = path
        .strip_prefix(prefix)
        .context("TUI proxy path must start with /thread/")?;
    if remainder.trim().is_empty() {
        return Err(anyhow!("TUI proxy path is missing thread key"));
    }
    Ok(remainder.trim().to_owned())
}

fn track_client_request(message: &WsMessage) -> Result<Option<(i64, TrackedRequestMethod)>> {
    let text = match message {
        WsMessage::Text(text) => text.as_str(),
        WsMessage::Binary(bytes) => {
            std::str::from_utf8(bytes).context("invalid utf8 client frame")?
        }
        _ => return Ok(None),
    };
    let payload: Value = serde_json::from_str(text).context("invalid TUI proxy client json")?;
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
    thread_key: &str,
    message: &WsMessage,
    tracked_request_method_by_id: &mut HashMap<i64, TrackedRequestMethod>,
    local_turn_ids: &mut HashSet<String>,
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
        debug!(event = "tui_proxy.request.failed", thread_key = %thread_key, method = ?method);
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
            local_turn_ids.insert(turn_id.to_owned());
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
    repository
        .set_tui_active_session_for_thread_key(thread_key, thread_id.to_owned())
        .await?;
    info!(
        event = "tui_proxy.session_tracked",
        thread_key = %thread_key,
        method = ?method,
        tui_active_codex_thread_id = %thread_id,
    );
    Ok(Some(thread_id.to_owned()))
}

async fn maybe_track_client_turn_start(
    workspace_path: &Path,
    current_session_id: Option<&str>,
    message: &WsMessage,
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
    let session_id = payload
        .get("params")
        .and_then(|params| params.get("threadId"))
        .and_then(Value::as_str)
        .or(current_session_id);
    let Some(session_id) = session_id else {
        return Ok(());
    };
    let Some(prompt) = extract_turn_prompt(&payload) else {
        return Ok(());
    };
    record_tui_proxy_prompt(workspace_path, session_id, &prompt).await?;
    Ok(())
}

fn extract_turn_prompt(payload: &Value) -> Option<String> {
    let input = payload
        .get("params")
        .and_then(|params| params.get("input"))
        .and_then(Value::as_array)?;
    let texts = input
        .iter()
        .filter(|item| item.get("type").and_then(Value::as_str) == Some("text"))
        .filter_map(|item| item.get("text").and_then(Value::as_str))
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>();
    if texts.is_empty() {
        None
    } else {
        Some(texts.join("\n\n"))
    }
}

async fn maybe_track_server_message(
    repository: &ThreadRepository,
    thread_key: &str,
    workspace_path: &Path,
    message: &WsMessage,
    tracked_request_method_by_id: &mut HashMap<i64, TrackedRequestMethod>,
    current_session_id: &mut Option<String>,
    local_turn_ids: &mut HashSet<String>,
    latest_assistant_message: &mut String,
) -> Result<()> {
    if let Some(session_id) = maybe_track_server_response(
        repository,
        thread_key,
        message,
        tracked_request_method_by_id,
        local_turn_ids,
    )
    .await?
    {
        record_tui_proxy_connected(workspace_path, thread_key, &session_id).await?;
        *current_session_id = Some(session_id);
    }

    if let Some(text) = maybe_extract_agent_message_text(message)? {
        if is_agent_message_delta(message)? {
            latest_assistant_message.push_str(&text);
        } else {
            *latest_assistant_message = text;
        }
    }

    if let Some(session_id) = current_session_id.as_deref() {
        if let Some(completed_turn_id) = extract_completed_turn_id(message)? {
            if local_turn_ids.remove(&completed_turn_id) {
                let final_text = if latest_assistant_message.trim().is_empty() {
                    None
                } else {
                    Some(latest_assistant_message.as_str())
                };
                record_tui_proxy_completed(
                    workspace_path,
                    session_id,
                    Some(&completed_turn_id),
                    final_text,
                )
                .await?;
            }
            latest_assistant_message.clear();
        } else if is_turn_completed(message)? {
            latest_assistant_message.clear();
        }
    }
    Ok(())
}

fn maybe_extract_agent_message_text(message: &WsMessage) -> Result<Option<String>> {
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
    let method = payload.get("method").and_then(Value::as_str);
    let params = payload.get("params");
    if method == Some("item/completed") {
        let item = params.and_then(|params| params.get("item"));
        if item
            .and_then(|item| item.get("type"))
            .and_then(Value::as_str)
            == Some("agent_message")
        {
            return Ok(item
                .and_then(|item| item.get("text"))
                .and_then(Value::as_str)
                .map(str::to_owned));
        }
    }
    if method == Some("item/agentMessage/delta") {
        return Ok(params
            .and_then(|params| params.get("delta"))
            .and_then(Value::as_str)
            .map(str::to_owned));
    }
    Ok(None)
}

fn is_agent_message_delta(message: &WsMessage) -> Result<bool> {
    let text = match message {
        WsMessage::Text(text) => text.as_str(),
        WsMessage::Binary(bytes) => {
            std::str::from_utf8(bytes).context("invalid utf8 daemon frame")?
        }
        _ => return Ok(false),
    };
    let payload: Value = match serde_json::from_str(text) {
        Ok(payload) => payload,
        Err(_) => return Ok(false),
    };
    Ok(payload.get("method").and_then(Value::as_str) == Some("item/agentMessage/delta"))
}

fn extract_completed_turn_id(message: &WsMessage) -> Result<Option<String>> {
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
    if payload.get("method").and_then(Value::as_str) != Some("turn/completed") {
        return Ok(None);
    }
    Ok(payload
        .get("params")
        .and_then(|params| params.get("turn"))
        .and_then(|turn| turn.get("id"))
        .and_then(Value::as_str)
        .map(str::to_owned))
}

fn is_turn_completed(message: &WsMessage) -> Result<bool> {
    let text = match message {
        WsMessage::Text(text) => text.as_str(),
        WsMessage::Binary(bytes) => {
            std::str::from_utf8(bytes).context("invalid utf8 daemon frame")?
        }
        _ => return Ok(false),
    };
    let payload: Value = match serde_json::from_str(text) {
        Ok(payload) => payload,
        Err(_) => return Ok(false),
    };
    Ok(payload.get("method").and_then(Value::as_str) == Some("turn/completed"))
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
    use super::{TrackedRequestMethod, should_reuse_existing_proxy, track_client_request};
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
        assert!(!should_reuse_existing_proxy(
            "ws://127.0.0.1:40111",
            "ws://127.0.0.1:40112",
            true
        ));
        assert!(!should_reuse_existing_proxy(
            "ws://127.0.0.1:40111",
            "ws://127.0.0.1:40111",
            false
        ));
        assert!(should_reuse_existing_proxy(
            "ws://127.0.0.1:40111",
            "ws://127.0.0.1:40111",
            true
        ));
    }
}
