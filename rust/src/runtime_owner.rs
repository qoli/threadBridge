use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::app_server_runtime::WorkspaceRuntimeManager;
use crate::config::RuntimeConfig;
use crate::workspace::{ensure_workspace_runtime, validate_seed_template};

#[derive(Debug, Clone, Default)]
pub struct RuntimeOwnerReconcileReport {
    pub scanned_workspaces: usize,
    pub ensured_workspaces: usize,
}

#[derive(Debug, Clone)]
pub struct DesktopRuntimeOwner {
    runtime: RuntimeConfig,
    seed_template_path: PathBuf,
    app_server_runtime: WorkspaceRuntimeManager,
}

impl DesktopRuntimeOwner {
    pub fn new(runtime: RuntimeConfig) -> Result<Self> {
        let seed_template_path = validate_seed_template(
            &runtime
                .codex_working_directory
                .join("templates")
                .join("AGENTS.md"),
        )?;
        Ok(Self {
            runtime,
            seed_template_path,
            app_server_runtime: WorkspaceRuntimeManager::new(),
        })
    }

    pub async fn reconcile_managed_workspaces<I, S>(
        &self,
        workspaces: I,
    ) -> Result<RuntimeOwnerReconcileReport>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let unique_workspaces = workspaces
            .into_iter()
            .map(|workspace| canonical_workspace_string(Path::new(workspace.as_ref())))
            .collect::<BTreeSet<_>>();
        let mut report = RuntimeOwnerReconcileReport {
            scanned_workspaces: unique_workspaces.len(),
            ensured_workspaces: 0,
        };
        for workspace in unique_workspaces {
            let workspace_path = Path::new(&workspace);
            ensure_workspace_runtime(
                &self.runtime.codex_working_directory,
                &self.runtime.data_root_path,
                &self.seed_template_path,
                workspace_path,
            )
            .await?;
            let _ = self
                .app_server_runtime
                .ensure_workspace_daemon(workspace_path)
                .await?;
            report.ensured_workspaces += 1;
        }
        Ok(report)
    }
}

fn canonical_workspace_string(workspace_path: &Path) -> String {
    workspace_path
        .canonicalize()
        .unwrap_or_else(|_| workspace_path.to_path_buf())
        .display()
        .to_string()
}
