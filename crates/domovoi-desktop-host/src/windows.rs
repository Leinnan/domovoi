//! What a desktop client sets up with Windows for this user. It goes under
//! `HKCU\Software\Classes`, so no administrator is needed:
//!
//! - The AppUserModelID that groups the client's windows on the taskbar and
//!   sends its toasts. An unpackaged app can only post toasts under an id
//!   that is registered.
//! - The `domovoi://` scheme. A release build of the WinUI app points it at
//!   itself. So does the Iced app, unless the installer put the WinUI app on
//!   this machine ([`INSTALLED_CLIENT`]): then links are the WinUI app's.
//!   The MSI registers it too, and removes it (with what the apps wrote) on
//!   uninstall.

use std::process::Command;
use std::sync::OnceLock;

use crate::Identity;

const SCHEME_KEY: &str = r"HKCU\Software\Classes\domovoi";
/// The MSI writes this value under [`SCHEME_KEY`]. While it's there, the
/// installed handler is left alone.
pub const INSTALLER_MARK: &str = "Domovoi.Installer";
/// The value the MSI writes when it installs the WinUI app (`Client`, under
/// this key).
pub const INSTALLED_CLIENT: (&str, &str) = (r"HKCU\Software\Domovoi\Installer", "Client");

/// Who points `domovoi://` at itself.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SchemeOwner {
    /// The WinUI app: links are its own.
    Primary,
    /// The Iced app: only where neither the installer nor an installed WinUI
    /// app has the links.
    Fallback,
}

static TOAST_APP_ID: OnceLock<&'static str> = OnceLock::new();

/// `DOMOVOI_NO_REGISTER=1`: a run that must leave Windows as it found it
/// (the packaging smoke test starts an unpacked copy).
fn registering() -> bool {
    std::env::var_os("DOMOVOI_NO_REGISTER").is_none_or(|v| v != "1")
}

/// The app id toasts are posted under, once [`register_app_id`] has set it up.
pub fn toast_app_id() -> Option<&'static str> {
    TOAST_APP_ID.get().copied()
}

/// Gives this process the client's AppUserModelID. Then, on a thread of its
/// own, it registers the id (its display name, and the icon at `icon`) so
/// toasts can use it. Call it before the first window opens.
pub fn register_app_id(identity: Identity, icon: Option<std::path::PathBuf>) {
    let id: Vec<u16> = identity.app_user_model_id.encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: `id` is a NUL-terminated UTF-16 string that outlives the call.
    let hr = unsafe { windows_sys::Win32::UI::Shell::SetCurrentProcessExplicitAppUserModelID(id.as_ptr()) };
    if hr < 0 {
        tracing::warn!("couldn't set the AppUserModelID ({hr:#x})");
    }
    if !registering() {
        return;
    }
    let _ = std::thread::Builder::new().name("domovoi-app-id".into()).spawn(move || {
        let key = format!(r"HKCU\Software\Classes\AppUserModelId\{}", identity.app_user_model_id);
        let mut ok = reg(&["add", &key, "/v", "DisplayName", "/d", identity.display_name, "/f"]).is_some();
        if let Some(icon) = icon {
            ok &= reg(&["add", &key, "/v", "IconUri", "/d", &icon.display().to_string(), "/f"]).is_some();
        }
        if ok {
            let _ = TOAST_APP_ID.set(identity.app_user_model_id);
        } else {
            tracing::warn!("couldn't register {} with Windows; its notifications show as PowerShell's", identity.app_user_model_id);
        }
    });
}

/// Points `domovoi://` (for this user) at this executable, unless it
/// already points here (or, for the fallback, another handler is installed).
/// Debug builds leave it alone. Runs on a thread of its own.
pub fn register_scheme(owner: SchemeOwner) {
    if cfg!(debug_assertions) || !registering() {
        return;
    }
    let _ = std::thread::Builder::new().name("domovoi-scheme".into()).spawn(move || {
        let Ok(exe) = std::env::current_exe() else { return };
        if owner == SchemeOwner::Fallback
            && (reg(&["query", SCHEME_KEY, "/v", INSTALLER_MARK]).is_some() || reg(&["query", INSTALLED_CLIENT.0, "/v", INSTALLED_CLIENT.1]).is_some())
        {
            return;
        }
        let command = format!("\"{}\" \"%1\"", exe.display());
        let open = format!(r"{SCHEME_KEY}\shell\open\command");
        if reg(&["query", &open, "/ve"]).is_some_and(|out| out.contains(&command)) {
            return;
        }
        let icon = format!("\"{}\",0", exe.display());
        let ok = reg(&["add", SCHEME_KEY, "/ve", "/d", "URL:Domovoi", "/f"]).is_some()
            && reg(&["add", SCHEME_KEY, "/v", "URL Protocol", "/d", "", "/f"]).is_some()
            && reg(&["add", &format!(r"{SCHEME_KEY}\DefaultIcon"), "/ve", "/d", &icon, "/f"]).is_some()
            && reg(&["add", &open, "/ve", "/d", &command, "/f"]).is_some();
        if !ok {
            tracing::warn!("couldn't register domovoi:// links with Windows");
        }
    });
}

#[allow(clippy::disallowed_methods, reason = "reg.exe exits at once and is waited for; the server's process helpers aren't in this crate")]
fn reg(args: &[&str]) -> Option<String> {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    let out = Command::new("reg").args(args).creation_flags(CREATE_NO_WINDOW).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}
