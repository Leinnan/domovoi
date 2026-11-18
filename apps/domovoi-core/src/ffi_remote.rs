//! The iroh bridge, exported to the native shells. The shell runs the core's
//! `Tunnel` effect with it: start a bridge for a remote server's ticket, then
//! point HTTP and the WebSocket at `http://127.0.0.1:<port>`.
//!
//! Calls block (dialing can take seconds); shells call them off the main
//! thread.

use std::sync::Mutex;
use domovoi_remote::{Bridge, Network, RelayUrl, SecretKey};

#[boltffi::export]
pub struct TunnelFfi {
    runtime: tokio::runtime::Runtime,
    bridge: Mutex<Option<Bridge>>,
}

impl Default for TunnelFfi {
    fn default() -> Self {
        Self::new()
    }
}

/// A new device key (64 hex characters) for `TunnelFfi::start`. The shell
/// keeps it in the Keychain: it is the device's identity to the server.
#[boltffi::export]
#[must_use]
pub fn tunnel_generate_key() -> String {
    hex(&SecretKey::generate().to_bytes())
}

#[boltffi::export]
impl TunnelFfi {
    #[must_use]
    pub fn new() -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build().expect("tokio runtime");
        Self { runtime, bridge: Mutex::new(None) }
    }

    /// Starts (or replaces) the bridge to `ticket` and returns its loopback
    /// port. `relays` is a comma-separated list of relay URLs; empty uses
    /// n0's public relays.
    pub fn start(&self, secret_key: String, ticket: String, relays: String) -> Result<u16, String> {
        let key = parse_key(&secret_key)?;
        let ticket = domovoi_remote::parse_ticket(&ticket).map_err(|e| e.to_string())?;
        let relays = relays
            .split(',')
            .map(str::trim)
            .filter(|r| !r.is_empty())
            .map(|r| r.parse::<RelayUrl>().map_err(|e| format!("relay {r}: {e}")))
            .collect::<Result<Vec<_>, _>>()?;
        self.stop();
        let bridge = self
            .runtime
            .block_on(Bridge::start(key, Network::Internet { relays }, ticket, 0))
            .map_err(|e| e.to_string())?;
        let port = bridge.port();
        *lock(&self.bridge) = Some(bridge);
        Ok(port)
    }

    /// The bridge's state as JSON (`connected`, `path`, `rttMs`, `error`,
    /// `listening`), or `null` when stopped.
    #[must_use]
    pub fn status(&self) -> String {
        let status = lock(&self.bridge).as_ref().map(Bridge::status);
        serde_json::to_string(&status).unwrap_or_else(|_| "null".into())
    }

    /// Re-dials if the connection dropped (after the app was suspended).
    pub fn ensure_connected(&self) -> Result<(), String> {
        let guard = lock(&self.bridge);
        let bridge = guard.as_ref().ok_or("no tunnel")?;
        self.runtime.block_on(bridge.ensure_connected()).map_err(|e| e.to_string())
    }

    /// The device's network changed (Wi-Fi to cellular).
    pub fn network_change(&self) {
        if let Some(bridge) = lock(&self.bridge).as_ref() {
            self.runtime.block_on(bridge.network_change());
        }
    }

    pub fn stop(&self) {
        let bridge = lock(&self.bridge).take();
        if let Some(bridge) = bridge {
            self.runtime.block_on(bridge.stop());
        }
    }
}

fn parse_key(s: &str) -> Result<SecretKey, String> {
    let s = s.trim();
    if s.len() != 64 || !s.is_ascii() {
        return Err("the device key must be 64 hex characters".into());
    }
    let mut bytes = [0u8; 32];
    for (i, b) in bytes.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).map_err(|_| "the device key must be hex")?;
    }
    Ok(SecretKey::from_bytes(&bytes))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_round_trip_through_hex() {
        let key = tunnel_generate_key();
        assert_eq!(hex(&parse_key(&key).unwrap().to_bytes()), key);
        assert!(parse_key("zz").is_err());
    }
}

/// A panic elsewhere must not turn every later call into a panic across the
/// FFI boundary.
fn lock<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}
