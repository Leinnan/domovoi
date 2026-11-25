//! The client core inside the Windows app, with the effect host the Iced app
//! uses too (`domovoi-desktop-host`), on a Tokio runtime of its own.
//!
//! What the host reports (view deltas, images, notification clicks) waits in
//! an inbox; each arrival wakes the UI thread, which takes all of it at once.

use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use domovoi_core::Event;
use domovoi_desktop_host::{HostConfig, HostEvent, HostHandle, Identity, SecureStore};

use crate::ui_thread;

/// This app's data folder, keychain service and Windows app id.
pub const IDENTITY: Identity = Identity::WINDOWS;

/// How often the core hears the time (relative ages, reconnect checks).
const TICK: Duration = Duration::from_secs(5);

/// The core. Cloning shares it.
#[derive(Clone)]
pub struct CoreLink {
    handle: HostHandle,
    inbox: Arc<Mutex<Vec<HostEvent>>>,
    runtime: Arc<tokio::runtime::Runtime>,
}

impl PartialEq for CoreLink {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inbox, &other.inbox)
    }
}

impl CoreLink {
    /// Starts the core and its effects. Call on the UI thread after
    /// [`ui_thread::install`]: what the host reports wakes it.
    pub fn start() -> std::io::Result<Self> {
        let runtime = Arc::new(tokio::runtime::Builder::new_multi_thread().worker_threads(2).thread_name("domovoi-core").enable_all().build()?);
        let store = Arc::new(SecureStore::from_env(IDENTITY));
        let (handle, mut events) = {
            let _entered = runtime.enter();
            domovoi_desktop_host::start(store, HostConfig::new(IDENTITY))
        };
        let inbox: Arc<Mutex<Vec<HostEvent>>> = Arc::default();
        let into = Arc::clone(&inbox);
        runtime.spawn(async move {
            while let Some(event) = events.recv().await {
                into.lock().unwrap_or_else(PoisonError::into_inner).push(event);
                ui_thread::wake();
            }
        });
        let ticker = handle.clone();
        runtime.spawn(async move {
            let mut every = tokio::time::interval(TICK);
            loop {
                every.tick().await;
                ticker.send(Event::Tick { now_ms: now_ms() });
            }
        });
        // Links from later launches (`domovoi_desktop_host::links`).
        if let Some(mut links) = domovoi_desktop_host::links::take_receiver() {
            let to = handle.clone();
            runtime.spawn(async move {
                while let Some(link) = links.recv().await {
                    to.send(Event::ConnectRemote { link });
                }
            });
        }
        Ok(CoreLink { handle, inbox, runtime })
    }

    /// Sends `event` to the core; what it changes arrives with a later wake-up.
    pub fn send(&self, event: Event) {
        self.handle.send(event);
    }

    /// Sends `event` after `delay` (the core's reconnect countdown).
    pub fn send_after(&self, delay: Duration, event: Event) {
        let handle = self.handle.clone();
        self.runtime.spawn(async move {
            tokio::time::sleep(delay).await;
            handle.send(event);
        });
    }

    /// Whether this window has the focus, and the thread it shows (no
    /// notification is posted for what the user is looking at).
    pub fn set_focus(&self, focused: bool, thread_id: Option<String>) {
        self.handle.set_focus_context(focused, thread_id);
    }

    /// What the host reported since the last call, oldest first.
    pub fn take(&self) -> Vec<HostEvent> {
        std::mem::take(&mut *self.inbox.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}
