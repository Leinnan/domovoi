//! The status bar icon (`domovoi-server --tray`): remote access at a glance, a
//! pairing link or QR code in two clicks, devices and Quit.
//!
//! The icon needs the main thread (macOS insists), so with `--tray` the
//! server runs on its own thread with the tokio runtime and the main thread
//! runs the menu's event loop: winit on macOS and Windows, GTK on Linux.
//! The server tells the menu things through a [`Notify`] callback that wakes
//! the loop; the menu drives the server through [`Control`] on the runtime.

mod app;
mod icon;
pub mod model;
#[cfg(target_os = "linux")]
mod runtime_gtk;
#[cfg(any(target_os = "macos", target_os = "windows"))]
mod runtime_winit;
#[cfg(windows)]
mod windows;

use crate::Fallible;
use crate::control::{Control, PairingLink, ServerStatus};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio_util::sync::CancellationToken;

/// What reaches the menu's thread.
pub enum TrayMessage {
    /// The server is up; this is how to drive it.
    Ready(Control),
    Status(ServerStatus),
    /// A menu item was clicked (its id).
    Menu(String),
    /// Turning remote access on or off finished.
    RemoteSet(Result<(), String>),
    /// A pairing link was minted, to copy or to show as a QR code.
    Link { result: Result<PairingLink, String>, qr: bool },
    /// A device was signed out.
    Revoked(Result<bool, String>),
    /// The server thread finished, with its error if it failed.
    ServerExited(Option<String>),
}

/// Sends a message to the menu's thread and wakes its loop.
pub type Notify = Arc<dyn Fn(TrayMessage) + Send + Sync>;

/// Called by the server once it is serving.
pub type OnReady = Box<dyn FnOnce(Control) + Send>;

/// Runs the server on its own thread and the status bar icon on this one,
/// until the server stops (Quit, a signal, `domovoi-server stop`). Without a
/// desktop to show an icon on, the server simply runs here.
pub fn run<F, Fut>(runtime: tokio::runtime::Runtime, shutdown: CancellationToken, serve: F) -> Fallible
where
    F: FnOnce(Option<OnReady>) -> Fut + Send + 'static,
    Fut: Future<Output = Fallible>,
{
    #[cfg(windows)]
    windows::detach_own_console();
    let (platform, notify) = match platform::Platform::new() {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("No status bar icon: {e}");
            return runtime.block_on(serve(None));
        }
    };
    let handle = runtime.handle().clone();
    let finished = Arc::new(AtomicBool::new(false));
    #[cfg(windows)]
    {
        let shutdown = shutdown.clone();
        windows::watch_session_end(move || shutdown.cancel(), finished.clone());
    }
    let server = {
        let notify = notify.clone();
        let finished = finished;
        std::thread::Builder::new().name("domovoi-server".into()).spawn(move || {
            let on_ready: OnReady = {
                let notify = notify.clone();
                Box::new(move |control: Control| {
                    let mut status = control.status();
                    let forward = notify.clone();
                    tokio::spawn(async move {
                        loop {
                            forward(TrayMessage::Status(status.borrow_and_update().clone()));
                            if status.changed().await.is_err() {
                                return;
                            }
                        }
                    });
                    notify(TrayMessage::Ready(control));
                })
            };
            let result = runtime.block_on(serve(Some(on_ready)));
            runtime.shutdown_timeout(std::time::Duration::from_secs(2));
            finished.store(true, Ordering::Release);
            notify(TrayMessage::ServerExited(result.as_ref().err().map(ToString::to_string)));
            result
        })?
    };
    let app = app::TrayApp::new(handle, notify);
    platform.run(app);
    // The loop ended first (the icon could not be made): stop the server too.
    shutdown.cancel();
    match server.join() {
        Ok(result) => result,
        Err(_) => Err("the server thread panicked".into()),
    }
}

#[cfg(target_os = "linux")]
use runtime_gtk as platform;
#[cfg(any(target_os = "macos", target_os = "windows"))]
use runtime_winit as platform;

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
mod platform {
    pub struct Platform;
    impl Platform {
        pub fn new() -> Result<(Self, super::Notify), String> {
            Err("status bar icons are not supported on this system".into())
        }
        pub fn run(self, _: super::app::TrayApp) {}
    }
}
