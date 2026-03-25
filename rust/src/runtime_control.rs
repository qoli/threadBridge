use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use tokio::net::TcpStream;
use tracing::info;

use crate::app_server_runtime::{WorkspaceRuntimeManager, WorkspaceRuntimeState};
use crate::codex::{CodexRunner, CodexWorkspace};
use crate::config::RuntimeConfig;
use crate::execution_mode::workspace_execution_mode;
use crate::hcodex_ingress::HcodexIngressManager;
use crate::repository::{LogDirection, SessionBinding, ThreadRecord, ThreadRepository};
use crate::workspace::ensure_workspace_runtime;
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
    pub codex: CodexRunner,
    pub app_server_runtime: WorkspaceRuntimeManager,
    pub hcodex_ingress: HcodexIngressManager,
    pub seed_template_path: PathBuf,
    pub runtime_ownership_mode: RuntimeOwnershipMode,
}

impl RuntimeControlContext {
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
        let runtime = self
            .ctx
            .app_server_runtime
            .ensure_workspace_daemon(&workspace)
            .await?;
        let _ = self
            .ctx
            .hcodex_ingress
            .ensure_workspace_ingress(&workspace, &runtime.daemon_ws_url)
            .await?;
        Ok(CodexWorkspace {
            working_directory: workspace,
            app_server_url: Some(runtime.daemon_ws_url),
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
            app_server_url: Some(runtime.daemon_ws_url),
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
        let Some(socket_addr) = state.daemon_ws_url.strip_prefix("ws://") else {
            bail!("owner-managed daemon url must start with ws://");
        };
        let _ = TcpStream::connect(socket_addr).await.with_context(|| {
            format!(
                "owner-managed daemon is unavailable: {}",
                state.daemon_ws_url
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
