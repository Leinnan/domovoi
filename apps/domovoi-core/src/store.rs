//! Secure key-value storage (the Keychain on Apple platforms): saved
//! environments, the active one, and each environment's bearer token.

use crux_core::capability::Operation;
use facet::Facet;
use serde::{Deserialize, Serialize};

#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[repr(C)]
pub enum StoreOperation {
    Load { key: String },
    Save { key: String, value: String },
    Delete { key: String },
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[repr(C)]
pub enum StoreOutput {
    /// The stored value (`None` when missing); also the answer to Save/Delete.
    Value { value: Option<String> },
}

impl Operation for StoreOperation {
    type Output = StoreOutput;
}

pub const KEY_ENVIRONMENTS: &str = "environments";
pub const KEY_ACTIVE: &str = "activeEnvironment";

pub fn token_key(environment_id: &str) -> String {
    format!("token:{environment_id}")
}
/// Set once this app has signed in to the server on its machine by itself
/// (or the user has forgotten a server): it does that at most once per
/// install, on a first launch with no saved servers.
pub const KEY_LOCAL_AUTO: &str = "localAuto";
