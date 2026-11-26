//! Domovoi over iroh.
//!
//! A Domovoi server is reachable by its iroh `EndpointId` from anywhere: iroh
//! dials by key, punches through NATs and falls back to an encrypted relay.
//! Each iroh bi-stream carries one HTTP/1.1 connection (with upgrades, so the
//! WebSocket works), served by the same axum router as the TCP listener.
//!
//! Clients run a [`Bridge`]: a loopback TCP listener whose connections are
//! piped into bi-streams on one shared iroh connection. HTTP and WebSocket
//! clients (URLSession, an image loader, a webview) then work unchanged
//! against `http://127.0.0.1:<port>`. The bridge grants nothing by itself;
//! the server authenticates every request.
//!
//! A client that reads its RPC socket in Rust does not take it through
//! HTTP and the loopback port: it opens a second connection ([`RPC_ALPN`])
//! whose streams carry the socket's frames as they are ([`frames`],
//! [`Bridge::rpc`]).

use iroh::endpoint::{Connection, QuicTransportConfig, presets};
use iroh::{Endpoint, RelayMode};
#[cfg(feature = "server")]
use iroh::Watcher;
use serde::Serialize;
use std::time::Duration;

pub use iroh::{EndpointAddr, EndpointId, RelayUrl, SecretKey, TransportAddr};
pub use iroh_tickets::Ticket;
pub use iroh_tickets::endpoint::EndpointTicket;

#[cfg(feature = "bridge")]
mod bridge;
pub mod frames;
#[cfg(feature = "server")]
mod server;

#[cfg(feature = "bridge")]
pub use bridge::{Bridge, BridgeStatus, ClientEndpoint};
#[cfg(feature = "server")]
pub use server::{RemotePeer, RemoteServer, RpcHandler, ServerStatus};

/// The protocol: HTTP/1.1, one connection per bi-stream.
pub const ALPN: &[u8] = b"domovoi/http/1";

/// The RPC socket without HTTP: one socket per bi-stream, as [`frames`].
/// A server that does not serve it refuses the connection, and the client
/// takes its socket through HTTP as before.
pub const RPC_ALPN: &[u8] = b"domovoi/rpc/1";

/// How long a QUIC path may stay silent; keep-alives keep it well under.
const KEEP_ALIVE: Duration = Duration::from_secs(15);

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct Error(String);

impl Error {
    pub(crate) fn msg(context: &str, e: impl std::fmt::Display) -> Self {
        Self(format!("{context}: {e}"))
    }
}

/// Where peers are looked up and which relays carry traffic when no direct
/// path exists.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Network {
    /// n0's address lookup, and relays: n0's public ones when the list is
    /// empty, else the listed (self-hosted) relays.
    Internet { relays: Vec<RelayUrl> },
    /// n0's lookup and relays, but no direct UDP paths: every byte goes
    /// through a relay (debugging, checking relay fallback).
    RelayOnly,
    /// Direct addresses only, no relays and no lookup (tests, one LAN). The
    /// UDP socket binds `port` on all IPv4 interfaces (0 picks one).
    Direct { port: u16 },
}

impl Network {
    /// n0's public relays and lookup.
    pub fn n0() -> Self {
        Self::Internet { relays: Vec::new() }
    }
}

/// Which way the traffic currently flows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PathKind {
    Direct,
    Relay,
}

pub(crate) async fn bind(secret: SecretKey, network: &Network, alpns: Vec<Vec<u8>>) -> Result<Endpoint, Error> {
    let transport = QuicTransportConfig::builder().keep_alive_interval(KEEP_ALIVE).build();
    let builder = match network {
        Network::Internet { relays } if relays.is_empty() => Endpoint::builder(presets::N0),
        Network::Internet { relays } => Endpoint::builder(presets::N0).relay_mode(RelayMode::custom(relays.clone())),
        Network::RelayOnly => Endpoint::builder(presets::N0).clear_ip_transports(),
        Network::Direct { port } => Endpoint::builder(presets::Minimal)
            .relay_mode(RelayMode::Disabled)
            .bind_addr((std::net::Ipv4Addr::UNSPECIFIED, *port))
            .map_err(|e| Error::msg("binding the iroh endpoint", e))?,
    };
    builder
        .secret_key(secret)
        .alpns(alpns)
        .transport_config(transport)
        .bind()
        .await
        .map_err(|e| Error::msg("binding the iroh endpoint", e))
}

/// The selected path of a connection and its round-trip time.
pub(crate) fn current_path(conn: &Connection) -> Option<(PathKind, Duration)> {
    let paths = conn.paths();
    let path = paths.iter().find(|p| p.is_selected())?;
    let kind = if path.is_relay() { PathKind::Relay } else { PathKind::Direct };
    Some((kind, path.rtt()))
}

/// The relay this endpoint is registered with, when connected to one.
#[cfg(feature = "server")]
pub(crate) fn home_relay(endpoint: &Endpoint) -> Option<String> {
    endpoint.home_relay_status().get().into_iter().find(|s| s.is_connected()).map(|s| s.url().to_string())
}

/// A ticket naming only the endpoint's loopback sockets (tests, same machine).
#[cfg(feature = "server")]
pub(crate) fn loopback_ticket(endpoint: &Endpoint) -> EndpointTicket {
    let addrs = endpoint.bound_sockets().into_iter().map(|mut a| {
        a.set_ip(match a {
            std::net::SocketAddr::V4(_) => std::net::Ipv4Addr::LOCALHOST.into(),
            std::net::SocketAddr::V6(_) => std::net::Ipv6Addr::LOCALHOST.into(),
        });
        TransportAddr::Ip(a)
    });
    EndpointTicket::new(EndpointAddr::from_parts(endpoint.id(), addrs))
}

/// Parses a ticket: the `endpoint…` string, or a bare endpoint id (found
/// through address lookup).
pub fn parse_ticket(s: &str) -> Result<EndpointTicket, Error> {
    let s = s.trim();
    if let Ok(t) = s.parse::<EndpointTicket>() {
        return Ok(t);
    }
    let id: EndpointId = s.parse().map_err(|e| Error::msg("not an iroh ticket or endpoint id", e))?;
    Ok(EndpointTicket::new(EndpointAddr::from_parts(id, [])))
}
