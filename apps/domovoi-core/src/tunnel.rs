//! The iroh tunnel capability. For a remote environment the shell starts a
//! loopback bridge (`TunnelFfi`) to the server's ticket and streams its state
//! back; the core then talks HTTP and WebSocket to `http://127.0.0.1:<port>`.

use crux_core::capability::Operation;
use facet::Facet;
use serde::{Deserialize, Serialize};

#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[repr(C)]
pub enum TunnelOperation {
    /// Start (or restart) the bridge to `ticket`, then stream `TunnelOutput`s
    /// until it stops: `Started` once, then `Status` every few seconds.
    Start { ticket: String },
    /// The device's network changed; re-probe paths now.
    NetworkChanged,
    Stop,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[repr(C)]
pub enum TunnelOutput {
    Started { port: u16 },
    /// `path` is `direct`, `relay` or empty (no live connection).
    /// `listening` false: the loopback port is gone (iOS takes a suspended
    /// app's listening socket); the tunnel must be started again.
    Status { connected: bool, path: String, rtt_ms: u32, error: String, listening: bool },
    Failed { reason: String },
}

impl Operation for TunnelOperation {
    type Output = TunnelOutput;
}
