use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Result;
use chrono::Utc;
use serde::Serialize;
use tokio::sync::{Mutex, RwLock};
use tracing::info;

use crate::app_server_runtime::{WorkspaceRuntimeManager, daemon_endpoint_is_live};
use crate::config::RuntimeConfig;
use crate::hcodex_ingress::hcodex_ingress_endpoint_is_live;
use crate::workspace::{ensure_workspace_runtime, validate_seed_template};

#[derive(Debug, Clone, Default, Serialize)]
pub struct RuntimeOwnerReconcileReport {
    pub scanned_workspaces: usize,
    pub ensured_workspaces: usize,
    pub ensured_ingresses: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct RuntimeOwnerStatus {
    pub state: &'static str,
    pub last_reconcile_started_at: Option<String>,
    pub last_reconcile_finished_at: Option<String>,
    pub last_successful_reconcile_at: Option<String>,
    pub last_error: Option<String>,
    pub last_report: RuntimeOwnerReconcileReport,
}

impl RuntimeOwnerStatus {
    pub fn inactive() -> Self {
        Self {
            state: "inactive",
            last_reconcile_started_at: None,
            last_reconcile_finished_at: None,
            last_successful_reconcile_at: None,
            last_error: None,
            last_report: RuntimeOwnerReconcileReport::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct WorkspaceRuntimeHeartbeat {
    pub workspace_cwd: String,
    pub app_server_status: &'static str,
    pub hcodex_ingress_status: &'static str,
    pub runtime_readiness: &'static str,
    pub last_checked_at: String,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct DesktopRuntimeOwner {
    runtime: RuntimeConfig,
    seed_template_path: PathBuf,
    app_server_runtime: WorkspaceRuntimeManager,
    status: Arc<RwLock<RuntimeOwnerStatus>>,
    workspace_heartbeats: Arc<RwLock<BTreeMap<String, WorkspaceRuntimeHeartbeat>>>,
    reconcile_lock: Arc<Mutex<()>>,
}

impl DesktopRuntimeOwner {
    pub async fn new(runtime: RuntimeConfig) -> Result<Self> {
        let seed_template_path = validate_seed_template(&runtime.runtime_template_path())?;
        Ok(Self {
            app_server_runtime: WorkspaceRuntimeManager::new_with_data_root(
                runtime.data_root_path.clone(),
            ),
            runtime,
            seed_template_path,
            status: Arc::new(RwLock::new(RuntimeOwnerStatus {
                state: "idle",
                last_reconcile_started_at: None,
                last_reconcile_finished_at: None,
                last_successful_reconcile_at: None,
                last_error: None,
                last_report: RuntimeOwnerReconcileReport::default(),
            })),
            workspace_heartbeats: Arc::new(RwLock::new(BTreeMap::new())),
            reconcile_lock: Arc::new(Mutex::new(())),
        })
    }

    pub async fn status(&self) -> RuntimeOwnerStatus {
        self.status.read().await.clone()
    }

    pub fn app_server_runtime(&self) -> WorkspaceRuntimeManager {
        self.app_server_runtime.clone()
    }

    pub async fn workspace_heartbeat(
        &self,
        workspace_path: &Path,
    ) -> Option<WorkspaceRuntimeHeartbeat> {
        let key = canonical_workspace_string(workspace_path);
        self.workspace_heartbeats.read().await.get(&key).cloned()
    }

    pub async fn reconcile_managed_workspaces<I, S>(
        &self,
        workspaces: I,
    ) -> Result<RuntimeOwnerReconcileReport>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        let _reconcile_guard = self.reconcile_lock.lock().await;
        let unique_workspaces = workspaces
            .into_iter()
            .map(|workspace| canonical_workspace_string(Path::new(workspace.as_ref())))
            .collect::<BTreeSet<_>>();
        let mut report = RuntimeOwnerReconcileReport {
            scanned_workspaces: unique_workspaces.len(),
            ensured_workspaces: 0,
            ensured_ingresses: 0,
        };
        let started_at = now_iso();
        {
            let mut status = self.status.write().await;
            status.state = "running";
            status.last_reconcile_started_at = Some(started_at);
            status.last_error = None;
        }
        for workspace in &unique_workspaces {
            let workspace_path = Path::new(workspace);
            info!(
                event = "runtime_owner.workspace.reconcile_started",
                workspace = %workspace_path.display(),
                "desktop runtime owner started reconciling workspace"
            );
            let step = async {
                ensure_workspace_runtime(
                    &self.runtime.runtime_assets_root_path,
                    &self.runtime.data_root_path,
                    &self.seed_template_path,
                    workspace_path,
                )
                .await?;
                let runtime = self
                    .app_server_runtime
                    .ensure_workspace_daemon(workspace_path)
                    .await?;
                info!(
                    event = "runtime_owner.workspace.app_server_ready",
                    workspace = %workspace_path.display(),
                    daemon_ws_url = %runtime.daemon_ws_url,
                    worker_ws_url = %runtime.worker_ws_url.as_deref().unwrap_or(""),
                    hcodex_ws_url = %runtime.hcodex_ws_url.as_deref().unwrap_or(""),
                    "desktop runtime owner ensured workspace app-server"
                );
                let ensured_ingress = runtime
                    .hcodex_ws_url
                    .as_deref()
                    .is_some_and(|url| !url.trim().is_empty());
                Ok::<bool, anyhow::Error>(ensured_ingress)
            }
            .await;
            let ensured_ingress = match step {
                Ok(ensured_ingress) => ensured_ingress,
                Err(error) => {
                    self.record_workspace_heartbeat(
                        workspace_path,
                        WorkspaceRuntimeHeartbeat {
                            workspace_cwd: workspace.clone(),
                            app_server_status: "unavailable",
                            hcodex_ingress_status: "unavailable",
                            runtime_readiness: "unavailable",
                            last_checked_at: now_iso(),
                            last_error: Some(error.to_string()),
                        },
                    )
                    .await;
                    let finished_at = now_iso();
                    let mut status = self.status.write().await;
                    status.state = "error";
                    status.last_reconcile_finished_at = Some(finished_at);
                    status.last_error = Some(error.to_string());
                    status.last_report = report.clone();
                    return Err(error);
                }
            };
            self.record_workspace_heartbeat(
                workspace_path,
                heartbeat_for_workspace(workspace_path).await,
            )
            .await;
            report.ensured_workspaces += 1;
            if ensured_ingress {
                report.ensured_ingresses += 1;
            }
        }
        self.prune_workspace_heartbeats(&unique_workspaces).await;
        let finished_at = now_iso();
        let mut status = self.status.write().await;
        status.state = "healthy";
        status.last_reconcile_finished_at = Some(finished_at.clone());
        status.last_successful_reconcile_at = Some(finished_at);
        status.last_error = None;
        status.last_report = report.clone();
        Ok(report)
    }

    async fn record_workspace_heartbeat(
        &self,
        workspace_path: &Path,
        heartbeat: WorkspaceRuntimeHeartbeat,
    ) {
        let key = canonical_workspace_string(workspace_path);
        self.workspace_heartbeats
            .write()
            .await
            .insert(key, heartbeat);
    }

    async fn prune_workspace_heartbeats(&self, workspaces: &BTreeSet<String>) {
        self.workspace_heartbeats
            .write()
            .await
            .retain(|workspace, _| workspaces.contains(workspace));
    }
}

fn canonical_workspace_string(workspace_path: &Path) -> String {
    workspace_path
        .canonicalize()
        .unwrap_or_else(|_| workspace_path.to_path_buf())
        .display()
        .to_string()
}

fn now_iso() -> String {
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

async fn heartbeat_for_workspace(workspace_path: &Path) -> WorkspaceRuntimeHeartbeat {
    let state_path = workspace_path
        .join(".threadbridge")
        .join("state")
        .join("app-server")
        .join("current.json");
    let workspace_cwd = canonical_workspace_string(workspace_path);
    let last_checked_at = now_iso();
    let contents = match tokio::fs::read_to_string(&state_path).await {
        Ok(contents) => contents,
        Err(error) => {
            return WorkspaceRuntimeHeartbeat {
                workspace_cwd,
                app_server_status: "missing",
                hcodex_ingress_status: "missing",
                runtime_readiness: "unavailable",
                last_checked_at,
                last_error: Some(format!("failed to read {}: {error}", state_path.display())),
            };
        }
    };
    let state: crate::app_server_runtime::WorkspaceRuntimeState =
        match serde_json::from_str(&contents) {
            Ok(state) => state,
            Err(error) => {
                return WorkspaceRuntimeHeartbeat {
                    workspace_cwd,
                    app_server_status: "invalid",
                    hcodex_ingress_status: "invalid",
                    runtime_readiness: "unavailable",
                    last_checked_at,
                    last_error: Some(format!("invalid {}: {error}", state_path.display())),
                };
            }
        };

    let app_server_running = daemon_endpoint_is_live(&state.daemon_ws_url).await;
    let proxy_running = match state.hcodex_ws_url.as_deref() {
        Some(url) => hcodex_ingress_endpoint_is_live(url).await,
        None => false,
    };
    let app_server_status = if app_server_running {
        "running"
    } else {
        "stale"
    };
    let hcodex_ingress_status = match state.hcodex_ws_url.as_deref() {
        Some(_) if proxy_running => "running",
        Some(_) => "stale",
        None => "missing",
    };
    let runtime_readiness = if app_server_running && proxy_running {
        "ready"
    } else if app_server_running {
        "degraded"
    } else {
        "unavailable"
    };

    WorkspaceRuntimeHeartbeat {
        workspace_cwd,
        app_server_status,
        hcodex_ingress_status,
        runtime_readiness,
        last_checked_at,
        last_error: (!app_server_running)
            .then_some("workspace app-server daemon is unavailable".to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::heartbeat_for_workspace;
    use crate::app_server_runtime::{WorkspaceRuntimeState, write_workspace_runtime_state_file};
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};
    use tokio::net::TcpListener;

    fn unique_temp_dir(name: &str) -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock drift")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("threadbridge-runtime-owner-{name}-{suffix}"));
        fs::create_dir_all(&dir).expect("failed to create temp dir");
        dir
    }

    #[tokio::test]
    async fn heartbeat_ignores_stale_worker_endpoint_when_daemon_is_live() {
        let root = unique_temp_dir("daemon-live");
        let workspace = root.join("workspace");
        fs::create_dir_all(&workspace).expect("failed to create workspace dir");
        let daemon = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind daemon listener");
        let daemon_port = daemon.local_addr().expect("local addr").port();
        let state = WorkspaceRuntimeState {
            schema_version: 3,
            workspace_cwd: workspace.display().to_string(),
            daemon_ws_url: format!("ws://127.0.0.1:{daemon_port}"),
            worker_ws_url: Some("ws://127.0.0.1:1".to_owned()),
            worker_pid: None,
            hcodex_ws_url: None,
        };
        write_workspace_runtime_state_file(&workspace, &state)
            .await
            .expect("write state");

        let heartbeat = heartbeat_for_workspace(&workspace).await;

        assert_eq!(heartbeat.app_server_status, "running");
        assert_eq!(heartbeat.runtime_readiness, "degraded");
        assert_eq!(heartbeat.last_error, None);

        drop(daemon);
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn heartbeat_marks_runtime_unavailable_when_daemon_is_down() {
        let root = unique_temp_dir("daemon-down");
        let workspace = root.join("workspace");
        fs::create_dir_all(&workspace).expect("failed to create workspace dir");
        let state = WorkspaceRuntimeState {
            schema_version: 3,
            workspace_cwd: workspace.display().to_string(),
            daemon_ws_url: "ws://127.0.0.1:9".to_owned(),
            worker_ws_url: Some("ws://127.0.0.1:1".to_owned()),
            worker_pid: None,
            hcodex_ws_url: None,
        };
        write_workspace_runtime_state_file(&workspace, &state)
            .await
            .expect("write state");

        let heartbeat = heartbeat_for_workspace(&workspace).await;

        assert_eq!(heartbeat.app_server_status, "stale");
        assert_eq!(heartbeat.runtime_readiness, "unavailable");
        assert_eq!(
            heartbeat.last_error.as_deref(),
            Some("workspace app-server daemon is unavailable")
        );

        let _ = fs::remove_dir_all(root);
    }
}
