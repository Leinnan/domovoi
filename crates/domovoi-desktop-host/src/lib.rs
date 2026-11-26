//! Runs `domovoi_core::DomovoiApp` on Tokio for the Rust desktop clients:
//! the Iced app (`apps/domovoi-iced`) and the WinUI 3 app for Windows.
//!
//! - [`start`] runs the core's six effects (`Render`, `Http`, `Socket`,
//!   `Tunnel`, `Store`, `Notify`). It merges bursts of renders into one
//!   `ViewDelta` and sends it, with everything else the UI needs to hear
//!   about, as a [`HostEvent`] on a channel.
//! - [`SecureStore`] backs the `Store` effect and this device's iroh key.
//! - [`links`] is single-instance `domovoi://` hand-off between launches.
//!
//! Nothing here knows the UI toolkit. Each client forwards the channel to its
//! own UI thread.

mod engine;
pub mod links;
pub mod logging;
mod store;
#[cfg(windows)]
pub mod windows;

pub use engine::{HostConfig, HostEvent, HostHandle, start};
pub use store::{SecureStore, default_data_dir, parse_secret_key};

/// What tells one desktop client's data, keychain entries, single-instance
/// channel and Windows app id apart from another's.
#[derive(Clone, Copy, Debug)]
pub struct Identity {
    /// The folder under the platform config and cache directories, and the
    /// name of the single-instance socket or pipe.
    pub dir_name: &'static str,
    /// The OS keyring service the secrets are filed under.
    pub keyring_service: &'static str,
    /// The Windows AppUserModelID: taskbar grouping and toasts. The installer's
    /// Start-menu shortcut carries the same one.
    pub app_user_model_id: &'static str,
    /// What Windows shows as the toasts' sender.
    pub display_name: &'static str,
}

impl Identity {
    /// Domovoi Desktop, the Iced app.
    pub const ICED: Identity = Identity {
        dir_name: "domovoi-desktop",
        keyring_service: "com.crayen.domovoi.client",
        app_user_model_id: "Domovoi.Desktop",
        display_name: "Domovoi Desktop",
    };

    /// Domovoi for Windows, the WinUI 3 app (`apps/domovoi-windows`). Paired
    /// on its own: it shares nothing with the Iced app.
    pub const WINDOWS: Identity = Identity {
        dir_name: "domovoi-windows",
        keyring_service: "com.crayen.domovoi.windows",
        app_user_model_id: "Domovoi.Client",
        display_name: "Domovoi",
    };
}

/// The name the server shows for this device when it pairs.
pub fn device_label() -> String {
    hostname::get()
        .ok()
        .and_then(|s| s.into_string().ok())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "Desktop".into())
}

pub fn device_os() -> &'static str {
    if cfg!(target_os = "macos") {
        "macOS"
    } else if cfg!(target_os = "windows") {
        "Windows"
    } else {
        "Linux"
    }
}

/// The IANA time zone, and its UTC offset in seconds (0 when unknown).
pub fn local_timezone() -> (String, i32) {
    let iana = iana_time_zone::get_timezone().unwrap_or_else(|_| "UTC".into());
    (iana, 0)
}
