use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use serde_json::Value;
use tokio::sync::Mutex;
use tokio::task::AbortHandle;
use tracing::warn;

use crate::codex::{
    CodexServerNotification, CodexServerRequest, CodexThreadEvent, observe_thread_with_handlers,
};
use crate::collaboration_mode::CollaborationMode;
use crate::process_transcript::process_entry_from_codex_event;
use crate::repository::{TranscriptMirrorOrigin, TranscriptMirrorPhase};
use crate::runtime_interaction::{
    RuntimeInteractionEvent, RuntimeInteractionRequest, RuntimeInteractionResolved,
    RuntimeInteractionSender, TurnCompletionSummary,
};
use crate::telegram_runtime::final_reply::compose_visible_final_reply;
use crate::workspace_status::{
    record_hcodex_ingress_completed, record_hcodex_ingress_preview_text,
    record_hcodex_ingress_process_event, record_hcodex_ingress_prompt,
};

#[derive(Debug, Clone)]
pub struct AppServerMirrorObserverManager {
    turn_modes: Arc<Mutex<HashMap<String, CollaborationMode>>>,
    inner: Arc<Mutex<HashMap<String, RunningObserver>>>,
    interaction_sender: Arc<Mutex<Option<RuntimeInteractionSender>>>,
}

#[derive(Debug, Clone)]
struct RunningObserver {
    thread_id: String,
    abort_handle: AbortHandle,
}

#[derive(Debug, Default)]
struct ObserverState {
    latest_assistant_message: String,
    latest_completed_plan_text: Option<String>,
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
        let mut inner = self.inner.lock().await;
        if let Some(existing) = inner.get(&key) {
            if existing.thread_id == thread_id {
                return Ok(());
            }
            existing.abort_handle.abort();
        }

        let workspace_path = workspace_path
            .canonicalize()
            .unwrap_or_else(|_| workspace_path.to_path_buf());
        let daemon_ws_url = daemon_ws_url.to_owned();
        let thread_key = thread_key.to_owned();
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
                thread_key,
                observer_thread_id,
            )
            .await
            {
                warn!(event = "app_server_observer.failed", error = %error);
            }
        });
        inner.insert(
            key,
            RunningObserver {
                thread_id,
                abort_handle: task.abort_handle(),
            },
        );
        Ok(())
    }

    pub async fn stop_thread_observer(&self, workspace_path: &Path, thread_key: &str) {
        let key = observer_key(workspace_path, thread_key);
        if let Some(existing) = self.inner.lock().await.remove(&key) {
            existing.abort_handle.abort();
        }
    }

    pub async fn record_turn_mode(&self, turn_id: &str, mode: CollaborationMode) {
        self.turn_modes
            .lock()
            .await
            .insert(turn_id.to_owned(), mode);
    }
}

fn observer_key(workspace_path: &Path, thread_key: &str) -> String {
    format!(
        "{}::{thread_key}",
        workspace_path
            .canonicalize()
            .unwrap_or_else(|_| workspace_path.to_path_buf())
            .display()
    )
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
        CodexThreadEvent::ThreadStarted { .. }
        | CodexThreadEvent::TurnStarted
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
