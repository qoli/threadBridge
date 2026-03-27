use std::ffi::OsString;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow, bail};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::process::Command;
use tokio::sync::Mutex;
use tokio_tungstenite::accept_async;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tracing::{debug, warn};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerReadyState {
    pub worker_ws_url: String,
    pub daemon_ws_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerThreadRunState {
    #[serde(rename = "threadId")]
    pub thread_id: String,
    #[serde(rename = "isBusy")]
    pub is_busy: bool,
    #[serde(rename = "activeTurnId")]
    pub active_turn_id: Option<String>,
    pub interruptible: bool,
    pub phase: Option<String>,
}

#[derive(Debug, Default)]
struct WorkerState {
    pending_turn_requests: HashMap<i64, String>,
    turn_to_thread: HashMap<String, String>,
    thread_runs: HashMap<String, WorkerThreadRunState>,
}

pub fn run_from_env() -> Result<()> {
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .context("failed to build worker runtime")?;
    runtime.block_on(run_cli(args))
}

async fn run_cli(args: Vec<OsString>) -> Result<()> {
    let config = WorkerCli::parse(&args)?;
    run_worker(config).await
}

#[derive(Debug)]
struct WorkerCli {
    workspace: PathBuf,
    listen_ws_url: String,
    ready_file: PathBuf,
}

impl WorkerCli {
    fn parse(args: &[OsString]) -> Result<Self> {
        let mut workspace: Option<PathBuf> = None;
        let mut listen_ws_url: Option<String> = None;
        let mut ready_file: Option<PathBuf> = None;
        let mut iter = args.iter();
        while let Some(flag) = iter.next() {
            let flag = flag
                .to_str()
                .ok_or_else(|| anyhow!("worker arguments must be valid utf-8"))?;
            match flag {
                "--workspace" => {
                    let value = iter.next().context("missing value for --workspace")?;
                    workspace = Some(PathBuf::from(value));
                }
                "--listen-ws-url" => {
                    let value = iter
                        .next()
                        .context("missing value for --listen-ws-url")?
                        .to_str()
                        .context("--listen-ws-url must be valid utf-8")?;
                    listen_ws_url = Some(value.to_owned());
                }
                "--ready-file" => {
                    let value = iter.next().context("missing value for --ready-file")?;
                    ready_file = Some(PathBuf::from(value));
                }
                other => bail!("unsupported app_server_ws_worker argument: {other}"),
            }
        }

        Ok(Self {
            workspace: workspace.context("missing required --workspace")?,
            listen_ws_url: listen_ws_url.context("missing required --listen-ws-url")?,
            ready_file: ready_file.context("missing required --ready-file")?,
        })
    }
}

async fn run_worker(config: WorkerCli) -> Result<()> {
    let workspace = config
        .workspace
        .canonicalize()
        .unwrap_or_else(|_| config.workspace.clone());
    let daemon_port = find_free_loopback_port().await?;
    let daemon_ws_url = format!("ws://127.0.0.1:{daemon_port}");
    let listen_addr = socket_addr_from_ws_url(&config.listen_ws_url)?;
    let listener = TcpListener::bind(listen_addr)
        .await
        .with_context(|| format!("failed to bind worker listener on {}", config.listen_ws_url))?;
    let local_addr = listener
        .local_addr()
        .context("failed to read worker listener addr")?;
    let worker_ws_url = format!("ws://127.0.0.1:{}", local_addr.port());

    let mut daemon = Command::new("codex")
        .args(["app-server", "--listen", &daemon_ws_url])
        .current_dir(&workspace)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("failed to spawn worker-owned codex app-server")?;

    if let Some(stderr) = daemon.stderr.take() {
        let mut stderr_lines = BufReader::new(stderr).lines();
        tokio::spawn(async move {
            while let Ok(Some(line)) = stderr_lines.next_line().await {
                debug!(event = "app_server_ws_worker.codex.stderr", line = %line);
            }
        });
    }

    wait_for_daemon(&daemon_ws_url).await?;
    write_ready_file(
        &config.ready_file,
        &WorkerReadyState {
            worker_ws_url,
            daemon_ws_url: daemon_ws_url.clone(),
        },
    )
    .await?;

    let worker_state = Arc::new(Mutex::new(WorkerState::default()));

    loop {
        tokio::select! {
            result = daemon.wait() => {
                let status = result.context("failed waiting for worker-owned codex app-server")?;
                bail!("worker-owned codex app-server exited unexpectedly: {status:?}");
            }
            accept = listener.accept() => {
                let (stream, _) = accept.context("worker listener accept failed")?;
                let upstream_url = daemon_ws_url.clone();
                let worker_state = worker_state.clone();
                tokio::spawn(async move {
                    if let Err(error) = proxy_client_session(stream, &upstream_url, worker_state).await {
                        warn!(event = "app_server_ws_worker.proxy.failed", error = %error);
                    }
                });
            }
        }
    }
}

async fn proxy_client_session(
    stream: TcpStream,
    upstream_url: &str,
    worker_state: Arc<Mutex<WorkerState>>,
) -> Result<()> {
    let client_ws = accept_async(stream)
        .await
        .context("failed to accept worker websocket client")?;
    let (upstream_ws, _) = connect_async(upstream_url)
        .await
        .with_context(|| format!("failed to connect worker upstream to {upstream_url}"))?;

    let (mut client_sink, mut client_stream) = client_ws.split();
    let (mut upstream_sink, mut upstream_stream) = upstream_ws.split();
    loop {
        tokio::select! {
            client_message = client_stream.next() => {
                let Some(client_message) = client_message else {
                    break;
                };
                let message = client_message.context("failed to read worker client websocket message")?;
                if handle_local_request(&message, &worker_state, &mut client_sink).await? {
                    continue;
                }
                track_client_message(&message, &worker_state).await?;
                upstream_sink
                    .send(message)
                    .await
                    .context("failed to forward worker client message upstream")?;
            }
            upstream_message = upstream_stream.next() => {
                let Some(upstream_message) = upstream_message else {
                    break;
                };
                let message = upstream_message.context("failed to read worker upstream websocket message")?;
                track_upstream_message(&message, &worker_state).await?;
                client_sink
                    .send(message)
                    .await
                    .context("failed to forward worker upstream message to client")?;
            }
        }
    }

    let _ = upstream_sink.send(WsMessage::Close(None)).await;
    let _ = client_sink.send(WsMessage::Close(None)).await;
    Ok(())
}

async fn write_ready_file(path: &Path, state: &WorkerReadyState) -> Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    tokio::fs::write(path, format!("{}\n", serde_json::to_string_pretty(state)?))
        .await
        .with_context(|| format!("failed to write {}", path.display()))
}

async fn wait_for_daemon(daemon_ws_url: &str) -> Result<()> {
    for _ in 0..20 {
        if connect_async(daemon_ws_url).await.is_ok() {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    bail!("worker-owned codex app-server did not become healthy at {daemon_ws_url}");
}

async fn find_free_loopback_port() -> Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .context("failed to allocate loopback worker port")?;
    let port = listener
        .local_addr()
        .context("missing worker loopback local addr")?
        .port();
    drop(listener);
    Ok(port)
}

fn socket_addr_from_ws_url(url: &str) -> Result<String> {
    let parsed = url::Url::parse(url).with_context(|| format!("invalid websocket url: {url}"))?;
    if parsed.scheme() != "ws" {
        bail!("worker websocket url must start with ws://");
    }
    if parsed.path() != "/" && !parsed.path().is_empty() {
        bail!("worker websocket url must use root path");
    }
    let host = parsed
        .host_str()
        .context("worker websocket url is missing host")?;
    let port = parsed
        .port()
        .context("worker websocket url is missing port")?;
    Ok(format!("{host}:{port}"))
}

async fn handle_local_request<S>(
    message: &WsMessage,
    worker_state: &Arc<Mutex<WorkerState>>,
    client_sink: &mut futures_util::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<S>,
        WsMessage,
    >,
) -> Result<bool>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let Some(payload) = parse_json_message(message)? else {
        return Ok(false);
    };
    let Some(request_id) = payload.get("id").and_then(Value::as_i64) else {
        return Ok(false);
    };
    let Some(method) = payload.get("method").and_then(Value::as_str) else {
        return Ok(false);
    };
    if method != "threadbridge/getThreadRunState" {
        return Ok(false);
    }
    let thread_id = payload
        .get("params")
        .and_then(|params| params.get("threadId"))
        .and_then(Value::as_str)
        .context("threadbridge/getThreadRunState missing threadId")?;
    let state = worker_state.lock().await;
    let run_state = state
        .thread_runs
        .get(thread_id)
        .cloned()
        .unwrap_or_else(|| WorkerThreadRunState {
            thread_id: thread_id.to_owned(),
            is_busy: false,
            active_turn_id: None,
            interruptible: false,
            phase: Some("idle".to_owned()),
        });
    client_sink
        .send(WsMessage::Text(
            json!({
                "jsonrpc": "2.0",
                "id": request_id,
                "result": serde_json::to_value(run_state)?,
            })
            .to_string()
            .into(),
        ))
        .await
        .context("failed to send worker local response")?;
    Ok(true)
}

async fn track_client_message(message: &WsMessage, worker_state: &Arc<Mutex<WorkerState>>) -> Result<()> {
    let Some(payload) = parse_json_message(message)? else {
        return Ok(());
    };
    let Some(request_id) = payload.get("id").and_then(Value::as_i64) else {
        return Ok(());
    };
    let Some(method) = payload.get("method").and_then(Value::as_str) else {
        return Ok(());
    };
    match method {
        "turn/start" => {
            let Some(thread_id) = payload
                .get("params")
                .and_then(|params| params.get("threadId"))
                .and_then(Value::as_str)
            else {
                return Ok(());
            };
            worker_state
                .lock()
                .await
                .pending_turn_requests
                .insert(request_id, thread_id.to_owned());
        }
        "turn/interrupt" => {
            let Some(thread_id) = payload
                .get("params")
                .and_then(|params| params.get("threadId"))
                .and_then(Value::as_str)
            else {
                return Ok(());
            };
            if let Some(run_state) = worker_state.lock().await.thread_runs.get_mut(thread_id) {
                run_state.interruptible = false;
                run_state.phase = Some("turn_interrupt_requested".to_owned());
            }
        }
        _ => {}
    }
    Ok(())
}

async fn track_upstream_message(message: &WsMessage, worker_state: &Arc<Mutex<WorkerState>>) -> Result<()> {
    let Some(payload) = parse_json_message(message)? else {
        return Ok(());
    };
    if let Some(response_id) = payload.get("id").and_then(Value::as_i64) {
        let turn = payload
            .get("result")
            .and_then(|result| result.get("turn"))
            .cloned();
        if let Some(turn) = turn {
            let mut state = worker_state.lock().await;
            if let Some(thread_id) = state.pending_turn_requests.remove(&response_id)
                && let Some(turn_id) = turn.get("id").and_then(Value::as_str)
            {
                state.turn_to_thread.insert(turn_id.to_owned(), thread_id.clone());
                state.thread_runs.insert(
                    thread_id.clone(),
                    WorkerThreadRunState {
                        thread_id,
                        is_busy: true,
                        active_turn_id: Some(turn_id.to_owned()),
                        interruptible: true,
                        phase: Some("turn_running".to_owned()),
                    },
                );
            }
        }
        return Ok(());
    }
    let Some(method) = payload.get("method").and_then(Value::as_str) else {
        return Ok(());
    };
    match method {
        "turn/started" => {
            let params = payload.get("params");
            let turn_id = params
                .and_then(|params| params.get("turn"))
                .and_then(|turn| turn.get("id"))
                .and_then(Value::as_str);
            let thread_id = params
                .and_then(|params| params.get("threadId"))
                .and_then(Value::as_str);
            let (Some(turn_id), Some(thread_id)) = (turn_id, thread_id) else {
                return Ok(());
            };
            let mut state = worker_state.lock().await;
            state
                .turn_to_thread
                .insert(turn_id.to_owned(), thread_id.to_owned());
            state.thread_runs.insert(
                thread_id.to_owned(),
                WorkerThreadRunState {
                    thread_id: thread_id.to_owned(),
                    is_busy: true,
                    active_turn_id: Some(turn_id.to_owned()),
                    interruptible: true,
                    phase: Some("turn_running".to_owned()),
                },
            );
        }
        "turn/completed" => {
            let turn = payload
                .get("params")
                .and_then(|params| params.get("turn"));
            let turn_id = turn.and_then(|turn| turn.get("id")).and_then(Value::as_str);
            let status = turn
                .and_then(|turn| turn.get("status"))
                .and_then(Value::as_str)
                .unwrap_or("completed");
            let Some(turn_id) = turn_id else {
                return Ok(());
            };
            let mut state = worker_state.lock().await;
            if let Some(thread_id) = state.turn_to_thread.remove(turn_id)
                && let Some(run_state) = state.thread_runs.get_mut(&thread_id)
            {
                run_state.is_busy = false;
                run_state.active_turn_id = None;
                run_state.interruptible = false;
                run_state.phase = Some(match status {
                    "interrupted" => "interrupted",
                    "failed" => "failed",
                    _ => "idle",
                }.to_owned());
            }
        }
        _ => {}
    }
    Ok(())
}

fn parse_json_message(message: &WsMessage) -> Result<Option<Value>> {
    let text = match message {
        WsMessage::Text(text) => text.as_str(),
        WsMessage::Binary(bytes) => std::str::from_utf8(bytes)
            .context("worker websocket binary payload was not valid utf-8")?,
        _ => return Ok(None),
    };
    let payload = match serde_json::from_str(text) {
        Ok(payload) => payload,
        Err(_) => return Ok(None),
    };
    Ok(Some(payload))
}

#[cfg(test)]
mod tests {
    use super::{WorkerState, track_client_message, track_upstream_message};
    use serde_json::json;
    use std::sync::Arc;
    use tokio::sync::Mutex;
    use tokio_tungstenite::tungstenite::Message as WsMessage;

    #[tokio::test]
    async fn track_turn_start_response_marks_thread_busy() {
        let state = Arc::new(Mutex::new(WorkerState::default()));
        track_client_message(
            &WsMessage::Text(
                json!({
                    "jsonrpc": "2.0",
                    "id": 42,
                    "method": "turn/start",
                    "params": {
                        "threadId": "thr_1"
                    }
                })
                .to_string()
                .into(),
            ),
            &state,
        )
        .await
        .unwrap();
        track_upstream_message(
            &WsMessage::Text(
                json!({
                    "jsonrpc": "2.0",
                    "id": 42,
                    "result": {
                        "turn": {
                            "id": "turn_1"
                        }
                    }
                })
                .to_string()
                .into(),
            ),
            &state,
        )
        .await
        .unwrap();

        let state = state.lock().await;
        let run = state.thread_runs.get("thr_1").expect("run state");
        assert!(run.is_busy);
        assert_eq!(run.active_turn_id.as_deref(), Some("turn_1"));
        assert!(run.interruptible);
        assert_eq!(run.phase.as_deref(), Some("turn_running"));
    }

    #[tokio::test]
    async fn turn_completed_clears_thread_busy() {
        let state = Arc::new(Mutex::new(WorkerState::default()));
        {
            let mut locked = state.lock().await;
            locked.turn_to_thread.insert("turn_1".to_owned(), "thr_1".to_owned());
            locked.thread_runs.insert(
                "thr_1".to_owned(),
                super::WorkerThreadRunState {
                    thread_id: "thr_1".to_owned(),
                    is_busy: true,
                    active_turn_id: Some("turn_1".to_owned()),
                    interruptible: true,
                    phase: Some("turn_running".to_owned()),
                },
            );
        }
        track_upstream_message(
            &WsMessage::Text(
                json!({
                    "jsonrpc": "2.0",
                    "method": "turn/completed",
                    "params": {
                        "turn": {
                            "id": "turn_1",
                            "status": "completed"
                        }
                    }
                })
                .to_string()
                .into(),
            ),
            &state,
        )
        .await
        .unwrap();

        let state = state.lock().await;
        let run = state.thread_runs.get("thr_1").expect("run state");
        assert!(!run.is_busy);
        assert_eq!(run.active_turn_id, None);
        assert!(!run.interruptible);
        assert_eq!(run.phase.as_deref(), Some("idle"));
    }

    #[tokio::test]
    async fn interrupted_turn_sets_interrupted_phase() {
        let state = Arc::new(Mutex::new(WorkerState::default()));
        {
            let mut locked = state.lock().await;
            locked.turn_to_thread.insert("turn_1".to_owned(), "thr_1".to_owned());
            locked.thread_runs.insert(
                "thr_1".to_owned(),
                super::WorkerThreadRunState {
                    thread_id: "thr_1".to_owned(),
                    is_busy: true,
                    active_turn_id: Some("turn_1".to_owned()),
                    interruptible: false,
                    phase: Some("turn_interrupt_requested".to_owned()),
                },
            );
        }
        track_upstream_message(
            &WsMessage::Text(
                json!({
                    "jsonrpc": "2.0",
                    "method": "turn/completed",
                    "params": {
                        "turn": {
                            "id": "turn_1",
                            "status": "interrupted"
                        }
                    }
                })
                .to_string()
                .into(),
            ),
            &state,
        )
        .await
        .unwrap();

        let state = state.lock().await;
        let run = state.thread_runs.get("thr_1").expect("run state");
        assert!(!run.is_busy);
        assert_eq!(run.phase.as_deref(), Some("interrupted"));
    }
}
