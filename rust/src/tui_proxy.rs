use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};

use anyhow::{Context, Result, anyhow};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use teloxide::payloads::SendMessageSetters;
use teloxide::requests::Requester;
use teloxide::types::{MessageId, ThreadId};
use teloxide::Bot;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, mpsc};
use tokio::task::AbortHandle;
use tokio_tungstenite::accept_hdr_async;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};
use tracing::{debug, info, warn};

use crate::app_server_runtime::{WorkspaceRuntimeState, write_workspace_runtime_state_file};
use crate::collaboration_mode::CollaborationMode;
use crate::interactive::{
    InteractiveRequestRegistry, ServerRequestResolvedNotification, ToolRequestUserInputParams,
};
use crate::process_transcript::{
    process_entry_from_codex_event, process_entry_from_workspace_message, workspace_item_diagnostic,
};
use crate::repository::ThreadRepository;
use crate::repository::TranscriptMirrorOrigin;
use crate::telegram_runtime::{
    final_reply::compose_visible_final_reply, render_request_user_input_prompt,
    request_user_input_markup, send_plan_implementation_prompt,
};
use crate::workspace_status::{
    record_tui_proxy_completed, record_tui_proxy_connected, record_tui_proxy_disconnected,
    record_tui_proxy_preview_text, record_tui_proxy_process_event, record_tui_proxy_prompt,
};

const PROXY_HEALTH_PATH: &str = "/healthz";

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
    daemon_request_channels: Arc<Mutex<HashMap<String, mpsc::UnboundedSender<WsMessage>>>>,
    telegram_bridge: Arc<Mutex<Option<TelegramInteractiveBridge>>>,
}

#[derive(Debug, Clone)]
struct WorkspaceTuiProxy {
    workspace_path: PathBuf,
    daemon_ws_url: String,
    proxy_base_ws_url: String,
    abort_handle: AbortHandle,
}

#[derive(Debug, Clone)]
struct TelegramInteractiveBridge {
    bot: Bot,
    registry: InteractiveRequestRegistry,
}

impl TuiProxyManager {
    pub fn new(repository: ThreadRepository) -> Self {
        Self {
            repository,
            inner: Arc::new(Mutex::new(HashMap::new())),
            daemon_request_channels: Arc::new(Mutex::new(HashMap::new())),
            telegram_bridge: Arc::new(Mutex::new(None)),
        }
    }

    pub async fn configure_telegram_bridge(
        &self,
        bot_token: String,
        registry: InteractiveRequestRegistry,
    ) {
        self.telegram_bridge
            .lock()
            .await
            .replace(TelegramInteractiveBridge {
                bot: Bot::new(bot_token),
                registry,
            });
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
            .context("no live TUI proxy channel for thread")?;
        channel
            .send(WsMessage::Text(payload))
            .map_err(|_| anyhow!("failed to forward response into live TUI proxy"))?;
        Ok(())
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
        let daemon_request_channels = self.daemon_request_channels.clone();
        let telegram_bridge = self.telegram_bridge.clone();
        let workspace_for_task = workspace_path.clone();
        let daemon_ws_url_for_task = daemon_ws_url.clone();

        let listener_task = tokio::spawn(async move {
            if let Err(error) = run_proxy_listener(
                listener,
                repository,
                daemon_request_channels,
                telegram_bridge,
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

pub async fn proxy_endpoint_is_live(url: &str) -> bool {
    let probe_url = format!("{url}{PROXY_HEALTH_PATH}");
    connect_async(&probe_url).await.is_ok()
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
    daemon_request_channels: Arc<Mutex<HashMap<String, mpsc::UnboundedSender<WsMessage>>>>,
    telegram_bridge: Arc<Mutex<Option<TelegramInteractiveBridge>>>,
    workspace_path: PathBuf,
    daemon_ws_url: String,
) -> Result<()> {
    loop {
        let (stream, remote_addr) = listener.accept().await?;
        let repository = repository.clone();
        let daemon_request_channels = daemon_request_channels.clone();
        let telegram_bridge = telegram_bridge.clone();
        let workspace_path = workspace_path.clone();
        let daemon_ws_url = daemon_ws_url.clone();
        tokio::spawn(async move {
            if let Err(error) = handle_proxy_connection(
                stream,
                remote_addr,
                repository,
                daemon_request_channels,
                telegram_bridge,
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
    daemon_request_channels: Arc<Mutex<HashMap<String, mpsc::UnboundedSender<WsMessage>>>>,
    telegram_bridge: Arc<Mutex<Option<TelegramInteractiveBridge>>>,
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
    if path == PROXY_HEALTH_PATH {
        let mut client_ws = client_ws;
        let _ = client_ws.close(None).await;
        return Ok(());
    }
    let thread_key = thread_key_from_path(&path)?;
    let (mut daemon_ws, _) = connect_async(&daemon_ws_url)
        .await
        .with_context(|| format!("failed to connect TUI proxy to daemon at {daemon_ws_url}"))?;
    let mut client_ws = client_ws;
    let (injected_tx, mut injected_rx) = mpsc::unbounded_channel();
    daemon_request_channels
        .lock()
        .await
        .insert(thread_key.clone(), injected_tx);
    let mut tracked_request_method_by_id: HashMap<i64, TrackedRequestMethod> = HashMap::new();
    let mut tracked_turn_mode_by_request_id: HashMap<i64, CollaborationMode> = HashMap::new();
    let mut current_session_id: Option<String> = None;
    let mut local_turn_ids: HashSet<String> = HashSet::new();
    let mut local_turn_modes: HashMap<String, CollaborationMode> = HashMap::new();
    let mut latest_assistant_message = String::new();
    let mut latest_preview_segment = String::new();
    let mut latest_plan_by_id: HashMap<String, String> = HashMap::new();
    let mut latest_completed_plan_text: Option<String> = None;

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
                    &mut tracked_turn_mode_by_request_id,
                    &mut current_session_id,
                    &mut local_turn_ids,
                    &mut local_turn_modes,
                    &mut latest_assistant_message,
                    &mut latest_preview_segment,
                    &mut latest_plan_by_id,
                    &mut latest_completed_plan_text,
                    &telegram_bridge,
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
    tracked_turn_mode_by_request_id: &mut HashMap<i64, CollaborationMode>,
    local_turn_ids: &mut HashSet<String>,
    local_turn_modes: &mut HashMap<String, CollaborationMode>,
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
            if let Some(mode) = tracked_turn_mode_by_request_id.remove(&response_id) {
                local_turn_modes.insert(turn_id.to_owned(), mode);
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
        .and_then(Value::as_str)
        .and_then(CollaborationMode::from_wire)
        .unwrap_or(CollaborationMode::Default);
    tracked_turn_mode_by_request_id.insert(request_id, mode);
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
    tracked_turn_mode_by_request_id: &mut HashMap<i64, CollaborationMode>,
    current_session_id: &mut Option<String>,
    local_turn_ids: &mut HashSet<String>,
    local_turn_modes: &mut HashMap<String, CollaborationMode>,
    latest_assistant_message: &mut String,
    latest_preview_segment: &mut String,
    latest_plan_by_id: &mut HashMap<String, String>,
    latest_completed_plan_text: &mut Option<String>,
    telegram_bridge: &Arc<Mutex<Option<TelegramInteractiveBridge>>>,
) -> Result<()> {
    maybe_bridge_request_user_input(
        repository,
        thread_key,
        message,
        telegram_bridge,
    )
    .await?;
    maybe_bridge_resolved_request(message, telegram_bridge).await?;

    if let Some(session_id) = maybe_track_server_response(
        repository,
        thread_key,
        message,
        tracked_request_method_by_id,
        tracked_turn_mode_by_request_id,
        local_turn_ids,
        local_turn_modes,
    )
    .await?
    {
        record_tui_proxy_connected(workspace_path, thread_key, &session_id).await?;
        *current_session_id = Some(session_id);
    }

    if should_reset_assistant_preview_segment(message)? {
        latest_assistant_message.clear();
        latest_preview_segment.clear();
    }

    let mut preview_text: Option<String> = None;
    if let Some(text) = maybe_extract_agent_message_text(message)? {
        if is_agent_message_delta(message)? {
            latest_assistant_message.push_str(&text);
            latest_preview_segment.push_str(&text);
            let preview = latest_preview_segment.trim();
            if !preview.is_empty() {
                preview_text = Some(preview.to_owned());
            }
        } else {
            *latest_assistant_message = text.clone();
            *latest_preview_segment = text.clone();
            preview_text = Some(text);
        }
    }

    if let Some((item_id, delta)) = maybe_extract_plan_delta(message)? {
        let accumulated = latest_plan_by_id.entry(item_id.clone()).or_default();
        accumulated.push_str(&delta);
        if let Some(session_id) = current_session_id.as_deref()
            && let Some(entry) = process_entry_from_codex_event(
                &crate::codex::CodexThreadEvent::ItemUpdated {
                    item: json!({
                        "type": "plan",
                        "id": item_id,
                        "text": accumulated,
                    }),
                },
                session_id,
                TranscriptMirrorOrigin::Tui,
            )
        {
            if let Some(crate::repository::TranscriptMirrorPhase::Plan) = entry.phase {
                record_tui_proxy_process_event(workspace_path, session_id, "plan", &entry.text)
                    .await?;
            }
        }
    }

    if let Some(plan_text) = maybe_extract_completed_plan_text(message)? {
        *latest_completed_plan_text = Some(plan_text);
    }

    if let Some(session_id) = current_session_id.as_deref() {
        if let Some(preview_text) = preview_text.as_deref() {
            record_tui_proxy_preview_text(workspace_path, session_id, preview_text).await?;
        }
        if let Some(entry) =
            process_entry_from_workspace_message(message, session_id, TranscriptMirrorOrigin::Tui)?
        {
            let phase = match entry.phase {
                Some(crate::repository::TranscriptMirrorPhase::Plan) => Some("plan"),
                Some(crate::repository::TranscriptMirrorPhase::Tool) => Some("tool"),
                None => None,
            };
            if let Some(phase) = phase {
                record_tui_proxy_process_event(workspace_path, session_id, phase, &entry.text)
                    .await?;
            }
        } else if let Some(diagnostic) = workspace_item_diagnostic(message)?
            && matches!(
                diagnostic.method.as_str(),
                "item.started"
                    | "item/started"
                    | "item.completed"
                    | "item/completed"
                    | "item.updated"
                    | "item/updated"
            )
        {
            debug!(
                event = "tui_proxy.process_transcript.unmatched_item",
                thread_key = %thread_key,
                session_id = %session_id,
                method = %diagnostic.method,
                item_type = %diagnostic.item_type,
                item_keys = ?diagnostic.item_keys,
                "workspace item notification did not map to process transcript"
            );
        }
        if let Some(completed_turn_id) = extract_completed_turn_id(message)? {
            if local_turn_ids.remove(&completed_turn_id) {
                let final_text = compose_visible_final_reply(
                    latest_assistant_message,
                    latest_completed_plan_text.as_deref(),
                );
                record_tui_proxy_completed(
                    workspace_path,
                    session_id,
                    Some(&completed_turn_id),
                    final_text.as_deref(),
                )
                .await?;
                if local_turn_modes.remove(&completed_turn_id) == Some(CollaborationMode::Plan)
                    && latest_completed_plan_text.is_some()
                {
                    maybe_send_plan_prompt_from_bridge(
                        repository,
                        thread_key,
                        telegram_bridge,
                    )
                    .await?;
                }
            }
            latest_assistant_message.clear();
            latest_preview_segment.clear();
            latest_plan_by_id.clear();
            *latest_completed_plan_text = None;
        } else if is_turn_completed(message)? {
            latest_assistant_message.clear();
            latest_preview_segment.clear();
            latest_plan_by_id.clear();
            *latest_completed_plan_text = None;
        }
    }
    Ok(())
}

async fn maybe_bridge_request_user_input(
    repository: &ThreadRepository,
    thread_key: &str,
    message: &WsMessage,
    telegram_bridge: &Arc<Mutex<Option<TelegramInteractiveBridge>>>,
) -> Result<()> {
    let Some((request_id, params)) = maybe_extract_request_user_input(message)? else {
        return Ok(());
    };
    let Some(bridge) = telegram_bridge.lock().await.clone() else {
        return Ok(());
    };
    let Some(record) = repository.find_active_thread_by_key(thread_key).await? else {
        return Ok(());
    };
    let Some(telegram_thread_id) = record.metadata.message_thread_id else {
        return Ok(());
    };
    if params.questions.iter().any(|question| question.is_secret) {
        return Ok(());
    }
    let snapshot = bridge
        .registry
        .register_tui(
            record.metadata.chat_id,
            telegram_thread_id,
            thread_key.to_owned(),
            request_id,
            params,
        )
        .await?;
    let text = render_request_user_input_prompt(&snapshot);
    let request = bridge
        .bot
        .send_message(teloxide::types::ChatId(record.metadata.chat_id), text)
        .message_thread_id(ThreadId(MessageId(telegram_thread_id)));
    let sent = if let Some(markup) = request_user_input_markup(snapshot.request_id, &snapshot.question)
    {
        request.reply_markup(markup).await?
    } else {
        request.await?
    };
    bridge
        .registry
        .set_prompt_message_id(record.metadata.chat_id, telegram_thread_id, sent.id.0)
        .await;
    Ok(())
}

async fn maybe_bridge_resolved_request(
    message: &WsMessage,
    telegram_bridge: &Arc<Mutex<Option<TelegramInteractiveBridge>>>,
) -> Result<()> {
    let Some(resolved) = maybe_extract_server_request_resolved(message)? else {
        return Ok(());
    };
    let Some(bridge) = telegram_bridge.lock().await.clone() else {
        return Ok(());
    };
    let Some(resolved_request) = bridge
        .registry
        .resolve_request_id(&resolved.thread_id, &resolved.request_id)
        .await
    else {
        return Ok(());
    };
    if let Some(message_id) = resolved_request.prompt_message_id {
        let _ = bridge
            .bot
            .edit_message_text(
                teloxide::types::ChatId(resolved_request.chat_id),
                MessageId(message_id),
                "Info: Questions resolved.",
            )
            .await;
    }
    Ok(())
}

async fn maybe_send_plan_prompt_from_bridge(
    repository: &ThreadRepository,
    thread_key: &str,
    telegram_bridge: &Arc<Mutex<Option<TelegramInteractiveBridge>>>,
) -> Result<()> {
    let Some(bridge) = telegram_bridge.lock().await.clone() else {
        return Ok(());
    };
    let Some(record) = repository.find_active_thread_by_key(thread_key).await? else {
        return Ok(());
    };
    let Some(telegram_thread_id) = record.metadata.message_thread_id else {
        return Ok(());
    };
    send_plan_implementation_prompt(
        &bridge.bot,
        teloxide::types::ChatId(record.metadata.chat_id),
        ThreadId(MessageId(telegram_thread_id)),
    )
    .await?;
    Ok(())
}

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

fn maybe_extract_request_user_input(
    message: &WsMessage,
) -> Result<Option<(i64, ToolRequestUserInputParams)>> {
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
    if payload.get("method").and_then(Value::as_str) != Some("item/tool/requestUserInput") {
        return Ok(None);
    }
    let Some(request_id) = payload.get("id").and_then(Value::as_i64) else {
        return Ok(None);
    };
    let params: ToolRequestUserInputParams = serde_json::from_value(
        payload.get("params").cloned().unwrap_or(Value::Null),
    )
    .context("invalid item/tool/requestUserInput params")?;
    Ok(Some((request_id, params)))
}

fn maybe_extract_server_request_resolved(
    message: &WsMessage,
) -> Result<Option<ServerRequestResolvedNotification>> {
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
    if payload.get("method").and_then(Value::as_str) != Some("serverRequest/resolved") {
        return Ok(None);
    }
    let params = serde_json::from_value(payload.get("params").cloned().unwrap_or(Value::Null))
        .context("invalid serverRequest/resolved params")?;
    Ok(Some(params))
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
    use super::{
        TrackedRequestMethod, maybe_extract_completed_plan_text, maybe_extract_plan_delta,
        should_reset_assistant_preview_segment, should_reuse_existing_proxy, track_client_request,
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
