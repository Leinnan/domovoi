//! WebSocket capability. The shell opens the socket and streams
//! [`SocketEvent`]s back for the lifetime of the connection; sends are
//! fire-and-forget notifications.

use crux_core::capability::Operation;
use facet::Facet;
use serde::{Deserialize, Serialize};

#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[repr(C)]
pub enum SocketOperation {
    /// Open a WebSocket to `url` (which carries a one-time ticket) and
    /// resolve with events until it closes.
    Open { url: String },
    /// Send one text frame on the open socket.
    Send { text: String },
    Close,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[repr(C)]
pub enum SocketEvent {
    Opened,
    Message { text: String },
    Closed { reason: String },
}

impl Operation for SocketOperation {
    type Output = SocketEvent;
}
