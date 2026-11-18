//! Launch at login: the server with its status bar icon, started when the
//! user signs in.
//!
//! - macOS, as "Domovoi Server.app": a login item (`SMAppService.mainAppService`).
//!   LaunchServices opens the app at login as a double-click would, so it
//!   works from any folder (Documents included) and needs no arguments: the
//!   bundle's executable starts with its icon.
//! - macOS, a bare binary: a LaunchAgent in `~/Library/LaunchAgents` (launchd
//!   loads it at the next login; turning it on does not start a second
//!   server now).
//! - Windows: a value under `HKCU\…\CurrentVersion\Run`.
//! - Linux: a desktop entry in `~/.config/autostart`.
//!
//! The entry runs this executable with `--tray --no-startup-code` and the
//! home directory and port it was turned on with. `DOMOVOI_AUTOSTART_DIR`
//! redirects the file (tests).

use std::path::PathBuf;

#[cfg_attr(windows, allow(dead_code))] // the Run value has its own name
pub const LABEL: &str = "com.crayen.domovoi.server";

/// The command a login runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub exe: PathBuf,
    pub args: Vec<String>,
}

impl Entry {
    /// This executable, serving `home` on `host:port` with the icon.
    pub fn current(home: &std::path::Path, host: &str, port: u16, relays: &[String]) -> std::io::Result<Entry> {
        let home = std::fs::canonicalize(home).unwrap_or_else(|_| home.to_path_buf());
        let mut args = vec!["--tray".to_owned(), "--no-startup-code".to_owned(), "--home-dir".to_owned(), home.to_string_lossy().into_owned(), "--port".to_owned(), port.to_string()];
        if host != "127.0.0.1" {
            args.extend(["--host".to_owned(), host.to_owned()]);
        }
        for relay in relays {
            args.extend(["--relay".to_owned(), relay.clone()]);
        }
        Ok(Entry { exe: std::env::current_exe()?, args })
    }

    /// Why a login could not run this, if it could not: on macOS, a program
    /// started by launchd may not read Documents, Desktop or Downloads (or
    /// iCloud Drive) without a privacy prompt nobody is there to answer, and
    /// it waits on it.
    pub fn problem(&self) -> Option<String> {
        if !cfg!(target_os = "macos") {
            return None;
        }
        self.guarded_folder()
    }

    fn guarded_folder(&self) -> Option<String> {
        let home = dirs::home_dir()?;
        let guarded = ["Documents", "Desktop", "Downloads", "Library/Mobile Documents"].map(|d| home.join(d));
        let home_dir = self.args.iter().position(|a| a == "--home-dir").and_then(|i| self.args.get(i + 1)).map(PathBuf::from);
        [Some(self.exe.clone()), home_dir].into_iter().flatten().find_map(|path| {
            let inside = guarded.iter().find(|g| path.starts_with(g))?;
            Some(format!(
                "{} is inside ~/{}, which macOS keeps from programs started at login; put domovoi-server elsewhere (`just tray-app` and move \"Domovoi Server.app\" to /Applications, or copy the binary to ~/.local/bin) and keep the home directory out of it too",
                path.display(),
                inside.strip_prefix(&home).unwrap_or(inside).display()
            ))
        })
    }

    fn argv(&self) -> impl Iterator<Item = String> + '_ {
        std::iter::once(self.exe.to_string_lossy().into_owned()).chain(self.args.iter().cloned())
    }
}

/// The LaunchAgent: started at login in the user's graphical session (the
/// icon needs one), not restarted when it quits.
#[cfg(any(target_os = "macos", test))]
fn launch_agent(entry: &Entry) -> String {
    let escape = |s: &str| s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;");
    let args: String = entry.argv().map(|a| format!("        <string>{}</string>\n", escape(&a))).collect();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LABEL}</string>
    <key>ProgramArguments</key>
    <array>
{args}    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <false/>
    <key>ProcessType</key>
    <string>Interactive</string>
    <key>LimitLoadToSessionType</key>
    <string>Aqua</string>
</dict>
</plist>
"#
    )
}

/// The XDG autostart entry. `Exec` arguments are quoted per the desktop
/// entry specification.
#[cfg(any(target_os = "linux", test))]
fn desktop_entry(entry: &Entry) -> String {
    let quote = |arg: String| {
        if !arg.is_empty() && arg.chars().all(|c| c.is_ascii_alphanumeric() || "-_./:=@".contains(c)) {
            return arg;
        }
        let mut out = String::from("\"");
        for c in arg.chars() {
            if matches!(c, '"' | '`' | '$' | '\\') {
                out.push('\\');
            }
            // A literal percent sign is doubled in Exec.
            if c == '%' {
                out.push('%');
            }
            out.push(c);
        }
        out.push('"');
        out
    };
    let exec: Vec<String> = entry.argv().map(quote).collect();
    format!("[Desktop Entry]\nType=Application\nName=Domovoi Server\nComment=Domovoi server with its status bar icon\nExec={}\nTerminal=false\nX-GNOME-Autostart-enabled=true\n", exec.join(" "))
}

/// The Run value: a Windows command line (arguments with spaces quoted).
#[cfg(any(windows, test))]
fn windows_command(entry: &Entry) -> String {
    let quote = |arg: String| {
        if !arg.is_empty() && !arg.contains([' ', '\t', '"']) {
            return arg;
        }
        // CommandLineToArgvW rules: backslashes before a quote are doubled.
        let mut out = String::from("\"");
        let mut backslashes = 0;
        for c in arg.chars() {
            match c {
                '\\' => backslashes += 1,
                '"' => {
                    out.extend(std::iter::repeat_n('\\', backslashes + 1));
                    backslashes = 0;
                }
                _ => backslashes = 0,
            }
            out.push(c);
        }
        out.extend(std::iter::repeat_n('\\', backslashes));
        out.push('"');
        out
    };
    entry.argv().map(quote).collect::<Vec<_>>().join(" ")
}

#[cfg(not(windows))]
fn file() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("DOMOVOI_AUTOSTART_DIR") {
        let name = if cfg!(target_os = "macos") { format!("{LABEL}.plist") } else { format!("{LABEL}.desktop") };
        return Some(PathBuf::from(dir).join(name));
    }
    #[cfg(target_os = "macos")]
    {
        Some(dirs::home_dir()?.join("Library").join("LaunchAgents").join(format!("{LABEL}.plist")))
    }
    #[cfg(not(target_os = "macos"))]
    {
        Some(dirs::config_dir()?.join("autostart").join(format!("{LABEL}.desktop")))
    }
}

#[cfg(not(windows))]
fn content(entry: &Entry) -> String {
    #[cfg(target_os = "macos")]
    {
        launch_agent(entry)
    }
    #[cfg(not(target_os = "macos"))]
    {
        desktop_entry(entry)
    }
}

#[cfg(not(windows))]
fn plain_is_enabled() -> bool {
    file().is_some_and(|f| f.exists())
}

#[cfg(not(windows))]
fn plain_enable(entry: &Entry) -> Result<(), String> {
    if let Some(problem) = entry.problem() {
        return Err(problem);
    }
    let file = file().ok_or("no home directory")?;
    if let Some(dir) = file.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    std::fs::write(&file, content(entry)).map_err(|e| format!("{}: {e}", file.display()))
}

#[cfg(not(windows))]
fn plain_disable() -> Result<(), String> {
    match file().map(std::fs::remove_file) {
        Some(Err(e)) if e.kind() != std::io::ErrorKind::NotFound => Err(e.to_string()),
        _ => Ok(()),
    }
}

/// Whether the entry on disk already runs `entry` (it is rewritten when this
/// executable moved).
#[cfg(not(windows))]
fn plain_is_current(entry: &Entry) -> bool {
    file().and_then(|f| std::fs::read_to_string(f).ok()).is_some_and(|text| text == content(entry))
}

#[cfg(windows)]
mod registry {
    use super::{Entry, windows_command};
    use windows_sys::Win32::System::Registry::{HKEY_CURRENT_USER, REG_SZ, RRF_RT_REG_SZ, RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW};

    const KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";
    const VALUE: &str = "Domovoi Server";

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn read() -> Option<String> {
        let (key, value) = (wide(KEY), wide(VALUE));
        let mut buf = vec![0u16; 4096];
        let mut size = (buf.len() * 2) as u32;
        // SAFETY: the strings are NUL-terminated; `buf` is valid for `size` bytes.
        let status = unsafe { RegGetValueW(HKEY_CURRENT_USER, key.as_ptr(), value.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), buf.as_mut_ptr().cast(), &raw mut size) };
        (status == 0).then(|| String::from_utf16_lossy(&buf[..(size as usize / 2).saturating_sub(1)]))
    }

    pub fn is_enabled() -> bool {
        read().is_some()
    }

    pub fn is_current(entry: &Entry) -> bool {
        read().is_some_and(|command| command == windows_command(entry))
    }

    pub fn enable(entry: &Entry) -> Result<(), String> {
        if let Some(problem) = entry.problem() {
            return Err(problem);
        }
        let (key, value, data) = (wide(KEY), wide(VALUE), wide(&windows_command(entry)));
        // SAFETY: the strings are NUL-terminated and `data` is valid for the size given.
        let status = unsafe { RegSetKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), value.as_ptr(), REG_SZ, data.as_ptr().cast(), (data.len() * 2) as u32) };
        if status == 0 { Ok(()) } else { Err(std::io::Error::from_raw_os_error(status as i32).to_string()) }
    }

    pub fn disable() -> Result<(), String> {
        let (key, value) = (wide(KEY), wide(VALUE));
        // SAFETY: the strings are NUL-terminated.
        let status = unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), value.as_ptr()) };
        // 2: the value was not there.
        if status == 0 || status == 2 { Ok(()) } else { Err(std::io::Error::from_raw_os_error(status as i32).to_string()) }
    }
}

#[cfg(windows)]
use registry::{disable as plain_disable, enable as plain_enable, is_current as plain_is_current, is_enabled as plain_is_enabled};

/// This binary is the executable of an app bundle ("Domovoi Server.app" from
/// `just tray-app`, started from Finder, `open` or Login Items): it runs as
/// `--tray --no-startup-code`, and launch at login is a login item.
/// LaunchServices has to start this binary itself: a launcher script that
/// `exec`s it changes the process's pid version, and the menu bar
/// (ControlCenter) then cannot match the process and keeps its status item
/// off screen.
#[cfg(target_os = "macos")]
pub fn is_app_executable(exe: &std::path::Path) -> bool {
    let Some(contents) = exe.parent().filter(|dir| dir.ends_with("MacOS")).and_then(std::path::Path::parent) else {
        return false;
    };
    contents.ends_with("Contents")
        && contents.parent().is_some_and(|app| app.extension().is_some_and(|ext| ext == "app"))
        && contents.join("Info.plist").is_file()
}

/// Launch at login goes through a login item: this is "Domovoi Server.app".
#[cfg(target_os = "macos")]
fn uses_login_item() -> bool {
    std::env::var_os("DOMOVOI_AUTOSTART_DIR").is_none() && std::env::current_exe().is_ok_and(|exe| is_app_executable(&exe))
}

/// The app bundle as a login item (System Settings › General › Login Items).
#[cfg(target_os = "macos")]
mod login_item {
    use objc2_service_management::{SMAppService, SMAppServiceStatus};

    fn status() -> SMAppServiceStatus {
        // SAFETY: `mainAppService` and `status` take no arguments and may be
        // called from any thread.
        unsafe { SMAppService::mainAppService().status() }
    }

    pub fn is_enabled() -> bool {
        let status = status();
        status == SMAppServiceStatus::Enabled || status == SMAppServiceStatus::RequiresApproval
    }

    /// `Ok(Some(note))` when the user still has to allow it in System Settings.
    pub fn enable() -> Result<Option<String>, String> {
        if status() != SMAppServiceStatus::Enabled {
            // SAFETY: as in `status`; the error is returned, not thrown.
            unsafe { SMAppService::mainAppService().registerAndReturnError() }.map_err(|e| e.localizedDescription().to_string())?;
        }
        if status() == SMAppServiceStatus::RequiresApproval {
            // SAFETY: takes no arguments; it opens System Settings.
            unsafe { SMAppService::openSystemSettingsLoginItems() };
            return Ok(Some("allow Domovoi Server in System Settings › General › Login Items".into()));
        }
        Ok(None)
    }

    pub fn disable() -> Result<(), String> {
        let status = status();
        if status == SMAppServiceStatus::NotRegistered || status == SMAppServiceStatus::NotFound {
            return Ok(());
        }
        // SAFETY: as in `status`; the error is returned, not thrown.
        unsafe { SMAppService::mainAppService().unregisterAndReturnError() }.map_err(|e| e.localizedDescription().to_string())
    }
}

pub fn is_enabled() -> bool {
    #[cfg(target_os = "macos")]
    if uses_login_item() {
        return login_item::is_enabled();
    }
    plain_is_enabled()
}

/// Turns it on. `Ok(Some(note))`: on, but the user has something left to do.
pub fn enable(entry: &Entry) -> Result<Option<String>, String> {
    #[cfg(target_os = "macos")]
    if uses_login_item() {
        // A LaunchAgent from before would start a second copy.
        plain_disable()?;
        return login_item::enable();
    }
    plain_enable(entry).map(|()| None)
}

pub fn disable() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    if uses_login_item() {
        login_item::disable()?;
    }
    plain_disable()
}

/// At start: an entry that points at an older copy of this executable (or
/// another home directory or port) is rewritten; in the app bundle, a
/// LaunchAgent from before becomes a login item.
pub fn ensure(entry: &Entry) {
    #[cfg(target_os = "macos")]
    if uses_login_item() {
        if plain_is_enabled() {
            match login_item::enable().and_then(|note| plain_disable().map(|()| note)) {
                Ok(note) => tracing::info!("Launch at login is now a login item{}", note.map(|n| format!(" ({n})")).unwrap_or_default()),
                Err(e) => tracing::warn!("Launch at login could not become a login item: {e}"),
            }
        }
        return;
    }
    if plain_is_enabled() && !plain_is_current(entry) {
        match plain_enable(entry) {
            Ok(()) => tracing::info!("Launch at login updated for this executable"),
            Err(e) => tracing::warn!("Launch at login could not be updated: {e}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry() -> Entry {
        Entry {
            exe: PathBuf::from("/Applications/Domovoi Server.app/Contents/MacOS/domovoi-server"),
            args: vec!["--tray".into(), "--home-dir".into(), "/Users/me/Domovoi & co".into(), "--port".into(), "13773".into()],
        }
    }

    #[test]
    fn the_launch_agent_runs_the_server_with_its_icon_at_login() {
        let plist = launch_agent(&entry());
        assert!(plist.contains("<string>com.crayen.domovoi.server</string>"));
        assert!(plist.contains("<string>/Applications/Domovoi Server.app/Contents/MacOS/domovoi-server</string>"));
        assert!(plist.contains("<string>/Users/me/Domovoi &amp; co</string>"), "XML-escaped: {plist}");
        assert!(plist.contains("<key>RunAtLoad</key>\n    <true/>"));
        assert!(plist.contains("<key>KeepAlive</key>\n    <false/>"), "Quit stays quit");
        assert!(plist.contains("<string>Aqua</string>"), "only in a graphical session");
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn folders_macos_guards_from_login_items_are_refused() {
        let home = dirs::home_dir().unwrap();
        let at = |exe: PathBuf, home_dir: PathBuf| Entry { exe, args: vec!["--tray".into(), "--home-dir".into(), home_dir.to_string_lossy().into_owned()] };
        let fine = home.join(".domovoi");
        assert_eq!(at(PathBuf::from("/Applications/Domovoi Server.app/Contents/MacOS/domovoi-server"), fine.clone()).problem(), None);
        let in_documents = at(home.join("Documents/code/domovoi/target/release/domovoi-server"), fine.clone()).problem().unwrap();
        assert!(in_documents.contains("~/Documents"), "{in_documents}");
        assert!(at(PathBuf::from("/usr/local/bin/domovoi-server"), home.join("Desktop/domovoi")).problem().unwrap().contains("~/Desktop"));
        assert!(at(PathBuf::from("/usr/local/bin/domovoi-server"), home.join("Library/Mobile Documents/x")).problem().is_some());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_app_bundles_own_executable_starts_with_the_icon() {
        let dir = std::env::temp_dir().join(format!("domovoi-app-exe-{}", std::process::id()));
        let contents = dir.join("Domovoi Server.app").join("Contents");
        std::fs::create_dir_all(contents.join("MacOS")).unwrap();
        let exe = contents.join("MacOS").join("domovoi-server");
        // No Info.plist yet: not an app bundle.
        assert!(!is_app_executable(&exe));
        std::fs::write(contents.join("Info.plist"), "<plist/>").unwrap();
        assert!(is_app_executable(&exe));
        assert!(!is_app_executable(&dir.join("domovoi-server")));
        assert!(!is_app_executable(std::path::Path::new("/usr/local/bin/domovoi-server")));
        let not_an_app = dir.join("Domovoi Server").join("Contents");
        std::fs::create_dir_all(not_an_app.join("MacOS")).unwrap();
        std::fs::write(not_an_app.join("Info.plist"), "<plist/>").unwrap();
        assert!(!is_app_executable(&not_an_app.join("MacOS").join("domovoi-server")));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn the_desktop_entry_quotes_its_command() {
        let text = desktop_entry(&entry());
        assert!(text.starts_with("[Desktop Entry]\nType=Application\n"));
        assert!(text.contains(r#"Exec="/Applications/Domovoi Server.app/Contents/MacOS/domovoi-server" --tray --home-dir "/Users/me/Domovoi & co" --port 13773"#), "{text}");
        let odd = Entry { exe: PathBuf::from("/opt/domovoi"), args: vec![r#"a"b$c\d%e"#.into()] };
        assert!(desktop_entry(&odd).contains(r#"Exec=/opt/domovoi "a\"b\$c\\d%%e""#), "{}", desktop_entry(&odd));
    }

    #[test]
    fn the_windows_command_follows_argv_quoting() {
        let e = Entry { exe: PathBuf::from(r"C:\Program Files\Domovoi\domovoi-server.exe"), args: vec!["--tray".into(), "--home-dir".into(), r"C:\Users\me\domovoi home\".into(), r#"say "hi""#.into()] };
        assert_eq!(windows_command(&e), r#""C:\Program Files\Domovoi\domovoi-server.exe" --tray --home-dir "C:\Users\me\domovoi home\\" "say \"hi\"""#);
    }

    #[cfg(not(windows))]
    #[test]
    fn turning_it_on_and_off_writes_and_removes_the_entry() {
        // The directory override is process-wide: this is the only test using it.
        let dir = std::env::temp_dir().join(format!("domovoi-autostart-{}", crate::util::new_uuid()));
        // SAFETY: no other thread reads or writes this variable.
        unsafe { std::env::set_var("DOMOVOI_AUTOSTART_DIR", &dir) };
        assert!(!is_enabled());
        let entry = || Entry { exe: PathBuf::from("/opt/domovoi/domovoi-server"), args: vec!["--tray".into(), "--home-dir".into(), "/var/domovoi".into()] };
        enable(&entry()).unwrap();
        assert!(is_enabled() && plain_is_current(&entry()));
        // The executable moved: the entry is rewritten at start.
        let moved = Entry { exe: PathBuf::from("/opt/domovoi-server"), ..entry() };
        assert!(!plain_is_current(&moved));
        ensure(&moved);
        assert!(plain_is_current(&moved));
        disable().unwrap();
        assert!(!is_enabled());
        disable().unwrap();
        unsafe { std::env::remove_var("DOMOVOI_AUTOSTART_DIR") };
        let _ = std::fs::remove_dir_all(dir);
    }
}
