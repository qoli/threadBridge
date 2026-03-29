use std::path::{Path, PathBuf};

use anyhow::anyhow;
use anyhow::{Context, Result, bail};
use serde::Serialize;
use tokio::fs;
use tokio::net::TcpStream;
use tokio::process::Command;
use tracing::info;

use crate::app_server_runtime::{WorkspaceRuntimeManager, WorkspaceRuntimeState};
use crate::collaboration_mode::CollaborationMode;
use crate::codex::{CodexRunner, CodexWorkspace};
use crate::config::RuntimeConfig;
use crate::delivery_bus::DeliveryBusCoordinator;
use crate::execution_mode::{
    ExecutionMode, workspace_execution_mode, write_workspace_execution_config,
};
use crate::hcodex_ingress::HcodexIngressManager;
use crate::repository::{
    LogDirection, RecentCodexSessionEntry, SessionBinding, ThreadRecord, ThreadRepository,
};
use crate::runtime_protocol::{
    InterruptRunningTurnState, LaunchLocalSessionTarget, RuntimeControlAction,
    RuntimeControlActionEnvelope, RuntimeControlActionRequest, RuntimeControlActionResult,
};
use crate::thread_state::{effective_busy_snapshot_for_binding, resolve_thread_state};
use crate::workspace::{ensure_workspace_runtime, validate_seed_template};
use crate::workspace_status::{
    SessionActivitySource, read_local_tui_session_claim, read_session_status,
    record_managed_runtime_interrupt_requested,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RuntimeOwnershipMode {
    SelfManaged,
    DesktopOwner,
}

#[derive(Clone)]
pub struct RuntimeControlContext {
    pub runtime: RuntimeConfig,
    pub repository: ThreadRepository,
    pub delivery_bus: DeliveryBusCoordinator,
    pub codex: CodexRunner,
    pub app_server_runtime: WorkspaceRuntimeManager,
    pub hcodex_ingress: Option<HcodexIngressManager>,
    pub seed_template_path: PathBuf,
    pub runtime_ownership_mode: RuntimeOwnershipMode,
}

impl RuntimeControlContext {
    pub async fn new(
        runtime: RuntimeConfig,
        app_server_runtime: WorkspaceRuntimeManager,
        hcodex_ingress: Option<HcodexIngressManager>,
        runtime_ownership_mode: RuntimeOwnershipMode,
    ) -> Result<Self> {
        let repository = ThreadRepository::open(&runtime.data_root_path).await?;
        let seed_template_path = validate_seed_template(
            &runtime
                .codex_working_directory
                .join("templates")
                .join("AGENTS.md"),
        )?;
        Ok(Self {
            delivery_bus: DeliveryBusCoordinator::new(&runtime.data_root_path).await?,
            codex: CodexRunner::new(runtime.codex_model.clone()),
            repository,
            app_server_runtime,
            hcodex_ingress: if runtime_ownership_mode == RuntimeOwnershipMode::SelfManaged {
                hcodex_ingress
            } else {
                None
            },
            seed_template_path,
            runtime,
            runtime_ownership_mode,
        })
    }

    pub fn workspace_runtime_service(&self) -> WorkspaceRuntimeService {
        WorkspaceRuntimeService { ctx: self.clone() }
    }

    pub fn workspace_session_service(&self) -> WorkspaceSessionService {
        WorkspaceSessionService { ctx: self.clone() }
    }

    pub fn session_routing_service(&self) -> SessionRoutingService {
        SessionRoutingService { ctx: self.clone() }
    }

    pub fn runtime_is_owner_managed(&self) -> bool {
        self.runtime_ownership_mode == RuntimeOwnershipMode::DesktopOwner
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct HcodexLaunchConfigView {
    pub workspace_cwd: String,
    pub thread_key: String,
    pub hcodex_path: String,
    pub hcodex_available: bool,
    pub workspace_execution_mode: ExecutionMode,
    pub current_execution_mode: Option<ExecutionMode>,
    pub current_approval_policy: Option<String>,
    pub current_sandbox_policy: Option<String>,
    pub mode_drift: bool,
    pub current_codex_thread_id: Option<String>,
    pub recent_codex_sessions: Vec<RecentCodexSessionEntry>,
    pub launch_new_command: String,
    pub launch_current_command: Option<String>,
    pub launch_resume_commands: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceExecutionModeView {
    pub thread_key: String,
    pub workspace_cwd: String,
    pub workspace_execution_mode: ExecutionMode,
    pub current_execution_mode: Option<ExecutionMode>,
    pub current_approval_policy: Option<String>,
    pub current_sandbox_policy: Option<String>,
    pub mode_drift: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceSurfaceProbeFile {
    pub path: String,
    pub exists: bool,
    pub summary: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceSurfaceProbe {
    pub canonical_workspace_cwd: String,
    pub threadbridge_exists: bool,
    pub bin_exists: bool,
    pub state_exists: bool,
    pub tool_requests_exists: bool,
    pub tool_results_exists: bool,
    pub workspace_config: WorkspaceSurfaceProbeFile,
    pub app_server_current: WorkspaceSurfaceProbeFile,
    pub runtime_observer_current: WorkspaceSurfaceProbeFile,
    pub runtime_observer_events: WorkspaceSurfaceProbeFile,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceBindingSummary {
    pub thread_key: String,
    pub title: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceAddPreflight {
    pub probe: WorkspaceSurfaceProbe,
    pub active_threads: Vec<WorkspaceBindingSummary>,
    pub archived_threads: Vec<WorkspaceBindingSummary>,
}

impl WorkspaceSurfaceProbe {
    pub fn render_text(&self) -> String {
        let mut lines = vec![
            "Workspace surface probe.".to_owned(),
            format!("workspace: {}", self.canonical_workspace_cwd),
            format!(
                ".threadbridge/: {}",
                present_label(self.threadbridge_exists)
            ),
            format!(".threadbridge/bin/: {}", present_label(self.bin_exists)),
            format!(".threadbridge/state/: {}", present_label(self.state_exists)),
            format!(
                ".threadbridge/tool_requests/: {}",
                present_label(self.tool_requests_exists)
            ),
            format!(
                ".threadbridge/tool_results/: {}",
                present_label(self.tool_results_exists)
            ),
            String::new(),
        ];
        append_probe_file_lines(&mut lines, "workspace-config.json", &self.workspace_config);
        append_probe_file_lines(
            &mut lines,
            "app-server/current.json",
            &self.app_server_current,
        );
        append_probe_file_lines(
            &mut lines,
            "runtime-observer/current.json",
            &self.runtime_observer_current,
        );
        append_probe_file_lines(
            &mut lines,
            "runtime-observer/events.jsonl",
            &self.runtime_observer_events,
        );
        lines.join("\n")
    }
}

impl WorkspaceAddPreflight {
    pub fn reset_required(&self) -> bool {
        self.probe.threadbridge_exists
    }

    pub fn blocking_reason(&self) -> Option<String> {
        if !self.active_threads.is_empty() {
            return Some(format!(
                "blocked_by_active_binding: workspace `{}` is already bound to active thread(s): {}",
                self.probe.canonical_workspace_cwd,
                render_binding_summaries(&self.active_threads)
            ));
        }
        if !self.archived_threads.is_empty() {
            return Some(format!(
                "blocked_by_archived_binding: workspace `{}` still has archived thread history: {}. Purge archived threads from the desktop tray before re-adding this workspace.",
                self.probe.canonical_workspace_cwd,
                render_binding_summaries(&self.archived_threads)
            ));
        }
        None
    }

    pub fn render_text(&self) -> String {
        let mut lines = vec![
            "Workspace add preflight.".to_owned(),
            format!("workspace: {}", self.probe.canonical_workspace_cwd),
            format!(
                "active_bindings: {}",
                if self.active_threads.is_empty() {
                    "none".to_owned()
                } else {
                    render_binding_summaries(&self.active_threads)
                }
            ),
            format!(
                "archived_bindings: {}",
                if self.archived_threads.is_empty() {
                    "none".to_owned()
                } else {
                    render_binding_summaries(&self.archived_threads)
                }
            ),
            format!("reset_required: {}", yes_no_label(self.reset_required())),
        ];
        if let Some(reason) = self.blocking_reason() {
            lines.push(format!("reset_allowed: no ({reason})"));
        } else {
            lines.push("reset_allowed: yes".to_owned());
        }
        lines.join("\n")
    }
}

fn present_label(value: bool) -> &'static str {
    if value { "present" } else { "missing" }
}

fn yes_no_label(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

fn workspace_binding_summary(record: &ThreadRecord) -> WorkspaceBindingSummary {
    WorkspaceBindingSummary {
        thread_key: record.metadata.thread_key.clone(),
        title: record.metadata.title.clone(),
    }
}

fn render_binding_summaries(bindings: &[WorkspaceBindingSummary]) -> String {
    bindings
        .iter()
        .map(|binding| {
            binding
                .title
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(|title| format!("{} ({title})", binding.thread_key))
                .unwrap_or_else(|| binding.thread_key.clone())
        })
        .collect::<Vec<_>>()
        .join(", ")
}

fn append_probe_file_lines(lines: &mut Vec<String>, label: &str, file: &WorkspaceSurfaceProbeFile) {
    lines.push(format!("{label}: {}", present_label(file.exists)));
    lines.push(format!("path: {}", file.path));
    if let Some(summary) = file.summary.as_deref() {
        lines.push(format!("summary: {summary}"));
    }
    lines.push(String::new());
}

pub async fn probe_workspace_surface(workspace_path: &Path) -> Result<WorkspaceSurfaceProbe> {
    let canonical_workspace = workspace_path
        .canonicalize()
        .unwrap_or_else(|_| workspace_path.to_path_buf());
    let canonical_workspace_cwd = canonical_workspace.display().to_string();
    let threadbridge_dir = canonical_workspace.join(".threadbridge");
    let state_dir = threadbridge_dir.join("state");
    Ok(WorkspaceSurfaceProbe {
        canonical_workspace_cwd,
        threadbridge_exists: tokio::fs::try_exists(&threadbridge_dir)
            .await
            .unwrap_or(false),
        bin_exists: tokio::fs::try_exists(threadbridge_dir.join("bin"))
            .await
            .unwrap_or(false),
        state_exists: tokio::fs::try_exists(&state_dir).await.unwrap_or(false),
        tool_requests_exists: tokio::fs::try_exists(threadbridge_dir.join("tool_requests"))
            .await
            .unwrap_or(false),
        tool_results_exists: tokio::fs::try_exists(threadbridge_dir.join("tool_results"))
            .await
            .unwrap_or(false),
        workspace_config: probe_json_file(state_dir.join("workspace-config.json")).await,
        app_server_current: probe_json_file(state_dir.join("app-server").join("current.json"))
            .await,
        runtime_observer_current: probe_json_file(
            state_dir.join("runtime-observer").join("current.json"),
        )
        .await,
        runtime_observer_events: probe_events_file(
            state_dir.join("runtime-observer").join("events.jsonl"),
        )
        .await,
    })
}

async fn probe_json_file(path: PathBuf) -> WorkspaceSurfaceProbeFile {
    let summary = match tokio::fs::read_to_string(&path).await {
        Ok(contents) => compact_probe_text(&contents),
        Err(_) => None,
    };
    WorkspaceSurfaceProbeFile {
        path: path.display().to_string(),
        exists: summary.is_some(),
        summary,
    }
}

async fn probe_events_file(path: PathBuf) -> WorkspaceSurfaceProbeFile {
    let summary = match tokio::fs::read_to_string(&path).await {
        Ok(contents) => Some(format!(
            "{} lines, {} bytes",
            contents.lines().count(),
            contents.len()
        )),
        Err(_) => None,
    };
    WorkspaceSurfaceProbeFile {
        path: path.display().to_string(),
        exists: summary.is_some(),
        summary,
    }
}

pub async fn preflight_workspace_add(
    repository: &ThreadRepository,
    workspace_path: &Path,
) -> Result<WorkspaceAddPreflight> {
    let probe = probe_workspace_surface(workspace_path).await?;
    let active_threads = repository
        .find_active_threads_by_workspace(&probe.canonical_workspace_cwd)
        .await?
        .iter()
        .map(workspace_binding_summary)
        .collect();
    let archived_threads = repository
        .find_archived_threads_by_workspace(&probe.canonical_workspace_cwd)
        .await?
        .iter()
        .map(workspace_binding_summary)
        .collect();
    Ok(WorkspaceAddPreflight {
        probe,
        active_threads,
        archived_threads,
    })
}

pub async fn reset_workspace_runtime_surface(workspace_path: &Path) -> Result<bool> {
    let runtime_dir = workspace_path.join(".threadbridge");
    if !fs::try_exists(&runtime_dir).await.unwrap_or(false) {
        return Ok(false);
    }
    fs::remove_dir_all(&runtime_dir)
        .await
        .with_context(|| format!("failed to remove {}", runtime_dir.display()))?;
    Ok(true)
}

fn compact_probe_text(contents: &str) -> Option<String> {
    let compact = contents.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.is_empty() {
        return None;
    }
    let truncated = compact.chars().take(220).collect::<String>();
    let needs_ellipsis = compact.chars().count() > truncated.chars().count();
    Some(if needs_ellipsis {
        format!("{truncated}...")
    } else {
        truncated
    })
}

#[derive(Clone)]
pub struct SharedControlHandle {
    ctx: RuntimeControlContext,
}

impl SharedControlHandle {
    pub fn new(ctx: RuntimeControlContext) -> Self {
        Self { ctx }
    }

    pub async fn resolve_workspace_add(
        &self,
        workspace_path: &Path,
    ) -> Result<WorkspaceAddResolution> {
        self.ctx
            .workspace_session_service()
            .resolve_workspace_add(workspace_path)
            .await
    }

    pub async fn create_thread(
        &self,
        chat_id: i64,
        message_thread_id: i32,
        title: String,
    ) -> Result<ThreadRecord> {
        let record = self
            .ctx
            .repository
            .create_thread(chat_id, message_thread_id, title)
            .await?;
        self.ctx
            .repository
            .append_log(
                &record,
                LogDirection::System,
                "Telegram thread created from desktop or management control.",
                None,
            )
            .await?;
        Ok(record)
    }

    pub async fn bind_workspace_record(
        &self,
        record: ThreadRecord,
        workspace_path: &Path,
        origin: &str,
    ) -> Result<ThreadRecord> {
        let updated = self
            .ctx
            .workspace_session_service()
            .bind_workspace_record(record, workspace_path)
            .await?;
        self.ctx
            .repository
            .append_log(
                &updated,
                LogDirection::System,
                format!(
                    "Bound Telegram thread to workspace {} from {origin}.",
                    workspace_path.display()
                ),
                None,
            )
            .await?;
        Ok(updated)
    }

    pub async fn adopt_tui_session(&self, thread_key: &str, origin: &str) -> Result<ThreadRecord> {
        let record = self.active_thread(thread_key).await?;
        let updated = self.ctx.repository.adopt_tui_active_session(record).await?;
        self.ctx
            .repository
            .append_log(
                &updated,
                LogDirection::System,
                format!("Adopted the active TUI session from {origin}."),
                None,
            )
            .await?;
        Ok(updated)
    }

    pub async fn reject_tui_session(&self, thread_key: &str, origin: &str) -> Result<ThreadRecord> {
        let record = self.active_thread(thread_key).await?;
        let updated = self.ctx.repository.clear_tui_adoption_state(record).await?;
        self.ctx
            .repository
            .append_log(
                &updated,
                LogDirection::System,
                format!("Rejected the active TUI session from {origin}."),
                None,
            )
            .await?;
        Ok(updated)
    }

    pub async fn archive_thread(&self, thread_key: &str, origin: &str) -> Result<ThreadRecord> {
        let record = self.active_thread(thread_key).await?;
        let archived = self.ctx.repository.archive_thread(record).await?;
        self.ctx
            .repository
            .append_log(
                &archived,
                LogDirection::System,
                format!("Thread archived from {origin}."),
                None,
            )
            .await?;
        Ok(archived)
    }

    pub async fn archived_thread(&self, chat_id: i64, thread_key: &str) -> Result<ThreadRecord> {
        self.ctx
            .repository
            .get_thread_by_key(chat_id, thread_key)
            .await?
            .context("thread_key is not a known thread")
    }

    pub async fn restore_thread(
        &self,
        record: ThreadRecord,
        message_thread_id: i32,
        title: String,
        origin: &str,
    ) -> Result<ThreadRecord> {
        let restored = self
            .ctx
            .repository
            .restore_thread(record, message_thread_id, title.clone())
            .await?;
        self.ctx
            .repository
            .append_log(
                &restored,
                LogDirection::System,
                format!(
                    "Thread restored from {origin} into Telegram thread \"{}\" (message_thread_id {}).",
                    title, message_thread_id
                ),
                None,
            )
            .await?;
        Ok(restored)
    }

    pub async fn repair_session_binding(
        &self,
        thread_key: &str,
        origin: &str,
    ) -> Result<SessionRepairResult> {
        let record = self.active_thread(thread_key).await?;
        let session = self.ctx.repository.read_session_binding(&record).await?;
        let Some(binding) = session.as_ref() else {
            bail!("This thread is not bound to a workspace yet.");
        };
        let result = self
            .ctx
            .workspace_session_service()
            .repair_session_binding(record, binding)
            .await?;
        self.ctx
            .repository
            .append_log(
                &result.record,
                LogDirection::System,
                if result.verified {
                    format!("Codex session revalidated from {origin}.")
                } else {
                    format!("Codex session revalidation failed from {origin}.")
                },
                None,
            )
            .await?;
        Ok(result)
    }

    pub async fn set_thread_collaboration_mode(
        &self,
        thread_key: &str,
        mode: CollaborationMode,
        origin: &str,
    ) -> Result<ThreadRecord> {
        let record = self.active_thread(thread_key).await?;
        let updated = self
            .ctx
            .repository
            .update_session_collaboration_mode(record, mode)
            .await?;
        self.ctx
            .repository
            .append_log(
                &updated,
                LogDirection::System,
                format!(
                    "Action `{}` changed collaboration mode to `{}` from {origin}.",
                    RuntimeControlAction::SetThreadCollaborationMode.as_str(),
                    mode.as_str()
                ),
                None,
            )
            .await?;
        Ok(updated)
    }

    pub async fn interrupt_running_turn(
        &self,
        thread_key: &str,
        origin: &str,
    ) -> Result<RuntimeControlActionResult> {
        let record = self.active_thread(thread_key).await?;
        let binding = self
            .ctx
            .repository
            .read_session_binding(&record)
            .await?
            .context("This thread is not bound to a workspace yet.")?;
        let resolved_state = resolve_thread_state(&record.metadata, Some(&binding)).await?;
        if !resolved_state.is_running() {
            bail!("No active turn is running for this workspace.");
        }
        let busy = effective_busy_snapshot_for_binding(Some(&binding))
            .await?
            .context("No active turn is running for this workspace.")?;
        if busy.phase == crate::workspace_status::WorkspaceStatusPhase::TurnFinalizing {
            return Ok(RuntimeControlActionResult::InterruptRunningTurn {
                thread_key: record.metadata.thread_key,
                session_id: busy.session_id,
                turn_id: busy.turn_id,
                state: InterruptRunningTurnState::AlreadyRequested,
            });
        }
        let turn_id = busy.turn_id.as_deref().context(format!(
            "A turn is running for session `{}`, but its turn id is unavailable, so it cannot be interrupted yet.",
            busy.session_id
        ))?;
        let workspace_path = workspace_path_from_binding(&binding)?;
        let codex_workspace = self
            .ctx
            .workspace_runtime_service()
            .shared_codex_workspace(workspace_path.clone())
            .await?;
        self.ctx
            .codex
            .interrupt_turn(&codex_workspace, &busy.session_id, turn_id)
            .await?;
        record_managed_runtime_interrupt_requested(&workspace_path, &busy.session_id, turn_id)
            .await?;
        self.ctx
            .repository
            .append_log(
                &record,
                LogDirection::System,
                format!(
                    "Action `{}` requested interrupt for session `{}` turn `{}` from {origin}.",
                    RuntimeControlAction::InterruptRunningTurn.as_str(),
                    busy.session_id,
                    turn_id
                ),
                None,
            )
            .await?;
        Ok(RuntimeControlActionResult::InterruptRunningTurn {
            thread_key: record.metadata.thread_key,
            session_id: busy.session_id,
            turn_id: Some(turn_id.to_owned()),
            state: InterruptRunningTurnState::Requested,
        })
    }

    pub async fn execute_runtime_control_action(
        &self,
        thread_key: &str,
        request: RuntimeControlActionRequest,
        origin: &str,
    ) -> Result<RuntimeControlActionEnvelope> {
        request.validate()?;
        let action = request.action();
        match request {
            RuntimeControlActionRequest::StartFreshSession => {
                let record = self.active_thread(thread_key).await?;
                let session = self.ctx.repository.read_session_binding(&record).await?;
                let binding = session
                    .as_ref()
                    .context("This thread is not bound to a workspace yet.")?;
                let workspace_path = workspace_path_from_binding(binding)?;
                let updated = self
                    .ctx
                    .workspace_session_service()
                    .start_fresh_session(record, workspace_path)
                    .await?;
                self.ctx
                    .repository
                    .append_log(
                        &updated,
                        LogDirection::System,
                        format!(
                            "Action `{}` started a fresh Codex session from {origin}.",
                            RuntimeControlAction::StartFreshSession.as_str()
                        ),
                        None,
                    )
                    .await?;
                let updated_binding = self.ctx.repository.read_session_binding(&updated).await?;
                Ok(RuntimeControlActionEnvelope {
                    ok: true,
                    action,
                    result: RuntimeControlActionResult::StartFreshSession {
                        thread_key: updated.metadata.thread_key,
                        current_codex_thread_id: updated_binding
                            .and_then(|binding| binding.current_codex_thread_id),
                    },
                })
            }
            RuntimeControlActionRequest::RepairSessionBinding => {
                let repaired = self.repair_session_binding(thread_key, origin).await?;
                let updated_binding = self
                    .ctx
                    .repository
                    .read_session_binding(&repaired.record)
                    .await?;
                Ok(RuntimeControlActionEnvelope {
                    ok: true,
                    action,
                    result: RuntimeControlActionResult::RepairSessionBinding {
                        thread_key: repaired.record.metadata.thread_key,
                        verified: repaired.verified,
                        session_broken_reason: updated_binding
                            .and_then(|binding| binding.session_broken_reason),
                    },
                })
            }
            RuntimeControlActionRequest::SetWorkspaceExecutionMode { execution_mode } => {
                let current = self.workspace_execution_mode_view(thread_key).await?;
                write_workspace_execution_config(Path::new(&current.workspace_cwd), execution_mode)
                    .await?;
                let updated = self.workspace_execution_mode_view(thread_key).await?;
                let record = self.active_thread(thread_key).await?;
                self.ctx
                    .repository
                    .append_log(
                        &record,
                        LogDirection::System,
                        format!(
                            "Action `{}` changed workspace execution mode to `{}` from {origin}.",
                            RuntimeControlAction::SetWorkspaceExecutionMode.as_str(),
                            execution_mode.as_str()
                        ),
                        None,
                    )
                    .await?;
                Ok(RuntimeControlActionEnvelope {
                    ok: true,
                    action,
                    result: RuntimeControlActionResult::SetWorkspaceExecutionMode {
                        thread_key: updated.thread_key,
                        workspace_cwd: updated.workspace_cwd,
                        workspace_execution_mode: updated.workspace_execution_mode,
                        current_execution_mode: updated.current_execution_mode,
                        current_approval_policy: updated.current_approval_policy,
                        current_sandbox_policy: updated.current_sandbox_policy,
                        mode_drift: updated.mode_drift,
                    },
                })
            }
            RuntimeControlActionRequest::LaunchLocalSession { target, session_id } => {
                let config = self.workspace_launch_config(thread_key).await?;
                if !config.hcodex_available {
                    bail!("Managed hcodex is unavailable for this workspace.");
                }
                let normalized_session_id = session_id
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty());
                let command =
                    match target {
                        LaunchLocalSessionTarget::New => config.launch_new_command.clone(),
                        LaunchLocalSessionTarget::ContinueCurrent => config
                            .launch_current_command
                            .clone()
                            .context("managed workspace is missing a current Telegram session")?,
                        LaunchLocalSessionTarget::Resume => hcodex_launch_command(
                            Path::new(&config.hcodex_path),
                            thread_key,
                            config.workspace_execution_mode,
                            Some(normalized_session_id.context(
                                "launch_local_session resume target requires session_id",
                            )?),
                        ),
                    };
                launch_hcodex_via_terminal(&command).await?;
                let record = self.active_thread(thread_key).await?;
                self.ctx
                    .repository
                    .append_log(
                        &record,
                        LogDirection::System,
                        format!(
                            "Action `{}` launched local hcodex via `{}` from {origin}.",
                            RuntimeControlAction::LaunchLocalSession.as_str(),
                            target.as_str()
                        ),
                        None,
                    )
                    .await?;
                Ok(RuntimeControlActionEnvelope {
                    ok: true,
                    action,
                    result: RuntimeControlActionResult::LaunchLocalSession {
                        thread_key: thread_key.to_owned(),
                        target,
                        command,
                        launched: true,
                    },
                })
            }
            RuntimeControlActionRequest::SetThreadCollaborationMode { mode } => {
                let updated = self
                    .set_thread_collaboration_mode(thread_key, mode, origin)
                    .await?;
                Ok(RuntimeControlActionEnvelope {
                    ok: true,
                    action,
                    result: RuntimeControlActionResult::SetThreadCollaborationMode {
                        thread_key: updated.metadata.thread_key,
                        mode,
                    },
                })
            }
            RuntimeControlActionRequest::InterruptRunningTurn => Ok(
                RuntimeControlActionEnvelope {
                    ok: true,
                    action,
                    result: self.interrupt_running_turn(thread_key, origin).await?,
                },
            ),
        }
    }

    pub async fn workspace_execution_mode_view(
        &self,
        thread_key: &str,
    ) -> Result<WorkspaceExecutionModeView> {
        workspace_execution_mode_view_for_repository(&self.ctx.repository, thread_key).await
    }

    pub async fn workspace_launch_config(
        &self,
        thread_key: &str,
    ) -> Result<HcodexLaunchConfigView> {
        workspace_launch_config_for_repository(&self.ctx.repository, thread_key).await
    }

    async fn active_thread(&self, thread_key: &str) -> Result<ThreadRecord> {
        self.ctx
            .repository
            .find_active_thread_by_key(thread_key)
            .await?
            .context("thread_key is not an active thread")
    }
}

#[derive(Clone)]
pub struct WorkspaceRuntimeService {
    ctx: RuntimeControlContext,
}

impl WorkspaceRuntimeService {
    pub async fn ensure_bound_workspace_runtime(
        &self,
        binding: &SessionBinding,
    ) -> Result<PathBuf> {
        let workspace = workspace_path_from_binding(binding)?;
        ensure_workspace_runtime(
            &self.ctx.runtime.codex_working_directory,
            &self.ctx.runtime.data_root_path,
            &self.ctx.seed_template_path,
            &workspace,
        )
        .await?;
        info!(
            event = "runtime_control.workspace.ensure_bound_runtime",
            workspace = %workspace.display(),
            owner_managed = self.ctx.runtime_is_owner_managed(),
            "runtime control ensured bound workspace surface"
        );
        let _ = self.resolve_shared_runtime_state(&workspace).await?;
        Ok(workspace)
    }

    pub async fn prepare_workspace_runtime_for_control(
        &self,
        workspace: PathBuf,
    ) -> Result<CodexWorkspace> {
        info!(
            event = "runtime_control.workspace.prepare_control_runtime",
            workspace = %workspace.display(),
            owner_managed = self.ctx.runtime_is_owner_managed(),
            "runtime control requested control-path workspace runtime"
        );
        let runtime_state = self.resolve_control_runtime_state(&workspace).await?;
        Ok(self.codex_workspace_from_runtime_state(workspace, &runtime_state))
    }

    pub async fn shared_codex_workspace(&self, workspace: PathBuf) -> Result<CodexWorkspace> {
        info!(
            event = "runtime_control.workspace.shared_runtime",
            workspace = %workspace.display(),
            owner_managed = self.ctx.runtime_is_owner_managed(),
            "runtime control requested shared workspace runtime"
        );
        let runtime_state = self.resolve_shared_runtime_state(&workspace).await?;
        Ok(self.codex_workspace_from_runtime_state(workspace, &runtime_state))
    }

    async fn resolve_control_runtime_state(
        &self,
        workspace: &Path,
    ) -> Result<WorkspaceRuntimeState> {
        if self.ctx.runtime_is_owner_managed() {
            return self.ensure_owner_managed_workspace_runtime(workspace).await;
        }

        self.ensure_self_managed_control_runtime(workspace).await
    }

    async fn resolve_shared_runtime_state(
        &self,
        workspace: &Path,
    ) -> Result<WorkspaceRuntimeState> {
        if self.ctx.runtime_is_owner_managed() {
            return self.ensure_owner_managed_workspace_runtime(workspace).await;
        }

        self.ctx
            .app_server_runtime
            .ensure_workspace_daemon(workspace)
            .await
    }

    async fn ensure_owner_managed_workspace_runtime(
        &self,
        workspace: &Path,
    ) -> Result<WorkspaceRuntimeState> {
        match self.read_owner_managed_workspace_runtime(workspace).await {
            Ok(state) => Ok(state),
            Err(error) => {
                info!(
                    event = "runtime_control.workspace.owner_runtime_recover",
                    workspace = %workspace.display(),
                    error = %error,
                    "owner-managed workspace runtime state was unavailable; recovering via runtime manager"
                );
                self.ctx
                    .app_server_runtime
                    .ensure_workspace_daemon(workspace)
                    .await
                    .with_context(|| {
                        format!(
                            "failed to recover owner-managed runtime state for {}",
                            workspace.display()
                        )
                    })
            }
        }
    }

    async fn ensure_self_managed_control_runtime(
        &self,
        workspace: &Path,
    ) -> Result<WorkspaceRuntimeState> {
        let runtime_state = self
            .ctx
            .app_server_runtime
            .ensure_workspace_daemon(workspace)
            .await?;
        let _ = self
            .ctx
            .hcodex_ingress
            .as_ref()
            .context("self-managed control runtime is missing hcodex ingress manager")?
            .ensure_workspace_ingress(
                workspace,
                runtime_state.client_ws_url(),
                runtime_state.client_ws_url(),
            )
            .await?;
        Ok(runtime_state)
    }

    fn codex_workspace_from_runtime_state(
        &self,
        workspace: PathBuf,
        runtime_state: &WorkspaceRuntimeState,
    ) -> CodexWorkspace {
        CodexWorkspace {
            working_directory: workspace,
            app_server_url: Some(runtime_state.client_ws_url().to_owned()),
        }
    }

    async fn read_owner_managed_workspace_runtime(
        &self,
        workspace: &Path,
    ) -> Result<WorkspaceRuntimeState> {
        let state_path = workspace
            .join(".threadbridge")
            .join("state")
            .join("app-server")
            .join("current.json");
        let contents = tokio::fs::read_to_string(&state_path)
            .await
            .with_context(|| {
                format!(
                    "missing owner-managed runtime state: {}",
                    state_path.display()
                )
            })?;
        let state: WorkspaceRuntimeState = serde_json::from_str(&contents).with_context(|| {
            format!(
                "invalid owner-managed runtime state: {}",
                state_path.display()
            )
        })?;
        let client_ws_url = state.client_ws_url();
        let Some(socket_addr) = client_ws_url.strip_prefix("ws://") else {
            bail!("owner-managed runtime url must start with ws://");
        };
        let _ = TcpStream::connect(socket_addr)
            .await
            .with_context(|| format!("owner-managed runtime is unavailable: {}", client_ws_url))?;
        Ok(state)
    }
}

#[derive(Debug, Clone)]
pub enum WorkspaceAddResolution {
    Existing(ThreadRecord),
    Create {
        canonical_workspace_cwd: String,
        suggested_title: String,
    },
}

#[derive(Debug, Clone)]
pub struct SessionRepairResult {
    pub record: ThreadRecord,
    pub verified: bool,
}

#[derive(Clone)]
pub struct WorkspaceSessionService {
    ctx: RuntimeControlContext,
}

impl WorkspaceSessionService {
    pub async fn resolve_workspace_add(
        &self,
        workspace_path: &Path,
    ) -> Result<WorkspaceAddResolution> {
        let preflight = preflight_workspace_add(&self.ctx.repository, workspace_path).await?;
        if let Some(reason) = preflight.blocking_reason() {
            bail!(reason);
        }
        Ok(WorkspaceAddResolution::Create {
            canonical_workspace_cwd: preflight.probe.canonical_workspace_cwd,
            suggested_title: workspace_thread_title(workspace_path),
        })
    }

    pub async fn bind_workspace_record(
        &self,
        record: ThreadRecord,
        workspace_path: &Path,
    ) -> Result<ThreadRecord> {
        let workspace_path = workspace_path
            .canonicalize()
            .unwrap_or_else(|_| workspace_path.to_path_buf());
        let conflicting_threads = self
            .ctx
            .repository
            .find_active_threads_by_workspace(&workspace_path.display().to_string())
            .await?;
        let has_conflict = conflicting_threads
            .iter()
            .any(|candidate| candidate.metadata.thread_key != record.metadata.thread_key);
        if has_conflict {
            bail!(
                "Workspace bind failed: another active thread is already bound to `{}`.",
                workspace_path.display()
            );
        }

        ensure_workspace_runtime(
            &self.ctx.runtime.codex_working_directory,
            &self.ctx.runtime.data_root_path,
            &self.ctx.seed_template_path,
            &workspace_path,
        )
        .await?;
        let codex_workspace = self
            .ctx
            .workspace_runtime_service()
            .prepare_workspace_runtime_for_control(workspace_path.clone())
            .await?;
        let execution_mode = workspace_execution_mode(&workspace_path).await?;
        let binding = self
            .ctx
            .codex
            .start_thread_with_mode(&codex_workspace, execution_mode)
            .await?;
        self.ctx
            .repository
            .bind_workspace(record, binding.cwd, binding.thread_id, binding.execution)
            .await
    }

    pub async fn start_fresh_session(
        &self,
        record: ThreadRecord,
        workspace_path: PathBuf,
    ) -> Result<ThreadRecord> {
        ensure_workspace_runtime(
            &self.ctx.runtime.codex_working_directory,
            &self.ctx.runtime.data_root_path,
            &self.ctx.seed_template_path,
            &workspace_path,
        )
        .await?;
        let codex_workspace = self
            .ctx
            .workspace_runtime_service()
            .prepare_workspace_runtime_for_control(workspace_path)
            .await?;
        let execution_mode = workspace_execution_mode(&codex_workspace.working_directory).await?;
        let binding = self
            .ctx
            .codex
            .start_thread_with_mode(&codex_workspace, execution_mode)
            .await?;
        self.ctx
            .repository
            .bind_workspace(record, binding.cwd, binding.thread_id, binding.execution)
            .await
    }

    pub async fn repair_session_binding(
        &self,
        record: ThreadRecord,
        binding: &SessionBinding,
    ) -> Result<SessionRepairResult> {
        let existing_thread_id = reconnect_target_thread_id(binding).context(
            "This workspace is missing a usable Codex session id. Use New Session first.",
        )?;
        let workspace_path = self
            .ctx
            .workspace_runtime_service()
            .ensure_bound_workspace_runtime(binding)
            .await?;
        let codex_workspace = self
            .ctx
            .workspace_runtime_service()
            .prepare_workspace_runtime_for_control(workspace_path)
            .await?;
        match self
            .ctx
            .codex
            .reconnect_session(&codex_workspace, existing_thread_id)
            .await
        {
            Ok(()) => Ok(SessionRepairResult {
                record: self
                    .ctx
                    .repository
                    .mark_session_binding_verified(record)
                    .await?,
                verified: true,
            }),
            Err(error) => Ok(SessionRepairResult {
                record: self
                    .ctx
                    .repository
                    .mark_session_binding_broken(record, error.to_string())
                    .await?,
                verified: false,
            }),
        }
    }
}

#[derive(Debug, Clone)]
pub enum SessionRoutingResult {
    Unchanged {
        record: ThreadRecord,
        session: Option<SessionBinding>,
    },
    AdoptedPending {
        record: ThreadRecord,
        session: Option<SessionBinding>,
    },
    AdoptedLiveTui {
        record: ThreadRecord,
        session: Option<SessionBinding>,
    },
    BrokenAfterFailedVerify {
        record: ThreadRecord,
        session: Option<SessionBinding>,
    },
}

impl SessionRoutingResult {
    pub fn into_record_session(self) -> (ThreadRecord, Option<SessionBinding>) {
        match self {
            Self::Unchanged { record, session }
            | Self::AdoptedPending { record, session }
            | Self::AdoptedLiveTui { record, session }
            | Self::BrokenAfterFailedVerify { record, session } => (record, session),
        }
    }
}

#[derive(Clone)]
pub struct SessionRoutingService {
    ctx: RuntimeControlContext,
}

impl SessionRoutingService {
    pub async fn maybe_route_telegram_input_to_tui_session(
        &self,
        record: ThreadRecord,
        session: Option<SessionBinding>,
    ) -> Result<SessionRoutingResult> {
        let Some(binding) = session.as_ref() else {
            return Ok(SessionRoutingResult::Unchanged { record, session });
        };
        let Some(tui_session_id) = binding.tui_active_codex_thread_id.clone() else {
            return Ok(SessionRoutingResult::Unchanged { record, session });
        };

        let routing_kind = if binding.tui_session_adoption_pending {
            Some((
                format!(
                    "Auto-adopted pending TUI session `{}` on the next Telegram input.",
                    tui_session_id
                ),
                SessionRoutingKind::Pending,
            ))
        } else if self
            .should_route_telegram_input_to_live_tui_session(&record, binding)
            .await?
        {
            Some((
                format!(
                    "Auto-adopted live TUI session `{}` for Telegram input routing.",
                    tui_session_id
                ),
                SessionRoutingKind::LiveTui,
            ))
        } else {
            None
        };

        let Some((log_message, kind)) = routing_kind else {
            return Ok(SessionRoutingResult::Unchanged { record, session });
        };

        let workspace_path = workspace_path_from_binding(binding)?;
        let workspace = self
            .ctx
            .workspace_runtime_service()
            .shared_codex_workspace(workspace_path)
            .await?;
        if let Err(error) = self
            .ctx
            .codex
            .reconnect_session(&workspace, &tui_session_id)
            .await
        {
            let reason = format!(
                "TUI session adoption verification failed for `{}`: {}",
                tui_session_id, error
            );
            let updated = self.ctx.repository.clear_tui_adoption_state(record).await?;
            self.ctx
                .repository
                .append_log(&updated, LogDirection::System, reason.clone(), None)
                .await?;
            let updated = self
                .ctx
                .repository
                .mark_session_binding_broken(updated, reason)
                .await?;
            let session = self.ctx.repository.read_session_binding(&updated).await?;
            return Ok(SessionRoutingResult::BrokenAfterFailedVerify {
                record: updated,
                session,
            });
        }

        let updated = self.ctx.repository.adopt_tui_active_session(record).await?;
        let session = self.ctx.repository.read_session_binding(&updated).await?;
        self.ctx
            .repository
            .append_log(&updated, LogDirection::System, log_message, None)
            .await?;
        Ok(match kind {
            SessionRoutingKind::Pending => SessionRoutingResult::AdoptedPending {
                record: updated,
                session,
            },
            SessionRoutingKind::LiveTui => SessionRoutingResult::AdoptedLiveTui {
                record: updated,
                session,
            },
        })
    }

    async fn should_route_telegram_input_to_live_tui_session(
        &self,
        record: &ThreadRecord,
        binding: &SessionBinding,
    ) -> Result<bool> {
        let Some(tui_session_id) = binding.tui_active_codex_thread_id.as_deref() else {
            return Ok(false);
        };
        if Some(tui_session_id) == current_bound_session_id(Some(binding)) {
            return Ok(false);
        }
        let workspace_path = workspace_path_from_binding(binding)?;
        let Some(local_tui_claim) = read_local_tui_session_claim(&workspace_path).await? else {
            return Ok(false);
        };
        if local_tui_claim.thread_key != record.metadata.thread_key
            || local_tui_claim.session_id.as_deref() != Some(tui_session_id)
        {
            return Ok(false);
        }
        let snapshot = read_session_status(&workspace_path, tui_session_id).await?;
        Ok(snapshot.as_ref().is_some_and(|snapshot| {
            snapshot.activity_source == SessionActivitySource::Tui && snapshot.live
        }))
    }
}

#[derive(Clone, Copy)]
enum SessionRoutingKind {
    Pending,
    LiveTui,
}

pub async fn workspace_execution_mode_view_for_repository(
    repository: &ThreadRepository,
    thread_key: &str,
) -> Result<WorkspaceExecutionModeView> {
    let record = repository
        .find_active_thread_by_key(thread_key)
        .await?
        .context("thread_key is not an active managed workspace")?;
    let binding = repository
        .read_session_binding(&record)
        .await?
        .context("managed workspace is missing session binding")?;
    workspace_execution_mode_view_for_record(&record, &binding).await
}

pub async fn workspace_execution_mode_view_for_record(
    record: &ThreadRecord,
    binding: &SessionBinding,
) -> Result<WorkspaceExecutionModeView> {
    let workspace_cwd = binding
        .workspace_cwd
        .clone()
        .context("managed workspace is missing workspace_cwd")?;
    let workspace_execution_mode = workspace_execution_mode(Path::new(&workspace_cwd)).await?;
    Ok(WorkspaceExecutionModeView {
        thread_key: record.metadata.thread_key.clone(),
        workspace_cwd,
        workspace_execution_mode,
        current_execution_mode: binding.current_execution_mode,
        current_approval_policy: binding.current_approval_policy.clone(),
        current_sandbox_policy: binding.current_sandbox_policy.clone(),
        mode_drift: binding.current_execution_mode != Some(workspace_execution_mode),
    })
}

pub async fn workspace_launch_config_for_repository(
    repository: &ThreadRepository,
    thread_key: &str,
) -> Result<HcodexLaunchConfigView> {
    let record = repository
        .find_active_thread_by_key(thread_key)
        .await?
        .context("thread_key is not an active managed workspace")?;
    let binding = repository
        .read_session_binding(&record)
        .await?
        .context("managed workspace is missing session binding")?;
    workspace_launch_config_for_record(repository, &record, &binding).await
}

pub async fn workspace_launch_config_for_record(
    repository: &ThreadRepository,
    record: &ThreadRecord,
    binding: &SessionBinding,
) -> Result<HcodexLaunchConfigView> {
    let view = workspace_execution_mode_view_for_record(record, binding).await?;
    let hcodex_path = Path::new(&view.workspace_cwd)
        .join(".threadbridge")
        .join("bin")
        .join("hcodex");
    let recent_codex_sessions = repository
        .read_recent_workspace_sessions(&view.workspace_cwd)
        .await
        .unwrap_or_default();
    Ok(HcodexLaunchConfigView {
        workspace_cwd: view.workspace_cwd,
        thread_key: record.metadata.thread_key.clone(),
        hcodex_path: hcodex_path.display().to_string(),
        hcodex_available: hcodex_path.exists(),
        workspace_execution_mode: view.workspace_execution_mode,
        current_execution_mode: view.current_execution_mode,
        current_approval_policy: view.current_approval_policy,
        current_sandbox_policy: view.current_sandbox_policy,
        mode_drift: view.mode_drift,
        current_codex_thread_id: binding.current_codex_thread_id.clone(),
        launch_new_command: hcodex_launch_command(
            &hcodex_path,
            &record.metadata.thread_key,
            view.workspace_execution_mode,
            None,
        ),
        launch_current_command: binding.current_codex_thread_id.as_ref().map(|session_id| {
            hcodex_launch_command(
                &hcodex_path,
                &record.metadata.thread_key,
                view.workspace_execution_mode,
                Some(session_id),
            )
        }),
        launch_resume_commands: recent_codex_sessions
            .iter()
            .map(|entry| {
                hcodex_launch_command(
                    &hcodex_path,
                    &record.metadata.thread_key,
                    view.workspace_execution_mode,
                    Some(&entry.session_id),
                )
            })
            .collect(),
        recent_codex_sessions,
    })
}

pub fn hcodex_launch_command(
    hcodex_path: &Path,
    thread_key: &str,
    execution_mode: ExecutionMode,
    session_id: Option<&str>,
) -> String {
    match session_id {
        Some(session_id) => format!(
            "{} --thread-key {} {} resume {}",
            shell_quote_path(hcodex_path),
            shell_quote(thread_key),
            execution_mode.hcodex_flag(),
            shell_quote(session_id)
        ),
        None => format!(
            "{} --thread-key {} {}",
            shell_quote_path(hcodex_path),
            shell_quote(thread_key),
            execution_mode.hcodex_flag()
        ),
    }
}

pub async fn launch_hcodex_via_terminal(command: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let script = format!(
            "tell application \"Terminal\"\nactivate\ndo script {}\nend tell",
            apple_script_string(command)
        );
        let status = Command::new("/usr/bin/osascript")
            .arg("-e")
            .arg(script)
            .status()
            .await
            .context("failed to launch Terminal via osascript")?;
        if !status.success() {
            return Err(anyhow!("osascript launch failed with status {status}"));
        }
        return Ok(());
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = command;
        Err(anyhow!("workspace launch is only implemented on macOS"))
    }
}

fn current_bound_session_id(session: Option<&SessionBinding>) -> Option<&str> {
    session
        .and_then(|session| session.current_codex_thread_id.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn reconnect_target_thread_id(binding: &SessionBinding) -> Option<&str> {
    binding
        .current_codex_thread_id
        .as_deref()
        .filter(|value| !value.trim().is_empty())
}

fn workspace_path_from_binding(binding: &SessionBinding) -> Result<PathBuf> {
    let workspace = binding
        .workspace_cwd
        .as_deref()
        .context("session binding is missing workspace_cwd")?;
    Ok(PathBuf::from(workspace))
}

pub fn workspace_thread_title(workspace_path: &Path) -> String {
    workspace_path
        .file_name()
        .and_then(|value| value.to_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| workspace_path.display().to_string())
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

fn shell_quote_path(path: &Path) -> String {
    shell_quote(&path.display().to_string())
}

fn apple_script_string(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

#[cfg(test)]
mod tests {
    use super::{
        preflight_workspace_add, probe_workspace_surface, reset_workspace_runtime_surface,
    };
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};
    use tokio::fs;

    use crate::repository::ThreadRepository;

    fn temp_dir(name: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock drift")
            .as_nanos();
        std::env::temp_dir().join(format!("threadbridge-runtime-control-{name}-{unique}"))
    }

    fn full_auto_snapshot() -> crate::execution_mode::SessionExecutionSnapshot {
        crate::execution_mode::SessionExecutionSnapshot {
            execution_mode: Some(crate::execution_mode::ExecutionMode::FullAuto),
            approval_policy: Some("never".to_owned()),
            sandbox_policy: Some("danger-full-access".to_owned()),
        }
    }

    #[tokio::test]
    async fn workspace_surface_probe_reports_threadbridge_state_files() {
        let workspace = temp_dir("probe");
        let state_dir = workspace.join(".threadbridge/state/runtime-observer");
        let app_server_dir = workspace.join(".threadbridge/state/app-server");
        fs::create_dir_all(&state_dir)
            .await
            .expect("create state dir");
        fs::create_dir_all(&app_server_dir)
            .await
            .expect("create app server dir");
        fs::write(
            workspace.join(".threadbridge/state/workspace-config.json"),
            "{\n  \"execution_mode\": \"full_auto\"\n}\n",
        )
        .await
        .expect("write workspace config");
        fs::write(
            app_server_dir.join("current.json"),
            "{\n  \"daemon_ws_url\": \"ws://127.0.0.1:62012\"\n}\n",
        )
        .await
        .expect("write app server state");
        fs::write(
            workspace.join(".threadbridge/state/runtime-observer/current.json"),
            "{\n  \"live_tui_session_ids\": []\n}\n",
        )
        .await
        .expect("write observer state");
        fs::write(
            workspace.join(".threadbridge/state/runtime-observer/events.jsonl"),
            "{\"event\":\"started\"}\n",
        )
        .await
        .expect("write observer events");

        let probe = probe_workspace_surface(&workspace)
            .await
            .expect("probe workspace");

        assert!(probe.threadbridge_exists);
        assert!(probe.state_exists);
        assert!(probe.workspace_config.exists);
        assert!(probe.app_server_current.exists);
        assert!(probe.runtime_observer_current.exists);
        assert!(probe.runtime_observer_events.exists);
        assert!(
            probe
                .workspace_config
                .summary
                .as_deref()
                .unwrap_or("")
                .contains("full_auto")
        );
        assert_eq!(
            probe.runtime_observer_events.summary.as_deref(),
            Some("1 lines, 20 bytes")
        );

        let _ = fs::remove_dir_all(workspace).await;
    }

    #[tokio::test]
    async fn preflight_blocks_active_workspace_bindings() {
        let root = temp_dir("preflight-active-root");
        let workspace = temp_dir("preflight-active-workspace");
        fs::create_dir_all(&workspace).await.unwrap();
        let repo = ThreadRepository::open(&root).await.unwrap();
        let record = repo.create_thread(1, 7, "Active".to_owned()).await.unwrap();
        let _ = repo
            .bind_workspace(
                record,
                workspace.display().to_string(),
                "thr_active".to_owned(),
                full_auto_snapshot(),
            )
            .await
            .unwrap();

        let preflight = preflight_workspace_add(&repo, &workspace).await.unwrap();
        assert_eq!(preflight.active_threads.len(), 1);
        assert!(
            preflight
                .blocking_reason()
                .unwrap()
                .contains("blocked_by_active_binding")
        );

        let _ = fs::remove_dir_all(root).await;
        let _ = fs::remove_dir_all(workspace).await;
    }

    #[tokio::test]
    async fn preflight_blocks_archived_workspace_bindings() {
        let root = temp_dir("preflight-archived-root");
        let workspace = temp_dir("preflight-archived-workspace");
        fs::create_dir_all(&workspace).await.unwrap();
        let repo = ThreadRepository::open(&root).await.unwrap();
        let record = repo
            .create_thread(1, 7, "Archived".to_owned())
            .await
            .unwrap();
        let record = repo
            .bind_workspace(
                record,
                workspace.display().to_string(),
                "thr_archived".to_owned(),
                full_auto_snapshot(),
            )
            .await
            .unwrap();
        let _ = repo.archive_thread(record).await.unwrap();

        let preflight = preflight_workspace_add(&repo, &workspace).await.unwrap();
        assert_eq!(preflight.archived_threads.len(), 1);
        assert!(
            preflight
                .blocking_reason()
                .unwrap()
                .contains("blocked_by_archived_binding")
        );

        let _ = fs::remove_dir_all(root).await;
        let _ = fs::remove_dir_all(workspace).await;
    }

    #[tokio::test]
    async fn reset_workspace_runtime_surface_removes_threadbridge_only() {
        let workspace = temp_dir("reset-runtime-surface");
        fs::create_dir_all(workspace.join(".threadbridge/bin"))
            .await
            .unwrap();
        fs::write(workspace.join("AGENTS.md"), "workspace instructions\n")
            .await
            .unwrap();
        fs::write(
            workspace.join(".threadbridge/bin/hcodex"),
            "#!/bin/sh\nexit 0\n",
        )
        .await
        .unwrap();

        let removed = reset_workspace_runtime_surface(&workspace).await.unwrap();
        assert!(removed);
        assert!(
            !fs::try_exists(workspace.join(".threadbridge"))
                .await
                .unwrap()
        );
        assert!(fs::try_exists(workspace.join("AGENTS.md")).await.unwrap());

        let _ = fs::remove_dir_all(workspace).await;
    }
}
