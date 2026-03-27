use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::execution_mode::{ExecutionMode, workspace_execution_mode};
use crate::repository::{
    RecentCodexSessionEntry, SessionBinding, ThreadRecord, ThreadRepository,
    TranscriptMirrorDelivery, TranscriptMirrorEntry, TranscriptMirrorOrigin, TranscriptMirrorPhase,
    TranscriptMirrorRole,
};
use crate::runtime_owner::{DesktopRuntimeOwner, RuntimeOwnerStatus, WorkspaceRuntimeHeartbeat};
use crate::thread_state::{
    BindingStatus, effective_busy_snapshot_for_binding, resolve_binding_status,
    resolve_lifecycle_status, resolve_thread_state,
};
use crate::workspace_status::{WorkspaceStatusPhase, read_session_status};

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
    pub hcodex_ingress_status: &'static str,
    pub runtime_readiness: &'static str,
    pub recovery_hint: Option<String>,
    pub runtime_owner: RuntimeOwnerStatus,
    pub managed_codex: ManagedCodexView,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeEventKind {
    SetupChanged,
    RuntimeHealthChanged,
    ManagedCodexChanged,
    ThreadStateChanged,
    WorkspaceStateChanged,
    ArchivedThreadChanged,
    WorkingSessionChanged,
    TranscriptChanged,
    Error,
}

impl RuntimeEventKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::SetupChanged => "setup_changed",
            Self::RuntimeHealthChanged => "runtime_health_changed",
            Self::ManagedCodexChanged => "managed_codex_changed",
            Self::ThreadStateChanged => "thread_state_changed",
            Self::WorkspaceStateChanged => "workspace_state_changed",
            Self::ArchivedThreadChanged => "archived_thread_changed",
            Self::WorkingSessionChanged => "working_session_changed",
            Self::TranscriptChanged => "transcript_changed",
            Self::Error => "error",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeControlAction {
    StartFreshSession,
    RepairSessionBinding,
    LaunchLocalSession,
    SetWorkspaceExecutionMode,
    SetThreadCollaborationMode,
    InterruptRunningTurn,
    AdoptTuiSession,
    RejectTuiSession,
    ArchiveThread,
    RestoreThread,
    RepairWorkspaceRuntime,
}

impl RuntimeControlAction {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::StartFreshSession => "start_fresh_session",
            Self::RepairSessionBinding => "repair_session_binding",
            Self::LaunchLocalSession => "launch_local_session",
            Self::SetWorkspaceExecutionMode => "set_workspace_execution_mode",
            Self::SetThreadCollaborationMode => "set_thread_collaboration_mode",
            Self::InterruptRunningTurn => "interrupt_running_turn",
            Self::AdoptTuiSession => "adopt_tui_session",
            Self::RejectTuiSession => "reject_tui_session",
            Self::ArchiveThread => "archive_thread",
            Self::RestoreThread => "restore_thread",
            Self::RepairWorkspaceRuntime => "repair_workspace_runtime",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeInteractionKind {
    RequestUserInput,
    RequestResolved,
    TurnCompleted,
}

impl RuntimeInteractionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RequestUserInput => "request_user_input",
            Self::RequestResolved => "request_resolved",
            Self::TurnCompleted => "turn_completed",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeEventOperation {
    Upsert,
    Remove,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RuntimeEvent {
    pub kind: RuntimeEventKind,
    pub op: RuntimeEventOperation,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
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
    pub run_phase: &'static str,
    pub current_codex_thread_id: Option<String>,
    pub tui_active_codex_thread_id: Option<String>,
    pub tui_session_adoption_pending: bool,
    pub last_used_at: Option<String>,
    pub conflict: bool,
    pub app_server_status: &'static str,
    pub hcodex_ingress_status: &'static str,
    pub runtime_readiness: &'static str,
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
    pub chat_id: i64,
    pub message_thread_id: Option<i32>,
    pub workspace_cwd: Option<String>,
    pub workspace_execution_mode: Option<ExecutionMode>,
    pub current_execution_mode: Option<ExecutionMode>,
    pub current_approval_policy: Option<String>,
    pub current_sandbox_policy: Option<String>,
    pub lifecycle_status: &'static str,
    pub binding_status: &'static str,
    pub run_status: &'static str,
    pub run_phase: &'static str,
    pub current_codex_thread_id: Option<String>,
    pub tui_active_codex_thread_id: Option<String>,
    pub tui_session_adoption_pending: bool,
    pub session_broken_reason: Option<String>,
    pub last_verified_at: Option<String>,
    pub last_codex_turn_at: Option<String>,
    pub archived_at: Option<String>,
    pub last_used_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ArchivedThreadView {
    pub thread_key: String,
    pub title: Option<String>,
    pub workspace_cwd: Option<String>,
    pub archived_at: Option<String>,
    pub previous_message_thread_ids: Vec<i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorkingSessionRecordKind {
    UserPrompt,
    AssistantFinal,
    ProcessPlan,
    ProcessTool,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkingSessionSummaryView {
    pub session_id: String,
    pub thread_key: String,
    pub workspace_cwd: String,
    pub started_at: Option<String>,
    pub updated_at: String,
    pub run_status: String,
    pub run_phase: String,
    pub origins_seen: Vec<TranscriptMirrorOrigin>,
    pub record_count: usize,
    pub tool_use_count: usize,
    pub has_final_reply: bool,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkingSessionRecordView {
    pub timestamp: String,
    pub session_id: String,
    pub kind: WorkingSessionRecordKind,
    pub origin: Option<TranscriptMirrorOrigin>,
    pub role: Option<TranscriptMirrorRole>,
    pub summary: String,
    pub text: String,
    pub delivery: Option<TranscriptMirrorDelivery>,
    pub phase: Option<TranscriptMirrorPhase>,
    pub source_ref: String,
}

#[derive(Debug)]
struct WorkspaceAggregateView {
    items: Vec<ManagedWorkspaceView>,
}

#[derive(Debug, Clone, Default)]
struct WorkingSessionAggregate {
    entries: Vec<TranscriptMirrorEntry>,
    status_updated_at: Option<String>,
    is_running: bool,
    phase: Option<WorkspaceStatusPhase>,
    run_status_override: Option<&'static str>,
    run_phase_override: Option<&'static str>,
    history_updated_at: Option<String>,
    last_error: Option<WorkingSessionError>,
}

#[derive(Debug, Clone)]
struct WorkingSessionError {
    timestamp: String,
    reason: String,
}

#[derive(Debug, Clone)]
pub struct WorkspaceRuntimeHealth {
    pub app_server_status: &'static str,
    pub hcodex_ingress_status: &'static str,
    pub runtime_readiness: &'static str,
    pub source: &'static str,
    pub last_checked_at: Option<String>,
    pub last_error: Option<String>,
}

impl WorkspaceRuntimeHealth {
    fn from_heartbeat(heartbeat: WorkspaceRuntimeHeartbeat) -> Self {
        Self {
            app_server_status: heartbeat.app_server_status,
            hcodex_ingress_status: heartbeat.hcodex_ingress_status,
            runtime_readiness: heartbeat.runtime_readiness,
            source: "owner_heartbeat",
            last_checked_at: Some(heartbeat.last_checked_at),
            last_error: heartbeat.last_error,
        }
    }
}

fn canonical_binding_broken_reason(
    metadata: &crate::repository::ThreadMetadata,
    binding: Option<&SessionBinding>,
) -> Option<String> {
    let binding = binding?;
    if !binding.session_broken {
        return None;
    }
    binding
        .session_broken_reason
        .clone()
        .or(metadata.session_broken_reason.clone())
}

fn canonical_binding_broken_at(
    metadata: &crate::repository::ThreadMetadata,
    binding: Option<&SessionBinding>,
) -> Option<String> {
    let binding = binding?;
    if !binding.session_broken {
        return None;
    }
    binding
        .session_broken_at
        .clone()
        .or(metadata.session_broken_at.clone())
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
        if binding.tui_session_adoption_pending && runtime_status.runtime_readiness == "ready" {
            runtime_status.runtime_readiness = "pending_adoption";
        }
        let recent_sessions = repository
            .read_recent_workspace_sessions(&workspace_cwd)
            .await
            .unwrap_or_default();
        let resolved_state = resolve_thread_state(&record.metadata, Some(&binding)).await;
        let session_broken_reason =
            canonical_binding_broken_reason(&record.metadata, Some(&binding));
        let (run_status, run_phase) = match resolved_state {
            Ok(state) => (state.run_status.as_str(), state.run_phase.as_str()),
            Err(error) => {
                runtime_status.last_error = Some(match runtime_status.last_error.take() {
                    Some(existing) => format!("{existing}; {error}"),
                    None => error.to_string(),
                });
                if runtime_status.runtime_readiness == "ready" {
                    runtime_status.runtime_readiness = "degraded";
                }
                if runtime_status.app_server_status == "running" {
                    runtime_status.app_server_status = "unavailable";
                }
                ("unavailable", "unavailable")
            }
        };
        let recovery_hint = workspace_recovery_hint(
            false,
            resolve_binding_status(&record.metadata, Some(&binding)).as_str(),
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
            binding_status: resolve_binding_status(&record.metadata, Some(&binding)).as_str(),
            run_status,
            run_phase,
            current_codex_thread_id: binding.current_codex_thread_id.clone(),
            tui_active_codex_thread_id: binding.tui_active_codex_thread_id.clone(),
            tui_session_adoption_pending: binding.tui_session_adoption_pending,
            last_used_at: record.metadata.last_codex_turn_at.clone(),
            conflict: false,
            app_server_status: runtime_status.app_server_status,
            hcodex_ingress_status: runtime_status.hcodex_ingress_status,
            runtime_readiness: runtime_status.runtime_readiness,
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
                    item.binding_status,
                    item.session_broken_reason.as_deref(),
                    &WorkspaceRuntimeHealth {
                        app_server_status: item.app_server_status,
                        hcodex_ingress_status: item.hcodex_ingress_status,
                        runtime_readiness: item.runtime_readiness,
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
        let resolved_state = resolve_thread_state(&record.metadata, binding.as_ref()).await;
        let metadata = record.metadata;
        let lifecycle_status = resolve_lifecycle_status(&metadata);
        let session_broken_reason = canonical_binding_broken_reason(&metadata, binding.as_ref());
        let binding_status = resolve_binding_status(&metadata, binding.as_ref());
        let (run_status, run_phase) = match resolved_state {
            Ok(state) => (state.run_status.as_str(), state.run_phase.as_str()),
            Err(_) => ("unavailable", "unavailable"),
        };
        views.push(ThreadStateView {
            thread_key: metadata.thread_key,
            title: metadata.title,
            chat_id: metadata.chat_id,
            message_thread_id: metadata.message_thread_id,
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
            lifecycle_status: lifecycle_status.as_str(),
            binding_status: binding_status.as_str(),
            run_status,
            run_phase,
            current_codex_thread_id: binding
                .as_ref()
                .and_then(|binding| binding.current_codex_thread_id.clone()),
            tui_active_codex_thread_id: binding
                .as_ref()
                .and_then(|binding| binding.tui_active_codex_thread_id.clone()),
            tui_session_adoption_pending: binding
                .as_ref()
                .is_some_and(|binding| binding.tui_session_adoption_pending),
            session_broken_reason: session_broken_reason.clone(),
            last_verified_at: binding
                .as_ref()
                .and_then(|binding| binding.last_verified_at.clone()),
            last_codex_turn_at: metadata.last_codex_turn_at.clone(),
            archived_at: metadata.archived_at,
            last_used_at: metadata.last_codex_turn_at,
        });
    }
    views.sort_by(|a, b| a.thread_key.cmp(&b.thread_key));
    Ok(views)
}

pub async fn build_working_session_summaries(
    repository: &ThreadRepository,
    record: &ThreadRecord,
    binding: &SessionBinding,
) -> Result<Vec<WorkingSessionSummaryView>> {
    let workspace_cwd = binding
        .workspace_cwd
        .clone()
        .context("managed workspace is missing workspace_cwd")?;
    let aggregates = build_working_session_aggregates(repository, record, binding).await?;
    let mut summaries = Vec::new();
    for (session_id, aggregate) in aggregates {
        let started_at = aggregate
            .entries
            .first()
            .map(|entry| entry.timestamp.clone());
        let updated_at = session_updated_at(&aggregate)
            .or_else(|| started_at.clone())
            .unwrap_or_else(|| binding.updated_at.clone());
        let last_error = aggregate
            .last_error
            .as_ref()
            .map(|error| error.reason.clone());
        let record_count = aggregate.entries.len() + usize::from(last_error.is_some());
        summaries.push(WorkingSessionSummaryView {
            session_id,
            thread_key: record.metadata.thread_key.clone(),
            workspace_cwd: workspace_cwd.clone(),
            started_at,
            updated_at,
            run_status: aggregate
                .run_status_override
                .unwrap_or(if aggregate.is_running {
                    "running"
                } else {
                    "idle"
                })
                .to_owned(),
            run_phase: aggregate
                .run_phase_override
                .unwrap_or(
                    aggregate
                        .phase
                        .unwrap_or(WorkspaceStatusPhase::Idle)
                        .as_str(),
                )
                .to_owned(),
            origins_seen: origins_seen_for_entries(&aggregate.entries),
            record_count,
            tool_use_count: aggregate
                .entries
                .iter()
                .filter(|entry| {
                    entry.delivery == TranscriptMirrorDelivery::Process
                        && entry.phase == Some(TranscriptMirrorPhase::Tool)
                })
                .count(),
            has_final_reply: aggregate.entries.iter().any(|entry| {
                entry.role == TranscriptMirrorRole::Assistant
                    && entry.delivery == TranscriptMirrorDelivery::Final
            }),
            last_error,
        });
    }
    summaries.sort_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then_with(|| a.session_id.cmp(&b.session_id))
    });
    Ok(summaries)
}

pub async fn build_working_session_records(
    repository: &ThreadRepository,
    record: &ThreadRecord,
    binding: &SessionBinding,
    session_id: &str,
) -> Result<Option<Vec<WorkingSessionRecordView>>> {
    let aggregates = build_working_session_aggregates(repository, record, binding).await?;
    let Some(aggregate) = aggregates.get(session_id) else {
        return Ok(None);
    };
    let mut records = aggregate
        .entries
        .iter()
        .filter_map(working_session_record_from_entry)
        .collect::<Vec<_>>();
    if let Some(error) = aggregate.last_error.as_ref() {
        records.push(WorkingSessionRecordView {
            timestamp: error.timestamp.clone(),
            session_id: session_id.to_owned(),
            kind: WorkingSessionRecordKind::Error,
            origin: None,
            role: None,
            summary: truncate_summary(&error.reason),
            text: error.reason.clone(),
            delivery: None,
            phase: None,
            source_ref: "session_binding".to_owned(),
        });
    }
    records.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
    Ok(Some(records))
}

async fn build_working_session_aggregates(
    repository: &ThreadRepository,
    record: &ThreadRecord,
    binding: &SessionBinding,
) -> Result<BTreeMap<String, WorkingSessionAggregate>> {
    let workspace_cwd = binding
        .workspace_cwd
        .clone()
        .context("managed workspace is missing workspace_cwd")?;
    let workspace_path = Path::new(&workspace_cwd);
    let mut session_ids = BTreeSet::new();
    let mut aggregates = BTreeMap::<String, WorkingSessionAggregate>::new();

    for entry in repository.read_full_transcript_mirror(record).await? {
        session_ids.insert(entry.session_id.clone());
        aggregates
            .entry(entry.session_id.clone())
            .or_default()
            .entries
            .push(entry);
    }

    if let Some(session_id) = binding.current_codex_thread_id.as_ref() {
        session_ids.insert(session_id.clone());
    }
    if let Some(session_id) = binding.tui_active_codex_thread_id.as_ref() {
        session_ids.insert(session_id.clone());
    }

    for session in repository
        .read_recent_workspace_sessions(&workspace_cwd)
        .await?
    {
        session_ids.insert(session.session_id.clone());
        aggregates
            .entry(session.session_id)
            .or_default()
            .history_updated_at = Some(session.updated_at);
    }

    if resolve_binding_status(&record.metadata, Some(binding)) == BindingStatus::Broken {
        if let Some(session_id) = binding.current_codex_thread_id.as_ref() {
            let reason = canonical_binding_broken_reason(&record.metadata, Some(binding));
            if let Some(reason) = reason {
                let timestamp = canonical_binding_broken_at(&record.metadata, Some(binding))
                    .unwrap_or_else(|| binding.updated_at.clone());
                aggregates.entry(session_id.clone()).or_default().last_error =
                    Some(WorkingSessionError { timestamp, reason });
            }
        }
    }

    let mut session_statuses = HashMap::new();
    for session_id in &session_ids {
        session_statuses.insert(
            session_id.clone(),
            read_session_status(workspace_path, session_id).await?,
        );
    }
    let authority_session_ids = [
        binding.current_codex_thread_id.as_deref(),
        binding.tui_active_codex_thread_id.as_deref(),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    let fallback_busy_snapshot = effective_busy_snapshot_for_binding(Some(binding)).await;

    for (session_id, status) in session_statuses {
        let aggregate = aggregates.entry(session_id.clone()).or_default();
        if let Some(status) = status {
            aggregate.status_updated_at = Some(status.updated_at);
            aggregate.is_running = status.phase.is_turn_busy();
            aggregate.phase = Some(status.phase);
        } else if let Ok(snapshot) = fallback_busy_snapshot.as_ref() {
            if snapshot
                .as_ref()
                .is_some_and(|snapshot| snapshot.session_id == session_id)
            {
                aggregate.status_updated_at = snapshot
                    .as_ref()
                    .map(|snapshot| snapshot.updated_at.clone());
                aggregate.is_running = true;
                aggregate.phase = snapshot.as_ref().map(|snapshot| snapshot.phase);
            }
        } else if authority_session_ids
            .iter()
            .any(|candidate| *candidate == session_id)
        {
            aggregate.status_updated_at = Some(binding.updated_at.clone());
            aggregate.run_status_override = Some("unavailable");
            aggregate.run_phase_override = Some("unavailable");
        }
    }

    Ok(aggregates)
}

fn session_updated_at(aggregate: &WorkingSessionAggregate) -> Option<String> {
    aggregate
        .entries
        .last()
        .map(|entry| entry.timestamp.clone())
        .or_else(|| aggregate.status_updated_at.clone())
        .or_else(|| aggregate.history_updated_at.clone())
}

fn origins_seen_for_entries(entries: &[TranscriptMirrorEntry]) -> Vec<TranscriptMirrorOrigin> {
    let mut seen = Vec::new();
    for origin in [
        TranscriptMirrorOrigin::Telegram,
        TranscriptMirrorOrigin::Tui,
        TranscriptMirrorOrigin::Local,
    ] {
        if entries.iter().any(|entry| entry.origin == origin) {
            seen.push(origin);
        }
    }
    seen
}

fn working_session_record_from_entry(
    entry: &TranscriptMirrorEntry,
) -> Option<WorkingSessionRecordView> {
    let kind = match (&entry.delivery, &entry.role, entry.phase.as_ref()) {
        (TranscriptMirrorDelivery::Final, TranscriptMirrorRole::User, _) => {
            WorkingSessionRecordKind::UserPrompt
        }
        (TranscriptMirrorDelivery::Final, TranscriptMirrorRole::Assistant, _) => {
            WorkingSessionRecordKind::AssistantFinal
        }
        (
            TranscriptMirrorDelivery::Process,
            TranscriptMirrorRole::Assistant,
            Some(TranscriptMirrorPhase::Plan),
        ) => WorkingSessionRecordKind::ProcessPlan,
        (
            TranscriptMirrorDelivery::Process,
            TranscriptMirrorRole::Assistant,
            Some(TranscriptMirrorPhase::Tool),
        ) => WorkingSessionRecordKind::ProcessTool,
        _ => return None,
    };

    Some(WorkingSessionRecordView {
        timestamp: entry.timestamp.clone(),
        session_id: entry.session_id.clone(),
        kind,
        origin: Some(entry.origin.clone()),
        role: Some(entry.role.clone()),
        summary: truncate_summary(&entry.text),
        text: entry.text.clone(),
        delivery: Some(entry.delivery.clone()),
        phase: entry.phase.clone(),
        source_ref: "transcript_mirror".to_owned(),
    })
}

fn truncate_summary(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() <= 120 {
        return trimmed.to_owned();
    }
    let truncated: String = trimmed.chars().take(117).collect();
    format!("{truncated}...")
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
    let hcodex_ingress_status = aggregate_running_status(
        workspaces
            .iter()
            .map(|workspace| workspace.hcodex_ingress_status),
    );
    let runtime_readiness = aggregate_runtime_readiness(
        workspaces
            .iter()
            .map(|workspace| workspace.runtime_readiness),
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
            .filter(|workspace| workspace.binding_status == "broken")
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
            .filter(|workspace| workspace.runtime_readiness == "ready")
            .count(),
        degraded_workspaces: workspaces
            .iter()
            .filter(|workspace| {
                matches!(workspace.runtime_readiness, "degraded" | "pending_adoption")
            })
            .count(),
        unavailable_workspaces: workspaces
            .iter()
            .filter(|workspace| {
                !matches!(
                    workspace.runtime_readiness,
                    "ready" | "degraded" | "pending_adoption"
                )
            })
            .count(),
        app_server_status,
        hcodex_ingress_status,
        runtime_readiness,
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
                hcodex_ingress_status: "missing",
                runtime_readiness: "unavailable",
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
            hcodex_ingress_status: "missing",
            runtime_readiness: "unavailable",
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
    binding_status: &str,
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
    if runtime_status.hcodex_ingress_status != "running" {
        return Some(
            "hcodex launch endpoint is not ready. Run Repair Runtime for this workspace, or Reconcile Runtime Owner to repair the workspace launch surface."
                .to_owned(),
        );
    }
    if adoption_pending || runtime_status.runtime_readiness == "pending_adoption" {
        return Some(
            "A live TUI session is waiting for adoption. Adopt TUI from Active Threads, or reject it to keep the original binding."
                .to_owned(),
        );
    }
    let has_live_tui_session = tui_active_codex_thread_id
        .map(str::trim)
        .is_some_and(|value| !value.is_empty());
    if binding_status == "broken" && has_live_tui_session {
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
    if binding_status == "broken" {
        return Some(
            "Codex continuity is marked broken. Use Repair Session after the runtime surface is healthy."
                .to_owned(),
        );
    }
    if runtime_status.runtime_readiness == "degraded" {
        return Some(
            "Runtime readiness is degraded. Reconcile Runtime Owner or Repair Runtime to restore app-server and proxy continuity."
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

pub fn aggregate_runtime_readiness<'a>(statuses: impl Iterator<Item = &'a str>) -> &'static str {
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
        WorkingSessionRecordKind, aggregate_runtime_readiness, build_runtime_health,
        build_thread_views, build_working_session_records, build_working_session_summaries,
        build_workspace_views,
    };
    use crate::app_server_runtime::WorkspaceRuntimeState;
    use crate::execution_mode::{ExecutionMode, SessionExecutionSnapshot};
    use crate::repository::{
        ThreadMetadata, ThreadRepository, TranscriptMirrorDelivery, TranscriptMirrorEntry,
        TranscriptMirrorOrigin, TranscriptMirrorPhase, TranscriptMirrorRole,
    };
    use crate::runtime_owner::RuntimeOwnerStatus;
    use crate::workspace_status::record_bot_status_event;
    use futures_util::{SinkExt, StreamExt};
    use std::path::PathBuf;
    use tokio::fs;
    use tokio::net::TcpListener;
    use tokio_tungstenite::accept_async;
    use tokio_tungstenite::tungstenite::Message as WsMessage;
    use uuid::Uuid;

    fn temp_path() -> PathBuf {
        std::env::temp_dir().join(format!(
            "threadbridge-runtime-protocol-test-{}",
            Uuid::new_v4()
        ))
    }

    fn full_auto_snapshot() -> SessionExecutionSnapshot {
        SessionExecutionSnapshot::from_mode(ExecutionMode::FullAuto)
    }

    async fn write_runtime_state(workspace_path: &std::path::Path, worker_ws_url: &str) {
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

    #[test]
    fn aggregate_runtime_readiness_treats_pending_adoption_as_degraded() {
        assert_eq!(
            aggregate_runtime_readiness(["ready", "pending_adoption"].into_iter()),
            "degraded"
        );
        assert_eq!(
            aggregate_runtime_readiness(["pending_adoption"].into_iter()),
            "degraded"
        );
    }

    #[test]
    fn thread_state_view_serializes_lifecycle_status() {
        let view = ThreadStateView {
            thread_key: "thread-1".to_owned(),
            title: Some("Workspace".to_owned()),
            chat_id: 1,
            message_thread_id: Some(42),
            workspace_cwd: Some("/tmp/workspace".to_owned()),
            workspace_execution_mode: Some(ExecutionMode::FullAuto),
            current_execution_mode: Some(ExecutionMode::FullAuto),
            current_approval_policy: Some("on-request".to_owned()),
            current_sandbox_policy: Some("workspace-write".to_owned()),
            lifecycle_status: "active",
            binding_status: "healthy",
            run_status: "idle",
            run_phase: "idle",
            current_codex_thread_id: Some("thr_current".to_owned()),
            tui_active_codex_thread_id: None,
            tui_session_adoption_pending: false,
            session_broken_reason: None,
            last_verified_at: Some("2026-03-24T09:00:00.000Z".to_owned()),
            last_codex_turn_at: Some("2026-03-24T10:00:00.000Z".to_owned()),
            archived_at: None,
            last_used_at: Some("2026-03-24T10:00:00.000Z".to_owned()),
        };

        let value = serde_json::to_value(view).unwrap();
        assert_eq!(value["chat_id"], 1);
        assert_eq!(value["message_thread_id"], 42);
        assert_eq!(value["lifecycle_status"], "active");
        assert_eq!(value["binding_status"], "healthy");
        assert_eq!(value["run_status"], "idle");
        assert_eq!(value["run_phase"], "idle");
        assert_eq!(value["last_verified_at"], "2026-03-24T09:00:00.000Z");
        assert_eq!(value["last_codex_turn_at"], "2026-03-24T10:00:00.000Z");
        assert_eq!(value["last_used_at"], "2026-03-24T10:00:00.000Z");
        assert!(value.get("last_error").is_none());
    }

    #[test]
    fn managed_workspace_view_serializes_canonical_fields() {
        let view = ManagedWorkspaceView {
            workspace_cwd: "/tmp/workspace".to_owned(),
            title: Some("Workspace".to_owned()),
            thread_key: Some("thread-1".to_owned()),
            workspace_execution_mode: ExecutionMode::FullAuto,
            current_execution_mode: Some(ExecutionMode::Yolo),
            current_approval_policy: Some("on-request".to_owned()),
            current_sandbox_policy: Some("workspace-write".to_owned()),
            mode_drift: true,
            binding_status: "broken",
            run_status: "idle",
            run_phase: "idle",
            current_codex_thread_id: Some("thr_current".to_owned()),
            tui_active_codex_thread_id: Some("thr_tui".to_owned()),
            tui_session_adoption_pending: false,
            last_used_at: Some("2026-03-24T10:00:00.000Z".to_owned()),
            conflict: false,
            app_server_status: "running",
            hcodex_ingress_status: "running",
            runtime_readiness: "ready",
            runtime_health_source: "owner_heartbeat",
            heartbeat_last_checked_at: Some("2026-03-24T10:00:00.000Z".to_owned()),
            heartbeat_last_error: None,
            session_broken_reason: Some("continuity lost".to_owned()),
            recovery_hint: Some("repair".to_owned()),
            hcodex_path: "/tmp/workspace/.threadbridge/bin/hcodex".to_owned(),
            hcodex_available: true,
            recent_codex_sessions: Vec::new(),
        };

        let value = serde_json::to_value(view).unwrap();
        assert_eq!(value["binding_status"], "broken");
        assert_eq!(value["run_status"], "idle");
        assert_eq!(value["run_phase"], "idle");
        assert_eq!(value["current_codex_thread_id"], "thr_current");
        assert_eq!(value["hcodex_ingress_status"], "running");
        assert!(value.get("tui_proxy_status").is_none());
        assert!(value.get("session_broken").is_none());
    }

    #[tokio::test]
    async fn build_thread_views_ignores_stale_metadata_broken_aliases() {
        let root = temp_path();
        let workspace = temp_path();
        fs::create_dir_all(&workspace).await.unwrap();
        let repo = ThreadRepository::open(&root).await.unwrap();
        let record = repo
            .create_thread(1, 7, "Workspace".to_owned())
            .await
            .unwrap();
        let record = repo
            .bind_workspace(
                record,
                workspace.display().to_string(),
                "thr_current".to_owned(),
                full_auto_snapshot(),
            )
            .await
            .unwrap();

        let mut metadata: ThreadMetadata =
            serde_json::from_str(&fs::read_to_string(&record.metadata_path).await.unwrap())
                .unwrap();
        metadata.session_broken = true;
        metadata.session_broken_reason = Some("stale metadata".to_owned());
        fs::write(
            &record.metadata_path,
            format!("{}\n", serde_json::to_string_pretty(&metadata).unwrap()),
        )
        .await
        .unwrap();

        let views = build_thread_views(&repo).await.unwrap();
        assert_eq!(views[0].binding_status, "healthy");
        assert_eq!(views[0].session_broken_reason, None);

        let _ = fs::remove_dir_all(root).await;
        let _ = fs::remove_dir_all(workspace).await;
    }

    #[tokio::test]
    async fn build_thread_views_marks_missing_worker_as_unavailable() {
        let root = temp_path();
        let workspace = temp_path();
        fs::create_dir_all(&workspace).await.unwrap();
        let repo = ThreadRepository::open(&root).await.unwrap();
        let record = repo
            .create_thread(1, 7, "Workspace".to_owned())
            .await
            .unwrap();
        let _record = repo
            .bind_workspace(
                record,
                workspace.display().to_string(),
                "thr_current".to_owned(),
                full_auto_snapshot(),
            )
            .await
            .unwrap();

        let views = build_thread_views(&repo).await.unwrap();
        assert_eq!(views[0].run_status, "unavailable");
        assert_eq!(views[0].run_phase, "unavailable");

        let _ = fs::remove_dir_all(root).await;
        let _ = fs::remove_dir_all(workspace).await;
    }

    #[tokio::test]
    async fn build_workspace_views_marks_missing_worker_as_unavailable() {
        let root = temp_path();
        let workspace = temp_path();
        fs::create_dir_all(&workspace).await.unwrap();
        let repo = ThreadRepository::open(&root).await.unwrap();
        let record = repo
            .create_thread(1, 7, "Workspace".to_owned())
            .await
            .unwrap();
        let _record = repo
            .bind_workspace(
                record,
                workspace.display().to_string(),
                "thr_current".to_owned(),
                full_auto_snapshot(),
            )
            .await
            .unwrap();

        let views = build_workspace_views(&repo, None).await.unwrap();
        assert_eq!(views[0].run_status, "unavailable");
        assert_eq!(views[0].run_phase, "unavailable");

        let _ = fs::remove_dir_all(root).await;
        let _ = fs::remove_dir_all(workspace).await;
    }

    #[tokio::test]
    async fn working_session_summaries_fall_back_to_worker_busy_state() {
        let root = temp_path();
        let workspace = temp_path();
        fs::create_dir_all(&workspace).await.unwrap();
        let worker_ws_url = start_mock_worker_for_busy_query().await;
        write_runtime_state(&workspace, &worker_ws_url).await;
        let repo = ThreadRepository::open(&root).await.unwrap();
        let record = repo
            .create_thread(1, 7, "Workspace".to_owned())
            .await
            .unwrap();
        let record = repo
            .bind_workspace(
                record,
                workspace.display().to_string(),
                "thr_current".to_owned(),
                full_auto_snapshot(),
            )
            .await
            .unwrap();
        let binding = repo
            .read_session_binding(&record)
            .await
            .unwrap()
            .expect("binding");

        let summaries = build_working_session_summaries(&repo, &record, &binding)
            .await
            .unwrap();

        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].session_id, "thr_current");
        assert_eq!(summaries[0].run_status, "running");
        assert_eq!(summaries[0].run_phase, "turn_running");

        let _ = fs::remove_dir_all(root).await;
        let _ = fs::remove_dir_all(workspace).await;
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
                run_phase: "turn_running",
                current_codex_thread_id: Some("thr-a".to_owned()),
                tui_active_codex_thread_id: None,
                tui_session_adoption_pending: false,
                last_used_at: None,
                conflict: true,
                app_server_status: "running",
                hcodex_ingress_status: "running",
                runtime_readiness: "ready",
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
                run_phase: "idle",
                current_codex_thread_id: Some("thr-b".to_owned()),
                tui_active_codex_thread_id: None,
                tui_session_adoption_pending: false,
                last_used_at: None,
                conflict: false,
                app_server_status: "missing",
                hcodex_ingress_status: "missing",
                runtime_readiness: "unavailable",
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
        assert_eq!(view.runtime_readiness, "unavailable");
    }

    #[tokio::test]
    async fn working_session_views_group_entries_and_records_by_session() {
        let root = temp_path();
        let workspace = temp_path();
        fs::create_dir_all(&workspace).await.unwrap();
        let repo = ThreadRepository::open(&root).await.unwrap();
        let record = repo
            .create_thread(1, 7, "Workspace".to_owned())
            .await
            .unwrap();
        let record = repo
            .bind_workspace(
                record,
                workspace.display().to_string(),
                "thr_current".to_owned(),
                full_auto_snapshot(),
            )
            .await
            .unwrap();

        for entry in [
            TranscriptMirrorEntry {
                timestamp: "2026-03-24T10:00:00.000Z".to_owned(),
                session_id: "thr_current".to_owned(),
                origin: TranscriptMirrorOrigin::Telegram,
                role: TranscriptMirrorRole::User,
                delivery: TranscriptMirrorDelivery::Final,
                phase: None,
                text: "hello".to_owned(),
            },
            TranscriptMirrorEntry {
                timestamp: "2026-03-24T10:00:01.000Z".to_owned(),
                session_id: "thr_current".to_owned(),
                origin: TranscriptMirrorOrigin::Telegram,
                role: TranscriptMirrorRole::Assistant,
                delivery: TranscriptMirrorDelivery::Process,
                phase: Some(TranscriptMirrorPhase::Plan),
                text: "Plan: inspect runtime".to_owned(),
            },
            TranscriptMirrorEntry {
                timestamp: "2026-03-24T10:00:02.000Z".to_owned(),
                session_id: "thr_current".to_owned(),
                origin: TranscriptMirrorOrigin::Telegram,
                role: TranscriptMirrorRole::Assistant,
                delivery: TranscriptMirrorDelivery::Final,
                phase: None,
                text: "done".to_owned(),
            },
            TranscriptMirrorEntry {
                timestamp: "2026-03-24T09:00:00.000Z".to_owned(),
                session_id: "thr_old".to_owned(),
                origin: TranscriptMirrorOrigin::Tui,
                role: TranscriptMirrorRole::User,
                delivery: TranscriptMirrorDelivery::Final,
                phase: None,
                text: "older".to_owned(),
            },
        ] {
            repo.append_transcript_mirror(&record, &entry)
                .await
                .unwrap();
        }

        let binding = repo.read_session_binding(&record).await.unwrap().unwrap();
        let summaries = build_working_session_summaries(&repo, &record, &binding)
            .await
            .unwrap();
        assert_eq!(summaries.len(), 2);
        assert_eq!(summaries[0].session_id, "thr_current");
        assert_eq!(summaries[0].tool_use_count, 0);
        assert!(summaries[0].has_final_reply);
        assert_eq!(
            summaries[0].origins_seen,
            vec![TranscriptMirrorOrigin::Telegram]
        );
        assert_eq!(summaries[0].record_count, 3);

        let records = build_working_session_records(&repo, &record, &binding, "thr_current")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            records
                .iter()
                .map(|record| &record.kind)
                .collect::<Vec<_>>(),
            vec![
                &WorkingSessionRecordKind::UserPrompt,
                &WorkingSessionRecordKind::ProcessPlan,
                &WorkingSessionRecordKind::AssistantFinal,
            ]
        );
    }

    #[tokio::test]
    async fn working_session_views_surface_running_and_broken_current_session_error() {
        let root = temp_path();
        let workspace = temp_path();
        fs::create_dir_all(&workspace).await.unwrap();
        let repo = ThreadRepository::open(&root).await.unwrap();
        let record = repo
            .create_thread(1, 7, "Workspace".to_owned())
            .await
            .unwrap();
        let record = repo
            .bind_workspace(
                record,
                workspace.display().to_string(),
                "thr_current".to_owned(),
                full_auto_snapshot(),
            )
            .await
            .unwrap();
        repo.append_transcript_mirror(
            &record,
            &TranscriptMirrorEntry {
                timestamp: "2026-03-24T10:00:00.000Z".to_owned(),
                session_id: "thr_current".to_owned(),
                origin: TranscriptMirrorOrigin::Telegram,
                role: TranscriptMirrorRole::User,
                delivery: TranscriptMirrorDelivery::Final,
                phase: None,
                text: "hello".to_owned(),
            },
        )
        .await
        .unwrap();

        record_bot_status_event(
            &workspace,
            "bot_turn_started",
            Some("thr_current"),
            None,
            Some("hello"),
        )
        .await
        .unwrap();

        let broken = repo
            .mark_session_binding_broken(record, "session continuity lost")
            .await
            .unwrap();
        let binding = repo.read_session_binding(&broken).await.unwrap().unwrap();

        let summaries = build_working_session_summaries(&repo, &broken, &binding)
            .await
            .unwrap();
        assert_eq!(summaries[0].session_id, "thr_current");
        assert_eq!(summaries[0].run_status, "running");
        assert_eq!(summaries[0].run_phase, "turn_running");
        assert_eq!(
            summaries[0].last_error.as_deref(),
            Some("session continuity lost")
        );
        assert_eq!(summaries[0].record_count, 2);

        let records = build_working_session_records(&repo, &broken, &binding, "thr_current")
            .await
            .unwrap()
            .unwrap();
        assert!(records.iter().any(|record| {
            record.kind == WorkingSessionRecordKind::Error && record.source_ref == "session_binding"
        }));
        assert!(
            records
                .iter()
                .any(|record| { record.kind == WorkingSessionRecordKind::UserPrompt })
        );
    }
}
