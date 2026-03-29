use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};
use chrono::Utc;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::TcpListener;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tracing::info;
use uuid::Uuid;

use crate::app_server_ws_worker::WorkerReadyState;

#[derive(Debug, Clone)]
pub struct WorkspaceRuntimeManager {
    inner: Arc<Mutex<HashMap<String, WorkspaceRuntime>>>,
    data_root_path: Option<PathBuf>,
}

#[derive(Debug, Clone)]
struct WorkspaceRuntime {
    workspace_path: PathBuf,
    daemon_url: String,
    worker_url: String,
    hcodex_url: Option<String>,
    child: Arc<Mutex<Child>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceRuntimeState {
    pub schema_version: u32,
    pub workspace_cwd: String,
    pub daemon_ws_url: String,
    #[serde(default)]
    pub worker_ws_url: Option<String>,
    #[serde(default)]
    pub worker_pid: Option<u32>,
    #[serde(default)]
    pub hcodex_ws_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HcodexLaunchTicket {
    pub schema_version: u32,
    pub workspace_cwd: String,
    pub thread_key: String,
    pub issued_at: String,
}

const APP_SERVER_STATE_DIR: &str = ".threadbridge/state/app-server";
const APP_SERVER_STATE_FILE: &str = "current.json";
const HCODEX_LAUNCH_TICKETS_DIR: &str = ".threadbridge/state/app-server/launch-tickets";
const RUNTIME_STATE_SCHEMA_VERSION: u32 = 3;

impl WorkspaceRuntimeState {
    pub fn client_ws_url(&self) -> &str {
        self.worker_ws_url.as_deref().unwrap_or(&self.daemon_ws_url)
    }
}

impl WorkspaceRuntimeManager {
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            data_root_path: None,
        }
    }

    pub fn new_with_data_root(data_root_path: PathBuf) -> Self {
        Self {
            inner: Arc::new(Mutex::new(HashMap::new())),
            data_root_path: Some(data_root_path),
        }
    }

    pub async fn ensure_workspace_daemon(
        &self,
        workspace_path: &Path,
    ) -> Result<WorkspaceRuntimeState> {
        let key = canonical_workspace_key(workspace_path)?;
        let mut inner = self.inner.lock().await;
        if let Some(existing) = inner.get(&key).cloned()
            && worker_is_healthy(&existing).await
        {
            let existing_proxy_url = read_workspace_runtime_state_file(&existing.workspace_path)
                .await
                .ok()
                .flatten()
                .and_then(|state| state.hcodex_ws_url);
            let hcodex_ws_url = if self.data_root_path.is_some() {
                let ensured = ensure_worker_hcodex_ingress(&existing.worker_url)
                    .await?
                    .or(existing_proxy_url)
                    .filter(|value| !value.trim().is_empty())
                    .context("owner-managed worker is missing hcodex launch endpoint")?;
                Some(ensured)
            } else {
                existing_proxy_url
            };
            let state = WorkspaceRuntimeState {
                schema_version: RUNTIME_STATE_SCHEMA_VERSION,
                workspace_cwd: existing.workspace_path.display().to_string(),
                daemon_ws_url: existing.daemon_url.clone(),
                worker_ws_url: Some(existing.worker_url.clone()),
                worker_pid: child_process_id(&existing.child).await,
                hcodex_ws_url,
            };
            info!(
                event = "app_server_runtime.reuse",
                workspace = %existing.workspace_path.display(),
                daemon_ws_url = %existing.daemon_url,
                worker_ws_url = %existing.worker_url,
                hcodex_ws_url = %state.hcodex_ws_url.as_deref().unwrap_or(""),
                "reusing shared app-server worker"
            );
            drop(inner);
            write_workspace_runtime_state_file(&existing.workspace_path, &state).await?;
            return Ok(state);
        }

        let runtime =
            spawn_workspace_runtime(workspace_path, self.data_root_path.as_deref()).await?;
        let hcodex_ws_url = if self.data_root_path.is_some() {
            let ensured = runtime
                .hcodex_url
                .clone()
                .or(ensure_worker_hcodex_ingress(&runtime.worker_url).await?)
                .filter(|value| !value.trim().is_empty())
                .context("owner-managed worker did not publish hcodex launch endpoint")?;
            Some(ensured)
        } else {
            runtime.hcodex_url.clone()
        };
        let state = WorkspaceRuntimeState {
            schema_version: RUNTIME_STATE_SCHEMA_VERSION,
            workspace_cwd: runtime.workspace_path.display().to_string(),
            daemon_ws_url: runtime.daemon_url.clone(),
            worker_ws_url: Some(runtime.worker_url.clone()),
            worker_pid: child_process_id(&runtime.child).await,
            hcodex_ws_url,
        };
        info!(
            event = "app_server_runtime.spawned",
            workspace = %runtime.workspace_path.display(),
            daemon_ws_url = %runtime.daemon_url,
            worker_ws_url = %runtime.worker_url,
            hcodex_ws_url = %runtime.hcodex_url.as_deref().unwrap_or(""),
            "spawned shared app-server worker"
        );
        inner.insert(key, runtime.clone());
        drop(inner);
        write_workspace_runtime_state_file(&runtime.workspace_path, &state).await?;
        Ok(state)
    }
}

fn canonical_workspace_key(workspace_path: &Path) -> Result<String> {
    Ok(workspace_path
        .canonicalize()
        .unwrap_or_else(|_| workspace_path.to_path_buf())
        .display()
        .to_string())
}

fn now_iso() -> String {
    Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}

async fn spawn_workspace_runtime(
    workspace_path: &Path,
    data_root_path: Option<&Path>,
) -> Result<WorkspaceRuntime> {
    let workspace_path = workspace_path
        .canonicalize()
        .unwrap_or_else(|_| workspace_path.to_path_buf());
    let worker_port = find_free_loopback_port().await?;
    let worker_url = format!("ws://127.0.0.1:{worker_port}");
    let ready_file = workspace_path
        .join(APP_SERVER_STATE_DIR)
        .join(format!("worker-ready-{}.json", Uuid::new_v4()));
    let worker_bin = resolve_worker_binary_path()?;
    let mut command = Command::new(&worker_bin);
    command.args([
        "--workspace",
        workspace_path
            .to_str()
            .context("workspace path must be valid utf-8")?,
        "--listen-ws-url",
        &worker_url,
        "--ready-file",
        ready_file
            .to_str()
            .context("ready file path must be valid utf-8")?,
    ]);
    if let Some(data_root_path) = data_root_path {
        command.args([
            "--data-root",
            data_root_path
                .to_str()
                .context("data root path must be valid utf-8")?,
        ]);
    }
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| {
            format!(
                "failed to spawn app_server_ws_worker at {}",
                worker_bin.display()
            )
        })?;

    if let Some(stderr) = child.stderr.take() {
        let mut stderr_lines = BufReader::new(stderr).lines();
        tokio::spawn(async move {
            while let Ok(Some(line)) = stderr_lines.next_line().await {
                info!(event = "threadbridge.app_server_worker.stderr", line = %line);
            }
        });
    }

    let ready = match wait_for_worker_ready(&ready_file, &mut child).await {
        Ok(ready) => ready,
        Err(error) => {
            let _ = cleanup_partial_workspace_runtime_state_file(&workspace_path, None).await;
            return Err(error);
        }
    };
    info!(
        event = "app_server_runtime.worker_ready",
        workspace = %workspace_path.display(),
        daemon_ws_url = %ready.daemon_ws_url,
        worker_ws_url = %ready.worker_ws_url,
        hcodex_ws_url = %ready.hcodex_ws_url.as_deref().unwrap_or(""),
        "shared app-server worker reported readiness"
    );
    let child = Arc::new(Mutex::new(child));
    let runtime = WorkspaceRuntime {
        workspace_path,
        daemon_url: ready.daemon_ws_url,
        worker_url: ready.worker_ws_url,
        hcodex_url: ready.hcodex_ws_url,
        child,
    };
    if let Err(error) = wait_for_daemon(&runtime).await {
        let _ = cleanup_partial_workspace_runtime_state_file(
            &runtime.workspace_path,
            Some(&runtime.daemon_url),
        )
        .await;
        return Err(error);
    }
    Ok(runtime)
}

async fn wait_for_daemon(runtime: &WorkspaceRuntime) -> Result<()> {
    for _ in 0..20 {
        if worker_is_healthy(runtime).await {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    bail!(
        "shared app-server worker did not become healthy for {}",
        runtime.workspace_path.display()
    );
}

async fn worker_is_healthy(runtime: &WorkspaceRuntime) -> bool {
    if let Ok(mut child) = runtime.child.try_lock()
        && child.try_wait().ok().flatten().is_some()
    {
        return false;
    }
    worker_endpoint_is_live(&runtime.worker_url).await
        && daemon_endpoint_is_live(&runtime.daemon_url).await
}

async fn ensure_worker_hcodex_ingress(worker_ws_url: &str) -> Result<Option<String>> {
    let request_id = 9_001_i64;
    let (mut worker_ws, _) = connect_async(worker_ws_url)
        .await
        .with_context(|| format!("failed to connect to worker endpoint {worker_ws_url}"))?;
    worker_ws
        .send(WsMessage::Text(
            json!({
                "jsonrpc": "2.0",
                "id": request_id,
                "method": "threadbridge/ensureHcodexIngress",
                "params": {},
            })
            .to_string()
            .into(),
        ))
        .await
        .context("failed to send worker ingress ensure request")?;

    while let Some(message) = worker_ws.next().await {
        let message = message.context("failed to read worker ingress ensure response")?;
        let payload = match message {
            WsMessage::Text(text) => serde_json::from_str::<Value>(text.as_str())
                .context("invalid json response from worker ingress ensure request")?,
            WsMessage::Binary(bytes) => serde_json::from_slice::<Value>(&bytes)
                .context("invalid binary json from worker ingress ensure request")?,
            WsMessage::Close(_) => break,
            _ => continue,
        };
        if payload.get("id").and_then(Value::as_i64) != Some(request_id) {
            continue;
        }
        if let Some(error) = payload.get("error") {
            return Err(anyhow!("worker ingress ensure request failed: {}", error));
        }
        return Ok(payload
            .get("result")
            .and_then(|result| result.get("hcodexWsUrl"))
            .and_then(Value::as_str)
            .map(str::to_owned));
    }
    bail!("worker closed before replying to ingress ensure request");
}

pub async fn daemon_endpoint_is_live(url: &str) -> bool {
    connect_async(url).await.is_ok()
}

pub async fn worker_endpoint_is_live(url: &str) -> bool {
    connect_async(url).await.is_ok()
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

pub async fn issue_hcodex_launch_ticket(workspace_path: &Path, thread_key: &str) -> Result<String> {
    let tickets_dir = workspace_path.join(HCODEX_LAUNCH_TICKETS_DIR);
    tokio::fs::create_dir_all(&tickets_dir)
        .await
        .with_context(|| format!("failed to create {}", tickets_dir.display()))?;
    let ticket = Uuid::new_v4().to_string();
    let payload = HcodexLaunchTicket {
        schema_version: RUNTIME_STATE_SCHEMA_VERSION,
        workspace_cwd: canonical_workspace_key(workspace_path)?,
        thread_key: thread_key.to_owned(),
        issued_at: now_iso(),
    };
    let path = tickets_dir.join(format!("{ticket}.json"));
    tokio::fs::write(
        &path,
        format!("{}\n", serde_json::to_string_pretty(&payload)?),
    )
    .await
    .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(ticket)
}

fn resolve_worker_binary_path() -> Result<PathBuf> {
    if let Ok(path) = std::env::var("THREADBRIDGE_APP_SERVER_WS_WORKER_BIN") {
        return Ok(PathBuf::from(path));
    }
    let current_exe = std::env::current_exe().context("failed to resolve current executable")?;
    Ok(resolve_worker_binary_path_from(&current_exe))
}

fn resolve_worker_binary_path_from(current_exe: &Path) -> PathBuf {
    let worker_name = if cfg!(windows) {
        "app_server_ws_worker.exe"
    } else {
        "app_server_ws_worker"
    };
    let Some(current_dir) = current_exe.parent() else {
        return PathBuf::from(worker_name);
    };
    for ancestor in current_dir.ancestors() {
        let candidate = ancestor.join(worker_name);
        if candidate.exists() {
            return candidate;
        }
    }
    current_dir.join(worker_name)
}

async fn wait_for_worker_ready(ready_file: &Path, child: &mut Child) -> Result<WorkerReadyState> {
    for _ in 0..50 {
        if let Some(status) = child
            .try_wait()
            .context("failed to poll app-server worker process")?
        {
            bail!("app-server worker exited unexpectedly before readiness: {status:?}");
        }
        match tokio::fs::read_to_string(ready_file).await {
            Ok(contents) => {
                let _ = tokio::fs::remove_file(ready_file).await;
                return serde_json::from_str(&contents)
                    .with_context(|| format!("failed to parse {}", ready_file.display()));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to read {}", ready_file.display()));
            }
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    bail!(
        "timed out waiting for app-server worker readiness at {}",
        ready_file.display()
    )
}

async fn cleanup_partial_workspace_runtime_state_file(
    workspace_path: &Path,
    daemon_ws_url: Option<&str>,
) -> Result<()> {
    let Some(state) = read_workspace_runtime_state_file(workspace_path).await? else {
        return Ok(());
    };
    if state.worker_ws_url.is_some() {
        return Ok(());
    }
    if daemon_ws_url.is_some_and(|expected| state.daemon_ws_url != expected) {
        return Ok(());
    }
    let path = workspace_path
        .join(APP_SERVER_STATE_DIR)
        .join(APP_SERVER_STATE_FILE);
    match tokio::fs::remove_file(&path).await {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("failed to remove {}", path.display())),
    }
}

#[cfg(test)]
mod tests {
    use super::resolve_worker_binary_path_from;
    use std::fs;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_temp_dir(name: &str) -> PathBuf {
        let suffix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock drift")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("threadbridge-{name}-{suffix}"));
        fs::create_dir_all(&dir).expect("failed to create temp dir");
        dir
    }

    #[test]
    fn resolves_worker_from_bundle_back_to_profile_dir() {
        let root = unique_temp_dir("bundle-worker");
        let profile_dir = root.join("target/debug");
        let bundle_dir = profile_dir.join("bundle/osx/threadBridge.app/Contents/MacOS");
        fs::create_dir_all(&bundle_dir).expect("failed to create bundle dir");
        let worker = profile_dir.join("app_server_ws_worker");
        fs::write(&worker, b"").expect("failed to create worker");

        let current_exe = bundle_dir.join("threadbridge_desktop");
        let resolved = resolve_worker_binary_path_from(&current_exe);

        assert_eq!(resolved, worker);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn resolves_worker_from_deps_dir_to_parent_profile_dir() {
        let root = unique_temp_dir("deps-worker");
        let deps_dir = root.join("target/debug/deps");
        fs::create_dir_all(&deps_dir).expect("failed to create deps dir");
        let worker = root.join("target/debug/app_server_ws_worker");
        fs::write(&worker, b"").expect("failed to create worker");

        let current_exe = deps_dir.join("threadbridge_desktop-hash");
        let resolved = resolve_worker_binary_path_from(&current_exe);

        assert_eq!(resolved, worker);
        let _ = fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn cleanup_partial_runtime_state_removes_half_written_state_for_attempted_daemon() {
        let root = unique_temp_dir("cleanup-partial-state");
        let workspace = root.join("workspace");
        fs::create_dir_all(&workspace).expect("failed to create workspace dir");
        let state = super::WorkspaceRuntimeState {
            schema_version: super::RUNTIME_STATE_SCHEMA_VERSION,
            workspace_cwd: workspace.display().to_string(),
            daemon_ws_url: "ws://127.0.0.1:61234".to_owned(),
            worker_ws_url: None,
            worker_pid: None,
            hcodex_ws_url: Some("ws://127.0.0.1:61239".to_owned()),
        };
        super::write_workspace_runtime_state_file(&workspace, &state)
            .await
            .expect("write state");

        super::cleanup_partial_workspace_runtime_state_file(
            &workspace,
            Some("ws://127.0.0.1:61234"),
        )
        .await
        .expect("cleanup state");

        let remaining = super::read_workspace_runtime_state_file(&workspace)
            .await
            .expect("read state");
        assert!(remaining.is_none());
        let _ = fs::remove_dir_all(root);
    }
}

async fn child_process_id(child: &Arc<Mutex<Child>>) -> Option<u32> {
    child.lock().await.id()
}

pub async fn consume_hcodex_launch_ticket(
    workspace_path: &Path,
    ticket: &str,
) -> Result<Option<HcodexLaunchTicket>> {
    // launch_ticket is intentionally single-use. Reconnect tolerance belongs in
    // hcodex_ws_bridge, which must keep the first upstream ingress session
    // alive instead of trying to consume the same ticket a second time.
    if !ticket
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
    {
        return Ok(None);
    }
    let path = workspace_path
        .join(HCODEX_LAUNCH_TICKETS_DIR)
        .join(format!("{ticket}.json"));
    let contents = match tokio::fs::read_to_string(&path).await {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", path.display()));
        }
    };
    tokio::fs::remove_file(&path)
        .await
        .with_context(|| format!("failed to remove {}", path.display()))?;
    let parsed = serde_json::from_str(&contents)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    Ok(Some(parsed))
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

pub async fn read_workspace_runtime_state_file(
    workspace_path: &Path,
) -> Result<Option<WorkspaceRuntimeState>> {
    let path = workspace_path
        .join(APP_SERVER_STATE_DIR)
        .join(APP_SERVER_STATE_FILE);
    let contents = match tokio::fs::read_to_string(&path).await {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", path.display()));
        }
    };
    let state = serde_json::from_str(&contents)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    Ok(Some(state))
}
