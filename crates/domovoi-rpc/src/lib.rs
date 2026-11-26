//! The RPC socket between domovoi-server and its clients: numbered requests,
//! each answered once or with a stream of items, as JSON text frames
//! ([`message`]), the large ones deflated for a client that asks
//! ([`packed`]). With the `server` feature, the loop that serves a connection
//! ([`connection`]) and the table of methods it calls ([`router`]).

#![allow(clippy::disallowed_types, reason = "the RPC framing: a payload is JSON until the handler's own type reads it")]

#[cfg(feature = "server")]
pub mod connection;
pub mod message;
pub mod packed;
#[cfg(feature = "server")]
pub mod router;
#[cfg(feature = "axum")]
pub mod axum_ws;

#[cfg(feature = "server")]
pub use connection::{ConnectionOptions, serve_connection};
pub use message::{CauseEncoded, ExitEncoded, FromClient, FromServer, Request, RequestId};
#[cfg(feature = "server")]
pub use router::{Encoded, Failure, Reply, RequestMeta, Router, encoded};
