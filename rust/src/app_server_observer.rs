use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use serde_json::Value;
use tokio::sync::{Mutex, oneshot};
use tokio::task::JoinHandle;
use tokio::time::{Duration, timeout};
use tracing::{info, warn};

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
    stop_tx: Option<oneshot::Sender<()>>,
    task_handle: JoinHandle<()>,
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
        observer_ws_url: &str,
        thread_key: &str,
        thread_id: &str,
    ) -> Result<()> {
        let key = observer_key(workspace_path, thread_key);
        if !self.source_needs_replacement(&key, thread_id).await? {
            return Ok(());
        }
        let workspace_path = workspace_path
            .canonicalize()
            .unwrap_or_else(|_| workspace_path.to_path_buf());
        let observer_ws_url = observer_ws_url.to_owned();
        let thread_key = thread_key.to_owned();
        let observer_thread_key = thread_key.clone();
        let thread_id = thread_id.to_owned();
        let observer_thread_id = thread_id.clone();
        let turn_modes = self.turn_modes.clone();
        let interaction_sender = self.interaction_sender.clone();
        let (stop_tx, stop_rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            if let Err(error) = run_thread_observer(
                turn_modes,
                interaction_sender,
                workspace_path,
                observer_ws_url,
                observer_thread_key,
                observer_thread_id,
                stop_rx,
            )
            .await
            {
                warn!(event = "app_server_observer.failed", error = %error);
            }
        });
        self.replace_observer(key, thread_key, thread_id, stop_tx, task)
            .await
    }

    pub async fn stop_thread_observer(&self, workspace_path: &Path, thread_key: &str) {
        let key = observer_key(workspace_path, thread_key);
        if let Some(existing) = self.inner.lock().await.remove(&key) {
            let thread_id = existing.thread_id.clone();
            stop_running_observer(existing).await;
            info!(
                event = "app_server_observer.source_closed",
                thread_key = %thread_key,
                thread_id = %thread_id,
            );
        }
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
        stop_tx: oneshot::Sender<()>,
        task_handle: JoinHandle<()>,
    ) -> Result<()> {
        let previous = {
            let mut inner = self.inner.lock().await;
            inner.remove(&key)
        };
        if let Some(previous) = previous {
            stop_running_observer(previous).await;
        }
        info!(
            event = "app_server_observer.source_registered",
            thread_key = %thread_key,
            thread_id = %thread_id,
            attach_mode = ObserverAttachMode::WorkerObserve.as_str(),
        );
        self.inner.lock().await.insert(
            key,
            RunningObserver {
                thread_id,
                stop_tx: Some(stop_tx),
                task_handle,
            },
        );
        Ok(())
    }

    async fn source_needs_replacement(&self, key: &str, thread_id: &str) -> Result<bool> {
        let inner = self.inner.lock().await;
        let Some(existing) = inner.get(key) else {
            return Ok(true);
        };
        if existing.task_handle.is_finished() {
            return Ok(true);
        }
        if existing.thread_id == thread_id {
            return Ok(false);
        }
        Ok(true)
    }
}

async fn stop_running_observer(mut observer: RunningObserver) {
    if let Some(stop_tx) = observer.stop_tx.take() {
        let _ = stop_tx.send(());
    }
    let task_handle = &mut observer.task_handle;
    match timeout(Duration::from_secs(2), task_handle).await {
        Ok(join_result) => {
            if let Err(error) = join_result {
                warn!(
                    event = "app_server_observer.source_join_failed",
                    thread_id = %observer.thread_id,
                    error = %error,
                );
            }
        }
        Err(_) => {
            observer.task_handle.abort();
            let _ = observer.task_handle.await;
            warn!(
                event = "app_server_observer.source_abort_after_timeout",
                thread_id = %observer.thread_id,
            );
        }
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

async fn run_thread_observer(
    turn_modes: Arc<Mutex<HashMap<String, CollaborationMode>>>,
    interaction_sender: Arc<Mutex<Option<RuntimeInteractionSender>>>,
    workspace_path: PathBuf,
    observer_ws_url: String,
    thread_key: String,
    thread_id: String,
    shutdown_rx: oneshot::Receiver<()>,
) -> Result<()> {
    let state = Arc::new(Mutex::new(ObserverState::default()));
    observe_thread_with_handlers(
        &observer_ws_url,
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
        Some(shutdown_rx),
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

    if let Some((turn_id, text)) = extract_agent_message_text(&event) {
        state.lock().await.latest_assistant_message = text.clone();
        record_hcodex_ingress_preview_text(workspace_path, thread_id, turn_id.as_deref(), &text)
            .await?;
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

fn extract_agent_message_text(event: &CodexThreadEvent) -> Option<(Option<String>, String)> {
    let (turn_id, item) = match event {
        CodexThreadEvent::ItemUpdated { turn_id, item }
        | CodexThreadEvent::ItemCompleted { turn_id, item } => (turn_id.clone(), item),
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
        .map(|text| (turn_id, text))
}

fn extract_completed_plan_text(event: &CodexThreadEvent) -> Option<String> {
    let item = match event {
        CodexThreadEvent::ItemCompleted { item, .. } => item,
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
        CodexThreadEvent::ItemCompleted { item, .. } => item,
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
    use super::{
        AppServerMirrorObserverManager, RunningObserver, extract_agent_message_text,
        extract_completed_plan_text, extract_user_prompt_text, observer_key, stop_running_observer,
    };
    use crate::codex::CodexThreadEvent;
    use serde_json::json;
    use std::collections::HashMap;
    use std::path::Path;
    use std::sync::Arc;
    use tokio::sync::{Mutex, oneshot};

    #[test]
    fn extract_agent_message_text_reads_item_text() {
        let event = CodexThreadEvent::ItemCompleted {
            turn_id: Some("turn-1".to_owned()),
            item: json!({
                "type": "agent_message",
                "text": " hello "
            }),
        };
        assert_eq!(
            extract_agent_message_text(&event),
            Some((Some("turn-1".to_owned()), "hello".to_owned()))
        );
    }

    #[test]
    fn extract_completed_plan_text_reads_plan_item() {
        let event = CodexThreadEvent::ItemCompleted {
            turn_id: Some("turn-1".to_owned()),
            item: json!({
                "type": "plan",
                "text": " final plan "
            }),
        };
        assert_eq!(
            extract_completed_plan_text(&event),
            Some("final plan".to_owned())
        );
    }

    #[test]
    fn extract_user_prompt_text_falls_back_to_content_segments() {
        let event = CodexThreadEvent::ItemCompleted {
            turn_id: Some("turn-1".to_owned()),
            item: json!({
                "type": "user_message",
                "content": [
                    {"text":"first"},
                    {"text":" second "}
                ]
            }),
        };
        assert_eq!(
            extract_user_prompt_text(&event),
            Some("first\n\nsecond".to_owned())
        );
    }

    #[tokio::test]
    async fn source_needs_replacement_when_task_finished() {
        let manager = AppServerMirrorObserverManager::new(Arc::new(Mutex::new(HashMap::new())));
        let key = observer_key(Path::new("/tmp/workspace"), "thread-key");
        let task_handle = tokio::spawn(async {});
        manager.inner.lock().await.insert(
            key.clone(),
            RunningObserver {
                thread_id: "thr_1".to_owned(),
                stop_tx: None,
                task_handle,
            },
        );
        tokio::task::yield_now().await;
        assert!(
            manager
                .source_needs_replacement(&key, "thr_1")
                .await
                .unwrap()
        );
        let existing = manager
            .inner
            .lock()
            .await
            .remove(&key)
            .expect("running observer");
        stop_running_observer(existing).await;
    }

    #[tokio::test]
    async fn source_needs_replacement_respects_running_task_and_thread_id() {
        let manager = AppServerMirrorObserverManager::new(Arc::new(Mutex::new(HashMap::new())));
        let key = observer_key(Path::new("/tmp/workspace"), "thread-key");
        let (stop_tx, stop_rx) = oneshot::channel();
        let task_handle = tokio::spawn(async move {
            let _ = stop_rx.await;
        });
        manager.inner.lock().await.insert(
            key.clone(),
            RunningObserver {
                thread_id: "thr_1".to_owned(),
                stop_tx: Some(stop_tx),
                task_handle,
            },
        );

        assert!(
            !manager
                .source_needs_replacement(&key, "thr_1")
                .await
                .unwrap()
        );
        assert!(
            manager
                .source_needs_replacement(&key, "thr_2")
                .await
                .unwrap()
        );

        let existing = manager
            .inner
            .lock()
            .await
            .remove(&key)
            .expect("running observer");
        stop_running_observer(existing).await;
    }
}
