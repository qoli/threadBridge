use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use serde_json::Value;
use tokio::sync::Mutex;
use tokio::task::AbortHandle;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tracing::{info, warn};

use crate::codex::{
    CodexRunner, CodexServerNotification, CodexServerRequest, CodexThreadEvent,
    observe_thread_with_handlers,
};
use crate::collaboration_mode::CollaborationMode;
use crate::interactive::ToolRequestUserInputParams;
use crate::process_transcript::process_entry_from_codex_event;
use crate::repository::{TranscriptMirrorOrigin, TranscriptMirrorPhase};
use crate::runtime_interaction::{
    RuntimeInteractionEvent, RuntimeInteractionRequest, RuntimeInteractionResolved,
    RuntimeInteractionSender, TurnCompletionSummary,
};
use crate::turn_completion::compose_visible_final_reply;
use crate::workspace_status::{
    ObserverAttachMode, record_hcodex_ingress_completed, record_hcodex_ingress_preview_text,
    record_hcodex_ingress_process_event, record_hcodex_ingress_prompt,
    record_hcodex_ingress_turn_started,
};

#[derive(Debug, Clone)]
pub struct AppServerMirrorObserverManager {
    turn_modes: Arc<Mutex<HashMap<String, CollaborationMode>>>,
    inner: Arc<Mutex<HashMap<String, RunningObserver>>>,
    interaction_sender: Arc<Mutex<Option<RuntimeInteractionSender>>>,
}

#[derive(Debug)]
struct RunningObserver {
    thread_id: String,
    mode: ObserverAttachMode,
    kind: RunningObserverKind,
}

#[derive(Debug)]
enum RunningObserverKind {
    ResumeTask { abort_handle: AbortHandle },
    LiveForwarded { state: Arc<Mutex<ObserverState>> },
}

#[derive(Debug, Default)]
struct ObserverState {
    latest_assistant_message: String,
    latest_completed_plan_text: Option<String>,
    latest_agent_message_by_id: HashMap<String, String>,
    latest_plan_by_id: HashMap<String, String>,
}

impl ObserverState {
    fn parse_forwarded_message(&mut self, text: &str) -> Result<ParsedObserverMessage> {
        parse_forwarded_message(
            text,
            &mut self.latest_agent_message_by_id,
            &mut self.latest_plan_by_id,
        )
    }
}

#[derive(Debug, Default)]
struct ParsedObserverMessage {
    event: Option<CodexThreadEvent>,
    server_request: Option<CodexServerRequest>,
    server_notification: Option<CodexServerNotification>,
}

impl AppServerMirrorObserverManager {
    pub(crate) fn new(turn_modes: Arc<Mutex<HashMap<String, CollaborationMode>>>) -> Self {
        Self {
            turn_modes,
            inner: Arc::new(Mutex::new(HashMap::new())),
            interaction_sender: Arc::new(Mutex::new(None)),
        }
    }

    pub async fn set_interaction_sender(&self, sender: Option<RuntimeInteractionSender>) {
        self.interaction_sender.lock().await.clone_from(&sender);
    }

    pub async fn ensure_thread_observer(
        &self,
        workspace_path: &Path,
        daemon_ws_url: &str,
        thread_key: &str,
        thread_id: &str,
    ) -> Result<()> {
        let key = observer_key(workspace_path, thread_key);
        if !self
            .source_needs_replacement(&key, thread_id, ObserverAttachMode::ResumeWs)
            .await?
        {
            return Ok(());
        }
        let workspace_path = workspace_path
            .canonicalize()
            .unwrap_or_else(|_| workspace_path.to_path_buf());
        let daemon_ws_url = daemon_ws_url.to_owned();
        let thread_key = thread_key.to_owned();
        let observer_thread_key = thread_key.clone();
        let thread_id = thread_id.to_owned();
        let observer_thread_id = thread_id.clone();
        let turn_modes = self.turn_modes.clone();
        let interaction_sender = self.interaction_sender.clone();
        let task = tokio::spawn(async move {
            if let Err(error) = run_thread_observer(
                turn_modes,
                interaction_sender,
                workspace_path,
                daemon_ws_url,
                observer_thread_key,
                observer_thread_id,
            )
            .await
            {
                warn!(event = "app_server_observer.failed", error = %error);
            }
        });
        self.replace_observer(
            key,
            thread_key,
            thread_id,
            ObserverAttachMode::ResumeWs,
            RunningObserverKind::ResumeTask {
                abort_handle: task.abort_handle(),
            },
        )
        .await
    }

    pub async fn register_live_forwarded_source(
        &self,
        workspace_path: &Path,
        thread_key: &str,
        thread_id: &str,
    ) -> Result<()> {
        let key = observer_key(workspace_path, thread_key);
        if !self
            .source_needs_replacement(&key, thread_id, ObserverAttachMode::LiveForwarded)
            .await?
        {
            return Ok(());
        }
        self.replace_observer(
            key,
            thread_key.to_owned(),
            thread_id.to_owned(),
            ObserverAttachMode::LiveForwarded,
            RunningObserverKind::LiveForwarded {
                state: Arc::new(Mutex::new(ObserverState::default())),
            },
        )
        .await
    }

    pub async fn stop_thread_observer(&self, workspace_path: &Path, thread_key: &str) {
        let key = observer_key(workspace_path, thread_key);
        if let Some(existing) = self.inner.lock().await.remove(&key) {
            if let RunningObserverKind::ResumeTask { abort_handle } = existing.kind {
                abort_handle.abort();
            }
            info!(
                event = "app_server_observer.source_closed",
                thread_key = %thread_key,
                thread_id = %existing.thread_id,
                attach_mode = existing.mode.as_str(),
            );
        }
    }

    pub async fn observe_forwarded_daemon_message(
        &self,
        workspace_path: &Path,
        thread_key: &str,
        message: &WsMessage,
    ) -> Result<()> {
        let key = observer_key(workspace_path, thread_key);
        let (thread_id, state) = {
            let inner = self.inner.lock().await;
            let Some(existing) = inner.get(&key) else {
                return Ok(());
            };
            if existing.mode != ObserverAttachMode::LiveForwarded {
                return Ok(());
            }
            let RunningObserverKind::LiveForwarded { state } = &existing.kind else {
                return Ok(());
            };
            (existing.thread_id.clone(), state.clone())
        };

        let text = match message {
            WsMessage::Text(text) => text.as_str(),
            WsMessage::Binary(bytes) => {
                std::str::from_utf8(bytes).context("invalid utf8 daemon frame")?
            }
            WsMessage::Ping(_) | WsMessage::Pong(_) | WsMessage::Frame(_) | WsMessage::Close(_) => {
                return Ok(());
            }
        };

        let parsed = {
            let mut state_guard = state.lock().await;
            state_guard.parse_forwarded_message(text)?
        };

        if let Some(notification) = parsed.server_notification {
            handle_server_notification(&self.interaction_sender, notification).await?;
        }
        if let Some(request) = parsed.server_request {
            handle_server_request(thread_key, &self.interaction_sender, request).await?;
        }
        if let Some(event) = parsed.event {
            handle_observer_event(
                workspace_path,
                thread_key,
                &thread_id,
                &self.turn_modes,
                &self.interaction_sender,
                &state,
                event,
            )
            .await?;
        }

        Ok(())
    }

    pub async fn record_turn_mode(&self, turn_id: &str, mode: CollaborationMode) {
        self.turn_modes
            .lock()
            .await
            .insert(turn_id.to_owned(), mode);
    }

    async fn replace_observer(
        &self,
        key: String,
        thread_key: String,
        thread_id: String,
        mode: ObserverAttachMode,
        new_kind: RunningObserverKind,
    ) -> Result<()> {
        let mut inner = self.inner.lock().await;
        if let Some(previous) = inner.remove(&key) {
            if let RunningObserverKind::ResumeTask { abort_handle } = previous.kind {
                abort_handle.abort();
            }
        }
        info!(
            event = "app_server_observer.source_registered",
            thread_key = %thread_key,
            thread_id = %thread_id,
            attach_mode = mode.as_str(),
        );
        inner.insert(
            key,
            RunningObserver {
                thread_id,
                mode,
                kind: new_kind,
            },
        );
        Ok(())
    }

    async fn source_needs_replacement(
        &self,
        key: &str,
        thread_id: &str,
        mode: ObserverAttachMode,
    ) -> Result<bool> {
        let inner = self.inner.lock().await;
        let Some(existing) = inner.get(key) else {
            return Ok(true);
        };
        if existing.thread_id == thread_id && existing.mode == mode {
            return Ok(false);
        }
        if existing.thread_id == thread_id && existing.mode != mode {
            warn!(
                event = "app_server_observer.source_rejected_mode_conflict",
                thread_id = %thread_id,
                existing_attach_mode = existing.mode.as_str(),
                requested_attach_mode = mode.as_str(),
            );
            bail!(
                "observer source mode conflict for thread {}: existing {}, requested {}",
                thread_id,
                existing.mode.as_str(),
                mode.as_str()
            );
        }
        Ok(true)
    }
}

fn observer_key(workspace_path: &Path, thread_key: &str) -> String {
    format!(
        "{}::{thread_key}",
        stable_workspace_path(workspace_path).display()
    )
}

fn stable_workspace_path(workspace_path: &Path) -> PathBuf {
    if workspace_path.is_absolute() {
        return workspace_path.to_path_buf();
    }
    std::env::current_dir()
        .map(|cwd| cwd.join(workspace_path))
        .unwrap_or_else(|_| workspace_path.to_path_buf())
}

fn parse_forwarded_message(
    text: &str,
    latest_agent_message_by_id: &mut HashMap<String, String>,
    latest_plan_by_id: &mut HashMap<String, String>,
) -> Result<ParsedObserverMessage> {
    let payload: Value = match serde_json::from_str(text) {
        Ok(payload) => payload,
        Err(_) => return Ok(ParsedObserverMessage::default()),
    };
    let Some(method) = payload.get("method").and_then(Value::as_str) else {
        return Ok(ParsedObserverMessage::default());
    };

    if let Some(request_id) = payload.get("id").and_then(Value::as_i64) {
        if method != "item/tool/requestUserInput" {
            return Ok(ParsedObserverMessage::default());
        }
        let params: ToolRequestUserInputParams =
            serde_json::from_value(payload.get("params").cloned().unwrap_or(Value::Null))
                .with_context(|| "invalid item/tool/requestUserInput params".to_owned())?;
        return Ok(ParsedObserverMessage {
            server_request: Some(CodexServerRequest::RequestUserInput { request_id, params }),
            ..ParsedObserverMessage::default()
        });
    }

    let params = payload.get("params").cloned().unwrap_or(Value::Null);
    Ok(ParsedObserverMessage {
        server_notification: CodexRunner::map_server_notification(method, params.clone()),
        event: CodexRunner::map_notification(
            method,
            params,
            latest_agent_message_by_id,
            latest_plan_by_id,
        )?,
        server_request: None,
    })
}

async fn run_thread_observer(
    turn_modes: Arc<Mutex<HashMap<String, CollaborationMode>>>,
    interaction_sender: Arc<Mutex<Option<RuntimeInteractionSender>>>,
    workspace_path: PathBuf,
    daemon_ws_url: String,
    thread_key: String,
    thread_id: String,
) -> Result<()> {
    let state = Arc::new(Mutex::new(ObserverState::default()));
    observe_thread_with_handlers(
        &daemon_ws_url,
        &thread_id,
        {
            let workspace_path = workspace_path.clone();
            let thread_key = thread_key.clone();
            let observer_thread_id = thread_id.clone();
            let turn_modes = turn_modes.clone();
            let interaction_sender = interaction_sender.clone();
            let state = state.clone();
            move |event| {
                let workspace_path = workspace_path.clone();
                let thread_key = thread_key.clone();
                let observer_thread_id = observer_thread_id.clone();
                let turn_modes = turn_modes.clone();
                let interaction_sender = interaction_sender.clone();
                let state = state.clone();
                async move {
                    handle_observer_event(
                        &workspace_path,
                        &thread_key,
                        &observer_thread_id,
                        &turn_modes,
                        &interaction_sender,
                        &state,
                        event,
                    )
                    .await
                }
            }
        },
        {
            let thread_key = thread_key.clone();
            let interaction_sender = interaction_sender.clone();
            move |request| {
                let thread_key = thread_key.clone();
                let interaction_sender = interaction_sender.clone();
                async move {
                    handle_server_request(&thread_key, &interaction_sender, request).await
                }
            }
        },
        {
            let interaction_sender = interaction_sender.clone();
            move |notification| {
                let interaction_sender = interaction_sender.clone();
                async move { handle_server_notification(&interaction_sender, notification).await }
            }
        },
    )
    .await
}

async fn handle_observer_event(
    workspace_path: &Path,
    thread_key: &str,
    thread_id: &str,
    turn_modes: &Arc<Mutex<HashMap<String, CollaborationMode>>>,
    interaction_sender: &Arc<Mutex<Option<RuntimeInteractionSender>>>,
    state: &Arc<Mutex<ObserverState>>,
    event: CodexThreadEvent,
) -> Result<()> {
    if let Some(prompt) = extract_user_prompt_text(&event) {
        record_hcodex_ingress_prompt(workspace_path, thread_id, &prompt).await?;
    }

    if let Some(text) = extract_agent_message_text(&event) {
        state.lock().await.latest_assistant_message = text.clone();
        record_hcodex_ingress_preview_text(workspace_path, thread_id, &text).await?;
    }

    if let Some(plan_text) = extract_completed_plan_text(&event) {
        state.lock().await.latest_completed_plan_text = Some(plan_text);
    }

    if let Some(entry) =
        process_entry_from_codex_event(&event, thread_id, TranscriptMirrorOrigin::Tui)
    {
        let phase = match entry.phase {
            Some(TranscriptMirrorPhase::Plan) => Some("plan"),
            Some(TranscriptMirrorPhase::Tool) => Some("tool"),
            None => None,
        };
        if let Some(phase) = phase {
            record_hcodex_ingress_process_event(workspace_path, thread_id, phase, &entry.text)
                .await?;
        }
    }

    match event {
        CodexThreadEvent::TurnCompleted { turn_id, .. } => {
            finalize_turn(
                workspace_path,
                thread_key,
                thread_id,
                turn_id.as_deref(),
                turn_modes,
                interaction_sender,
                state,
                None,
            )
            .await?;
        }
        CodexThreadEvent::TurnInterrupted { turn_id, .. } => {
            finalize_turn(
                workspace_path,
                thread_key,
                thread_id,
                turn_id.as_deref(),
                turn_modes,
                interaction_sender,
                state,
                None,
            )
            .await?;
        }
        CodexThreadEvent::TurnFailed { turn_id, error } => {
            finalize_turn(
                workspace_path,
                thread_key,
                thread_id,
                turn_id.as_deref(),
                turn_modes,
                interaction_sender,
                state,
                Some(error.to_string()),
            )
            .await?;
        }
        CodexThreadEvent::TurnStarted { turn_id } => {
            record_hcodex_ingress_turn_started(workspace_path, thread_id, turn_id.as_deref())
                .await?;
        }
        CodexThreadEvent::ThreadStarted { .. }
        | CodexThreadEvent::Error { .. }
        | CodexThreadEvent::ItemStarted { .. }
        | CodexThreadEvent::ItemUpdated { .. }
        | CodexThreadEvent::ItemCompleted { .. } => {}
    }
    Ok(())
}

async fn finalize_turn(
    workspace_path: &Path,
    thread_key: &str,
    thread_id: &str,
    turn_id: Option<&str>,
    turn_modes: &Arc<Mutex<HashMap<String, CollaborationMode>>>,
    interaction_sender: &Arc<Mutex<Option<RuntimeInteractionSender>>>,
    state: &Arc<Mutex<ObserverState>>,
    fallback_error: Option<String>,
) -> Result<()> {
    let mut state_guard = state.lock().await;
    let final_text = compose_visible_final_reply(
        &state_guard.latest_assistant_message,
        state_guard.latest_completed_plan_text.as_deref(),
    )
    .or_else(|| fallback_error.as_deref().map(str::to_owned));
    record_hcodex_ingress_completed(workspace_path, thread_id, turn_id, final_text.as_deref())
        .await?;
    let collaboration_mode = match turn_id {
        Some(turn_id) => turn_modes.lock().await.remove(turn_id),
        None => None,
    }
    .unwrap_or(CollaborationMode::Default);
    emit_runtime_interaction(
        interaction_sender,
        RuntimeInteractionEvent::TurnCompleted(TurnCompletionSummary {
            thread_key: thread_key.to_owned(),
            collaboration_mode,
            final_text: final_text.clone(),
            has_plan: state_guard.latest_completed_plan_text.is_some(),
        }),
    )
    .await;
    state_guard.latest_assistant_message.clear();
    state_guard.latest_completed_plan_text = None;
    Ok(())
}

async fn handle_server_request(
    thread_key: &str,
    interaction_sender: &Arc<Mutex<Option<RuntimeInteractionSender>>>,
    request: CodexServerRequest,
) -> Result<()> {
    let CodexServerRequest::RequestUserInput { request_id, params } = request;
    emit_runtime_interaction(
        interaction_sender,
        RuntimeInteractionEvent::RequestUserInput(RuntimeInteractionRequest {
            thread_key: thread_key.to_owned(),
            request_id,
            params,
        }),
    )
    .await;
    Ok(())
}

async fn handle_server_notification(
    interaction_sender: &Arc<Mutex<Option<RuntimeInteractionSender>>>,
    notification: CodexServerNotification,
) -> Result<()> {
    let CodexServerNotification::ServerRequestResolved(resolved) = notification;
    emit_runtime_interaction(
        interaction_sender,
        RuntimeInteractionEvent::RequestResolved(RuntimeInteractionResolved {
            thread_id: resolved.thread_id,
            request_id: resolved.request_id,
        }),
    )
    .await;
    Ok(())
}

async fn emit_runtime_interaction(
    interaction_sender: &Arc<Mutex<Option<RuntimeInteractionSender>>>,
    event: RuntimeInteractionEvent,
) {
    let Some(sender) = interaction_sender.lock().await.as_ref().cloned() else {
        return;
    };
    let _ = sender.send(event);
}

fn extract_agent_message_text(event: &CodexThreadEvent) -> Option<String> {
    let item = match event {
        CodexThreadEvent::ItemUpdated { item } | CodexThreadEvent::ItemCompleted { item } => item,
        _ => return None,
    };
    if item.get("type").and_then(Value::as_str) != Some("agent_message") {
        return None;
    }
    item.get("text")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

fn extract_completed_plan_text(event: &CodexThreadEvent) -> Option<String> {
    let item = match event {
        CodexThreadEvent::ItemCompleted { item } => item,
        _ => return None,
    };
    if item.get("type").and_then(Value::as_str) != Some("plan") {
        return None;
    }
    item.get("text")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

fn extract_user_prompt_text(event: &CodexThreadEvent) -> Option<String> {
    let item = match event {
        CodexThreadEvent::ItemCompleted { item } => item,
        _ => return None,
    };
    if item.get("type").and_then(Value::as_str) != Some("user_message") {
        return None;
    }
    item.get("text")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            item.get("content")
                .and_then(Value::as_array)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|value| value.get("text").and_then(Value::as_str))
                        .map(str::trim)
                        .filter(|text| !text.is_empty())
                        .collect::<Vec<_>>()
                        .join("\n\n")
                })
                .filter(|text| !text.is_empty())
        })
}

#[cfg(test)]
mod tests {
    use super::AppServerMirrorObserverManager;
    use crate::collaboration_mode::CollaborationMode;
    use crate::workspace_status::{
        ObserverAttachMode, events_path, read_session_status, record_hcodex_ingress_connected,
    };
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::sync::Arc;
    use tokio::fs;
    use tokio::sync::Mutex;
    use tokio_tungstenite::tungstenite::Message as WsMessage;
    use uuid::Uuid;

    fn temp_path() -> PathBuf {
        std::env::temp_dir().join(format!(
            "threadbridge-app-server-observer-test-{}",
            Uuid::new_v4()
        ))
    }

    #[tokio::test]
    async fn live_forwarded_source_writes_preview_and_completion_events() {
        let workspace = temp_path();
        let manager = AppServerMirrorObserverManager::new(Arc::new(Mutex::new(HashMap::<
            String,
            CollaborationMode,
        >::new())));
        manager
            .register_live_forwarded_source(&workspace, "thread-key", "thr_tui")
            .await
            .unwrap();
        record_hcodex_ingress_connected(
            &workspace,
            "thread-key",
            "thr_tui",
            ObserverAttachMode::LiveForwarded,
        )
        .await
        .unwrap();

        manager
            .observe_forwarded_daemon_message(
                &workspace,
                "thread-key",
                &WsMessage::Text(
                    serde_json::json!({
                        "method": "item/completed",
                        "params": {
                            "item": {
                                "id": "msg_1",
                                "type": "agent_message",
                                "text": "hello from live forwarding"
                            }
                        }
                    })
                    .to_string()
                    .into(),
                ),
            )
            .await
            .unwrap();
        manager
            .observe_forwarded_daemon_message(
                &workspace,
                "thread-key",
                &WsMessage::Text(
                    serde_json::json!({
                        "method": "turn/completed",
                        "params": {
                            "turn": {
                                "id": "turn-1",
                                "status": "completed"
                            }
                        }
                    })
                    .to_string()
                    .into(),
                ),
            )
            .await
            .unwrap();

        let events = fs::read_to_string(events_path(&workspace)).await.unwrap();
        assert!(events.contains("\"event\":\"preview_text\""), "{events}");
        assert!(events.contains("\"event\":\"turn_completed\""), "{events}");

        let session = read_session_status(&workspace, "thr_tui")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            session.observer_attach_mode,
            Some(ObserverAttachMode::LiveForwarded)
        );

        let _ = fs::remove_dir_all(workspace).await;
    }

    #[tokio::test]
    async fn live_forwarded_source_rejects_resume_ws_handoff_for_same_thread() {
        let workspace = temp_path();
        let manager = AppServerMirrorObserverManager::new(Arc::new(Mutex::new(HashMap::<
            String,
            CollaborationMode,
        >::new())));
        manager
            .register_live_forwarded_source(&workspace, "thread-key", "thr_tui")
            .await
            .unwrap();

        let error = manager
            .ensure_thread_observer(&workspace, "ws://127.0.0.1:1", "thread-key", "thr_tui")
            .await
            .unwrap_err();
        assert!(error.to_string().contains("mode conflict"));

        let _ = fs::remove_dir_all(workspace).await;
    }
}
