//! Serving an axum router to iroh peers.

use crate::{ALPN, EndpointTicket, Error, Network, SecretKey, bind, home_relay, loopback_ticket};
use hyper_util::rt::TokioIo;
use hyper_util::service::TowerToHyperService;
use iroh::endpoint::Connection;
use iroh::{Endpoint, EndpointId};
use serde::Serialize;
use std::time::Duration;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

/// The iroh peer a request came from, in the request's extensions. iroh's
/// TLS handshake proves the peer holds this key. Requests from the TCP
/// listener have no `RemotePeer`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RemotePeer(pub EndpointId);

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerStatus {
    pub endpoint_id: String,
    /// The relay the server is registered with, when connected to one.
    pub relay: Option<String>,
    pub direct_addresses: Vec<String>,
}

pub struct RemoteServer {
    id: EndpointId,
    /// `None` once shut down: the last handle must go for the UDP port to be
    /// free again (a restart on the same port follows).
    endpoint: std::sync::Mutex<Option<Endpoint>>,
    cancel: CancellationToken,
    task: std::sync::Mutex<Option<JoinHandle<()>>>,
}

/// A peer that opens a stream and sends no request is dropped after this.
const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(20);

impl RemoteServer {
    /// Starts accepting iroh connections and serving `router` on each stream.
    pub async fn start(secret: SecretKey, network: Network, router: axum::Router) -> Result<Self, Error> {
        let endpoint = bind(secret, &network, vec![ALPN.to_vec()]).await?;
        let cancel = CancellationToken::new();
        let task = tokio::spawn(accept_loop(endpoint.clone(), router, cancel.clone()));
        Ok(Self { id: endpoint.id(), endpoint: std::sync::Mutex::new(Some(endpoint)), cancel, task: std::sync::Mutex::new(Some(task)) })
    }

    fn endpoint(&self) -> Option<Endpoint> {
        self.endpoint.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()
    }

    pub fn id(&self) -> EndpointId {
        self.id
    }

    /// Waits until the endpoint is registered with a relay (or the timeout).
    pub async fn online(&self, timeout: Duration) -> bool {
        match self.endpoint() {
            Some(endpoint) => tokio::time::timeout(timeout, endpoint.online()).await.is_ok(),
            None => false,
        }
    }

    /// A ticket with the id, the home relay and the known direct addresses.
    pub fn ticket(&self) -> EndpointTicket {
        match self.endpoint() {
            Some(endpoint) => EndpointTicket::new(endpoint.addr()),
            None => EndpointTicket::new(iroh::EndpointAddr::from_parts(self.id, [])),
        }
    }

    /// A ticket naming only loopback addresses (tests, same machine).
    pub fn loopback_ticket(&self) -> EndpointTicket {
        match self.endpoint() {
            Some(endpoint) => loopback_ticket(&endpoint),
            None => self.ticket(),
        }
    }

    pub fn status(&self) -> ServerStatus {
        let endpoint = self.endpoint();
        ServerStatus {
            endpoint_id: self.id.to_string(),
            relay: endpoint.as_ref().and_then(home_relay),
            direct_addresses: endpoint.map(|e| e.addr().ip_addrs().map(ToString::to_string).collect()).unwrap_or_default(),
        }
    }

    /// The UDP port the endpoint bound on IPv4 (to reuse after a restart, so
    /// tickets naming a LAN address keep working).
    pub fn local_port(&self) -> Option<u16> {
        self.endpoint()?.bound_sockets().into_iter().find(std::net::SocketAddr::is_ipv4).map(|a| a.port())
    }

    /// Stops accepting, closes every connection and frees the port. Safe to
    /// call twice.
    pub async fn shutdown(&self) {
        self.cancel.cancel();
        let endpoint = self.endpoint.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
        if let Some(endpoint) = endpoint {
            endpoint.close().await;
        }
        let task = self.task.lock().unwrap_or_else(std::sync::PoisonError::into_inner).take();
        if let Some(task) = task {
            let _ = task.await;
        }
    }
}

async fn accept_loop(endpoint: Endpoint, router: axum::Router, cancel: CancellationToken) {
    loop {
        let incoming = tokio::select! {
            () = cancel.cancelled() => return,
            incoming = endpoint.accept() => match incoming {
                Some(incoming) => incoming,
                None => return,
            },
        };
        let router = router.clone();
        let cancel = cancel.clone();
        tokio::spawn(async move {
            let conn = match incoming.await {
                Ok(conn) => conn,
                Err(e) => {
                    tracing::debug!("iroh: handshake failed: {e}");
                    return;
                }
            };
            if conn.alpn() != ALPN {
                conn.close(0u32.into(), b"unsupported protocol");
                return;
            }
            tokio::select! {
                () = cancel.cancelled() => conn.close(0u32.into(), b"server stopping"),
                () = serve_connection(conn.clone(), router) => {}
            }
        });
    }
}

/// Serves every bi-stream the peer opens as one HTTP/1.1 connection.
async fn serve_connection(conn: Connection, router: axum::Router) {
    let peer = conn.remote_id();
    tracing::info!(peer = %peer.fmt_short(), "iroh: peer connected");
    let router = router.layer(axum::Extension(RemotePeer(peer)));
    while let Ok((send, recv)) = conn.accept_bi().await {
        let service = TowerToHyperService::new(router.clone());
        tokio::spawn(async move {
            let io = TokioIo::new(tokio::io::join(recv, send));
            // QUIC caps open streams per connection (100); the timeout frees
            // the ones that never send a request.
            let served = hyper::server::conn::http1::Builder::new()
                .timer(hyper_util::rt::TokioTimer::new())
                .header_read_timeout(HEADER_READ_TIMEOUT)
                .serve_connection(io, service)
                .with_upgrades()
                .await;
            if let Err(e) = served {
                tracing::debug!("iroh: stream ended: {e}");
            }
        });
    }
    tracing::info!(peer = %peer.fmt_short(), "iroh: peer disconnected");
}
