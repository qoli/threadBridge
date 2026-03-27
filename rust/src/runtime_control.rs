use std::path::{Path, PathBuf};

use anyhow::anyhow;
use anyhow::{Context, Result, bail};
use serde::Serialize;
use tokio::net::TcpStream;
use tokio::process::Command;
use tracing::info;

use crate::app_server_runtime::{WorkspaceRuntimeManager, WorkspaceRuntimeState};
use crate::codex::{CodexRunner, CodexWorkspace};
use crate::config::RuntimeConfig;
use crate::delivery_bus::DeliveryBusCoordinator;
use crate::execution_mode::{ExecutionMode, workspace_execution_mode};
use crate::hcodex_ingress::HcodexIngressManager;
use crate::repository::{
    LogDirection, RecentCodexSessionEntry, SessionBinding, ThreadRecord, ThreadRepository,
};
use crate::workspace::{ensure_workspace_runtime, validate_seed_template};
use crate::workspace_status::{
    SessionActivitySource, read_local_tui_session_claim, read_session_status,
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
    pub hcodex_ingress: HcodexIngressManager,
    pub seed_template_path: PathBuf,
    pub runtime_ownership_mode: RuntimeOwnershipMode,
}

impl RuntimeControlContext {
    pub async fn new(
        runtime: RuntimeConfig,
        app_server_runtime: WorkspaceRuntimeManager,
        hcodex_ingress: HcodexIngressManager,
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
            hcodex_ingress,
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
        if self.ctx.runtime_is_owner_managed() {
            let _ = self
                .read_owner_managed_workspace_runtime(&workspace)
                .await?;
        } else {
            let _ = self
                .ctx
                .app_server_runtime
                .ensure_workspace_daemon(&workspace)
                .await?;
        }
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
        let runtime = if self.ctx.runtime_is_owner_managed() {
            self.read_owner_managed_workspace_runtime(&workspace).await?
        } else {
            let runtime = self
                .ctx
                .app_server_runtime
                .ensure_workspace_daemon(&workspace)
                .await?;
            let _ = self
                .ctx
                .hcodex_ingress
                .ensure_workspace_ingress(
                    &workspace,
                    runtime.client_ws_url(),
                    runtime.client_ws_url(),
                )
                .await?;
            runtime
        };
        Ok(CodexWorkspace {
            working_directory: workspace,
            app_server_url: Some(runtime.client_ws_url().to_owned()),
        })
    }

    pub async fn shared_codex_workspace(&self, workspace: PathBuf) -> Result<CodexWorkspace> {
        info!(
            event = "runtime_control.workspace.shared_runtime",
            workspace = %workspace.display(),
            owner_managed = self.ctx.runtime_is_owner_managed(),
            "runtime control requested shared workspace runtime"
        );
        let runtime = if self.ctx.runtime_is_owner_managed() {
            self.read_owner_managed_workspace_runtime(&workspace)
                .await?
        } else {
            self.ctx
                .app_server_runtime
                .ensure_workspace_daemon(&workspace)
                .await?
        };
        Ok(CodexWorkspace {
            working_directory: workspace,
            app_server_url: Some(runtime.client_ws_url().to_owned()),
        })
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
        let _ = TcpStream::connect(socket_addr).await.with_context(|| {
            format!(
                "owner-managed runtime is unavailable: {}",
                client_ws_url
            )
        })?;
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
        let canonical_workspace_cwd = workspace_path.display().to_string();
        let active_threads = self
            .ctx
            .repository
            .find_active_threads_by_workspace(&canonical_workspace_cwd)
            .await?;
        if active_threads.len() > 1 {
            bail!(
                "Workspace already has multiple active thread bindings: {}",
                canonical_workspace_cwd
            );
        }
        if let Some(record) = active_threads.into_iter().next() {
            return Ok(WorkspaceAddResolution::Existing(record));
        }
        Ok(WorkspaceAddResolution::Create {
            canonical_workspace_cwd,
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
