use std::ffi::OsString;
#[cfg(unix)]
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;
use tokio::fs;
use tokio::net::TcpStream;
use tokio::process::Child;
use tokio::process::Command;
use url::Url;

#[cfg(unix)]
use tokio::signal::unix::{SignalKind, signal};

use crate::app_server_runtime::{WorkspaceRuntimeState, issue_hcodex_launch_ticket};
use crate::hcodex_ws_bridge::is_codex_safe_remote_ws_url;
use crate::process_utils::process_exists;
use crate::repository::ThreadRepository;
use crate::workspace_status::{
    has_live_local_tui_session, record_hcodex_launcher_ended, record_hcodex_launcher_started,
};

macro_rules! hcodex_debug {
    ($($arg:tt)*) => {
        if hcodex_debug_enabled() {
            eprintln!($($arg)*);
        }
    };
}

fn hcodex_debug_enabled() -> bool {
    matches!(
        std::env::var("THREADBRIDGE_HCODEX_DEBUG").as_deref(),
        Ok("1") | Ok("true") | Ok("TRUE") | Ok("yes") | Ok("YES")
    )
}

pub async fn maybe_run_from_args(args: Vec<OsString>) -> Result<bool> {
    let Some(command) = args.first().and_then(|value| value.to_str()) else {
        return Ok(false);
    };
    match command {
        "ensure-hcodex-runtime" => {
            let config = EnsureHcodexRuntimeCli::parse(&args[1..])?;
            ensure_hcodex_runtime_inner(
                &config.workspace,
                &config.data_root,
                config.parent_pid,
                config.ready_file.as_deref(),
            )
            .await?;
            Ok(true)
        }
        "run-hcodex-session" => {
            let config = RunHcodexSessionCli::parse(&args[1..])?;
            run_hcodex_session(&config).await?;
            Ok(true)
        }
        "resolve-hcodex-launch" => {
            let config = ResolveHcodexLaunchCli::parse(&args[1..])?;
            resolve_hcodex_launch(&config).await?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

struct EnsureHcodexRuntimeCli {
    workspace: PathBuf,
    data_root: PathBuf,
    parent_pid: Option<u32>,
    ready_file: Option<PathBuf>,
}

struct RunHcodexSessionCli {
    workspace: PathBuf,
    data_root: PathBuf,
    thread_key: String,
    codex_bin: PathBuf,
    launch_ws_url: String,
    codex_args: Vec<OsString>,
}

struct ResolveHcodexLaunchCli {
    workspace: PathBuf,
    data_root: PathBuf,
    thread_key: Option<String>,
}

impl EnsureHcodexRuntimeCli {
    fn parse(args: &[OsString]) -> Result<Self> {
        let mut workspace: Option<PathBuf> = None;
        let mut data_root: Option<PathBuf> = None;
        let mut parent_pid: Option<u32> = None;
        let mut ready_file: Option<PathBuf> = None;
        let mut iter = args.iter();
        while let Some(flag) = iter.next() {
            let flag = flag
                .to_str()
                .ok_or_else(|| anyhow!("ensure-hcodex-runtime arguments must be valid utf-8"))?;
            match flag {
                "--workspace" => {
                    let value = iter.next().context("missing value for --workspace")?;
                    workspace = Some(PathBuf::from(value));
                }
                "--data-root" => {
                    let value = iter.next().context("missing value for --data-root")?;
                    data_root = Some(PathBuf::from(value));
                }
                "--parent-pid" => {
                    let value = iter
                        .next()
                        .context("missing value for --parent-pid")?
                        .to_str()
                        .context("--parent-pid must be valid utf-8")?;
                    parent_pid = Some(
                        value
                            .parse::<u32>()
                            .with_context(|| format!("invalid --parent-pid: {value}"))?,
                    );
                }
                "--ready-file" => {
                    let value = iter.next().context("missing value for --ready-file")?;
                    ready_file = Some(PathBuf::from(value));
                }
                other => bail!("unsupported ensure-hcodex-runtime argument: {other}"),
            }
        }

        Ok(Self {
            workspace: workspace.context("missing required --workspace")?,
            data_root: data_root.context("missing required --data-root")?,
            parent_pid,
            ready_file,
        })
    }
}

impl RunHcodexSessionCli {
    fn parse(args: &[OsString]) -> Result<Self> {
        let mut workspace: Option<PathBuf> = None;
        let mut data_root: Option<PathBuf> = None;
        let mut thread_key: Option<String> = None;
        let mut codex_bin: Option<PathBuf> = None;
        let mut launch_ws_url: Option<String> = None;
        let mut codex_args = Vec::new();
        let mut iter = args.iter();
        while let Some(flag) = iter.next() {
            let flag = flag
                .to_str()
                .ok_or_else(|| anyhow!("run-hcodex-session arguments must be valid utf-8"))?;
            match flag {
                "--workspace" => {
                    let value = iter.next().context("missing value for --workspace")?;
                    workspace = Some(PathBuf::from(value));
                }
                "--data-root" => {
                    let value = iter.next().context("missing value for --data-root")?;
                    data_root = Some(PathBuf::from(value));
                }
                "--thread-key" => {
                    let value = iter
                        .next()
                        .context("missing value for --thread-key")?
                        .to_str()
                        .context("--thread-key must be valid utf-8")?;
                    thread_key = Some(value.to_owned());
                }
                "--codex-bin" => {
                    let value = iter.next().context("missing value for --codex-bin")?;
                    codex_bin = Some(PathBuf::from(value));
                }
                "--remote-ws-url" => {
                    let value = iter
                        .next()
                        .context("missing value for --remote-ws-url")?
                        .to_str()
                        .context("--remote-ws-url must be valid utf-8")?;
                    launch_ws_url = Some(value.to_owned());
                }
                "--" => {
                    codex_args.extend(iter.cloned());
                    break;
                }
                other => bail!("unsupported run-hcodex-session argument: {other}"),
            }
        }

        Ok(Self {
            workspace: workspace.context("missing required --workspace")?,
            data_root: data_root.context("missing required --data-root")?,
            thread_key: thread_key.context("missing required --thread-key")?,
            codex_bin: codex_bin.context("missing required --codex-bin")?,
            launch_ws_url: launch_ws_url.context("missing required --remote-ws-url")?,
            codex_args,
        })
    }
}

impl ResolveHcodexLaunchCli {
    fn parse(args: &[OsString]) -> Result<Self> {
        let mut workspace: Option<PathBuf> = None;
        let mut data_root: Option<PathBuf> = None;
        let mut thread_key: Option<String> = None;
        let mut iter = args.iter();
        while let Some(flag) = iter.next() {
            let flag = flag
                .to_str()
                .ok_or_else(|| anyhow!("resolve-hcodex-launch arguments must be valid utf-8"))?;
            match flag {
                "--workspace" => {
                    let value = iter.next().context("missing value for --workspace")?;
                    workspace = Some(PathBuf::from(value));
                }
                "--data-root" => {
                    let value = iter.next().context("missing value for --data-root")?;
                    data_root = Some(PathBuf::from(value));
                }
                "--thread-key" => {
                    let value = iter
                        .next()
                        .context("missing value for --thread-key")?
                        .to_str()
                        .context("--thread-key must be valid utf-8")?;
                    thread_key = Some(value.to_owned());
                }
                other => bail!("unsupported resolve-hcodex-launch argument: {other}"),
            }
        }
        Ok(Self {
            workspace: workspace.context("missing required --workspace")?,
            data_root: data_root.context("missing required --data-root")?,
            thread_key,
        })
    }
}

async fn ensure_hcodex_runtime_inner(
    workspace: &Path,
    data_root: &Path,
    parent_pid: Option<u32>,
    ready_file: Option<&Path>,
) -> Result<()> {
    if runtime_state_is_live(workspace).await? {
        write_ready_file(ready_file).await?;
        if let Some(parent_pid) = parent_pid {
            while process_exists(parent_pid) {
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
        return Ok(());
    }

    let _ = ThreadRepository::open(data_root).await?;
    bail!(
        "hcodex requires the desktop runtime owner. Start threadbridge_desktop and repair the workspace runtime for {}.",
        workspace.display()
    )
}

async fn run_hcodex_session(config: &RunHcodexSessionCli) -> Result<()> {
    // resolve-hcodex-launch returns an ingress launch URL, which may carry
    // sideband state like launch_ticket in the query string. Upstream Codex
    // only accepts bare ws://host:port endpoints for --remote. Keep the
    // compatibility boundary here: launch URLs must be adapted through the
    // standalone hcodex-ws-bridge process before upstream Codex sees them.
    let current_exe =
        std::env::current_exe().context("failed to resolve current threadbridge executable")?;
    let (codex_remote_ws_url, mut bridge_child, bridge_ready_file) =
        if is_codex_safe_remote_ws_url(&config.launch_ws_url) {
            (config.launch_ws_url.clone(), None, None)
        } else {
            let ready_file = bridge_ready_file_path();
            let mut bridge_child = Command::new(&current_exe)
                .arg("hcodex-ws-bridge")
                .arg("--upstream")
                .arg(&config.launch_ws_url)
                .arg("--ready-file")
                .arg(&ready_file)
                .current_dir(&config.workspace)
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::inherit())
                .spawn()
                .with_context(|| {
                    format!("failed to spawn {} hcodex-ws-bridge", current_exe.display())
                })?;
            let codex_remote_ws_url =
                wait_for_bridge_ready_file(&ready_file)
                    .await
                    .map_err(|error| {
                        let _ = bridge_child.start_kill();
                        error
                    })?;
            (codex_remote_ws_url, Some(bridge_child), Some(ready_file))
        };
    let codex_command_line =
        format_codex_command_line(&config.codex_bin, &codex_remote_ws_url, &config.codex_args);
    hcodex_debug!("hcodex: launch ws url {}", config.launch_ws_url);
    hcodex_debug!("hcodex: codex remote ws url {}", codex_remote_ws_url);
    hcodex_debug!("hcodex: exec {codex_command_line}");
    let mut command = Command::new(&config.codex_bin);
    command
        .current_dir(&config.workspace)
        .arg("--remote")
        .arg(&codex_remote_ws_url)
        .args(&config.codex_args)
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit());
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            if let Some(bridge_child) = bridge_child.as_mut() {
                let _ = bridge_child.start_kill();
            }
            return Err(error)
                .with_context(|| format!("failed to spawn {}", config.codex_bin.display()));
        }
    };
    let child_pid = child.id().context("spawned codex child is missing pid")?;
    let shell_pid = std::process::id();
    record_hcodex_launcher_started(
        &config.workspace,
        &config.thread_key,
        shell_pid,
        child_pid,
        &codex_command_line,
    )
    .await?;

    let status = wait_for_codex_child(&mut child, child_pid).await?;
    if let Some(bridge_child) = bridge_child.as_mut() {
        let _ = bridge_child.start_kill();
    }
    if let Some(ready_file) = bridge_ready_file.as_deref() {
        let _ = fs::remove_file(ready_file).await;
    }
    record_hcodex_launcher_ended(&config.workspace, &config.thread_key, shell_pid, child_pid)
        .await?;

    let repository = ThreadRepository::open(&config.data_root).await?;
    if let Some(record) = repository
        .find_active_thread_by_key(&config.thread_key)
        .await?
        && let Some(binding) = repository.read_session_binding(&record).await?
    {
        let has_live_tui_session = has_live_local_tui_session(
            &config.workspace,
            &config.thread_key,
            binding.tui_active_codex_thread_id.as_deref(),
        )
        .await
        .unwrap_or(false);
        if has_live_tui_session {
            let _ = repository
                .mark_tui_adoption_pending_for_thread_key(&config.thread_key)
                .await?;
        } else {
            let _ = repository.clear_tui_adoption_state(record).await?;
        }
    }

    std::process::exit(exit_code_from_status(status));
}

#[cfg(unix)]
async fn wait_for_codex_child(child: &mut Child, child_pid: u32) -> Result<ExitStatus> {
    let mut sigint =
        signal(SignalKind::interrupt()).context("failed to install hcodex SIGINT handler")?;
    let mut sighup =
        signal(SignalKind::hangup()).context("failed to install hcodex SIGHUP handler")?;
    let mut sigterm =
        signal(SignalKind::terminate()).context("failed to install hcodex SIGTERM handler")?;
    let mut sigquit =
        signal(SignalKind::quit()).context("failed to install hcodex SIGQUIT handler")?;

    tokio::select! {
        status = child.wait() => status.context("failed waiting for codex child"),
        _ = sigint.recv() => terminate_child_after_signal(child, child_pid, "INT").await,
        _ = sighup.recv() => terminate_child_after_signal(child, child_pid, "HUP").await,
        _ = sigterm.recv() => terminate_child_after_signal(child, child_pid, "TERM").await,
        _ = sigquit.recv() => terminate_child_after_signal(child, child_pid, "QUIT").await,
    }
}

#[cfg(not(unix))]
async fn wait_for_codex_child(child: &mut Child, _child_pid: u32) -> Result<ExitStatus> {
    child.wait().await.context("failed waiting for codex child")
}

#[cfg(unix)]
async fn terminate_child_after_signal(
    child: &mut Child,
    child_pid: u32,
    signal_name: &str,
) -> Result<ExitStatus> {
    hcodex_debug!(
        "hcodex: launcher received SIG{}, forwarding to child {}",
        signal_name,
        child_pid
    );
    let _ = send_signal_to_child_process_tree(child_pid, signal_name);

    match tokio::time::timeout(Duration::from_secs(3), child.wait()).await {
        Ok(status) => {
            let status = status.context("failed waiting for codex child after forwarded signal")?;
            Ok(status)
        }
        Err(_) => {
            hcodex_debug!(
                "hcodex: child {} ignored SIG{}, forcing kill",
                child_pid,
                signal_name
            );
            let _ = child.start_kill();
            match tokio::time::timeout(Duration::from_secs(1), child.wait()).await {
                Ok(status) => status.context("failed waiting for codex child after forced kill"),
                Err(_) => {
                    bail!(
                        "hcodex: child {} did not exit after forwarded signal {}",
                        child_pid,
                        signal_name
                    );
                }
            }
        }
    }
}

#[cfg(unix)]
fn send_signal_to_child_process_tree(child_pid: u32, signal_name: &str) -> Result<()> {
    let target = process_group_id(child_pid)
        .map(|pgid| format!("-{pgid}"))
        .unwrap_or_else(|| child_pid.to_string());
    let status = std::process::Command::new("kill")
        .arg(format!("-{signal_name}"))
        .arg(&target)
        .status()
        .with_context(|| format!("failed to send SIG{signal_name} to {target}"))?;
    if status.success() {
        return Ok(());
    }
    let fallback = std::process::Command::new("kill")
        .arg(format!("-{signal_name}"))
        .arg(child_pid.to_string())
        .status()
        .with_context(|| format!("failed to send SIG{signal_name} to child {child_pid}"))?;
    if fallback.success() {
        Ok(())
    } else {
        bail!("kill -{signal_name} failed for child {child_pid}");
    }
}

#[cfg(unix)]
fn process_group_id(pid: u32) -> Option<u32> {
    let output = std::process::Command::new("ps")
        .arg("-o")
        .arg("pgid=")
        .arg("-p")
        .arg(pid.to_string())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse::<u32>()
        .ok()
}

fn exit_code_from_status(status: ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    if let Some(signal) = status.signal() {
        return 128 + signal;
    }
    1
}

fn bridge_ready_file_path() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir().join(format!("threadbridge-hcodex-ws-bridge-{nanos}.json"))
}

async fn wait_for_bridge_ready_file(path: &Path) -> Result<String> {
    for _ in 0..40 {
        if let Ok(contents) = fs::read_to_string(path).await {
            let payload: Value = serde_json::from_str(&contents)
                .with_context(|| format!("invalid bridge ready payload in {}", path.display()))?;
            if let Some(ws_url) = payload.get("ws_url").and_then(Value::as_str) {
                if !ws_url.trim().is_empty() {
                    return Ok(ws_url.to_owned());
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    bail!("hcodex: local websocket bridge did not become ready")
}

fn format_codex_command_line(
    codex_bin: &Path,
    codex_remote_ws_url: &str,
    codex_args: &[OsString],
) -> String {
    let mut parts = Vec::with_capacity(codex_args.len() + 3);
    parts.push(shell_quote(&codex_bin.display().to_string()));
    parts.push("--remote".to_owned());
    parts.push(shell_quote(codex_remote_ws_url));
    for arg in codex_args {
        parts.push(shell_quote(&arg.to_string_lossy()));
    }
    parts.join(" ")
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', r"'\''"))
}

#[derive(Debug, Clone)]
struct BoundThreadLaunchMatch {
    thread_key: String,
    current_codex_thread_id: Option<String>,
}

async fn resolve_hcodex_launch(config: &ResolveHcodexLaunchCli) -> Result<()> {
    let state = read_runtime_state(&config.workspace).await?;
    let hcodex_ws_url = state
        .hcodex_ws_url
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .context("hcodex: app-server state is missing the workspace launch endpoint")?;
    let matches = bound_threads_for_workspace(&config.data_root, &config.workspace).await?;
    let selected = select_bound_thread(matches, config.thread_key.as_deref())?;
    let current_thread = selected
        .current_codex_thread_id
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .context("hcodex: bound Telegram thread is missing current_codex_thread_id")?;
    let ticket = issue_hcodex_launch_ticket(&config.workspace, &selected.thread_key).await?;
    // This URL is for the hcodex ingress handshake, not a codex --remote URL.
    // run-hcodex-session is responsible for bridging it to a local canonical
    // ws://host:port/ endpoint before spawning upstream Codex.
    let launch_ws_url = build_launch_ws_url(hcodex_ws_url, &ticket)?;
    println!(
        "{}\t{}\t{}",
        launch_ws_url, selected.thread_key, current_thread
    );
    Ok(())
}

fn build_launch_ws_url(hcodex_ws_url: &str, ticket: &str) -> Result<String> {
    let mut parsed = Url::parse(hcodex_ws_url)
        .with_context(|| format!("invalid hcodex websocket url: {hcodex_ws_url}"))?;
    // Always materialize "/" before appending the query. Without an explicit
    // root path, some websocket clients normalize ws://host:port?query
    // inconsistently, which can drop launch_ticket before ingress sees it.
    if parsed.path().is_empty() {
        parsed.set_path("/");
    }
    parsed
        .query_pairs_mut()
        .append_pair("launch_ticket", ticket);
    Ok(parsed.to_string())
}

async fn write_ready_file(path: Option<&Path>) -> Result<()> {
    let Some(path) = path else {
        return Ok(());
    };
    fs::write(path, "{\n  \"ready\": true\n}\n")
        .await
        .with_context(|| format!("failed to write {}", path.display()))
}

async fn runtime_state_is_live(workspace: &Path) -> Result<bool> {
    let state_path = runtime_state_path(workspace);
    let exists = fs::try_exists(&state_path)
        .await
        .with_context(|| format!("failed to inspect {}", state_path.display()))?;
    if !exists {
        return Ok(false);
    }
    let state = read_runtime_state(workspace).await?;
    let client_live = tcp_endpoint_is_live(state.client_ws_url()).await;
    let proxy_live = match state.hcodex_ws_url.as_deref() {
        Some(url) => tcp_endpoint_is_live(url).await,
        None => false,
    };
    Ok(client_live && proxy_live)
}

async fn tcp_endpoint_is_live(url: &str) -> bool {
    let Some(socket_addr) = url.strip_prefix("ws://") else {
        return false;
    };
    TcpStream::connect(socket_addr).await.is_ok()
}

async fn read_runtime_state(workspace: &Path) -> Result<WorkspaceRuntimeState> {
    let state_path = runtime_state_path(workspace);
    let contents = fs::read_to_string(&state_path)
        .await
        .with_context(|| format!("failed to read {}", state_path.display()))?;
    serde_json::from_str(&contents)
        .or_else(|_| {
            let payload: Value = serde_json::from_str(&contents)?;
            serde_json::from_value(payload)
        })
        .with_context(|| format!("failed to parse {}", state_path.display()))
}

fn runtime_state_path(workspace: &Path) -> PathBuf {
    workspace
        .join(".threadbridge")
        .join("state")
        .join("app-server")
        .join("current.json")
}

fn canonicalize_lossy(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

async fn bound_threads_for_workspace(
    data_root: &Path,
    workspace: &Path,
) -> Result<Vec<BoundThreadLaunchMatch>> {
    let repository = ThreadRepository::open(data_root).await?;
    let workspace = canonicalize_lossy(workspace);
    let mut matches = Vec::new();
    for record in repository.list_active_threads().await? {
        let Some(binding) = repository.read_session_binding(&record).await? else {
            continue;
        };
        let Some(bound_workspace) = binding.workspace_cwd.as_deref() else {
            continue;
        };
        if canonicalize_lossy(Path::new(bound_workspace)) != workspace {
            continue;
        }
        matches.push(BoundThreadLaunchMatch {
            thread_key: record.metadata.thread_key,
            current_codex_thread_id: binding.current_codex_thread_id,
        });
    }
    matches.sort_by(|left, right| left.thread_key.cmp(&right.thread_key));
    Ok(matches)
}

fn select_bound_thread(
    matches: Vec<BoundThreadLaunchMatch>,
    requested_thread_key: Option<&str>,
) -> Result<BoundThreadLaunchMatch> {
    let filtered = if let Some(requested) = requested_thread_key {
        let item = matches
            .into_iter()
            .find(|item| item.thread_key == requested)
            .with_context(|| {
                format!(
                    "hcodex: no active Telegram thread binding found for --thread-key {requested}"
                )
            })?;
        return Ok(item);
    } else {
        matches
    };

    match filtered.len() {
        0 => bail!("hcodex: no active Telegram thread binding found for this workspace"),
        1 => Ok(filtered.into_iter().next().expect("single match")),
        _ => bail!(
            "hcodex: multiple active Telegram thread bindings use this workspace; pass --thread-key"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::{build_launch_ws_url, maybe_run_from_args};
    use crate::app_server_runtime::WorkspaceRuntimeState;
    use std::ffi::OsString;

    #[tokio::test]
    async fn ignores_other_commands() {
        let ran = maybe_run_from_args(vec![OsString::from("threadbridge")])
            .await
            .unwrap();
        assert!(!ran);
    }

    #[test]
    fn build_launch_ws_url_adds_root_path_before_query() {
        let launch_ws_url =
            build_launch_ws_url("ws://127.0.0.1:61399", "test-ticket").expect("launch url");
        assert_eq!(
            launch_ws_url,
            "ws://127.0.0.1:61399/?launch_ticket=test-ticket"
        );
    }

    #[test]
    fn build_launch_ws_url_preserves_existing_query_pairs() {
        let launch_ws_url = build_launch_ws_url("ws://127.0.0.1:61399/?existing=1", "test-ticket")
            .expect("launch url");
        assert_eq!(
            launch_ws_url,
            "ws://127.0.0.1:61399/?existing=1&launch_ticket=test-ticket"
        );
    }

    #[test]
    fn workspace_runtime_state_prefers_worker_client_url() {
        let state = WorkspaceRuntimeState {
            schema_version: 3,
            workspace_cwd: "/tmp/workspace".to_owned(),
            daemon_ws_url: "ws://127.0.0.1:4100".to_owned(),
            worker_ws_url: Some("ws://127.0.0.1:4101".to_owned()),
            worker_pid: Some(7),
            hcodex_ws_url: Some("ws://127.0.0.1:4102".to_owned()),
        };

        assert_eq!(state.client_ws_url(), "ws://127.0.0.1:4101");
    }
}
