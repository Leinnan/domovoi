//! Live RPC WebSockets per session: revoking a session closes its sockets,
//! the access list shows who is connected, and push skips devices whose app
//! is in the foreground (it has a socket open and notifies itself).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use crate::util::locked;
use crate::time_fmt::now_millis;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Default)]
pub struct Connections(Arc<Inner>);

#[derive(Default)]
struct Inner {
    next: AtomicU64,
    /// Token hash -> open sockets (id, cancel).
    open: Mutex<HashMap<String, Vec<(u64, CancellationToken)>>>,
    /// Token hash -> when a socket last opened or closed (ms).
    last_seen: Mutex<HashMap<String, i64>>,
    /// Poked whenever a socket opens or closes.
    pulse: Arc<tokio::sync::Notify>,
}

/// One open socket; dropping it unregisters it.
pub struct Live {
    pub cancel: CancellationToken,
    conns: Connections,
    token_hash: String,
    pub id: u64,
}

impl Connections {
    /// Connections that poke `pulse` whenever a socket opens or closes.
    pub fn new(pulse: Arc<tokio::sync::Notify>) -> Self {
        Connections(Arc::new(Inner { pulse, ..Default::default() }))
    }

    /// Forgets when sessions that no longer exist were last seen.
    pub fn retain_seen(&self, keep: impl Fn(&str) -> bool) {
        locked(&self.0.last_seen).retain(|hash, _| keep(hash));
    }

    pub fn open(&self, token_hash: &str) -> Live {
        let id = self.0.next.fetch_add(1, Ordering::Relaxed);
        let cancel = CancellationToken::new();
        locked(&self.0.open).entry(token_hash.to_owned()).or_default().push((id, cancel.clone()));
        self.touch(token_hash);
        Live { cancel, conns: self.clone(), token_hash: token_hash.to_owned(), id }
    }

    pub fn is_live(&self, token_hash: &str) -> bool {
        locked(&self.0.open).get(token_hash).is_some_and(|v| !v.is_empty())
    }

    pub fn last_seen(&self, token_hash: &str) -> Option<i64> {
        locked(&self.0.last_seen).get(token_hash).copied()
    }

    /// Closes every socket of a session (after it was revoked).
    pub fn close_all(&self, token_hash: &str) {
        if let Some(sockets) = locked(&self.0.open).remove(token_hash) {
            for (_, cancel) in sockets {
                cancel.cancel();
            }
        }
    }

    /// Closes every socket (the server is shutting down).
    pub fn close_everything(&self) {
        for (_, sockets) in locked(&self.0.open).drain() {
            for (_, cancel) in sockets {
                cancel.cancel();
            }
        }
    }

    /// Token hashes of sessions with a socket open.
    pub fn live_hashes(&self) -> Vec<String> {
        locked(&self.0.open).iter().filter(|(_, v)| !v.is_empty()).map(|(hash, _)| hash.clone()).collect()
    }

    fn touch(&self, token_hash: &str) {
        locked(&self.0.last_seen).insert(token_hash.to_owned(), now_millis());
        self.0.pulse.notify_one();
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        let mut open = locked(&self.conns.0.open);
        if let Some(sockets) = open.get_mut(&self.token_hash) {
            sockets.retain(|(id, _)| *id != self.id);
            if sockets.is_empty() {
                open.remove(&self.token_hash);
            }
        }
        drop(open);
        self.conns.touch(&self.token_hash);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sockets_register_and_close() {
        let c = Connections::default();
        let a = c.open("h");
        let b = c.open("h");
        assert!(c.is_live("h"));
        drop(a);
        assert!(c.is_live("h"));
        c.close_all("h");
        assert!(b.cancel.is_cancelled());
        assert!(!c.is_live("h"));
        drop(b);
        assert!(c.last_seen("h").is_some());
    }
}
