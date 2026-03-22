use std::path::PathBuf;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::repository::{SessionBinding, ThreadMetadata, ThreadStatus};
use crate::workspace_status::{
    SessionCurrentStatus, WorkspaceStatusCache, read_session_status,
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
    if binding_workspace_path(binding).is_none() {
        return BindingStatus::Unbound;
    }
    if binding.is_some_and(|binding| binding.session_broken) || metadata.session_broken {
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
    let Some(workspace_path) = binding_workspace_path(Some(binding)) else {
        return Ok(None);
    };

    let current_snapshot = if let Some(session_id) = current_session_id(binding) {
        read_session_status(&workspace_path, session_id).await?
    } else {
        None
    };
    if current_snapshot
        .as_ref()
        .is_some_and(|snapshot| snapshot.phase.is_turn_busy())
    {
        return Ok(current_snapshot);
    }

    let Some(tui_session_id) = tui_session_id(binding) else {
        return Ok(current_snapshot);
    };
    if Some(tui_session_id) == current_session_id(binding) {
        return Ok(current_snapshot);
    }

    let tui_snapshot = read_session_status(&workspace_path, tui_session_id).await?;
    if tui_snapshot
        .as_ref()
        .is_some_and(|snapshot| snapshot.phase.is_turn_busy())
    {
        Ok(tui_snapshot)
    } else {
        Ok(current_snapshot)
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
    Ok(
        if effective_busy_snapshot_for_binding(binding)
            .await?
            .as_ref()
            .is_some_and(|snapshot| snapshot.phase.is_turn_busy())
        {
            RunStatus::Running
        } else {
            RunStatus::Idle
        },
    )
}

pub async fn resolve_thread_state(
    metadata: &ThreadMetadata,
    binding: Option<&SessionBinding>,
) -> Result<ResolvedThreadState> {
    Ok(ResolvedThreadState {
        lifecycle_status: resolve_lifecycle_status(metadata),
        binding_status: resolve_binding_status(metadata, binding),
        run_status: resolve_run_status(binding).await?,
    })
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::{
        BindingStatus, LifecycleStatus, RunStatus, effective_busy_snapshot_for_binding,
        resolve_thread_state,
    };
    use crate::repository::{SessionBinding, ThreadMetadata, ThreadScope, ThreadStatus};
    use crate::workspace_status::{
        SessionCurrentStatus, SessionStatusOwner, WorkspaceStatusPhase,
        ensure_workspace_status_surface, session_status_path,
    };
    use tokio::fs;
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
        owner: SessionStatusOwner,
        phase: WorkspaceStatusPhase,
    ) {
        let session = SessionCurrentStatus {
            schema_version: 2,
            workspace_cwd: workspace_path.display().to_string(),
            session_id: session_id.to_owned(),
            owner,
            live: owner == SessionStatusOwner::Local,
            phase,
            shell_pid: None,
            child_pid: None,
            child_pgid: None,
            child_command: None,
            client: Some("threadbridge".to_owned()),
            turn_id: (phase.is_turn_busy()).then_some("turn-1".to_owned()),
            summary: None,
            updated_at: "2026-03-22T00:00:00.000Z".to_owned(),
        };
        fs::write(
            session_status_path(workspace_path, session_id),
            format!("{}\n", serde_json::to_string_pretty(&session).unwrap()),
        )
        .await
        .unwrap();
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
            SessionStatusOwner::Bot,
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
            SessionStatusOwner::Bot,
            WorkspaceStatusPhase::Idle,
        )
        .await;
        write_session(
            &workspace,
            "thr_tui",
            SessionStatusOwner::Local,
            WorkspaceStatusPhase::TurnRunning,
        )
        .await;

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
}
