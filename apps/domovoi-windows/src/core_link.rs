//! The client core inside the Windows app, with the effect host the Iced app
//! uses too (`domovoi-desktop-host`), on a Tokio runtime of its own.
//!
//! What the host reports (view deltas, images, notification clicks) waits in
//! an inbox; each arrival wakes the UI thread, which takes all of it at once.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use domovoi_core::Event;
use domovoi_desktop_host::attach::Attachment;
use domovoi_desktop_host::system::{self, NetworkSignal, NetworkWatch};
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
    /// The server is on this machine and not reached over a tunnel: it is there with no network.
    server_is_local: Arc<AtomicBool>,
    /// Pictures made ready for a message (or why one could not be), until the window takes them.
    attachments: Arc<Mutex<Vec<Result<Attachment, String>>>>,
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
        let server_is_local: Arc<AtomicBool> = Arc::default();
        let (ticker, local) = (handle.clone(), Arc::clone(&server_is_local));
        runtime.spawn(async move {
            let mut every = tokio::time::interval(TICK);
            let (mut last_ms, mut network) = (now_ms(), NetworkWatch::default());
            loop {
                every.tick().await;
                let now = now_ms();
                if now - last_ms > system::SLEEP_GAP_MS {
                    // The machine slept and the socket died with it: drop it now
                    // rather than wait out a ping, and connect again once it is shut.
                    ticker.send(Event::Suspend);
                    tokio::time::sleep(Duration::from_millis(500)).await;
                    ticker.send(Event::Resume);
                }
                last_ms = now;
                ticker.send(Event::Tick { now_ms: now });
                match network.probed(system::probe_route().await, local.load(Ordering::Relaxed)) {
                    Some(NetworkSignal::Status { online }) => ticker.send(Event::NetworkStatus { online }),
                    Some(NetworkSignal::Changed) => ticker.send(Event::NetworkChanged),
                    None => {}
                }
            }
        });
        // Links from later launches (`domovoi_desktop_host::links`). They come
        // from outside the app, so the core asks before it connects.
        if let Some(mut links) = domovoi_desktop_host::links::take_receiver() {
            let to = handle.clone();
            runtime.spawn(async move {
                while let Some(link) = links.recv().await {
                    to.send(Event::OpenLink { link });
                }
            });
        }
        Ok(CoreLink { handle, inbox, runtime, server_is_local, attachments: Arc::default() })
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

    /// Asks the host for the image at `url` (a signed link on the server);
    /// it arrives as `HostEvent::ImageLoaded` or `ImageFailed`.
    pub fn fetch_image(&self, url: String) {
        self.handle.fetch_image(url);
    }

    /// Makes a picture ready for a message away from the UI thread (reading a
    /// file, encoding the clipboard's bitmap); the window is woken to take it.
    pub fn attach(&self, read: impl FnOnce() -> Result<Attachment, String> + Send + 'static) {
        let into = Arc::clone(&self.attachments);
        self.runtime.spawn_blocking(move || {
            let read = read();
            into.lock().unwrap_or_else(PoisonError::into_inner).push(read);
            ui_thread::wake();
        });
    }

    /// The pictures made ready since the last call.
    pub fn take_attachments(&self) -> Vec<Result<Attachment, String>> {
        std::mem::take(&mut *self.attachments.lock().unwrap_or_else(PoisonError::into_inner))
    }

    /// Says whether the server is on this machine (`url`) and reached without a tunnel.
    pub fn set_server(&self, url: &str, remote: bool) {
        self.server_is_local.store(!remote && system::is_loopback_url(url), Ordering::Relaxed);
    }

    /// Ends the core's effects (the window is closing): sockets shut and the tunnel stops.
    pub fn shutdown(&self) {
        self.handle.shutdown();
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
