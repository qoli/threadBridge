use std::ffi::OsString;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use serde_json::json;
use tokio::fs;
use tokio::net::TcpListener;
use tokio::time::{Duration, timeout};
use tokio_tungstenite::accept_async;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Error as WsError;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::tungstenite::error::ProtocolError;

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
                    let value = iter
                        .next()
                        .context("missing value for --ready-file")?;
                    ready_file = Some(PathBuf::from(value));
                }
                other => bail!("unsupported hcodex-ws-bridge argument: {other}"),
            }
        }
        let upstream = upstream.context("missing required --upstream")?;
        let ready_file = ready_file.context("missing required --ready-file")?;
        Ok(Self {
            upstream,
            ready_file,
        })
    }
}

pub async fn run_bridge(upstream_url: &str, ready_file: &Path) -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .context("failed to bind local hcodex websocket bridge")?;
    let local_addr = listener
        .local_addr()
        .context("failed to determine local hcodex websocket bridge addr")?;
    let ready_payload = json!({
        "ws_url": format!("ws://127.0.0.1:{}", local_addr.port()),
    });
    fs::write(
        ready_file,
        format!("{}\n", serde_json::to_string(&ready_payload)?),
    )
    .await
    .with_context(|| format!("failed to write bridge ready file {}", ready_file.display()))?;

    let mut accepted_any_client = false;
    loop {
        let accept_result = if accepted_any_client {
            match timeout(Duration::from_secs(2), listener.accept()).await {
                Ok(result) => result.context("failed to accept local hcodex websocket client")?,
                Err(_) => break,
            }
        } else {
            listener
                .accept()
                .await
                .context("failed to accept local hcodex websocket client")?
        };
        accepted_any_client = true;
        bridge_single_client(accept_result.0, upstream_url).await?;
    }

    Ok(())
}

async fn bridge_single_client(
    stream: tokio::net::TcpStream,
    upstream_url: &str,
) -> Result<()> {
    let client_ws = accept_async(stream)
        .await
        .context("failed to accept local hcodex websocket handshake")?;
    let (upstream_ws, _) = connect_async(upstream_url)
        .await
        .with_context(|| format!("failed to connect bridge to upstream websocket {upstream_url}"))?;
    let (mut client_write, mut client_read) = futures_util::StreamExt::split(client_ws);
    let (mut upstream_write, mut upstream_read) = futures_util::StreamExt::split(upstream_ws);

    let client_to_upstream = async {
        while let Some(client_message) = futures_util::StreamExt::next(&mut client_read).await {
            let client_message = match client_message {
                Ok(client_message) => client_message,
                Err(error) if is_graceful_disconnect(&error) => break,
                Err(error) => return Err(error).context("failed to read local hcodex websocket message"),
            };
            let is_close = matches!(client_message, WsMessage::Close(_));
            match futures_util::SinkExt::send(&mut upstream_write, client_message).await {
                Ok(()) => {}
                Err(error) if is_close && is_graceful_disconnect(&error) => break,
                Err(error) => {
                    return Err(error)
                        .context("failed to forward local hcodex websocket message upstream");
                }
            }
            if is_close {
                break;
            }
        }
        let _ = futures_util::SinkExt::close(&mut upstream_write).await;
        Result::<()>::Ok(())
    };

    let upstream_to_client = async {
        while let Some(upstream_message) = futures_util::StreamExt::next(&mut upstream_read).await {
            let upstream_message = match upstream_message {
                Ok(upstream_message) => upstream_message,
                Err(error) if is_graceful_disconnect(&error) => break,
                Err(error) => return Err(error).context("failed to read upstream websocket message"),
            };
            let is_close = matches!(upstream_message, WsMessage::Close(_));
            match futures_util::SinkExt::send(&mut client_write, upstream_message).await {
                Ok(()) => {}
                Err(error) if is_close && is_graceful_disconnect(&error) => break,
                Err(error) => {
                    return Err(error)
                        .context("failed to forward upstream websocket message to local hcodex client");
                }
            }
            if is_close {
                break;
            }
        }
        let _ = futures_util::SinkExt::close(&mut client_write).await;
        Result::<()>::Ok(())
    };

    tokio::try_join!(client_to_upstream, upstream_to_client)?;

    Ok(())
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
    use super::{is_graceful_disconnect, maybe_run_from_args, run_bridge};
    use anyhow::{Context, Result};
    use serde_json::Value;
    use std::ffi::OsString;
    use std::time::{SystemTime, UNIX_EPOCH};
    use tokio::fs;
    use tokio::net::TcpListener;
    use tokio::time::{Duration, timeout};
    use tokio_tungstenite::accept_async;
    use tokio_tungstenite::connect_async;
    use tokio_tungstenite::tungstenite::Message as WsMessage;

    fn temp_path(name: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        std::env::temp_dir().join(format!("threadbridge-{name}-{nanos}"))
    }

    #[tokio::test]
    async fn ignores_other_commands() {
        let ran = maybe_run_from_args(vec![OsString::from("threadbridge")])
            .await
            .unwrap();
        assert!(!ran);
    }

    #[tokio::test]
    async fn bridges_a_single_client_session() -> Result<()> {
        let upstream_listener = TcpListener::bind("127.0.0.1:0").await?;
        let upstream_addr = upstream_listener.local_addr()?;
        let upstream_url = format!("ws://127.0.0.1:{}", upstream_addr.port());
        let upstream_task = tokio::spawn(async move {
            let (stream, _) = upstream_listener.accept().await?;
            let mut ws = accept_async(stream).await?;
            while let Some(message) = futures_util::StreamExt::next(&mut ws).await {
                let message = match message {
                    Ok(message) => message,
                    Err(error) if is_graceful_disconnect(&error) => break,
                    Err(error) => return Err(error.into()),
                };
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

        let bridge_ws_url = timeout(Duration::from_secs(2), async {
            loop {
                if let Ok(contents) = fs::read_to_string(&ready_file).await {
                    let payload: Value = serde_json::from_str(&contents)?;
                    if let Some(ws_url) = payload.get("ws_url").and_then(Value::as_str) {
                        return Result::<String>::Ok(ws_url.to_owned());
                    }
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await??;

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
}
