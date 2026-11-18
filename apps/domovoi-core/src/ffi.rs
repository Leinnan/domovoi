//! FFI surface for native shells (boltffi), as in Crux's examples.
//!
//! A panic must not cross into Swift or Kotlin: boltffi's exports are plain
//! `extern "C"` functions, where an unwinding panic aborts the whole app.
//! Each call catches one; the core it happened in is replaced by a new one
//! (crux's locks are poisoned by then) and the shell, told by
//! [`CoreFfi::take_reset`], starts it again as at launch.

use crate::delta::FfiApp;
use crux_core::{
    Core,
    bridge::{BincodeFfiFormat, Bridge, BridgeError, EffectId, FfiFormat},
};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

#[boltffi::export]
pub struct CoreFfi {
    core: Mutex<Bridge<FfiApp>>,
    /// The core was replaced since the shell last asked.
    reset: AtomicBool,
}

impl Default for CoreFfi {
    fn default() -> Self {
        Self::new()
    }
}

#[boltffi::export]
impl CoreFfi {
    #[must_use]
    pub fn new() -> Self {
        Self { core: Mutex::new(fresh()), reset: AtomicBool::new(false) }
    }

    /// Sends a bincode-serialized `Event`; returns serialized effect requests.
    #[must_use]
    pub fn update(&self, data: &[u8]) -> Vec<u8> {
        self.guarded("update", no_requests, |core| {
            let mut effects = Vec::new();
            core.update(data, &mut effects).map(|()| effects)
        })
    }

    /// Resolves effect `id` with a serialized output; returns new effect requests.
    #[must_use]
    pub fn resolve(&self, id: u32, data: &[u8]) -> Vec<u8> {
        self.guarded("resolve", no_requests, |core| {
            let mut effects = Vec::new();
            match core.resolve(EffectId(id), data, &mut effects) {
                Ok(()) => Ok(effects),
                // An answer the core no longer waits for (its request finished,
                // or was dropped with the connection it belonged to) asks for
                // nothing: the shell reads no requests.
                Err(BridgeError::ProcessResponse(_)) => Ok(no_requests()),
                Err(e) => Err(e),
            }
        })
    }

    /// The serialized `ViewDelta`: what changed since the last call (the
    /// first call: everything). Apply every one, in order.
    ///
    /// After a reset the delta is the new core's whole view: the shell drops
    /// what it built from earlier deltas first (see [`Self::take_reset`]).
    #[must_use]
    pub fn view(&self) -> Vec<u8> {
        let read = |core: &Bridge<FfiApp>| {
            let mut view_model = Vec::new();
            core.view(&mut view_model).map(|()| view_model)
        };
        if let Some(view) = self.call("view", read) {
            return view;
        }
        // The new core's first view, which nothing has touched yet.
        match catch_unwind(AssertUnwindSafe(|| read(&self.lock()))) {
            Ok(Ok(view)) => view,
            _ => panic!("a new core's first view could not be built"),
        }
    }

    /// Whether the core was replaced (after a panic) since the last call.
    /// The shell then forgets the view it built, reads the new one whole,
    /// and sends the launch event again.
    #[must_use]
    pub fn take_reset(&self) -> bool {
        self.reset.swap(false, Ordering::AcqRel)
    }
}

impl CoreFfi {
    fn lock(&self) -> MutexGuard<'_, Bridge<FfiApp>> {
        self.core.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Runs `body` on the core, or, when it fails, gives `fallback`.
    fn guarded(&self, what: &str, fallback: fn() -> Vec<u8>, body: impl FnOnce(&Bridge<FfiApp>) -> Result<Vec<u8>, BridgeError>) -> Vec<u8> {
        self.call(what, body).unwrap_or_else(fallback)
    }

    /// Runs `body` on the core. A bridge error is logged and answered with
    /// nothing; a panic also replaces the core.
    fn call(&self, what: &str, body: impl FnOnce(&Bridge<FfiApp>) -> Result<Vec<u8>, BridgeError>) -> Option<Vec<u8>> {
        let mut core = self.lock();
        match catch_unwind(AssertUnwindSafe(|| body(&core))) {
            Ok(Ok(out)) => Some(out),
            Ok(Err(e)) => {
                eprintln!("domovoi core: {what} failed: {e}");
                None
            }
            Err(payload) => {
                let message = payload
                    .downcast_ref::<&str>()
                    .map(|s| (*s).to_owned())
                    .or_else(|| payload.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "unknown panic".into());
                eprintln!("domovoi core: {what} panicked ({message}); starting a new core");
                *core = fresh();
                self.reset.store(true, Ordering::Release);
                None
            }
        }
    }
}

fn fresh() -> Bridge<FfiApp> {
    Bridge::new(Core::new())
}

/// An empty list of effect requests, as the shells decode it.
fn no_requests() -> Vec<u8> {
    let mut out = Vec::new();
    match BincodeFfiFormat::serialize(&mut out, &Vec::<u8>::new()) {
        Ok(()) => out,
        Err(e) => panic!("{e}"),
    }
}

/// Tests only: the next event the core takes panics inside the app, with
/// crux's locks held, as a bug in `update` would.
#[cfg(test)]
pub(crate) static PANIC_IN_NEXT_UPDATE: AtomicBool = AtomicBool::new(false);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::socket::SocketEvent;

    #[test]
    fn an_answer_nobody_waits_for_asks_for_nothing() {
        let core = CoreFfi::new();
        let mut event = Vec::new();
        BincodeFfiFormat::serialize(&mut event, &SocketEvent::Opened).unwrap();
        // No request has this id; a shell's late answer must not bring the app down.
        let effects = core.resolve(4242, &event);
        // What the shells decode as a list of no requests.
        let requests: Vec<u64> = BincodeFfiFormat::deserialize(&effects).unwrap();
        assert!(requests.is_empty() && !effects.is_empty(), "an encoded empty list, not no bytes at all");
    }

    #[test]
    fn a_panic_in_the_core_starts_a_new_one_instead_of_aborting() {
        let core = CoreFfi::new();
        let _ = core.view();
        let mut event = Vec::new();
        BincodeFfiFormat::serialize(&mut event, &crate::Event::AppFocus { focused: true }).unwrap();
        PANIC_IN_NEXT_UPDATE.store(true, Ordering::SeqCst);
        let effects = core.update(&event);
        let requests: Vec<u64> = BincodeFfiFormat::deserialize(&effects).unwrap();
        assert!(requests.is_empty(), "a failed event asks for nothing");
        assert!(core.take_reset(), "the shell is told to start the core again");
        assert!(!core.take_reset(), "and told once");
        // The new core works: it takes events and gives its whole view.
        let _ = core.update(&event);
        assert!(!core.view().is_empty());
        assert!(!core.take_reset());
    }

    #[test]
    fn an_event_that_does_not_decode_asks_for_nothing() {
        let core = CoreFfi::new();
        let effects = core.update(&[0xff, 0xff, 0xff, 0xff, 0xff]);
        let requests: Vec<u64> = BincodeFfiFormat::deserialize(&effects).unwrap();
        assert!(requests.is_empty());
        assert!(!core.take_reset(), "nothing panicked: the core is kept");
    }
}
