use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;
use serde::Serialize;

use crate::execution_mode::{ExecutionMode, workspace_execution_mode};
use crate::repository::{RecentCodexSessionEntry, SessionBinding, ThreadRepository};
use crate::runtime_owner::{DesktopRuntimeOwner, RuntimeOwnerStatus, WorkspaceRuntimeHeartbeat};
use crate::thread_state::resolve_thread_state;

#[derive(Debug, Clone, Serialize)]
pub struct RuntimeHealthView {
    pub management_bind_addr: String,
    pub broken_threads: usize,
    pub running_workspaces: usize,
    pub conflicted_workspaces: usize,
    pub ready_workspaces: usize,
    pub degraded_workspaces: usize,
    pub unavailable_workspaces: usize,
    pub app_server_status: &'static str,
    pub tui_proxy_status: &'static str,
    pub handoff_readiness: &'static str,
    pub recovery_hint: Option<String>,
    pub runtime_owner: RuntimeOwnerStatus,
    pub managed_codex: ManagedCodexView,
}

#[derive(Debug, Clone, Serialize)]
pub struct ManagedCodexView {
    pub source: &'static str,
    pub source_file_path: String,
    pub build_config_file_path: String,
    pub build_info_file_path: String,
    pub binary_path: String,
    pub binary_ready: bool,
    pub version: Option<String>,
    pub build_defaults: ManagedCodexBuildDefaultsView,
    pub build_info: Option<ManagedCodexBuildInfoView>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ManagedCodexBuildInfoView {
    pub source_repo: Option<String>,
    pub source_rs_dir: Option<String>,
    pub build_profile: Option<String>,
    pub git_rev: Option<String>,
    pub binary: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ManagedCodexBuildDefaultsView {
    pub source_repo: String,
    pub source_rs_dir: String,
    pub build_profile: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ManagedWorkspaceView {
    pub workspace_cwd: String,
    pub title: Option<String>,
    pub thread_key: Option<String>,
    pub workspace_execution_mode: ExecutionMode,
    pub current_execution_mode: Option<ExecutionMode>,
    pub current_approval_policy: Option<String>,
    pub current_sandbox_policy: Option<String>,
    pub mode_drift: bool,
    pub binding_status: &'static str,
    pub run_status: &'static str,
    pub current_codex_thread_id: Option<String>,
    pub tui_active_codex_thread_id: Option<String>,
    pub tui_session_adoption_pending: bool,
    pub session_broken: bool,
    pub last_used_at: Option<String>,
    pub conflict: bool,
    pub app_server_status: &'static str,
    pub tui_proxy_status: &'static str,
    pub handoff_readiness: &'static str,
    pub runtime_health_source: &'static str,
    pub heartbeat_last_checked_at: Option<String>,
    pub heartbeat_last_error: Option<String>,
    pub session_broken_reason: Option<String>,
    pub recovery_hint: Option<String>,
    pub hcodex_path: String,
    pub hcodex_available: bool,
    pub recent_codex_sessions: Vec<RecentCodexSessionEntry>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ThreadStateView {
    pub thread_key: String,
    pub title: Option<String>,
    pub workspace_cwd: Option<String>,
    pub workspace_execution_mode: Option<ExecutionMode>,
    pub current_execution_mode: Option<ExecutionMode>,
    pub current_approval_policy: Option<String>,
    pub current_sandbox_policy: Option<String>,
    pub lifecycle_status: &'static str,
    pub binding_status: &'static str,
    pub run_status: &'static str,
    pub current_codex_thread_id: Option<String>,
    pub tui_active_codex_thread_id: Option<String>,
    pub tui_session_adoption_pending: bool,
    pub archived_at: Option<String>,
    pub last_used_at: Option<String>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ArchivedThreadView {
    pub thread_key: String,
    pub title: Option<String>,
    pub workspace_cwd: Option<String>,
    pub archived_at: Option<String>,
    pub previous_message_thread_ids: Vec<i32>,
}

#[derive(Debug)]
struct WorkspaceAggregateView {
    items: Vec<ManagedWorkspaceView>,
}

#[derive(Debug, Clone)]
pub struct WorkspaceRuntimeHealth {
    pub app_server_status: &'static str,
    pub tui_proxy_status: &'static str,
    pub handoff_readiness: &'static str,
    pub source: &'static str,
    pub last_checked_at: Option<String>,
    pub last_error: Option<String>,
}

impl WorkspaceRuntimeHealth {
    fn from_heartbeat(heartbeat: WorkspaceRuntimeHeartbeat) -> Self {
        Self {
            app_server_status: heartbeat.app_server_status,
            tui_proxy_status: heartbeat.tui_proxy_status,
            handoff_readiness: heartbeat.handoff_readiness,
            source: "owner_heartbeat",
            last_checked_at: Some(heartbeat.last_checked_at),
            last_error: heartbeat.last_error,
        }
    }
}

pub async fn build_workspace_views(
    repository: &ThreadRepository,
    runtime_owner: Option<&DesktopRuntimeOwner>,
) -> Result<Vec<ManagedWorkspaceView>> {
    let active_threads = repository.list_active_threads().await?;
    let mut grouped: BTreeMap<String, WorkspaceAggregateView> = BTreeMap::new();
    for record in active_threads {
        let Some(binding) = repository.read_session_binding(&record).await? else {
            continue;
        };
        let Some(workspace_cwd) = binding.workspace_cwd.clone() else {
            continue;
        };
        let aggregate = grouped
            .entry(workspace_cwd.clone())
            .or_insert_with(|| WorkspaceAggregateView { items: Vec::new() });
        let workspace_path = Path::new(&workspace_cwd);
        let workspace_execution_mode = workspace_execution_mode(workspace_path)
            .await
            .unwrap_or_default();
        let hcodex_path = workspace_path
            .join(".threadbridge")
            .join("bin")
            .join("hcodex");
        let mut runtime_status = read_workspace_runtime_health(workspace_path, runtime_owner).await;
        if binding.tui_session_adoption_pending && runtime_status.handoff_readiness == "ready" {
            runtime_status.handoff_readiness = "pending_adoption";
        }
        let recent_sessions = repository
            .read_recent_workspace_sessions(&workspace_cwd)
            .await
            .unwrap_or_default();
        let resolved_state = resolve_thread_state(&record.metadata, Some(&binding)).await?;
        let session_broken = resolved_state.is_broken();
        let session_broken_reason = binding
            .session_broken_reason
            .clone()
            .or(record.metadata.session_broken_reason.clone());
        let recovery_hint = workspace_recovery_hint(
            false,
            session_broken,
            session_broken_reason.as_deref(),
            &runtime_status,
            binding.tui_session_adoption_pending,
            binding.tui_active_codex_thread_id.as_deref(),
        );
        aggregate.items.push(ManagedWorkspaceView {
            workspace_cwd: workspace_cwd.clone(),
            title: record.metadata.title.clone(),
            thread_key: Some(record.metadata.thread_key.clone()),
            workspace_execution_mode,
            current_execution_mode: binding.current_execution_mode,
            current_approval_policy: binding.current_approval_policy.clone(),
            current_sandbox_policy: binding.current_sandbox_policy.clone(),
            mode_drift: workspace_mode_drift(workspace_execution_mode, &binding),
            binding_status: resolved_state.binding_status.as_str(),
            run_status: resolved_state.run_status.as_str(),
            current_codex_thread_id: binding.current_codex_thread_id.clone(),
            tui_active_codex_thread_id: binding.tui_active_codex_thread_id.clone(),
            tui_session_adoption_pending: binding.tui_session_adoption_pending,
            session_broken,
            last_used_at: record.metadata.last_codex_turn_at.clone(),
            conflict: false,
            app_server_status: runtime_status.app_server_status,
            tui_proxy_status: runtime_status.tui_proxy_status,
            handoff_readiness: runtime_status.handoff_readiness,
            runtime_health_source: runtime_status.source,
            heartbeat_last_checked_at: runtime_status.last_checked_at,
            heartbeat_last_error: runtime_status.last_error,
            session_broken_reason,
            recovery_hint,
            hcodex_path: hcodex_path.display().to_string(),
            hcodex_available: hcodex_path.exists(),
            recent_codex_sessions: recent_sessions,
        });
    }

    let mut views = Vec::new();
    for aggregate in grouped.into_values() {
        let conflict = aggregate.items.len() > 1;
        if conflict {
            for mut item in aggregate.items {
                item.conflict = true;
                item.recovery_hint = workspace_recovery_hint(
                    true,
                    item.session_broken,
                    item.session_broken_reason.as_deref(),
                    &WorkspaceRuntimeHealth {
                        app_server_status: item.app_server_status,
                        tui_proxy_status: item.tui_proxy_status,
                        handoff_readiness: item.handoff_readiness,
                        source: item.runtime_health_source,
                        last_checked_at: item.heartbeat_last_checked_at.clone(),
                        last_error: item.heartbeat_last_error.clone(),
                    },
                    item.tui_session_adoption_pending,
                    item.tui_active_codex_thread_id.as_deref(),
                );
                views.push(item);
            }
            continue;
        }
        let item = aggregate
            .items
            .into_iter()
            .next()
            .expect("workspace group is non-empty");
        views.push(item);
    }
    views.sort_by(|a, b| a.workspace_cwd.cmp(&b.workspace_cwd));
    Ok(views)
}

pub async fn build_thread_views(repository: &ThreadRepository) -> Result<Vec<ThreadStateView>> {
    let active_threads = repository.list_active_threads().await?;
    let mut views = Vec::new();
    for record in active_threads {
        let binding = repository.read_session_binding(&record).await?;
        let workspace_cwd = binding
            .as_ref()
            .and_then(|binding| binding.workspace_cwd.clone());
        let workspace_execution_mode = match workspace_cwd.as_deref() {
            Some(workspace_cwd) => Some(
                workspace_execution_mode(Path::new(workspace_cwd))
                    .await
                    .unwrap_or_default(),
            ),
            None => None,
        };
        let resolved_state = resolve_thread_state(&record.metadata, binding.as_ref()).await?;
        views.push(ThreadStateView {
            thread_key: record.metadata.thread_key,
            title: record.metadata.title,
            workspace_cwd,
            workspace_execution_mode,
            current_execution_mode: binding
                .as_ref()
                .and_then(|binding| binding.current_execution_mode),
            current_approval_policy: binding
                .as_ref()
                .and_then(|binding| binding.current_approval_policy.clone()),
            current_sandbox_policy: binding
                .as_ref()
                .and_then(|binding| binding.current_sandbox_policy.clone()),
            lifecycle_status: resolved_state.lifecycle_status.as_str(),
            binding_status: resolved_state.binding_status.as_str(),
            run_status: resolved_state.run_status.as_str(),
            current_codex_thread_id: binding
                .as_ref()
                .and_then(|binding| binding.current_codex_thread_id.clone()),
            tui_active_codex_thread_id: binding
                .as_ref()
                .and_then(|binding| binding.tui_active_codex_thread_id.clone()),
            tui_session_adoption_pending: binding
                .as_ref()
                .is_some_and(|binding| binding.tui_session_adoption_pending),
            archived_at: record.metadata.archived_at,
            last_used_at: record.metadata.last_codex_turn_at,
            last_error: binding
                .as_ref()
                .and_then(|binding| binding.session_broken_reason.clone())
                .or(record.metadata.session_broken_reason),
        });
    }
    views.sort_by(|a, b| a.thread_key.cmp(&b.thread_key));
    Ok(views)
}

pub async fn build_archived_thread_views(
    repository: &ThreadRepository,
) -> Result<Vec<ArchivedThreadView>> {
    let archived = repository.list_all_archived_threads().await?;
    let mut views = Vec::with_capacity(archived.len());
    for record in archived {
        let binding = repository.read_session_binding(&record).await?;
        views.push(ArchivedThreadView {
            thread_key: record.metadata.thread_key,
            title: record.metadata.title,
            workspace_cwd: binding.and_then(|binding| binding.workspace_cwd),
            archived_at: record.metadata.archived_at,
            previous_message_thread_ids: record.metadata.previous_message_thread_ids,
        });
    }
    Ok(views)
}

pub fn build_runtime_health(
    management_bind_addr: String,
    workspaces: &[ManagedWorkspaceView],
    runtime_owner: RuntimeOwnerStatus,
    managed_codex: ManagedCodexView,
) -> RuntimeHealthView {
    let app_server_status = aggregate_running_status(
        workspaces
            .iter()
            .map(|workspace| workspace.app_server_status),
    );
    let tui_proxy_status = aggregate_running_status(
        workspaces
            .iter()
            .map(|workspace| workspace.tui_proxy_status),
    );
    let handoff_readiness = aggregate_handoff_status(
        workspaces
            .iter()
            .map(|workspace| workspace.handoff_readiness),
    );
    let recovery_hint = runtime_recovery_hint(
        &runtime_owner,
        workspaces
            .iter()
            .map(|workspace| workspace.recovery_hint.as_deref()),
        workspaces.iter().any(|workspace| workspace.conflict),
    );
    RuntimeHealthView {
        management_bind_addr,
        broken_threads: workspaces
            .iter()
            .filter(|workspace| workspace.session_broken)
            .count(),
        running_workspaces: workspaces
            .iter()
            .filter(|workspace| workspace.run_status == "running")
            .count(),
        conflicted_workspaces: workspaces
            .iter()
            .filter(|workspace| workspace.conflict)
            .count(),
        ready_workspaces: workspaces
            .iter()
            .filter(|workspace| workspace.handoff_readiness == "ready")
            .count(),
        degraded_workspaces: workspaces
            .iter()
            .filter(|workspace| {
                matches!(workspace.handoff_readiness, "degraded" | "pending_adoption")
            })
            .count(),
        unavailable_workspaces: workspaces
            .iter()
            .filter(|workspace| {
                !matches!(
                    workspace.handoff_readiness,
                    "ready" | "degraded" | "pending_adoption"
                )
            })
            .count(),
        app_server_status,
        tui_proxy_status,
        handoff_readiness,
        recovery_hint,
        runtime_owner,
        managed_codex,
    }
}

pub async fn read_workspace_runtime_health(
    workspace_path: &Path,
    runtime_owner: Option<&DesktopRuntimeOwner>,
) -> WorkspaceRuntimeHealth {
    match runtime_owner {
        Some(owner) => {
            if let Some(heartbeat) = owner.workspace_heartbeat(workspace_path).await {
                return WorkspaceRuntimeHealth::from_heartbeat(heartbeat);
            }
            WorkspaceRuntimeHealth {
                app_server_status: "missing",
                tui_proxy_status: "missing",
                handoff_readiness: "unavailable",
                source: "owner_pending",
                last_checked_at: None,
                last_error: Some(format!(
                    "desktop runtime owner has not published a heartbeat for {} yet",
                    workspace_path.display()
                )),
            }
        }
        None => WorkspaceRuntimeHealth {
            app_server_status: "missing",
            tui_proxy_status: "missing",
            handoff_readiness: "unavailable",
            source: "owner_required",
            last_checked_at: None,
            last_error: Some(
                "desktop runtime owner is required for managed workspace runtime".to_owned(),
            ),
        },
    }
}

pub fn workspace_recovery_hint(
    conflict: bool,
    session_broken: bool,
    session_broken_reason: Option<&str>,
    runtime_status: &WorkspaceRuntimeHealth,
    adoption_pending: bool,
    tui_active_codex_thread_id: Option<&str>,
) -> Option<String> {
    if conflict {
        return Some(
            "Resolve the active workspace binding conflict first. Tray launch stays disabled until only one active binding remains."
                .to_owned(),
        );
    }
    if matches!(runtime_status.source, "owner_required" | "owner_pending") {
        return Some(
            "Desktop runtime owner is required for this workspace. Start threadbridge_desktop and run Repair Runtime or Reconcile Runtime Owner."
                .to_owned(),
        );
    }
    if runtime_status.app_server_status != "running" {
        return Some(
            "App-server is not ready. Run Repair Runtime for this workspace, or Reconcile Runtime Owner for all managed workspaces."
                .to_owned(),
        );
    }
    if runtime_status.tui_proxy_status != "running" {
        return Some(
            "TUI proxy is not ready. Run Repair Runtime for this workspace, or Reconcile Runtime Owner to rebuild proxy state."
                .to_owned(),
        );
    }
    if adoption_pending || runtime_status.handoff_readiness == "pending_adoption" {
        return Some(
            "A live TUI session is waiting for adoption. Adopt TUI from Active Threads, or reject it to keep the original binding."
                .to_owned(),
        );
    }
    let has_live_tui_session = tui_active_codex_thread_id
        .map(str::trim)
        .is_some_and(|value| !value.is_empty());
    if session_broken && has_live_tui_session {
        return Some(
            "The saved Codex session is no longer the best recovery target, but this workspace has a live TUI session. Use Adopt TUI to promote that live session, or New Session to start fresh."
                .to_owned(),
        );
    }
    let unloaded_thread = session_broken_reason
        .map(str::to_ascii_lowercase)
        .is_some_and(|reason| {
            reason.contains("thread/read failed") && reason.contains("thread not loaded")
        });
    if unloaded_thread {
        return Some(
            "The saved Codex session is no longer loaded by app-server. Use New Session to start a fresh session, or Adopt TUI if this workspace already has a live TUI session."
                .to_owned(),
        );
    }
    if session_broken {
        return Some(
            "Codex continuity is marked broken. Use Repair Session after the runtime surface is healthy."
                .to_owned(),
        );
    }
    if runtime_status.handoff_readiness == "degraded" {
        return Some(
            "Handoff is degraded. Reconcile Runtime Owner or Repair Runtime to restore app-server and proxy continuity."
                .to_owned(),
        );
    }
    runtime_status
        .last_error
        .as_ref()
        .map(|error| format!("Inspect the latest runtime error before retrying: {error}"))
}

pub fn runtime_recovery_hint<'a>(
    runtime_owner: &RuntimeOwnerStatus,
    workspace_hints: impl Iterator<Item = Option<&'a str>>,
    has_conflicts: bool,
) -> Option<String> {
    if has_conflicts {
        return Some(
            "At least one workspace has multiple active bindings. Resolve conflicts in Managed Workspaces before trusting tray launch actions."
                .to_owned(),
        );
    }
    if runtime_owner.state == "error" {
        let suffix = runtime_owner
            .last_error
            .as_deref()
            .map(|error| format!(" Last owner error: {error}"))
            .unwrap_or_default();
        return Some(format!(
            "Desktop runtime owner is unhealthy. Run Reconcile Runtime Owner from this page.{suffix}"
        ));
    }
    workspace_hints.flatten().next().map(|hint| hint.to_owned())
}

pub fn aggregate_running_status<'a>(statuses: impl Iterator<Item = &'a str>) -> &'static str {
    let mut saw_any = false;
    let mut has_non_running = false;
    for status in statuses {
        saw_any = true;
        match status {
            "running" => {}
            _ => has_non_running = true,
        }
    }
    if !saw_any {
        return "missing";
    }
    if has_non_running {
        return "unavailable";
    }
    "running"
}

pub fn aggregate_handoff_status<'a>(statuses: impl Iterator<Item = &'a str>) -> &'static str {
    let mut saw_any = false;
    let mut has_unavailable = false;
    let mut has_degraded = false;
    for status in statuses {
        saw_any = true;
        match status {
            "ready" => {}
            "pending_adoption" => has_degraded = true,
            "degraded" => has_degraded = true,
            _ => has_unavailable = true,
        }
    }
    if !saw_any {
        return "missing";
    }
    if has_unavailable {
        return "unavailable";
    }
    if has_degraded {
        return "degraded";
    }
    "ready"
}

pub fn workspace_mode_drift(
    workspace_execution_mode: ExecutionMode,
    binding: &SessionBinding,
) -> bool {
    binding.current_codex_thread_id.is_some()
        && binding.current_execution_mode != Some(workspace_execution_mode)
}

#[cfg(test)]
mod tests {
    use super::{
        ManagedCodexBuildDefaultsView, ManagedCodexView, ManagedWorkspaceView, ThreadStateView,
        aggregate_handoff_status, build_runtime_health,
    };
    use crate::execution_mode::ExecutionMode;
    use crate::runtime_owner::RuntimeOwnerStatus;

    #[test]
    fn aggregate_handoff_status_treats_pending_adoption_as_degraded() {
        assert_eq!(
            aggregate_handoff_status(["ready", "pending_adoption"].into_iter()),
            "degraded"
        );
        assert_eq!(
            aggregate_handoff_status(["pending_adoption"].into_iter()),
            "degraded"
        );
    }

    #[test]
    fn thread_state_view_serializes_lifecycle_status() {
        let view = ThreadStateView {
            thread_key: "thread-1".to_owned(),
            title: Some("Workspace".to_owned()),
            workspace_cwd: Some("/tmp/workspace".to_owned()),
            workspace_execution_mode: Some(ExecutionMode::FullAuto),
            current_execution_mode: Some(ExecutionMode::FullAuto),
            current_approval_policy: Some("on-request".to_owned()),
            current_sandbox_policy: Some("workspace-write".to_owned()),
            lifecycle_status: "active",
            binding_status: "healthy",
            run_status: "idle",
            current_codex_thread_id: Some("thr_current".to_owned()),
            tui_active_codex_thread_id: None,
            tui_session_adoption_pending: false,
            archived_at: None,
            last_used_at: None,
            last_error: None,
        };

        let value = serde_json::to_value(view).unwrap();
        assert_eq!(value["lifecycle_status"], "active");
        assert_eq!(value["binding_status"], "healthy");
        assert_eq!(value["run_status"], "idle");
    }

    #[test]
    fn runtime_health_counts_use_workspace_view_conflict_flag() {
        let workspaces = vec![
            ManagedWorkspaceView {
                workspace_cwd: "/tmp/a".to_owned(),
                title: Some("A".to_owned()),
                thread_key: Some("thread-a".to_owned()),
                workspace_execution_mode: ExecutionMode::FullAuto,
                current_execution_mode: Some(ExecutionMode::FullAuto),
                current_approval_policy: None,
                current_sandbox_policy: None,
                mode_drift: false,
                binding_status: "healthy",
                run_status: "running",
                current_codex_thread_id: Some("thr-a".to_owned()),
                tui_active_codex_thread_id: None,
                tui_session_adoption_pending: false,
                session_broken: false,
                last_used_at: None,
                conflict: true,
                app_server_status: "running",
                tui_proxy_status: "running",
                handoff_readiness: "ready",
                runtime_health_source: "owner_heartbeat",
                heartbeat_last_checked_at: None,
                heartbeat_last_error: None,
                session_broken_reason: None,
                recovery_hint: Some("conflict".to_owned()),
                hcodex_path: "/tmp/a/.threadbridge/bin/hcodex".to_owned(),
                hcodex_available: true,
                recent_codex_sessions: Vec::new(),
            },
            ManagedWorkspaceView {
                workspace_cwd: "/tmp/b".to_owned(),
                title: Some("B".to_owned()),
                thread_key: Some("thread-b".to_owned()),
                workspace_execution_mode: ExecutionMode::Yolo,
                current_execution_mode: Some(ExecutionMode::Yolo),
                current_approval_policy: None,
                current_sandbox_policy: None,
                mode_drift: false,
                binding_status: "broken",
                run_status: "idle",
                current_codex_thread_id: Some("thr-b".to_owned()),
                tui_active_codex_thread_id: None,
                tui_session_adoption_pending: false,
                session_broken: true,
                last_used_at: None,
                conflict: false,
                app_server_status: "missing",
                tui_proxy_status: "missing",
                handoff_readiness: "unavailable",
                runtime_health_source: "owner_pending",
                heartbeat_last_checked_at: None,
                heartbeat_last_error: Some("missing".to_owned()),
                session_broken_reason: Some("broken".to_owned()),
                recovery_hint: Some("repair".to_owned()),
                hcodex_path: "/tmp/b/.threadbridge/bin/hcodex".to_owned(),
                hcodex_available: false,
                recent_codex_sessions: Vec::new(),
            },
        ];
        let view = build_runtime_health(
            "127.0.0.1:0".to_owned(),
            &workspaces,
            RuntimeOwnerStatus::inactive(),
            ManagedCodexView {
                source: "brew",
                source_file_path: "source.txt".to_owned(),
                build_config_file_path: "build-config.json".to_owned(),
                build_info_file_path: "build-info.txt".to_owned(),
                binary_path: "codex".to_owned(),
                binary_ready: true,
                version: Some("1.0.0".to_owned()),
                build_defaults: ManagedCodexBuildDefaultsView {
                    source_repo: "repo".to_owned(),
                    source_rs_dir: "rs".to_owned(),
                    build_profile: "dev".to_owned(),
                },
                build_info: None,
            },
        );

        assert_eq!(view.running_workspaces, 1);
        assert_eq!(view.broken_threads, 1);
        assert_eq!(view.conflicted_workspaces, 1);
        assert_eq!(view.app_server_status, "unavailable");
        assert_eq!(view.handoff_readiness, "unavailable");
    }
}
