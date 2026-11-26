//! What a desktop client sets up with Windows for this user. It goes under
//! `HKCU\Software\Classes` (through the Registry API), so no administrator
//! is needed:
//!
//! - The AppUserModelID that groups the client's windows on the taskbar and
//!   sends its toasts. An unpackaged app can only post toasts under an id
//!   that is registered.
//! - The `domovoi://` scheme. A release build of the WinUI app points it at
//!   itself. So does the Iced app, unless the installer put the WinUI app on
//!   this machine ([`INSTALLED_CLIENT`]): then links are the WinUI app's.
//!   The MSI registers it too, and removes it (with what the apps wrote) on
//!   uninstall.

use std::sync::OnceLock;

use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, REG_SZ, RRF_RT_ANY, RRF_RT_REG_SZ, RegGetValueW, RegSetKeyValueW};

use crate::Identity;

/// Under `HKEY_CURRENT_USER`, as the other keys here.
const SCHEME_KEY: &str = r"Software\Classes\domovoi";
/// The MSI writes this value under [`SCHEME_KEY`]. While it's there, the
/// installed handler is left alone.
pub const INSTALLER_MARK: &str = "Domovoi.Installer";
/// The value the MSI writes when it installs the WinUI app (`Client`, under
/// this key of `HKEY_CURRENT_USER`).
pub const INSTALLED_CLIENT: (&str, &str) = (r"Software\Domovoi\Installer", "Client");

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
        let key = format!(r"Software\Classes\AppUserModelId\{}", identity.app_user_model_id);
        let mut ok = set_string(&key, Some("DisplayName"), identity.display_name);
        if let Some(icon) = icon {
            ok &= set_string(&key, Some("IconUri"), &icon.display().to_string());
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
        if owner == SchemeOwner::Fallback && (has_value(SCHEME_KEY, INSTALLER_MARK) || has_value(INSTALLED_CLIENT.0, INSTALLED_CLIENT.1)) {
            return;
        }
        let command = format!("\"{}\" \"%1\"", exe.display());
        let open = format!(r"{SCHEME_KEY}\shell\open\command");
        if get_string(&open, None).as_deref() == Some(command.as_str()) {
            return;
        }
        let icon = format!("\"{}\",0", exe.display());
        let ok = set_string(SCHEME_KEY, None, "URL:Domovoi")
            && set_string(SCHEME_KEY, Some("URL Protocol"), "")
            && set_string(&format!(r"{SCHEME_KEY}\DefaultIcon"), None, &icon)
            && set_string(&open, None, &command);
        if !ok {
            tracing::warn!("couldn't register domovoi:// links with Windows");
        }
    });
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Sets the string value `name` (the key's default value for `None`) of the
/// key `subkey` of `HKEY_CURRENT_USER`, made if missing.
fn set_string(subkey: &str, name: Option<&str>, value: &str) -> bool {
    let (subkey, name, data) = (wide(subkey), name.map(wide), wide(value));
    let bytes = (data.len() * 2) as u32;
    // SAFETY: NUL-terminated strings, and `bytes` is `data`'s size with its NUL.
    let err = unsafe {
        RegSetKeyValueW(HKEY_CURRENT_USER, subkey.as_ptr(), name.as_ref().map_or(std::ptr::null(), |n| n.as_ptr()), REG_SZ, data.as_ptr().cast(), bytes)
    };
    err == ERROR_SUCCESS
}

/// The string value `name` (the default value for `None`) of `subkey` of
/// `HKEY_CURRENT_USER`; `None` when it is missing or not a string.
fn get_string(subkey: &str, name: Option<&str>) -> Option<String> {
    let (subkey, name) = (wide(subkey), name.map(wide));
    let name = name.as_ref().map_or(std::ptr::null(), |n| n.as_ptr());
    let mut bytes = 0u32;
    // SAFETY: asking for the size only.
    let err = unsafe { RegGetValueW(HKEY_CURRENT_USER, subkey.as_ptr(), name, RRF_RT_REG_SZ, std::ptr::null_mut(), std::ptr::null_mut(), &mut bytes) };
    if err != ERROR_SUCCESS {
        return None;
    }
    let mut data = vec![0u16; (bytes as usize).div_ceil(2)];
    // SAFETY: `data` holds `bytes` bytes.
    let err = unsafe { RegGetValueW(HKEY_CURRENT_USER, subkey.as_ptr(), name, RRF_RT_REG_SZ, std::ptr::null_mut(), data.as_mut_ptr().cast(), &mut bytes) };
    if err != ERROR_SUCCESS {
        return None;
    }
    data.truncate((bytes as usize / 2).saturating_sub(1));
    Some(String::from_utf16_lossy(&data))
}

/// Whether `subkey` of `HKEY_CURRENT_USER` has a value `name`, of any type.
fn has_value(subkey: &str, name: &str) -> bool {
    let (subkey, name) = (wide(subkey), wide(name));
    // SAFETY: NUL-terminated strings; no data is asked for.
    let err = unsafe { RegGetValueW(HKEY_CURRENT_USER, subkey.as_ptr(), name.as_ptr(), RRF_RT_ANY, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut()) };
    err == ERROR_SUCCESS
}

#[cfg(test)]
mod tests {
    use windows_sys::Win32::System::Registry::{RegDeleteTreeW, HKEY_CURRENT_USER};

    /// Values written are read back as written, under a key of this test's own.
    #[test]
    fn registry_values_round_trip() {
        let key = format!(r"Software\DomovoiTest-{}", std::process::id());
        let command = r#""C:\Program Files\Domovoi\Domovoi.exe" "%1""#;
        assert!(super::set_string(&key, None, "URL:Test"));
        assert!(super::set_string(&key, Some("Empty"), ""));
        assert!(super::set_string(&format!(r"{key}\shell\open\command"), None, command));
        assert_eq!(super::get_string(&key, None).as_deref(), Some("URL:Test"));
        assert_eq!(super::get_string(&key, Some("Empty")).as_deref(), Some(""));
        assert_eq!(super::get_string(&format!(r"{key}\shell\open\command"), None).as_deref(), Some(command));
        assert!(super::has_value(&key, "Empty"));
        assert!(!super::has_value(&key, "Missing"));
        assert_eq!(super::get_string(&key, Some("Missing")), None);
        let name = super::wide(&key);
        // SAFETY: a NUL-terminated name of the key made above.
        let err = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, name.as_ptr()) };
        assert_eq!(err, 0, "the test's key removed");
        // RegDeleteTreeW leaves the (now empty) key itself.
        // SAFETY: as above.
        unsafe { windows_sys::Win32::System::Registry::RegDeleteKeyW(HKEY_CURRENT_USER, name.as_ptr()) };
    }
}
