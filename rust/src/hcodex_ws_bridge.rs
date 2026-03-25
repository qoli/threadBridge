use std::collections::VecDeque;

use anyhow::{Context, Result};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::AbortHandle;
use tokio::time::{Duration, Sleep};
use tokio_tungstenite::accept_async;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Error as WsError;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::tungstenite::error::ProtocolError;
use tracing::warn;
use url::Url;

pub(crate) struct PreparedCodexRemote {
    pub(crate) codex_remote_ws_url: String,
    pub(crate) bridge_abort_handle: Option<AbortHandle>,
}

const LOCAL_RECONNECT_WINDOW: Duration = Duration::from_secs(2);

// threadBridge has two distinct websocket contracts:
// 1. hcodex ingress launch URLs may carry sideband handshake state such as
//    launch_ticket in the query string.
// 2. upstream Codex --remote only accepts bare ws://host:port endpoints.
//
// This adapter is the compatibility boundary between those contracts. Keep the
// bridge even if launch URL generation changes elsewhere; otherwise it is easy
// to reintroduce the regression where a ticketed ingress URL is passed straight
// into codex --remote and rejected before the handshake starts.
pub(crate) async fn prepare_codex_remote_ws_url(
    launch_ws_url: &str,
) -> Result<PreparedCodexRemote> {
    if is_codex_safe_remote_ws_url(launch_ws_url) {
        return Ok(PreparedCodexRemote {
            codex_remote_ws_url: launch_ws_url.to_owned(),
            bridge_abort_handle: None,
        });
    }

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .context("failed to bind local hcodex websocket bridge")?;
    let local_addr = listener
        .local_addr()
        .context("failed to determine local hcodex websocket bridge addr")?;
    let local_bridge_ws_url = format!("ws://127.0.0.1:{}", local_addr.port());
    let launch_ws_url = launch_ws_url.to_owned();
    let bridge_task = tokio::spawn(async move {
        if let Err(error) = run_bridge(listener, &launch_ws_url).await {
            warn!(
                event = "hcodex_ws_bridge.failed",
                upstream_launch_ws_url = %launch_ws_url,
                error = %error,
                "hcodex websocket bridge failed"
            );
        }
    });

    Ok(PreparedCodexRemote {
        codex_remote_ws_url: local_bridge_ws_url,
        bridge_abort_handle: Some(bridge_task.abort_handle()),
    })
}

fn is_codex_safe_remote_ws_url(remote_ws_url: &str) -> bool {
    // Mirror upstream Codex's remote-address contract rather than "loosely"
    // accepting whatever Url::parse can decode. If this diverges, maintainers
    // may incorrectly conclude that the bridge is dead code and remove it.
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

async fn run_bridge(listener: TcpListener, upstream_launch_ws_url: &str) -> Result<()> {
    // The local listener intentionally presents a canonical ws://127.0.0.1:port
    // endpoint to Codex, while preserving the original ingress launch URL when
    // dialing upstream so launch_ticket and any future sideband data survive.
    //
    // Important: launch_ticket is single-use at ingress. This bridge must spend
    // that ticket exactly once, then keep the upstream websocket session alive
    // across any short-lived local Codex reconnects. Re-dialing the same
    // upstream_launch_ws_url after the first successful handshake reintroduces
    // the old "failed to connect to remote app server" regression.
    let mut local_client = Some(accept_local_client(&listener).await?);
    let (mut upstream_ws, _) = connect_async(upstream_launch_ws_url)
        .await
        .with_context(|| {
            format!("failed to connect bridge to upstream websocket {upstream_launch_ws_url}")
        })?;
    let mut buffered_upstream_messages = VecDeque::new();
    let mut reconnect_deadline: Option<std::pin::Pin<Box<Sleep>>> = None;

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
                    // A local client disconnect does not mean the upstream
                    // ingress session should close. Hold it open briefly so a
                    // reconnect can reattach without spending launch_ticket
                    // again.
                    local_client = None;
                    reconnect_deadline =
                        Some(Box::pin(tokio::time::sleep(LOCAL_RECONNECT_WINDOW)));
                }
                AttachedAction::ClientMessage(client_message) => {
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
                    let (stream, _) = accept_result
                        .context("failed to accept local hcodex websocket client")?;
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
            }
            DetachedAction::ReconnectTimedOut | DetachedAction::UpstreamClosed => break,
            DetachedAction::UpstreamMessage(upstream_message) => {
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
    Ok(())
}

async fn accept_local_client(listener: &TcpListener) -> Result<tokio_tungstenite::WebSocketStream<TcpStream>> {
    let (stream, _) = listener
        .accept()
        .await
        .context("failed to accept local hcodex websocket client")?;
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
    use super::{prepare_codex_remote_ws_url, run_bridge};
    use anyhow::{Context, Result};
    use std::sync::{Arc, Mutex as StdMutex};
    use tokio::net::TcpListener;
    use tokio::time::{Duration, timeout};
    use tokio_tungstenite::accept_hdr_async;
    use tokio_tungstenite::connect_async;
    use tokio_tungstenite::tungstenite::Message as WsMessage;
    use tokio_tungstenite::tungstenite::handshake::server::{Request, Response};

    #[tokio::test]
    async fn bare_remote_ws_url_passes_through_without_bridge() -> Result<()> {
        let prepared = prepare_codex_remote_ws_url("ws://127.0.0.1:4500").await?;
        assert_eq!(prepared.codex_remote_ws_url, "ws://127.0.0.1:4500");
        assert!(prepared.bridge_abort_handle.is_none());
        Ok(())
    }

    #[tokio::test]
    async fn launch_url_with_query_is_bridged_to_local_canonical_ws_url() -> Result<()> {
        let upstream_listener = TcpListener::bind("127.0.0.1:0").await?;
        let upstream_addr = upstream_listener.local_addr()?;
        let upstream_launch_ws_url = format!(
            "ws://127.0.0.1:{}/?launch_ticket=test-ticket",
            upstream_addr.port()
        );
        let captured_query = Arc::new(StdMutex::new(None::<Option<String>>));
        let captured_query_for_task = captured_query.clone();
        let upstream_task = tokio::spawn(async move {
            let (stream, _) = upstream_listener.accept().await?;
            let mut ws = accept_hdr_async(stream, move |request: &Request, response: Response| {
                *captured_query_for_task.lock().expect("capture query") =
                    Some(request.uri().query().map(str::to_owned));
                Ok(response)
            })
            .await?;
            futures_util::SinkExt::send(&mut ws, WsMessage::Close(None)).await?;
            Result::<()>::Ok(())
        });

        let prepared = prepare_codex_remote_ws_url(&upstream_launch_ws_url).await?;
        assert_ne!(prepared.codex_remote_ws_url, upstream_launch_ws_url);
        assert!(
            prepared.codex_remote_ws_url.starts_with("ws://127.0.0.1:"),
            "bridge should expose a local canonical ws://host:port URL"
        );
        assert!(prepared.bridge_abort_handle.is_some());

        let (mut client_ws, _) = connect_async(&prepared.codex_remote_ws_url).await?;
        let _ = futures_util::StreamExt::next(&mut client_ws).await;
        drop(client_ws);

        let _ = timeout(Duration::from_secs(2), upstream_task).await??;
        assert_eq!(
            *captured_query.lock().expect("read captured query"),
            Some(Some("launch_ticket=test-ticket".to_owned()))
        );
        if let Some(abort_handle) = prepared.bridge_abort_handle.as_ref() {
            abort_handle.abort();
        }
        Ok(())
    }

    #[tokio::test]
    async fn bridge_reuses_full_launch_url_for_upstream_handshake() -> Result<()> {
        let upstream_listener = TcpListener::bind("127.0.0.1:0").await?;
        let upstream_addr = upstream_listener.local_addr()?;
        let upstream_launch_ws_url = format!(
            "ws://127.0.0.1:{}/bridge/path?launch_ticket=sideband",
            upstream_addr.port()
        );
        let captured_path = Arc::new(StdMutex::new(None::<(String, Option<String>)>));
        let captured_path_for_task = captured_path.clone();
        let upstream_task = tokio::spawn(async move {
            let (stream, _) = upstream_listener.accept().await?;
            let mut ws = accept_hdr_async(stream, move |request: &Request, response: Response| {
                *captured_path_for_task.lock().expect("capture path") = Some((
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

        let bridge_listener = TcpListener::bind("127.0.0.1:0").await?;
        let bridge_addr = bridge_listener.local_addr()?;
        let bridge_task = tokio::spawn({
            let upstream_launch_ws_url = upstream_launch_ws_url.clone();
            async move { run_bridge(bridge_listener, &upstream_launch_ws_url).await }
        });

        let local_bridge_ws_url = format!("ws://127.0.0.1:{}", bridge_addr.port());
        let (mut client_ws, _) = connect_async(&local_bridge_ws_url).await?;
        futures_util::SinkExt::send(&mut client_ws, WsMessage::Text("ping".into())).await?;
        let echoed = timeout(
            Duration::from_secs(2),
            futures_util::StreamExt::next(&mut client_ws),
        )
        .await?
        .context("missing echoed websocket message")??;
        assert_eq!(echoed.into_text()?, "ping");
        drop(client_ws);

        let _ = timeout(Duration::from_secs(2), upstream_task).await??;
        let captured = captured_path.lock().expect("read captured path").clone();
        assert_eq!(
            captured,
            Some((
                "/bridge/path".to_owned(),
                Some("launch_ticket=sideband".to_owned())
            ))
        );
        bridge_task.abort();
        Ok(())
    }

    #[tokio::test]
    async fn local_reconnect_reuses_existing_upstream_session() -> Result<()> {
        let upstream_listener = TcpListener::bind("127.0.0.1:0").await?;
        let upstream_addr = upstream_listener.local_addr()?;
        let upstream_launch_ws_url = format!(
            "ws://127.0.0.1:{}/?launch_ticket=reconnect-ticket",
            upstream_addr.port()
        );
        let handshake_count = Arc::new(StdMutex::new(0usize));
        let handshake_count_for_task = handshake_count.clone();
        let upstream_task = tokio::spawn(async move {
            let (stream, _) = upstream_listener.accept().await?;
            let mut ws = accept_hdr_async(stream, move |_request: &Request, response: Response| {
                *handshake_count_for_task.lock().expect("count handshake") += 1;
                Ok(response)
            })
            .await?;

            let first_message = timeout(Duration::from_secs(2), futures_util::StreamExt::next(&mut ws))
                .await?
                .context("missing first upstream message")??;
            assert_eq!(first_message.into_text()?, "first");

            let second_message = timeout(Duration::from_secs(2), futures_util::StreamExt::next(&mut ws))
                .await?
                .context("missing second upstream message")??;
            assert_eq!(second_message.into_text()?, "second");

            futures_util::SinkExt::send(&mut ws, WsMessage::Text("buffered".into())).await?;
            let _ = timeout(Duration::from_secs(2), futures_util::StreamExt::next(&mut ws)).await;
            Result::<()>::Ok(())
        });

        let bridge_listener = TcpListener::bind("127.0.0.1:0").await?;
        let bridge_addr = bridge_listener.local_addr()?;
        let bridge_task = tokio::spawn({
            let upstream_launch_ws_url = upstream_launch_ws_url.clone();
            async move { run_bridge(bridge_listener, &upstream_launch_ws_url).await }
        });

        let local_bridge_ws_url = format!("ws://127.0.0.1:{}", bridge_addr.port());
        let (mut first_client_ws, _) = connect_async(&local_bridge_ws_url).await?;
        futures_util::SinkExt::send(&mut first_client_ws, WsMessage::Text("first".into())).await?;
        drop(first_client_ws);

        let (mut second_client_ws, _) = connect_async(&local_bridge_ws_url).await?;
        futures_util::SinkExt::send(&mut second_client_ws, WsMessage::Text("second".into())).await?;
        let buffered = timeout(
            Duration::from_secs(2),
            futures_util::StreamExt::next(&mut second_client_ws),
        )
        .await?
        .context("missing buffered upstream message after reconnect")??;
        assert_eq!(buffered.into_text()?, "buffered");
        drop(second_client_ws);

        bridge_task.abort();
        let _ = timeout(Duration::from_secs(2), upstream_task).await??;
        assert_eq!(*handshake_count.lock().expect("read handshake count"), 1);
        Ok(())
    }
}
