use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde_json::Value;
use tokio::fs;
use tokio::net::TcpStream;
use tokio::process::Command;

use crate::app_server_runtime::WorkspaceRuntimeState;
use crate::repository::ThreadRepository;
use crate::workspace_status::{record_hcodex_launcher_ended, record_hcodex_launcher_started};

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
    remote_ws_url: String,
    codex_args: Vec<OsString>,
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
        let mut remote_ws_url: Option<String> = None;
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
                    remote_ws_url = Some(value.to_owned());
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
            remote_ws_url: remote_ws_url.context("missing required --remote-ws-url")?,
            codex_args,
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
            while process_is_alive(parent_pid) {
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
    let mut command = Command::new(&config.codex_bin);
    command
        .current_dir(&config.workspace)
        .arg("--remote")
        .arg(&config.remote_ws_url)
        .args(&config.codex_args)
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::inherit())
        .stderr(std::process::Stdio::inherit());
    let mut child = command
        .spawn()
        .with_context(|| format!("failed to spawn {}", config.codex_bin.display()))?;
    let child_pid = child.id().context("spawned codex child is missing pid")?;
    let shell_pid = std::process::id();
    let child_command = format!(
        "{} --remote {}",
        config.codex_bin.display(),
        config.remote_ws_url
    );
    record_hcodex_launcher_started(
        &config.workspace,
        &config.thread_key,
        shell_pid,
        child_pid,
        &child_command,
    )
    .await?;

    let status = child
        .wait()
        .await
        .context("failed waiting for codex child")?;
    record_hcodex_launcher_ended(&config.workspace, &config.thread_key, shell_pid, child_pid)
        .await?;

    let repository = ThreadRepository::open(&config.data_root).await?;
    let _ = repository
        .mark_tui_adoption_pending_for_thread_key(&config.thread_key)
        .await?;

    std::process::exit(status.code().unwrap_or(1));
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
    let state_path = workspace
        .join(".threadbridge")
        .join("state")
        .join("app-server")
        .join("current.json");
    let contents = match fs::read_to_string(&state_path).await {
        Ok(contents) => contents,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", state_path.display()));
        }
    };
    let state: WorkspaceRuntimeState = serde_json::from_str(&contents)
        .or_else(|_| {
            let payload: Value = serde_json::from_str(&contents)?;
            serde_json::from_value(payload)
        })
        .with_context(|| format!("failed to parse {}", state_path.display()))?;
    let daemon_live = tcp_endpoint_is_live(&state.daemon_ws_url).await;
    let proxy_live = match state.tui_proxy_base_ws_url.as_deref() {
        Some(url) => tcp_endpoint_is_live(url).await,
        None => false,
    };
    Ok(daemon_live && proxy_live)
}

async fn tcp_endpoint_is_live(url: &str) -> bool {
    let Some(socket_addr) = url.strip_prefix("ws://") else {
        return false;
    };
    TcpStream::connect(socket_addr).await.is_ok()
}

fn process_is_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::maybe_run_from_args;
    use std::ffi::OsString;

    #[tokio::test]
    async fn ignores_other_commands() {
        let ran = maybe_run_from_args(vec![OsString::from("threadbridge")])
            .await
            .unwrap();
        assert!(!ran);
    }
}
