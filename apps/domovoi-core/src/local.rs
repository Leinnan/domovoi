//! The server on this machine. A desktop shell watches for the descriptor a
//! Domovoi server on the same machine writes (`domovoi_local`), and writes a
//! request file when the core asks to sign in to it: proof that this app
//! runs as the server's user, which the server takes in place of a pairing
//! code. `environments.rs` has what the core does with both.
//!
//! Mobile shells are never asked (`Watch` is requested on the desktop
//! surface only); one asked anyway answers `Unavailable`.

use crux_core::capability::Operation;
use facet::Facet;
use serde::{Deserialize, Serialize};

#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[repr(C)]
pub enum LocalOperation {
    /// Stream `Descriptor`s: what the descriptor file says now, and again at
    /// each change (`None` when there is none, or it is not this user's).
    Watch,
    /// Write a request file beside the descriptor last reported: answered
    /// with `Request`, or `Unavailable` when there is nowhere to write one.
    Request,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[repr(C)]
pub enum LocalOutput {
    /// The descriptor file's text, as it is (the core reads it).
    Descriptor { text: Option<String> },
    /// The request made: its id goes to the server, and an answer counts
    /// only if it carries `secret`.
    Request { id: String, secret: String },
    Unavailable,
}

impl Operation for LocalOperation {
    type Output = LocalOutput;
}

/// The server on this machine, as its descriptor names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalServer {
    /// `http://127.0.0.1:<port>`.
    pub url: String,
    pub port: u16,
    /// The server's own id.
    pub environment_id: String,
    pub label: String,
}

impl LocalServer {
    /// A descriptor's text, read; `None` for anything that is not one.
    pub fn parse(text: &str) -> Option<LocalServer> {
        let descriptor = domovoi_local::Descriptor::parse(text)?;
        let port = descriptor.address()?.port();
        let label = crate::environments::shown_name(&descriptor.label).unwrap_or_else(|| "This computer".to_owned());
        Some(LocalServer { url: descriptor.url, port, environment_id: descriptor.environment_id, label })
    }
}

/// A sign-in to the server on this machine, under way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalAttempt {
    /// The connection attempt it belongs to (`Model::attempt`).
    pub attempt: u64,
    pub environment_id: String,
    /// The server it signs in to.
    pub server: LocalServer,
    /// What the server's answer must carry, once the request file is written.
    pub secret: Option<String>,
    /// The user asked for it (the card): a failure is said, not just left.
    pub asked: bool,
    /// The environment was made for this attempt, and goes with its failure.
    pub made: bool,
}
