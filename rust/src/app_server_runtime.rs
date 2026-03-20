use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tracing::debug;

#[derive(Debug, Clone)]
pub struct WorkspaceRuntimeManager {
    inner: Arc<Mutex<HashMap<String, WorkspaceRuntime>>>,
}

#[derive(Debug, Clone)]
struct WorkspaceRuntime {
    workspace_path: PathBuf,
    daemon_url: String,
    child: Arc<Mutex<Child>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceRuntimeState {
    pub schema_version: u32,
    pub workspace_cwd: String,
    pub daemon_ws_url: String,
    #[serde(default)]
    pub tui_proxy_base_ws_url: Option<String>,
}

const APP_SERVER_STATE_DIR: &str = ".threadbridge/state/app-server";
const APP_SERVER_STATE_FILE: &str = "current.json";

impl WorkspaceRuntimeManager {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn ensure_workspace_daemon(
        &self,
        workspace_path: &Path,
    ) -> Result<WorkspaceRuntimeState> {
        let key = canonical_workspace_key(workspace_path)?;
        if let Some(existing) = self.inner.lock().await.get(&key).cloned()
            && daemon_is_healthy(&existing).await
        {
            let state = WorkspaceRuntimeState {
                schema_version: 1,
                workspace_cwd: existing.workspace_path.display().to_string(),
                daemon_ws_url: existing.daemon_url.clone(),
                tui_proxy_base_ws_url: None,
            };
            write_workspace_runtime_state_file(&existing.workspace_path, &state).await?;
            return Ok(state);
        }

        let runtime = spawn_workspace_runtime(workspace_path).await?;
        let state = WorkspaceRuntimeState {
            schema_version: 1,
            workspace_cwd: runtime.workspace_path.display().to_string(),
            daemon_ws_url: runtime.daemon_url.clone(),
            tui_proxy_base_ws_url: None,
        };
        write_workspace_runtime_state_file(&runtime.workspace_path, &state).await?;
        self.inner.lock().await.insert(key, runtime);
        Ok(state)
    }
}

async fn spawn_workspace_runtime(workspace_path: &Path) -> Result<WorkspaceRuntime> {
    let workspace_path = workspace_path
        .canonicalize()
        .unwrap_or_else(|_| workspace_path.to_path_buf());
    let port = find_free_loopback_port().await?;
    let daemon_url = format!("ws://127.0.0.1:{port}");
    let mut child = Command::new("codex")
        .args(["app-server", "--listen", &daemon_url])
        .current_dir(&workspace_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("failed to spawn shared codex app-server")?;

    if let Some(stderr) = child.stderr.take() {
        let mut stderr_lines = BufReader::new(stderr).lines();
        tokio::spawn(async move {
            while let Ok(Some(line)) = stderr_lines.next_line().await {
                debug!(event = "codex.shared_app_server.stderr", line = %line);
            }
        });
    }

    let child = Arc::new(Mutex::new(child));
    let runtime = WorkspaceRuntime {
        workspace_path,
        daemon_url,
        child,
    };
    wait_for_daemon(&runtime).await?;
    Ok(runtime)
}

async fn wait_for_daemon(runtime: &WorkspaceRuntime) -> Result<()> {
    for _ in 0..20 {
        if daemon_is_healthy(runtime).await {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    bail!(
        "shared codex app-server did not become healthy for {}",
        runtime.workspace_path.display()
    );
}

async fn daemon_is_healthy(runtime: &WorkspaceRuntime) -> bool {
    let Ok(addr) = daemon_socket_addr(&runtime.daemon_url) else {
        return false;
    };
    if let Ok(mut child) = runtime.child.try_lock()
        && child.try_wait().ok().flatten().is_some()
    {
        return false;
    }
    TcpStream::connect(addr).await.is_ok()
}

fn daemon_socket_addr(url: &str) -> Result<SocketAddr> {
    let host_port = url
        .strip_prefix("ws://")
        .context("daemon url must start with ws://")?;
    host_port
        .parse::<SocketAddr>()
        .with_context(|| format!("invalid daemon socket address: {url}"))
}

async fn find_free_loopback_port() -> Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .context("failed to allocate loopback app-server port")?;
    let port = listener
        .local_addr()
        .context("missing loopback app-server local addr")?
        .port();
    drop(listener);
    Ok(port)
}

fn canonical_workspace_key(workspace_path: &Path) -> Result<String> {
    Ok(workspace_path
        .canonicalize()
        .unwrap_or_else(|_| workspace_path.to_path_buf())
        .display()
        .to_string())
}

pub async fn write_workspace_runtime_state_file(
    workspace_path: &Path,
    state: &WorkspaceRuntimeState,
) -> Result<()> {
    let state_dir = workspace_path.join(APP_SERVER_STATE_DIR);
    tokio::fs::create_dir_all(&state_dir)
        .await
        .with_context(|| format!("failed to create {}", state_dir.display()))?;
    let path = state_dir.join(APP_SERVER_STATE_FILE);
    tokio::fs::write(path, format!("{}\n", serde_json::to_string_pretty(state)?))
        .await
        .context("failed to write workspace app-server state")
}
