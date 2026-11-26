//! `/api/preview/tunnel?port=<n>` (t3code `PreviewTunnel.ts`): carries a
//! client's preview traffic to this machine's loopback ports, one TCP
//! connection per WebSocket, as raw bytes (so pages keep their own origin,
//! HMR sockets and TLS). It works over iroh as over TCP: the iroh bridge
//! serves the same router.
//!
//! As t3code's: a WebSocket upgrade with a port 1..=65535 (else 400 `Bad
//! Request`), authenticated like a media request with `terminal:operate`;
//! refused with 403 and `x-domovoi-tunnel-disabled: 1` while `enablePreviewTunnel`
//! is off or a managed policy switches the tunnel off; 403 with
//! `x-domovoi-tunnel-port-refused: 1` for this server's own port (a page would get
//! an unauthenticated loopback client of it); 502 with the same header when
//! nothing accepts on `127.0.0.1` or `[::1]` within 5 s. Only loopback is
//! ever dialled: the request names a port, never a host. The port is dialled
//! before the upgrade, so a dead port fails the handshake. A tunnel closes
//! with its session (revoke, sign-out, stop) and when the tunnel is switched
//! off.
//!
//! Limits: the managed policy is read through `policy_allows_preview_tunnel`,
//! which allows until the managed-policy workstream wires it.

use super::{Peer, authenticate_media};
use crate::state::SharedState;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use domovoi_model::typed::ServerSettings;
use futures::{SinkExt, StreamExt};
use std::collections::HashMap;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::broadcast::error::RecvError;

pub const ROUTE: &str = "/api/preview/tunnel";
/// A refusal about the port itself (nothing listens there, or it is this
/// server's), as opposed to a proxy in front of the server answering 502.
pub const PORT_REFUSED_HEADER: &str = "x-domovoi-tunnel-port-refused";
/// A refusal because the tunnel is switched off here.
pub const DISABLED_HEADER: &str = "x-domovoi-tunnel-disabled";

const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// What one read from the loopback connection carries at most.
const READ_CHUNK: usize = 64 * 1024;

/// `managedPolicy.limits.previewTunnel !== false` (`crate::policy`).
pub fn policy_allows_preview_tunnel() -> bool {
    crate::policy::current().allows_preview_tunnel()
}

/// t3code `allowsPreviewTunnel`: the setting is on (it defaults to on) and no policy switches it off.
pub fn allows_preview_tunnel(settings: &ServerSettings) -> bool {
    settings.enable_preview_tunnel != Some(false) && policy_allows_preview_tunnel()
}

/// The port this server's HTTP listener serves (the one the tunnel refuses).
fn served_port() -> Option<u16> {
    let endpoint = crate::mcp::ENDPOINT.get()?;
    url::Url::parse(endpoint).ok()?.port()
}

fn refused(status: StatusCode, header: &'static str, message: String) -> Response {
    let mut res = (status, message).into_response();
    res.headers_mut().insert(header, HeaderValue::from_static("1"));
    res
}

/// Dials the loopback port: IPv4 first, then IPv6.
async fn dial(port: u16) -> std::io::Result<TcpStream> {
    let attempt = |host: &'static str| async move {
        match tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect((host, port))).await {
            Ok(r) => r,
            Err(_) => Err(std::io::Error::new(std::io::ErrorKind::TimedOut, "connect timed out")),
        }
    };
    match attempt("127.0.0.1").await {
        Ok(s) => Ok(s),
        Err(_) => attempt("::1").await,
    }
}

pub async fn handler(
    State(state): State<SharedState>,
    headers: HeaderMap,
    peer: Peer,
    Query(q): Query<HashMap<String, String>>,
    upgrade: Result<WebSocketUpgrade, axum::extract::ws::rejection::WebSocketUpgradeRejection>,
) -> Response {
    let port = q.get("port").and_then(|p| p.parse::<u32>().ok()).filter(|p| (1..=65_535).contains(p));
    let (Ok(upgrade), Some(port)) = (upgrade, port) else {
        return (StatusCode::BAD_REQUEST, "Bad Request").into_response();
    };
    let port = port as u16;
    let session = match authenticate_media(&state, &headers, &peer, q.get("wsTicket").map(String::as_str), "terminal:operate") {
        Ok(s) => s,
        Err(r) => return r,
    };
    // Subscribed before the setting is read, so a switch-off while dialling is not missed.
    let mut changes = crate::settings::subscribe();
    if !allows_preview_tunnel(&state.settings.read().await.clone()) {
        return refused(StatusCode::FORBIDDEN, DISABLED_HEADER, "The preview tunnel is turned off on this Domovoi server.".into());
    }
    if served_port() == Some(port) {
        return refused(StatusCode::FORBIDDEN, PORT_REFUSED_HEADER, format!("Port {port} is this Domovoi server's own port."));
    }
    let upstream = match dial(port).await {
        Ok(s) => s,
        Err(_) => return refused(StatusCode::BAD_GATEWAY, PORT_REFUSED_HEADER, format!("Nothing accepted a connection on loopback port {port}.")),
    };
    let _ = upstream.set_nodelay(true);
    let state2 = state.clone();
    upgrade.on_upgrade(move |socket| async move {
        let ended = super::session_ended(state.clone(), session.token_hash.clone());
        let turned_off = async move {
            loop {
                match changes.recv().await {
                    Ok(settings) if !allows_preview_tunnel(&settings) => return,
                    Ok(_) => {}
                    Err(RecvError::Lagged(_)) => {
                        if !allows_preview_tunnel(&state2.settings.read().await.clone()) {
                            return;
                        }
                    }
                    // A change stream that ends never switches the tunnel off.
                    Err(RecvError::Closed) => std::future::pending::<()>().await,
                }
            }
        };
        tokio::select! {
            () = pump(socket, upstream) => {}
            () = ended => {}
            () = turned_off => {}
        }
    })
}

/// Copies bytes both ways until either side ends. Each WebSocket write
/// waits for the client to take it, so a slow client holds the loopback
/// reads back instead of queueing a whole response.
async fn pump(socket: WebSocket, upstream: TcpStream) {
    let (mut ws_tx, mut ws_rx) = socket.split();
    let (mut tcp_rx, mut tcp_tx) = upstream.into_split();
    let down = async {
        let mut buf = vec![0u8; READ_CHUNK];
        loop {
            match tcp_rx.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if ws_tx.send(Message::Binary(buf[..n].to_vec().into())).await.is_err() {
                        break;
                    }
                }
            }
        }
        let _ = ws_tx.send(Message::Close(None)).await;
    };
    let up = async {
        while let Some(Ok(message)) = ws_rx.next().await {
            let bytes = match message {
                Message::Binary(b) => b.to_vec(),
                Message::Text(t) => t.as_bytes().to_vec(),
                Message::Close(_) => break,
                Message::Ping(_) | Message::Pong(_) => continue,
            };
            if tcp_tx.write_all(&bytes).await.is_err() {
                break;
            }
        }
        let _ = tcp_tx.shutdown().await;
    };
    tokio::select! {
        () = down => {}
        () = up => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_setting_and_the_policy_both_gate_the_tunnel() {
        let mut settings = ServerSettings::default();
        assert!(allows_preview_tunnel(&settings), "on by default");
        settings.enable_preview_tunnel = Some(false);
        assert!(!allows_preview_tunnel(&settings));
        settings.enable_preview_tunnel = Some(true);
        assert!(allows_preview_tunnel(&settings));
    }
}
