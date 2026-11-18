//! Crux core for a t3code-compatible client: pairs with a server, keeps the
//! shell (projects + threads), the selected thread, terminals, diffs, files,
//! git status and settings in sync over Effect RPC, and turns user intents into
//! orchestration commands. Shells render the view model and run effects.
//!
//! The thread projection rules are `domovoi_model`'s, the same ones domovoi-server uses.

boltffi::scaffolding!();

pub mod app;
pub mod delta;
pub mod diff;
pub mod environments;
pub mod highlight;
pub mod ffi;
#[cfg(feature = "remote")]
pub mod ffi_remote;
pub mod markdown;
pub mod model;
pub mod notify;
pub mod offline;
pub mod present;
pub mod replies;
pub mod requests;
pub mod rpc;
pub mod settings;
pub mod sidebar;
pub mod socket;
pub mod store;
pub mod terminal;
pub mod tunnel;
pub mod update;
pub mod view;

pub use app::*;
pub use crux_core::Core;
pub use model::Model;
