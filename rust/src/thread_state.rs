use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::app_server_runtime::read_workspace_runtime_state_file;
use crate::codex::{CodexRunner, CodexWorkspace};
use crate::repository::{SessionBinding, ThreadMetadata, ThreadStatus};
use crate::workspace_status::{
    SessionActivitySource, SessionCurrentStatus, WorkspaceStatusCache, WorkspaceStatusPhase,
    read_workspace_status_with_cache,
};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleStatus {
    Active,
    Archived,
}

impl LifecycleStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Archived => "archived",
        }
    }
}

impl From<ThreadStatus> for LifecycleStatus {
    fn from(value: ThreadStatus) -> Self {
        match value {
            ThreadStatus::Active => Self::Active,
            ThreadStatus::Archived => Self::Archived,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BindingStatus {
    Unbound,
    Healthy,
    Broken,
}

impl BindingStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unbound => "unbound",
            Self::Healthy => "healthy",
            Self::Broken => "broken",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Idle,
    Running,
}

impl RunStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Running => "running",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResolvedThreadState {
    pub lifecycle_status: LifecycleStatus,
    pub binding_status: BindingStatus,
    pub run_status: RunStatus,
    pub run_phase: WorkspaceStatusPhase,
}

impl ResolvedThreadState {
    pub fn is_archived(self) -> bool {
        self.lifecycle_status == LifecycleStatus::Archived
    }

    pub fn is_unbound(self) -> bool {
        self.binding_status == BindingStatus::Unbound
    }

    pub fn is_broken(self) -> bool {
        self.binding_status == BindingStatus::Broken
    }

    pub fn is_running(self) -> bool {
        self.run_status == RunStatus::Running
    }
}

fn binding_workspace_path(binding: Option<&SessionBinding>) -> Option<PathBuf> {
    binding
        .and_then(|binding| binding.workspace_cwd.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
}

fn current_session_id(binding: &SessionBinding) -> Option<&str> {
    binding
        .current_codex_thread_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn tui_session_id(binding: &SessionBinding) -> Option<&str> {
    binding
        .tui_active_codex_thread_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

pub fn resolve_lifecycle_status(metadata: &ThreadMetadata) -> LifecycleStatus {
    metadata.status.clone().into()
}

pub fn resolve_binding_status(
    metadata: &ThreadMetadata,
    binding: Option<&SessionBinding>,
) -> BindingStatus {
    let _ = metadata;
    if binding_workspace_path(binding).is_none() {
        return BindingStatus::Unbound;
    }
    if binding.is_some_and(|binding| binding.session_broken) {
        BindingStatus::Broken
    } else {
        BindingStatus::Healthy
    }
}

pub async fn effective_busy_snapshot_for_binding(
    binding: Option<&SessionBinding>,
) -> Result<Option<SessionCurrentStatus>> {
    let Some(binding) = binding else {
        return Ok(None);
    };
    worker_busy_snapshot_for_binding(binding).await
}

async fn worker_busy_snapshot_for_binding(
    binding: &SessionBinding,
) -> Result<Option<SessionCurrentStatus>> {
    let Some(workspace_path) = binding_workspace_path(Some(binding)) else {
        return Ok(None);
    };
    let Some(session_ids) = candidate_session_ids(binding) else {
        return Ok(None);
    };
    let Some(runtime_state) = read_workspace_runtime_state_file(&workspace_path).await? else {
        bail!(
            "workspace runtime state is unavailable for {}",
            workspace_path.display()
        );
    };
    let Some(worker_ws_url) = runtime_state.worker_ws_url.as_deref() else {
        bail!(
            "workspace worker endpoint is unavailable for {}",
            workspace_path.display()
        );
    };

    let workspace = CodexWorkspace {
        working_directory: workspace_path.clone(),
        app_server_url: Some(worker_ws_url.to_owned()),
    };
    let runner = CodexRunner::new(None);
    for session_id in session_ids {
        let run_state = runner
            .read_thread_run_state(&workspace, &session_id)
            .await
            .with_context(|| {
                format!(
                    "failed to read worker run state for session `{session_id}` in {}",
                    workspace_path.display()
                )
            })?;
        if !run_state.is_busy {
            continue;
        }
        return Ok(Some(SessionCurrentStatus {
            schema_version: 2,
            workspace_cwd: workspace_path.display().to_string(),
            session_id,
            activity_source: SessionActivitySource::ManagedRuntime,
            live: true,
            phase: match run_state.phase.as_deref() {
                Some("turn_interrupt_requested") => WorkspaceStatusPhase::TurnFinalizing,
                Some("interrupted") | Some("failed") | Some("idle") => WorkspaceStatusPhase::Idle,
                _ => WorkspaceStatusPhase::TurnRunning,
            },
            shell_pid: None,
            child_pid: None,
            child_pgid: None,
            child_command: Some("app_server_ws_worker".to_owned()),
            client: Some("threadbridge-app-server-worker".to_owned()),
            turn_id: run_state.active_turn_id,
            summary: None,
            pending_interrupt_turn_id: None,
            pending_interrupt_requested_at: None,
            observer_attach_mode: None,
            updated_at: run_state.last_transition_at.unwrap_or_else(|| {
                chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
            }),
        }));
    }

    Ok(None)
}

fn candidate_session_ids(binding: &SessionBinding) -> Option<Vec<String>> {
    let mut session_ids = Vec::new();
    for candidate in [current_session_id(binding), tui_session_id(binding)]
        .into_iter()
        .flatten()
    {
        if session_ids.iter().any(|existing| existing == candidate) {
            continue;
        }
        session_ids.push(candidate.to_owned());
    }
    if session_ids.is_empty() {
        None
    } else {
        Some(session_ids)
    }
}

pub async fn cached_effective_busy_snapshot_for_binding(
    cache: &WorkspaceStatusCache,
    binding: Option<&SessionBinding>,
) -> Result<Option<SessionCurrentStatus>> {
    let Some(workspace_path) = binding_workspace_path(binding) else {
        return Ok(None);
    };
    let _ = read_workspace_status_with_cache(cache, &workspace_path).await?;
    effective_busy_snapshot_for_binding(binding).await
}

pub async fn resolve_run_status(binding: Option<&SessionBinding>) -> Result<RunStatus> {
    Ok(if resolve_run_phase(binding).await?.is_turn_busy() {
        RunStatus::Running
    } else {
        RunStatus::Idle
    })
}

pub async fn resolve_run_status_with_cache(
    cache: &WorkspaceStatusCache,
    binding: Option<&SessionBinding>,
) -> Result<RunStatus> {
    Ok(
        if resolve_run_phase_with_cache(cache, binding)
            .await?
            .is_turn_busy()
        {
            RunStatus::Running
        } else {
            RunStatus::Idle
        },
    )
}

pub async fn resolve_run_phase(binding: Option<&SessionBinding>) -> Result<WorkspaceStatusPhase> {
    Ok(effective_busy_snapshot_for_binding(binding)
        .await?
        .map(|snapshot| snapshot.phase)
        .unwrap_or(WorkspaceStatusPhase::Idle))
}

pub async fn resolve_run_phase_with_cache(
    cache: &WorkspaceStatusCache,
    binding: Option<&SessionBinding>,
) -> Result<WorkspaceStatusPhase> {
    Ok(cached_effective_busy_snapshot_for_binding(cache, binding)
        .await?
        .map(|snapshot| snapshot.phase)
        .unwrap_or(WorkspaceStatusPhase::Idle))
}

pub async fn resolve_thread_state(
    metadata: &ThreadMetadata,
    binding: Option<&SessionBinding>,
) -> Result<ResolvedThreadState> {
    let run_phase = resolve_run_phase(binding).await?;
    Ok(ResolvedThreadState {
        lifecycle_status: resolve_lifecycle_status(metadata),
        binding_status: resolve_binding_status(metadata, binding),
        run_status: if run_phase.is_turn_busy() {
            RunStatus::Running
        } else {
            RunStatus::Idle
        },
        run_phase,
    })
}

pub async fn resolve_thread_state_with_cache(
    metadata: &ThreadMetadata,
    binding: Option<&SessionBinding>,
    cache: &WorkspaceStatusCache,
) -> Result<ResolvedThreadState> {
    let run_phase = resolve_run_phase_with_cache(cache, binding).await?;
    Ok(ResolvedThreadState {
        lifecycle_status: resolve_lifecycle_status(metadata),
        binding_status: resolve_binding_status(metadata, binding),
        run_status: if run_phase.is_turn_busy() {
            RunStatus::Running
        } else {
            RunStatus::Idle
        },
        run_phase,
    })
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{
        BindingStatus, LifecycleStatus, ResolvedThreadState, RunStatus,
        effective_busy_snapshot_for_binding, resolve_thread_state,
    };
    use crate::app_server_runtime::WorkspaceRuntimeState;
    use crate::codex::{BackendThreadRunState, CodexRunner, CodexWorkspace};
    use crate::repository::{SessionBinding, ThreadMetadata, ThreadScope, ThreadStatus};
    use crate::workspace_status::{
        SessionActivitySource, SessionCurrentStatus, WorkspaceStatusPhase,
        default_local_tui_session_claim, ensure_workspace_status_surface, read_session_status,
        session_status_path, write_local_tui_session_claim,
    };
    use futures_util::{SinkExt, StreamExt};
    use tokio::fs;
    use tokio::net::TcpListener;
    use tokio_tungstenite::accept_async;
    use tokio_tungstenite::tungstenite::Message as WsMessage;
    use uuid::Uuid;

    fn temp_path() -> PathBuf {
        std::env::temp_dir().join(format!("threadbridge-thread-state-test-{}", Uuid::new_v4()))
    }

    fn metadata(status: ThreadStatus, session_broken: bool) -> ThreadMetadata {
        ThreadMetadata {
            archived_at: None,
            chat_id: 1,
            created_at: "2026-03-22T00:00:00.000Z".to_owned(),
            last_codex_turn_at: None,
            message_thread_id: Some(100),
            previous_message_thread_ids: Vec::new(),
            scope: ThreadScope::Thread,
            session_broken,
            session_broken_at: None,
            session_broken_reason: None,
            status,
            title: Some("Workspace".to_owned()),
            updated_at: "2026-03-22T00:00:00.000Z".to_owned(),
            thread_key: "thread-1".to_owned(),
        }
    }

    fn binding(
        workspace_path: &Path,
        current_codex_thread_id: Option<&str>,
        tui_active_codex_thread_id: Option<&str>,
        session_broken: bool,
    ) -> SessionBinding {
        serde_json::from_value(serde_json::json!({
            "schema_version": 3,
            "current_codex_thread_id": current_codex_thread_id,
            "workspace_cwd": workspace_path.display().to_string(),
            "bound_at": null,
            "initialized_at": null,
            "last_verified_at": null,
            "session_broken": session_broken,
            "session_broken_at": null,
            "session_broken_reason": null,
            "tui_active_codex_thread_id": tui_active_codex_thread_id,
            "tui_session_adoption_pending": false,
            "tui_session_adoption_prompt_message_id": null,
            "updated_at": "2026-03-22T00:00:00.000Z"
        }))
        .unwrap()
    }

    async fn write_session(
        workspace_path: &Path,
        session_id: &str,
        activity_source: SessionActivitySource,
        phase: WorkspaceStatusPhase,
    ) {
        let session = SessionCurrentStatus {
            schema_version: 2,
            workspace_cwd: workspace_path.display().to_string(),
            session_id: session_id.to_owned(),
            activity_source,
            live: activity_source == SessionActivitySource::Tui,
            phase,
            shell_pid: None,
            child_pid: None,
            child_pgid: None,
            child_command: None,
            client: Some("threadbridge".to_owned()),
            turn_id: (phase.is_turn_busy()).then_some("turn-1".to_owned()),
            summary: None,
            pending_interrupt_turn_id: None,
            pending_interrupt_requested_at: None,
            observer_attach_mode: None,
            updated_at: "2026-03-22T00:00:00.000Z".to_owned(),
        };
        fs::write(
            session_status_path(workspace_path, session_id),
            format!("{}\n", serde_json::to_string_pretty(&session).unwrap()),
        )
        .await
        .unwrap();
    }

    async fn write_runtime_state(workspace_path: &Path, worker_ws_url: &str) {
        let state_dir = workspace_path.join(".threadbridge/state/app-server");
        fs::create_dir_all(&state_dir).await.unwrap();
        let state = WorkspaceRuntimeState {
            schema_version: 3,
            workspace_cwd: workspace_path.display().to_string(),
            daemon_ws_url: "ws://127.0.0.1:1".to_owned(),
            worker_ws_url: Some(worker_ws_url.to_owned()),
            worker_pid: None,
            hcodex_ws_url: None,
        };
        fs::write(
            state_dir.join("current.json"),
            format!("{}\n", serde_json::to_string_pretty(&state).unwrap()),
        )
        .await
        .unwrap();
    }

    async fn start_mock_worker_for_busy_query() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                tokio::spawn(async move {
                    let mut ws = accept_async(stream).await.unwrap();
                    while let Some(message) = ws.next().await {
                        let Ok(message) = message else {
                            break;
                        };
                        let text = match message {
                            WsMessage::Text(text) => text.to_string(),
                            _ => continue,
                        };
                        let payload: serde_json::Value = serde_json::from_str(&text).unwrap();
                        let method = payload.get("method").and_then(|value| value.as_str());
                        let id = payload.get("id").and_then(|value| value.as_i64());
                        match method {
                            Some("initialize") => {
                                ws.send(WsMessage::Text(
                                    serde_json::json!({
                                        "jsonrpc": "2.0",
                                        "id": id.unwrap(),
                                        "result": {}
                                    })
                                    .to_string()
                                    .into(),
                                ))
                                .await
                                .unwrap();
                            }
                            Some("initialized") => {}
                            Some("threadbridge/getThreadRunState") => {
                                ws.send(WsMessage::Text(
                                    serde_json::json!({
                                        "jsonrpc": "2.0",
                                        "id": id.unwrap(),
                                        "result": {
                                            "threadId": "thr_current",
                                            "isBusy": true,
                                            "activeTurnId": "turn_worker",
                                            "interruptible": true,
                                            "phase": "turn_running"
                                        }
                                    })
                                    .to_string()
                                    .into(),
                                ))
                                .await
                                .unwrap();
                            }
                            _ => {}
                        }
                    }
                });
            }
        });
        format!("ws://127.0.0.1:{}", addr.port())
    }

    #[tokio::test]
    async fn resolve_thread_state_returns_idle_for_unbound_thread() {
        let state = resolve_thread_state(&metadata(ThreadStatus::Active, false), None)
            .await
            .unwrap();
        assert_eq!(state.lifecycle_status, LifecycleStatus::Active);
        assert_eq!(state.binding_status, BindingStatus::Unbound);
        assert_eq!(state.run_status, RunStatus::Idle);
    }

    #[tokio::test]
    async fn resolve_thread_state_returns_running_for_busy_current_session() {
        let workspace = temp_path();
        ensure_workspace_status_surface(&workspace).await.unwrap();
        write_session(
            &workspace,
            "thr_current",
            SessionActivitySource::ManagedRuntime,
            WorkspaceStatusPhase::TurnRunning,
        )
        .await;

        let state = resolve_thread_state(
            &metadata(ThreadStatus::Active, false),
            Some(&binding(&workspace, Some("thr_current"), None, false)),
        )
        .await
        .unwrap();

        assert_eq!(state.binding_status, BindingStatus::Healthy);
        assert_eq!(state.run_status, RunStatus::Running);

        let _ = fs::remove_dir_all(workspace).await;
    }

    #[tokio::test]
    async fn effective_busy_snapshot_prefers_tui_when_current_is_idle() {
        let workspace = temp_path();
        ensure_workspace_status_surface(&workspace).await.unwrap();
        write_session(
            &workspace,
            "thr_current",
            SessionActivitySource::ManagedRuntime,
            WorkspaceStatusPhase::Idle,
        )
        .await;
        write_session(
            &workspace,
            "thr_tui",
            SessionActivitySource::Tui,
            WorkspaceStatusPhase::TurnRunning,
        )
        .await;
        let mut claim = default_local_tui_session_claim(&workspace, "thread-1", std::process::id());
        claim.session_id = Some("thr_tui".to_owned());
        write_local_tui_session_claim(&workspace, &claim)
            .await
            .unwrap();

        let snapshot = effective_busy_snapshot_for_binding(Some(&binding(
            &workspace,
            Some("thr_current"),
            Some("thr_tui"),
            false,
        )))
        .await
        .unwrap()
        .unwrap();

        assert_eq!(snapshot.session_id, "thr_tui");
        assert_eq!(snapshot.phase, WorkspaceStatusPhase::TurnRunning);

        let _ = fs::remove_dir_all(workspace).await;
    }

    #[tokio::test]
    async fn resolve_thread_state_recovers_stale_busy_current_tui_session() {
        let workspace = temp_path();
        ensure_workspace_status_surface(&workspace).await.unwrap();
        write_session(
            &workspace,
            "thr_current",
            SessionActivitySource::Tui,
            WorkspaceStatusPhase::TurnRunning,
        )
        .await;

        let state = resolve_thread_state(
            &metadata(ThreadStatus::Active, false),
            Some(&binding(&workspace, Some("thr_current"), None, false)),
        )
        .await
        .unwrap();

        let session = read_session_status(&workspace, "thr_current")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(state.run_status, RunStatus::Idle);
        assert!(!session.live);
        assert_eq!(session.phase, WorkspaceStatusPhase::Idle);
        assert_eq!(session.turn_id, None);

        let _ = fs::remove_dir_all(workspace).await;
    }

    #[tokio::test]
    async fn effective_busy_snapshot_keeps_live_current_tui_session_busy() {
        let workspace = temp_path();
        ensure_workspace_status_surface(&workspace).await.unwrap();
        write_session(
            &workspace,
            "thr_current",
            SessionActivitySource::Tui,
            WorkspaceStatusPhase::TurnRunning,
        )
        .await;
        let mut claim = default_local_tui_session_claim(&workspace, "thread-1", std::process::id());
        claim.session_id = Some("thr_current".to_owned());
        write_local_tui_session_claim(&workspace, &claim)
            .await
            .unwrap();

        let snapshot = effective_busy_snapshot_for_binding(Some(&binding(
            &workspace,
            Some("thr_current"),
            None,
            false,
        )))
        .await
        .unwrap()
        .unwrap();

        assert_eq!(snapshot.session_id, "thr_current");
        assert_eq!(snapshot.phase, WorkspaceStatusPhase::TurnRunning);

        let _ = fs::remove_dir_all(workspace).await;
    }

    #[tokio::test]
    async fn effective_busy_snapshot_falls_back_to_worker_state() {
        let workspace = temp_path();
        ensure_workspace_status_surface(&workspace).await.unwrap();
        let worker_ws_url = start_mock_worker_for_busy_query().await;
        write_runtime_state(&workspace, &worker_ws_url).await;
        let direct = CodexRunner::new(None)
            .read_thread_run_state(
                &CodexWorkspace {
                    working_directory: workspace.clone(),
                    app_server_url: Some(worker_ws_url.clone()),
                },
                "thr_current",
            )
            .await;
        assert!(matches!(
            direct,
            Ok(BackendThreadRunState {
                is_busy: true,
                active_turn_id: Some(ref turn_id),
                ..
            }) if turn_id == "turn_worker"
        ));

        let snapshot = effective_busy_snapshot_for_binding(Some(&binding(
            &workspace,
            Some("thr_current"),
            None,
            false,
        )))
        .await
        .unwrap()
        .unwrap();

        assert_eq!(snapshot.session_id, "thr_current");
        assert_eq!(snapshot.turn_id.as_deref(), Some("turn_worker"));
        assert_eq!(snapshot.phase, WorkspaceStatusPhase::TurnRunning);

        let _ = fs::remove_dir_all(workspace).await;
    }

    #[tokio::test]
    async fn worker_interrupt_requested_maps_to_turn_finalizing() {
        let workspace = temp_path();
        ensure_workspace_status_surface(&workspace).await.unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                tokio::spawn(async move {
                    let mut ws = accept_async(stream).await.unwrap();
                    while let Some(message) = ws.next().await {
                        let Ok(message) = message else {
                            break;
                        };
                        let text = match message {
                            WsMessage::Text(text) => text.to_string(),
                            _ => continue,
                        };
                        let payload: serde_json::Value = serde_json::from_str(&text).unwrap();
                        let method = payload.get("method").and_then(|value| value.as_str());
                        let id = payload.get("id").and_then(|value| value.as_i64());
                        match method {
                            Some("initialize") => {
                                ws.send(WsMessage::Text(
                                    serde_json::json!({
                                        "jsonrpc": "2.0",
                                        "id": id.unwrap(),
                                        "result": {}
                                    })
                                    .to_string()
                                    .into(),
                                ))
                                .await
                                .unwrap();
                            }
                            Some("initialized") => {}
                            Some("threadbridge/getThreadRunState") => {
                                ws.send(WsMessage::Text(
                                    serde_json::json!({
                                        "jsonrpc": "2.0",
                                        "id": id.unwrap(),
                                        "result": {
                                            "threadId": "thr_current",
                                            "isBusy": true,
                                            "activeTurnId": "turn_worker",
                                            "interruptible": false,
                                            "phase": "turn_interrupt_requested"
                                        }
                                    })
                                    .to_string()
                                    .into(),
                                ))
                                .await
                                .unwrap();
                            }
                            _ => {}
                        }
                    }
                });
            }
        });
        write_runtime_state(&workspace, &format!("ws://127.0.0.1:{}", addr.port())).await;

        let snapshot = effective_busy_snapshot_for_binding(Some(&binding(
            &workspace,
            Some("thr_current"),
            None,
            false,
        )))
        .await
        .unwrap()
        .unwrap();

        assert_eq!(snapshot.phase, WorkspaceStatusPhase::TurnFinalizing);

        let _ = fs::remove_dir_all(workspace).await;
    }

    #[tokio::test]
    async fn resolve_thread_state_keeps_broken_binding_idle_without_busy_snapshot() {
        let workspace = temp_path();
        ensure_workspace_status_surface(&workspace).await.unwrap();
        let state = resolve_thread_state(
            &metadata(ThreadStatus::Archived, true),
            Some(&binding(&workspace, Some("thr_current"), None, true)),
        )
        .await
        .unwrap();

        assert_eq!(state.lifecycle_status, LifecycleStatus::Archived);
        assert_eq!(state.binding_status, BindingStatus::Broken);
        assert_eq!(state.run_status, RunStatus::Idle);

        let _ = fs::remove_dir_all(workspace).await;
    }

    #[tokio::test]
    async fn resolve_thread_state_ignores_stale_metadata_broken_flag_when_binding_is_healthy() {
        let workspace = temp_path();
        ensure_workspace_status_surface(&workspace).await.unwrap();
        let state = resolve_thread_state(
            &metadata(ThreadStatus::Active, true),
            Some(&binding(&workspace, Some("thr_current"), None, false)),
        )
        .await
        .unwrap();

        assert_eq!(state.binding_status, BindingStatus::Healthy);

        let _ = fs::remove_dir_all(workspace).await;
    }

    #[test]
    fn resolved_state_helpers_follow_canonical_axes() {
        let archived = ResolvedThreadState {
            lifecycle_status: LifecycleStatus::Archived,
            binding_status: BindingStatus::Healthy,
            run_status: RunStatus::Idle,
            run_phase: WorkspaceStatusPhase::Idle,
        };
        assert!(archived.is_archived());
        assert!(!archived.is_unbound());
        assert!(!archived.is_broken());
        assert!(!archived.is_running());

        let broken_running = ResolvedThreadState {
            lifecycle_status: LifecycleStatus::Active,
            binding_status: BindingStatus::Broken,
            run_status: RunStatus::Running,
            run_phase: WorkspaceStatusPhase::TurnRunning,
        };
        assert!(!broken_running.is_archived());
        assert!(!broken_running.is_unbound());
        assert!(broken_running.is_broken());
        assert!(broken_running.is_running());

        let unbound_idle = ResolvedThreadState {
            lifecycle_status: LifecycleStatus::Active,
            binding_status: BindingStatus::Unbound,
            run_status: RunStatus::Idle,
            run_phase: WorkspaceStatusPhase::Idle,
        };
        assert!(unbound_idle.is_unbound());
    }
}
