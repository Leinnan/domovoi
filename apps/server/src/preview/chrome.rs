//! The headless browser's executable and process (t3code `PreviewBrowser.ts`
//! and `ServerBrowserContexts.ts`).
//!
//! t3code downloads a pinned `chrome-headless-shell` into its home and
//! launches it through Playwright. Domovoi downloads nothing: it uses
//! `DOMOVOI_PREVIEW_BROWSER_BINARY` when set, else a shell t3code installed
//! in this home (`tools/chrome-headless-shell/<platform>/<version>/`), else a
//! locally installed Chrome, Chromium or Edge, and drives it over the DevTools
//! protocol (`cdp.rs`). With none of these the browser is unavailable and
//! says so (t3code's `PreviewBrowserUnsupportedError` message, reworded).
//!
//! Each persistent profile is one browser process with its own user-data
//! folder (`<userdata>/server-browser/profiles/<id>`, as t3code's
//! `launchPersistentContext`); incognito tabs and HTML previews get a fresh
//! browser context in the default profile's process.
//!
//! Limits: no download or install; no sandbox or missing-library diagnosis
//! (`PreviewBrowserHost.ts`, Linux hosts); no agent scope wrapper.

use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long a browser has to say where its DevTools endpoint is.
const LAUNCH_TIMEOUT: Duration = Duration::from_secs(20);

/// Overrides where the browser is found; a path that does not exist makes it unavailable.
pub const BINARY_ENV: &str = "DOMOVOI_PREVIEW_BROWSER_BINARY";

/// `T3CODE_SERVER_BROWSER_SANDBOX=0` turns Chrome's sandbox off (t3code `sandboxDisabled`).
const SANDBOX_ENV: &str = "T3CODE_SERVER_BROWSER_SANDBOX";

/// Why the browser cannot start.
#[derive(Debug, Clone, thiserror::Error)]
pub enum LaunchError {
    /// No browser is installed (t3code: `PreviewBrowserUnsupportedError`).
    #[error("T3's headless browser is not available on this machine: no Chrome, Chromium or Edge is installed. Install one, or set {BINARY_ENV} to its executable, then try again.")]
    Unavailable,
    /// This build has no browser (`--no-default-features` without `preview-browser`).
    #[cfg_attr(feature = "preview-browser", expect(dead_code, reason = "only a build without the browser says so"))]
    #[error("This T3 Code server was built without its headless browser (the `preview-browser` feature).")]
    NotBuilt,
    /// t3code `ServerBrowserLaunchError`.
    #[error("The server preview browser could not start: {0}")]
    Failed(String),
}

/// Where t3code's own downloaded shell lives for this platform.
fn shell_platform() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Some("linux64"),
        ("linux", "aarch64") => Some("linux-arm64"),
        ("macos", "aarch64") => Some("mac-arm64"),
        ("macos", "x86_64") => Some("mac-x64"),
        ("windows", "x86_64" | "aarch64") => Some("win64"),
        ("windows", "x86") => Some("win32"),
        _ => None,
    }
}

fn is_executable(path: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        path.is_file()
    }
}

/// The newest `chrome-headless-shell` installed under `home`, if any.
fn installed_shell(home: &Path) -> Option<PathBuf> {
    let root = home.join("tools").join("chrome-headless-shell").join(shell_platform()?);
    let name = if cfg!(windows) { "chrome-headless-shell.exe" } else { "chrome-headless-shell" };
    let mut versions: Vec<String> = std::fs::read_dir(&root).ok()?.flatten().filter_map(|e| e.file_name().into_string().ok()).filter(|n| !n.starts_with('.')).collect();
    versions.sort_by(|a, b| version_key(b).cmp(&version_key(a)));
    versions.into_iter().map(|v| root.join(v).join(name)).find(|p| is_executable(p))
}

fn version_key(v: &str) -> Vec<u64> {
    v.split('.').map(|part| part.parse().unwrap_or(0)).collect()
}

/// Locally installed browsers, most capable first.
fn system_browsers() -> Vec<PathBuf> {
    let mut found = vec![];
    if cfg!(target_os = "macos") {
        let apps = [
            "Google Chrome.app/Contents/MacOS/Google Chrome",
            "Chromium.app/Contents/MacOS/Chromium",
            "Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
            "Google Chrome Canary.app/Contents/MacOS/Google Chrome Canary",
        ];
        let mut roots = vec![PathBuf::from("/Applications")];
        if let Some(home) = dirs::home_dir() {
            roots.push(home.join("Applications"));
        }
        for root in roots {
            found.extend(apps.iter().map(|app| root.join(app)));
        }
    } else if cfg!(windows) {
        for var in ["PROGRAMFILES", "PROGRAMFILES(X86)", "LOCALAPPDATA"] {
            if let Some(base) = std::env::var_os(var).map(PathBuf::from) {
                found.push(base.join("Google/Chrome/Application/chrome.exe"));
                found.push(base.join("Microsoft/Edge/Application/msedge.exe"));
                found.push(base.join("Chromium/Application/chrome.exe"));
            }
        }
    }
    let names = ["google-chrome", "google-chrome-stable", "chromium", "chromium-browser", "microsoft-edge", "microsoft-edge-stable", "chrome"];
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            found.extend(names.iter().map(|name| dir.join(name)));
        }
    }
    found
}

/// The browser to run: the override, t3code's shell in `home`, or one installed here.
pub fn executable(home: &Path) -> Result<PathBuf, LaunchError> {
    if let Some(path) = std::env::var_os(BINARY_ENV).filter(|v| !v.is_empty()) {
        let path = PathBuf::from(path);
        return if is_executable(&path) { Ok(path) } else { Err(LaunchError::Unavailable) };
    }
    installed_shell(home).or_else(|| system_browsers().into_iter().find(|p| is_executable(p))).ok_or(LaunchError::Unavailable)
}

/// A browser process and its DevTools endpoint. Dropping it kills the process.
pub struct Process {
    pub port: u16,
    pub path: String,
    child: tokio::process::Child,
    /// A user-data folder of its own, removed with it.
    scratch: Option<PathBuf>,
}

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
        if let Some(dir) = self.scratch.take() {
            // The process may still hold files for a moment.
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(500));
                let _ = std::fs::remove_dir_all(dir);
            });
        }
    }
}

/// Starts `exe` headless with `user_data` as its profile folder (`scratch`:
/// a folder to remove when it stops), and waits for its DevTools endpoint.
pub async fn launch(exe: &Path, user_data: &Path, scratch: bool) -> Result<Process, LaunchError> {
    let failed = |e: &dyn std::fmt::Display| LaunchError::Failed(e.to_string());
    std::fs::create_dir_all(user_data).map_err(|e| failed(&e))?;
    let port_file = user_data.join("DevToolsActivePort");
    let _ = std::fs::remove_file(&port_file);
    let mut cmd = crate::process::command(exe);
    cmd.args([
        "--headless=new",
        "--remote-debugging-port=0",
        "--no-first-run",
        "--no-default-browser-check",
        "--disable-gpu",
        "--disable-blink-features=AutomationControlled",
        "--disable-background-networking",
        "--disable-component-update",
        "--disable-sync",
        // A page restored from the back-forward cache fires no load; every history step loads.
        "--disable-features=Translate,MediaRouter,OptimizationHints,BackForwardCache",
        "--metrics-recording-only",
        "--mute-audio",
        "--password-store=basic",
        "--use-mock-keychain",
        "--hide-crash-restore-bubble",
    ]);
    cmd.arg(format!("--user-data-dir={}", user_data.display()));
    if std::env::var(SANDBOX_ENV).as_deref() == Ok("0") {
        cmd.arg("--no-sandbox");
    }
    cmd.arg("about:blank");
    cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    let mut child = cmd.spawn().map_err(|e| failed(&e))?;
    let deadline = tokio::time::Instant::now() + LAUNCH_TIMEOUT;
    loop {
        if let Ok(text) = std::fs::read_to_string(&port_file) {
            let mut lines = text.lines();
            if let (Some(port), Some(path)) = (lines.next().and_then(|p| p.trim().parse::<u16>().ok()), lines.next()) {
                return Ok(Process { port, path: path.trim().to_owned(), child, scratch: scratch.then(|| user_data.to_owned()) });
            }
        }
        if let Ok(Some(status)) = child.try_wait() {
            return Err(LaunchError::Failed(format!("the browser exited at start ({status})")));
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(LaunchError::Failed("the browser did not open its DevTools endpoint within 20 seconds".into()));
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// `profileDirectory`: one folder per profile id, `.` and `..` spelled out.
pub fn profile_dir(userdata: &Path, profile_id: &str) -> PathBuf {
    // `encodeURIComponent`.
    let encoded: String = profile_id
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect();
    let encoded = if profile_id == "." || profile_id == ".." { encoded.replace('.', "%2E") } else { encoded };
    userdata.join("server-browser").join("profiles").join(encoded)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_folders_are_one_level_and_named_by_id() {
        let root = Path::new("/u");
        assert_eq!(profile_dir(root, "default"), Path::new("/u/server-browser/profiles/default"));
        assert_eq!(profile_dir(root, "a/b"), Path::new("/u/server-browser/profiles/a%2Fb"));
        assert_eq!(profile_dir(root, ".."), Path::new("/u/server-browser/profiles/%2E%2E"));
        assert!(version_key("157.0.8103.78") > version_key("99.1"));
    }

    #[test]
    fn a_missing_override_makes_the_browser_unavailable() {
        // Read through the function the override feeds, not the environment of the test run.
        assert!(!is_executable(Path::new("/nonexistent/chrome")));
    }
}
