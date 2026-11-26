//! Server side of Effect's RPC protocol (`effect/rpc`) with JSON serialization,
//! wire-compatible with the `RpcServer` + `RpcSerialization.layerJson` setup used
//! by t3code's WebSocket API.

#![allow(clippy::disallowed_types, reason = "the RPC framing: a payload is JSON until the handler's own type reads it")]

#[cfg(feature = "server")]
pub mod connection;
pub mod message;
#[cfg(feature = "server")]
pub mod router;
#[cfg(feature = "axum")]
pub mod axum_ws;

#[cfg(feature = "server")]
pub use connection::{ConnectionOptions, serve_connection};
pub use message::{CauseEncoded, ExitEncoded, FromClient, FromServer, RequestEncoded, RequestId};
#[cfg(feature = "server")]
pub use router::{Failure, Reply, RequestMeta, Router};
