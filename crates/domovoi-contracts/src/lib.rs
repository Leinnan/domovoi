//! Wire types for the subset of t3code's server API that domovoi-server implements.
//!
//! Field names and optionality follow `packages/contracts` exactly; responses are
//! checked against the real Effect schemas by `conformance/wire-check.ts`.

pub mod auth;
pub mod defaults;
pub mod environment;
pub mod shell;
pub mod time_fmt;

pub use time_fmt::{iso_now, now_millis};
