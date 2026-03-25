use std::collections::{HashMap, VecDeque};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::{Value, json};
use tokio::fs;
use tokio::net::{TcpListener, TcpStream};
use tokio::time::{Duration, Sleep};
use tokio_tungstenite::accept_async;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Error as WsError;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::tungstenite::error::ProtocolError;
use url::Url;

const LOCAL_RECONNECT_WINDOW: Duration = Duration::from_secs(2);

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

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ReplayRequestKey {
    method: String,
    params_json: String,
}

pub async fn maybe_run_from_args(args: Vec<OsString>) -> Result<bool> {
    let Some(command) = args.first().and_then(|value| value.to_str()) else {
        return Ok(false);
    };
    if command != "hcodex-ws-bridge" {
        return Ok(false);
    }
    let config = BridgeCli::parse(&args[1..])?;
    run_bridge(&config.upstream, &config.ready_file).await?;
    Ok(true)
}

struct BridgeCli {
    upstream: String,
    ready_file: PathBuf,
}

impl BridgeCli {
    fn parse(args: &[OsString]) -> Result<Self> {
        let mut upstream: Option<String> = None;
        let mut ready_file: Option<PathBuf> = None;
        let mut iter = args.iter();
        while let Some(flag) = iter.next() {
            let flag = flag
                .to_str()
                .ok_or_else(|| anyhow!("hcodex-ws-bridge arguments must be valid utf-8"))?;
            match flag {
                "--upstream" => {
                    let value = iter
                        .next()
                        .context("missing value for --upstream")?
                        .to_str()
                        .context("--upstream must be valid utf-8")?;
                    upstream = Some(value.to_owned());
                }
                "--ready-file" => {
                    let value = iter.next().context("missing value for --ready-file")?;
                    ready_file = Some(PathBuf::from(value));
                }
                other => bail!("unsupported hcodex-ws-bridge argument: {other}"),
            }
        }
        Ok(Self {
            upstream: upstream.context("missing required --upstream")?,
            ready_file: ready_file.context("missing required --ready-file")?,
        })
    }
}

// threadBridge has two distinct websocket contracts:
// 1. hcodex ingress launch URLs may carry sideband handshake state such as
//    launch_ticket in the query string.
// 2. upstream Codex --remote only accepts bare ws://host:port endpoints.
//
// This check mirrors upstream Codex exactly. If maintainers loosen it here, it
// becomes easy to incorrectly skip the local bridge and reintroduce the remote
// address regression.
pub(crate) fn is_codex_safe_remote_ws_url(remote_ws_url: &str) -> bool {
    let Ok(parsed) = Url::parse(remote_ws_url) else {
        return false;
    };
    matches!(parsed.scheme(), "ws" | "wss")
        && parsed.host_str().is_some()
        && parsed.port().is_some()
        && parsed.path() == "/"
        && parsed.query().is_none()
        && parsed.fragment().is_none()
}

pub async fn run_bridge(upstream_url: &str, ready_file: &Path) -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .context("failed to bind local hcodex websocket bridge")?;
    let local_addr = listener
        .local_addr()
        .context("failed to determine local hcodex websocket bridge addr")?;
    let local_ws_url = format!("ws://127.0.0.1:{}/", local_addr.port());
    let ready_payload = json!({
        "ws_url": local_ws_url,
    });
    fs::write(
        ready_file,
        format!("{}\n", serde_json::to_string(&ready_payload)?),
    )
    .await
    .with_context(|| format!("failed to write bridge ready file {}", ready_file.display()))?;
    hcodex_debug!(
        "hcodex: bridge local ws {} -> launch {}",
        ready_payload["ws_url"].as_str().unwrap_or_default(),
        upstream_url
    );

    run_bridge_listener(listener, upstream_url).await
}

async fn run_bridge_listener(listener: TcpListener, upstream_launch_ws_url: &str) -> Result<()> {
    // This is the pre-refactor hcodex bridge model: spend the ingress launch
    // ticket exactly once, then keep that upstream websocket alive across a
    // short local reconnect window. The TUI/client side may transiently
    // reconnect during startup; re-dialing the launch URL would consume the
    // one-shot ticket again and reproduce "failed to connect to remote app
    // server".
    let mut local_client = Some(accept_local_client(&listener).await?);
    let (mut upstream_ws, _) = connect_async(upstream_launch_ws_url)
        .await
        .with_context(|| {
            format!("failed to connect bridge to upstream websocket {upstream_launch_ws_url}")
        })?;
    hcodex_debug!(
        "hcodex: bridge connected upstream launch websocket {}",
        upstream_launch_ws_url
    );
    let mut buffered_upstream_messages = VecDeque::new();
    let mut cached_responses: HashMap<ReplayRequestKey, Value> = HashMap::new();
    let mut pending_cacheable_requests: HashMap<String, ReplayRequestKey> = HashMap::new();
    let mut reconnect_deadline: Option<std::pin::Pin<Box<Sleep>>> = None;
    let mut replaying_startup = false;

    loop {
        if let Some(client_ws) = local_client.as_mut() {
            while let Some(upstream_message) = buffered_upstream_messages.pop_front() {
                let is_close = matches!(upstream_message, WsMessage::Close(_));
                send_ws_message(
                    client_ws,
                    upstream_message,
                    is_close,
                    "failed to forward buffered upstream websocket message to local hcodex client",
                )
                .await?;
                if is_close {
                    let _ = futures_util::SinkExt::close(&mut upstream_ws).await;
                    return Ok(());
                }
            }

            enum AttachedAction {
                ClientDetached,
                ClientMessage(WsMessage),
                UpstreamClosed,
                UpstreamMessage(WsMessage),
            }

            let action = tokio::select! {
                client_message = futures_util::StreamExt::next(client_ws) => {
                    match client_message {
                        Some(Ok(WsMessage::Close(_))) | None => AttachedAction::ClientDetached,
                        Some(Ok(client_message)) => AttachedAction::ClientMessage(client_message),
                        Some(Err(error)) if is_graceful_disconnect(&error) => AttachedAction::ClientDetached,
                        Some(Err(error)) => {
                            return Err(error).context("failed to read local hcodex websocket message");
                        }
                    }
                }
                upstream_message = futures_util::StreamExt::next(&mut upstream_ws) => {
                    match upstream_message {
                        Some(Ok(upstream_message)) => AttachedAction::UpstreamMessage(upstream_message),
                        None => AttachedAction::UpstreamClosed,
                        Some(Err(error)) if is_graceful_disconnect(&error) => AttachedAction::UpstreamClosed,
                        Some(Err(error)) => {
                            return Err(error).context("failed to read upstream websocket message");
                        }
                    }
                }
            };

            match action {
                AttachedAction::ClientDetached => {
                    local_client = None;
                    reconnect_deadline = Some(Box::pin(tokio::time::sleep(LOCAL_RECONNECT_WINDOW)));
                    replaying_startup = true;
                }
                AttachedAction::ClientMessage(client_message) => {
                    if replaying_startup {
                        if should_swallow_reconnect_notification(&client_message)? {
                            hcodex_debug!(
                                "hcodex: bridge swallowed reconnect notification {}",
                                describe_ws_message(&client_message)
                            );
                            continue;
                        }
                        if let Some((request_id, request_key)) =
                            replay_request_signature(&client_message)?
                        {
                            if let Some(cached_response) = cached_responses.get(&request_key) {
                                hcodex_debug!(
                                    "hcodex: bridge replayed cached response for reconnect request {}",
                                    request_key.method
                                );
                                send_ws_message(
                                    client_ws,
                                    replay_response_message(cached_response, request_id)?,
                                    false,
                                    "failed to replay cached reconnect response to local hcodex client",
                                )
                                .await?;
                                continue;
                            }
                        }
                        replaying_startup = false;
                    }
                    hcodex_debug!(
                        "hcodex: bridge client frame {}",
                        describe_ws_message(&client_message)
                    );
                    if let Some((request_id, request_key)) =
                        replay_request_signature(&client_message)?
                    {
                        pending_cacheable_requests
                            .insert(request_id_key(&request_id)?, request_key);
                    }
                    send_ws_message(
                        &mut upstream_ws,
                        client_message,
                        false,
                        "failed to forward local hcodex websocket message upstream",
                    )
                    .await?;
                }
                AttachedAction::UpstreamClosed => break,
                AttachedAction::UpstreamMessage(upstream_message) => {
                    cache_upstream_response(
                        &upstream_message,
                        &mut pending_cacheable_requests,
                        &mut cached_responses,
                    )?;
                    hcodex_debug!(
                        "hcodex: bridge upstream frame {}",
                        describe_ws_message(&upstream_message)
                    );
                    let is_close = matches!(upstream_message, WsMessage::Close(_));
                    send_ws_message(
                        client_ws,
                        upstream_message,
                        is_close,
                        "failed to forward upstream websocket message to local hcodex client",
                    )
                    .await?;
                    if is_close {
                        break;
                    }
                }
            }
            continue;
        }

        enum DetachedAction {
            LocalClientAccepted(TcpStream),
            ReconnectTimedOut,
            UpstreamClosed,
            UpstreamMessage(WsMessage),
        }

        let action = {
            let reconnect_deadline = reconnect_deadline
                .as_mut()
                .expect("reconnect deadline exists when no local client is attached");
            tokio::select! {
                accept_result = listener.accept() => {
                    let (stream, remote_addr) = accept_result
                        .context("failed to accept local hcodex websocket client")?;
                    hcodex_debug!(
                        "hcodex: bridge accepted local codex websocket from {}",
                        remote_addr
                    );
                    DetachedAction::LocalClientAccepted(stream)
                }
                _ = reconnect_deadline.as_mut() => DetachedAction::ReconnectTimedOut,
                upstream_message = futures_util::StreamExt::next(&mut upstream_ws) => {
                    match upstream_message {
                        Some(Ok(upstream_message)) => DetachedAction::UpstreamMessage(upstream_message),
                        None => DetachedAction::UpstreamClosed,
                        Some(Err(error)) if is_graceful_disconnect(&error) => DetachedAction::UpstreamClosed,
                        Some(Err(error)) => {
                            return Err(error).context("failed to read upstream websocket message");
                        }
                    }
                }
            }
        };

        match action {
            DetachedAction::LocalClientAccepted(stream) => {
                local_client = Some(
                    accept_async(stream)
                        .await
                        .context("failed to accept local hcodex websocket handshake")?,
                );
                reconnect_deadline = None;
                replaying_startup = true;
            }
            DetachedAction::ReconnectTimedOut | DetachedAction::UpstreamClosed => break,
            DetachedAction::UpstreamMessage(upstream_message) => {
                cache_upstream_response(
                    &upstream_message,
                    &mut pending_cacheable_requests,
                    &mut cached_responses,
                )?;
                hcodex_debug!(
                    "hcodex: bridge upstream frame {}",
                    describe_ws_message(&upstream_message)
                );
                if matches!(upstream_message, WsMessage::Close(_)) {
                    break;
                }
                buffered_upstream_messages.push_back(upstream_message);
            }
        }
    }

    if let Some(client_ws) = local_client.as_mut() {
        let _ = futures_util::SinkExt::close(client_ws).await;
    }
    let _ = futures_util::SinkExt::close(&mut upstream_ws).await;
    hcodex_debug!(
        "hcodex: bridge closed for launch websocket {}",
        upstream_launch_ws_url
    );
    Ok(())
}

async fn accept_local_client(
    listener: &TcpListener,
) -> Result<tokio_tungstenite::WebSocketStream<TcpStream>> {
    let (stream, remote_addr) = listener
        .accept()
        .await
        .context("failed to accept local hcodex websocket client")?;
    hcodex_debug!(
        "hcodex: bridge accepted local codex websocket from {}",
        remote_addr
    );
    accept_async(stream)
        .await
        .context("failed to accept local hcodex websocket handshake")
}

async fn send_ws_message<S>(
    target: &mut tokio_tungstenite::WebSocketStream<S>,
    message: WsMessage,
    is_close: bool,
    context_message: &str,
) -> Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    match futures_util::SinkExt::send(target, message).await {
        Ok(()) => Ok(()),
        Err(error) if is_close && is_graceful_disconnect(&error) => Ok(()),
        Err(error) => Err(error).context(context_message.to_owned()),
    }
}

fn describe_ws_message(message: &WsMessage) -> String {
    match message {
        WsMessage::Text(text) => format!("text {}", summarize_text(text)),
        WsMessage::Binary(bytes) => format!("binary {} bytes", bytes.len()),
        WsMessage::Ping(bytes) => format!("ping {} bytes", bytes.len()),
        WsMessage::Pong(bytes) => format!("pong {} bytes", bytes.len()),
        WsMessage::Close(frame) => {
            let reason = frame
                .as_ref()
                .map(|frame| frame.reason.to_string())
                .filter(|reason| !reason.is_empty())
                .unwrap_or_else(|| "<no-reason>".to_owned());
            format!("close {}", summarize_text(&reason))
        }
        WsMessage::Frame(_) => "frame".to_owned(),
    }
}

fn summarize_text(text: &str) -> String {
    const LIMIT: usize = 280;
    let normalized = text.replace('\n', "\\n").replace('\r', "\\r");
    if normalized.len() <= LIMIT {
        normalized
    } else {
        format!("{}...", &normalized[..LIMIT])
    }
}

fn should_swallow_reconnect_notification(message: &WsMessage) -> Result<bool> {
    let Some(payload) = parse_json_message(message)? else {
        return Ok(false);
    };
    Ok(payload.get("id").is_none()
        && payload.get("method").and_then(Value::as_str) == Some("initialized"))
}

fn replay_request_signature(message: &WsMessage) -> Result<Option<(Value, ReplayRequestKey)>> {
    let Some(payload) = parse_json_message(message)? else {
        return Ok(None);
    };
    let Some(request_id) = payload.get("id").cloned() else {
        return Ok(None);
    };
    let Some(method) = payload.get("method").and_then(Value::as_str) else {
        return Ok(None);
    };
    let params_json = serde_json::to_string(payload.get("params").unwrap_or(&Value::Null))
        .context("failed to serialize reconnect request params")?;
    Ok(Some((
        request_id,
        ReplayRequestKey {
            method: method.to_owned(),
            params_json,
        },
    )))
}

fn cache_upstream_response(
    message: &WsMessage,
    pending_cacheable_requests: &mut HashMap<String, ReplayRequestKey>,
    cached_responses: &mut HashMap<ReplayRequestKey, Value>,
) -> Result<()> {
    let Some(payload) = parse_json_message(message)? else {
        return Ok(());
    };
    let Some(response_id) = payload.get("id").cloned() else {
        return Ok(());
    };
    let Some(request_key) = pending_cacheable_requests.remove(&request_id_key(&response_id)?)
    else {
        return Ok(());
    };
    if payload.get("result").is_some() || payload.get("error").is_some() {
        cached_responses.insert(request_key, payload);
    }
    Ok(())
}

fn replay_response_message(template: &Value, request_id: Value) -> Result<WsMessage> {
    let mut payload = template.clone();
    payload["id"] = request_id;
    Ok(WsMessage::Text(
        serde_json::to_string(&payload).context("failed to serialize replay response")?,
    ))
}

fn request_id_key(id: &Value) -> Result<String> {
    serde_json::to_string(id).context("failed to serialize request id")
}

fn parse_json_message(message: &WsMessage) -> Result<Option<Value>> {
    let text = match message {
        WsMessage::Text(text) => text.as_str(),
        WsMessage::Binary(bytes) => {
            std::str::from_utf8(bytes).context("invalid utf8 websocket frame")?
        }
        _ => return Ok(None),
    };
    let payload: Value = match serde_json::from_str(text) {
        Ok(payload) => payload,
        Err(_) => return Ok(None),
    };
    Ok(Some(payload))
}

fn is_graceful_disconnect(error: &WsError) -> bool {
    matches!(
        error,
        WsError::ConnectionClosed
            | WsError::Protocol(ProtocolError::ResetWithoutClosingHandshake)
            | WsError::Protocol(ProtocolError::SendAfterClosing)
    )
}

#[cfg(test)]
mod tests {
    use super::{
        LOCAL_RECONNECT_WINDOW, is_codex_safe_remote_ws_url, maybe_run_from_args, run_bridge,
    };
    use anyhow::{Context, Result};
    use serde_json::{Value, json};
    use std::collections::HashMap;
    use std::ffi::OsString;
    use std::sync::{Arc, Mutex};
    use std::time::{SystemTime, UNIX_EPOCH};
    use tokio::fs;
    use tokio::net::TcpListener;
    use tokio::time::{Duration, timeout};
    use tokio_tungstenite::accept_async;
    use tokio_tungstenite::accept_hdr_async;
    use tokio_tungstenite::connect_async;
    use tokio_tungstenite::tungstenite::Message as WsMessage;
    use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};

    fn temp_path(name: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!("threadbridge-{name}-{nanos}"))
    }

    async fn wait_for_bridge_ws_url(ready_file: &std::path::Path) -> Result<String> {
        timeout(Duration::from_secs(2), async {
            loop {
                if let Ok(contents) = fs::read_to_string(ready_file).await {
                    let payload: Value = serde_json::from_str(&contents)?;
                    if let Some(ws_url) = payload.get("ws_url").and_then(Value::as_str) {
                        return Result::<String>::Ok(ws_url.to_owned());
                    }
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await?
    }

    async fn expect_json_response<S>(
        client_ws: &mut tokio_tungstenite::WebSocketStream<S>,
    ) -> Result<Value>
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    {
        let message = timeout(
            Duration::from_secs(2),
            futures_util::StreamExt::next(client_ws),
        )
        .await?
        .context("missing websocket response")??;
        let text = message.into_text()?;
        serde_json::from_str(&text).context("failed to decode websocket response json")
    }

    async fn send_json<S>(
        client_ws: &mut tokio_tungstenite::WebSocketStream<S>,
        payload: Value,
    ) -> Result<()>
    where
        S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
    {
        futures_util::SinkExt::send(client_ws, WsMessage::Text(payload.to_string())).await?;
        Ok(())
    }

    #[tokio::test]
    async fn ignores_other_commands() {
        let ran = maybe_run_from_args(vec![OsString::from("threadbridge")])
            .await
            .unwrap();
        assert!(!ran);
    }

    #[tokio::test]
    async fn codex_safe_remote_requires_root_without_query() {
        assert!(is_codex_safe_remote_ws_url("ws://127.0.0.1:4500/"));
        assert!(!is_codex_safe_remote_ws_url(
            "ws://127.0.0.1:4500/?launch_ticket=test"
        ));
        assert!(!is_codex_safe_remote_ws_url(
            "ws://127.0.0.1:4500/thread/test"
        ));
    }

    #[tokio::test]
    async fn bridges_a_single_client_session() -> Result<()> {
        let upstream_listener = TcpListener::bind("127.0.0.1:0").await?;
        let upstream_addr = upstream_listener.local_addr()?;
        let upstream_url = format!("ws://127.0.0.1:{}/", upstream_addr.port());
        let upstream_task = tokio::spawn(async move {
            let (stream, _) = upstream_listener.accept().await?;
            let mut ws = accept_async(stream).await?;
            while let Some(message) = futures_util::StreamExt::next(&mut ws).await {
                let message = message?;
                let is_close = matches!(message, WsMessage::Close(_));
                futures_util::SinkExt::send(&mut ws, message).await?;
                if is_close {
                    break;
                }
            }
            Result::<()>::Ok(())
        });

        let ready_file = temp_path("hcodex-ws-bridge-ready.json");
        let bridge_task = tokio::spawn({
            let ready_file = ready_file.clone();
            let upstream_url = upstream_url.clone();
            async move { run_bridge(&upstream_url, &ready_file).await }
        });

        let bridge_ws_url = wait_for_bridge_ws_url(&ready_file).await?;

        let (mut client_ws, _) = connect_async(&bridge_ws_url).await?;
        futures_util::SinkExt::send(&mut client_ws, WsMessage::Text("ping".into())).await?;
        let echoed = timeout(
            Duration::from_secs(2),
            futures_util::StreamExt::next(&mut client_ws),
        )
        .await?
        .context("missing echoed websocket message")??;
        assert_eq!(echoed.into_text()?, "ping");
        drop(client_ws);

        bridge_task.abort();
        upstream_task.abort();
        let _ = fs::remove_file(&ready_file).await;
        Ok(())
    }

    #[tokio::test]
    async fn bridge_preserves_full_launch_url_for_upstream_handshake() -> Result<()> {
        let upstream_listener = TcpListener::bind("127.0.0.1:0").await?;
        let upstream_addr = upstream_listener.local_addr()?;
        let upstream_url = format!(
            "ws://127.0.0.1:{}/bridge/path?launch_ticket=sideband",
            upstream_addr.port()
        );
        let captured_path =
            std::sync::Arc::new(std::sync::Mutex::new(None::<(String, Option<String>)>));
        let captured_for_task = captured_path.clone();
        let upstream_task = tokio::spawn(async move {
            let (stream, _) = upstream_listener.accept().await?;
            let mut ws = accept_hdr_async(stream, move |request: &Request, response: Response| {
                *captured_for_task.lock().expect("capture path") = Some((
                    request.uri().path().to_owned(),
                    request.uri().query().map(str::to_owned),
                ));
                Ok(response)
            })
            .await?;
            let message = futures_util::StreamExt::next(&mut ws)
                .await
                .context("missing upstream websocket message")??;
            futures_util::SinkExt::send(&mut ws, message).await?;
            Result::<()>::Ok(())
        });

        let ready_file = temp_path("hcodex-ws-bridge-ready-full-url.json");
        let bridge_task = tokio::spawn({
            let ready_file = ready_file.clone();
            let upstream_url = upstream_url.clone();
            async move { run_bridge(&upstream_url, &ready_file).await }
        });
        let bridge_ws_url = wait_for_bridge_ws_url(&ready_file).await?;

        let (mut client_ws, _) = connect_async(&bridge_ws_url).await?;
        futures_util::SinkExt::send(&mut client_ws, WsMessage::Text("ping".into())).await?;
        let echoed = timeout(
            Duration::from_secs(2),
            futures_util::StreamExt::next(&mut client_ws),
        )
        .await?
        .context("missing echoed websocket message")??;
        assert_eq!(echoed.into_text()?, "ping");
        drop(client_ws);

        bridge_task.abort();
        let _ = timeout(Duration::from_secs(2), upstream_task).await??;
        assert_eq!(
            captured_path.lock().expect("read captured path").clone(),
            Some((
                "/bridge/path".to_owned(),
                Some("launch_ticket=sideband".to_owned())
            ))
        );
        let _ = fs::remove_file(&ready_file).await;
        Ok(())
    }

    #[tokio::test]
    async fn replays_startup_requests_across_local_reconnect() -> Result<()> {
        let upstream_listener = TcpListener::bind("127.0.0.1:0").await?;
        let upstream_addr = upstream_listener.local_addr()?;
        let upstream_url = format!(
            "ws://127.0.0.1:{}/?launch_ticket=reconnect-test",
            upstream_addr.port()
        );
        let seen_methods = Arc::new(Mutex::new(HashMap::<String, usize>::new()));
        let seen_methods_for_task = seen_methods.clone();
        let upstream_task = tokio::spawn(async move {
            let (stream, _) = upstream_listener.accept().await?;
            let mut ws = accept_async(stream).await?;
            while let Some(message) = futures_util::StreamExt::next(&mut ws).await {
                let message = message?;
                let text = match message {
                    WsMessage::Text(text) => text.to_string(),
                    WsMessage::Binary(bytes) => String::from_utf8(bytes.to_vec())
                        .context("upstream saw invalid utf8 websocket frame")?,
                    WsMessage::Close(_) => break,
                    _ => continue,
                };
                let payload: Value = serde_json::from_str(&text)?;
                if let Some(method) = payload.get("method").and_then(Value::as_str) {
                    *seen_methods_for_task
                        .lock()
                        .expect("track seen methods")
                        .entry(method.to_owned())
                        .or_insert(0) += 1;
                    let Some(request_id) = payload.get("id").cloned() else {
                        continue;
                    };
                    let response = match method {
                        "initialize" => json!({
                            "id": request_id,
                            "result": {"serverInfo": {"name": "test-upstream"}}
                        }),
                        "account/read" => json!({
                            "id": request_id,
                            "result": {"account": "ok"}
                        }),
                        "model/list" => json!({
                            "id": request_id,
                            "result": {"models": ["gpt-test"]}
                        }),
                        "account/rateLimits/read" => json!({
                            "id": request_id,
                            "result": {"limits": []}
                        }),
                        "thread/start" => json!({
                            "id": request_id,
                            "result": {"thread_id": "thread-1"}
                        }),
                        other => json!({
                            "id": request_id,
                            "error": {"code": -32601, "message": format!("unexpected method {other}")}
                        }),
                    };
                    futures_util::SinkExt::send(&mut ws, WsMessage::Text(response.to_string()))
                        .await?;
                }
            }
            Result::<HashMap<String, usize>>::Ok(
                seen_methods_for_task
                    .lock()
                    .expect("final seen methods")
                    .clone(),
            )
        });

        let ready_file = temp_path("hcodex-ws-bridge-ready-reconnect.json");
        let bridge_task = tokio::spawn({
            let ready_file = ready_file.clone();
            let upstream_url = upstream_url.clone();
            async move { run_bridge(&upstream_url, &ready_file).await }
        });
        let bridge_ws_url = wait_for_bridge_ws_url(&ready_file).await?;

        let (mut first_client, _) = connect_async(&bridge_ws_url).await?;
        send_json(&mut first_client, json!({"id": 1, "method": "initialize"})).await?;
        assert_eq!(
            expect_json_response(&mut first_client).await?,
            json!({"id": 1, "result": {"serverInfo": {"name": "test-upstream"}}})
        );
        send_json(&mut first_client, json!({"method": "initialized"})).await?;
        send_json(
            &mut first_client,
            json!({"id": 2, "method": "account/read"}),
        )
        .await?;
        assert_eq!(
            expect_json_response(&mut first_client).await?,
            json!({"id": 2, "result": {"account": "ok"}})
        );
        send_json(&mut first_client, json!({"id": 3, "method": "model/list"})).await?;
        assert_eq!(
            expect_json_response(&mut first_client).await?,
            json!({"id": 3, "result": {"models": ["gpt-test"]}})
        );
        send_json(
            &mut first_client,
            json!({"id": 4, "method": "account/rateLimits/read"}),
        )
        .await?;
        assert_eq!(
            expect_json_response(&mut first_client).await?,
            json!({"id": 4, "result": {"limits": []}})
        );
        drop(first_client);

        tokio::time::sleep(Duration::from_millis(100)).await;

        let (mut second_client, _) = connect_async(&bridge_ws_url).await?;
        send_json(
            &mut second_client,
            json!({"id": "init-2", "method": "initialize"}),
        )
        .await?;
        assert_eq!(
            expect_json_response(&mut second_client).await?,
            json!({"id": "init-2", "result": {"serverInfo": {"name": "test-upstream"}}})
        );
        send_json(&mut second_client, json!({"method": "initialized"})).await?;
        send_json(
            &mut second_client,
            json!({"id": "acct-2", "method": "account/read"}),
        )
        .await?;
        assert_eq!(
            expect_json_response(&mut second_client).await?,
            json!({"id": "acct-2", "result": {"account": "ok"}})
        );
        send_json(
            &mut second_client,
            json!({"id": "model-2", "method": "model/list"}),
        )
        .await?;
        assert_eq!(
            expect_json_response(&mut second_client).await?,
            json!({"id": "model-2", "result": {"models": ["gpt-test"]}})
        );
        send_json(
            &mut second_client,
            json!({"id": "limits-2", "method": "account/rateLimits/read"}),
        )
        .await?;
        assert_eq!(
            expect_json_response(&mut second_client).await?,
            json!({"id": "limits-2", "result": {"limits": []}})
        );
        send_json(
            &mut second_client,
            json!({"id": 5, "method": "thread/start"}),
        )
        .await?;
        assert_eq!(
            expect_json_response(&mut second_client).await?,
            json!({"id": 5, "result": {"thread_id": "thread-1"}})
        );
        drop(second_client);

        let _ = timeout(Duration::from_secs(4), bridge_task).await??;
        let seen_methods = timeout(
            LOCAL_RECONNECT_WINDOW + Duration::from_secs(2),
            upstream_task,
        )
        .await???;
        assert_eq!(seen_methods.get("initialize"), Some(&1));
        assert_eq!(seen_methods.get("initialized"), Some(&1));
        assert_eq!(seen_methods.get("account/read"), Some(&1));
        assert_eq!(seen_methods.get("model/list"), Some(&1));
        assert_eq!(seen_methods.get("account/rateLimits/read"), Some(&1));
        assert_eq!(seen_methods.get("thread/start"), Some(&1));
        let _ = fs::remove_file(&ready_file).await;
        Ok(())
    }
}
