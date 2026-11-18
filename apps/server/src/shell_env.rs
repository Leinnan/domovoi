//! The PATH your terminal has, for a server started without one.
//!
//! Started by launchd (the LaunchAgent, Finder, Login Items) the server gets
//! only `/usr/bin:/bin:/usr/sbin:/sbin`, so `claude` in `~/.local/bin`,
//! `codex` from a Node version manager and Homebrew's `gh` are not found.
//! Like t3code's `fixPath`, the login shell is asked for its PATH once at
//! startup, before any thread starts, and that goes in front of the inherited
//! one.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

/// Read from the login shell. `__MISE_DIFF` travels with mise's PATH (without
/// it `mise x` wrappers can re-run themselves forever); `JAVA_HOME` is only
/// set by profiles for keg-only JDKs.
const NAMES: [&str; 3] = ["PATH", "__MISE_DIFF", "JAVA_HOME"];
const TIMEOUT: Duration = Duration::from_secs(5);
/// A profile can be slow while the machine boots; waited for once.
const SLOW_TIMEOUT: Duration = Duration::from_secs(20);

/// What happened, for the log (written once logging is up).
#[derive(Debug, Default)]
pub struct Report {
    /// The shell the PATH came from, or `launchctl`.
    pub source: Option<String>,
    pub entries: usize,
    pub warnings: Vec<String>,
}

impl Report {
    pub fn log(&self) {
        for w in &self.warnings {
            tracing::warn!("{w}");
        }
        match &self.source {
            Some(source) => tracing::info!("PATH from {source}: {} folders", self.entries),
            None => tracing::info!("PATH: {} folders (no login shell PATH)", self.entries),
        }
    }
}

/// Puts the login shell's PATH (and friends) into this process's environment.
/// Call before any other thread starts. `DOMOVOI_NO_SHELL_ENV=1` turns it off.
pub fn hydrate() -> Report {
    let mut report = Report::default();
    if cfg!(windows) || std::env::var_os("DOMOVOI_NO_SHELL_ENV").is_some_and(|v| !v.is_empty()) {
        report.entries = std::env::var("PATH").map(|p| split(&p).len()).unwrap_or(0);
        return report;
    }
    let mut found = Vec::new();
    let mut waited = false;
    for shell in shell_candidates() {
        match read_login_shell(&shell, TIMEOUT) {
            Ok(values) => found = values,
            Err(Failure::TimedOut) if !waited => {
                waited = true;
                report.warnings.push(format!("{shell} took over {}s to start; waiting longer", TIMEOUT.as_secs()));
                match read_login_shell(&shell, SLOW_TIMEOUT) {
                    Ok(values) => found = values,
                    Err(e) => report.warnings.push(format!("could not read PATH from {shell}: {e}")),
                }
            }
            Err(e) => report.warnings.push(format!("could not read PATH from {shell}: {e}")),
        }
        if value(&found, "PATH").is_some() {
            report.source = Some(shell);
            break;
        }
    }
    // An interactive profile that hangs: a login-only one (no .zshrc) still
    // has Homebrew's and most installers' PATH.
    if value(&found, "PATH").is_none()
        && let Some(shell) = shell_candidates().into_iter().next()
    {
        match read_shell(&shell, "-lc", TIMEOUT) {
            Ok(values) if value(&values, "PATH").is_some() => {
                found = values;
                report.source = Some(format!("{shell} (login only)"));
            }
            Ok(_) => {}
            Err(e) => report.warnings.push(format!("could not read PATH from {shell} -l: {e}")),
        }
    }
    let mut shell_path = value(&found, "PATH").map(str::to_owned);
    if shell_path.is_none() && cfg!(target_os = "macos") {
        shell_path = launchctl_path();
        if shell_path.is_some() {
            report.source = Some("launchctl".into());
        }
    }
    let inherited = std::env::var("PATH").unwrap_or_default();
    let known = well_known_dirs(&crate::util::home_dir());
    let merged = merge(&[shell_path.as_deref().unwrap_or(""), &inherited, &known.join(":")]);
    report.entries = split(&merged).len();
    // SAFETY: called from `main` before the runtime, the log writer or any
    // other thread exists, so nothing reads the environment concurrently.
    unsafe {
        std::env::set_var("PATH", &merged);
        if shell_path.is_some()
            && let Some(diff) = value(&found, "__MISE_DIFF")
        {
            std::env::set_var("__MISE_DIFF", diff);
        }
        if std::env::var_os("JAVA_HOME").is_none()
            && let Some(java) = value(&found, "JAVA_HOME")
        {
            std::env::set_var("JAVA_HOME", java);
        }
    }
    report
}

#[derive(Debug)]
enum Failure {
    TimedOut,
    Other(String),
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Failure::TimedOut => write!(f, "timed out"),
            Failure::Other(e) => write!(f, "{e}"),
        }
    }
}

/// `$SHELL`, the account's login shell, then the platform default.
fn shell_candidates() -> Vec<String> {
    let fallback = if cfg!(target_os = "macos") { "/bin/zsh" } else { "/bin/bash" };
    let mut out: Vec<String> = Vec::new();
    for s in [std::env::var("SHELL").ok(), account_shell(), Some(fallback.to_owned())].into_iter().flatten() {
        let s = s.trim().to_owned();
        if !s.is_empty() && !out.contains(&s) {
            out.push(s);
        }
    }
    out
}

#[cfg(unix)]
fn account_shell() -> Option<String> {
    // SAFETY: getpwuid returns a pointer into static storage or null; it is
    // read at once, before anything else could call it (single-threaded).
    unsafe {
        let pw = libc::getpwuid(libc::getuid());
        if pw.is_null() || (*pw).pw_shell.is_null() {
            return None;
        }
        Some(std::ffi::CStr::from_ptr((*pw).pw_shell).to_string_lossy().into_owned())
    }
}

#[cfg(not(unix))]
fn account_shell() -> Option<String> {
    None
}

fn start_marker(name: &str) -> String {
    format!("__DOMOVOI_ENV_{name}_START__")
}

fn end_marker(name: &str) -> String {
    format!("__DOMOVOI_ENV_{name}_END__")
}

/// Prints each name between markers, so profile output around it is ignored.
fn capture_command(shell: &str) -> String {
    let capture = NAMES
        .iter()
        // `env printf`, not the shell's own: a shell may buffer what its
        // builtins print until it exits, and a lingering one never does.
        .map(|n| format!("env printf '%s\\n' '{}'; printenv {n} || true; env printf '%s\\n' '{}'", start_marker(n), end_marker(n)))
        .collect::<Vec<_>>()
        .join("; ");
    // A DEBUG trap that echoes commands (into a terminal title, say) would
    // print the markers too.
    let name = Path::new(shell).file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default();
    if matches!(name.as_str(), "bash" | "ksh" | "zsh" | "sh") { format!("(trap - DEBUG) 2>/dev/null && trap - DEBUG; {capture}") } else { capture }
}

fn parse(output: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for name in NAMES {
        let (start, end) = (start_marker(name), end_marker(name));
        let Some(from) = output.find(&start) else { continue };
        let rest = &output[from + start.len()..];
        let Some(to) = rest.find(&end) else { continue };
        let value = rest[..to].trim();
        if !value.is_empty() {
            out.push((name.to_owned(), value.to_owned()));
        }
    }
    out
}

fn value<'a>(values: &'a [(String, String)], name: &str) -> Option<&'a str> {
    values.iter().find(|(n, _)| n == name).map(|(_, v)| v.as_str())
}

fn read_login_shell(shell: &str, timeout: Duration) -> Result<Vec<(String, String)>, Failure> {
    read_shell(shell, "-ilc", timeout)
}

/// Runs `<shell> <flags> <capture>` and returns once the last marker is
/// printed: what a profile does after that (exit hooks, history sync, a
/// background job holding stdout) is not waited for; the shell is killed.
fn read_shell(shell: &str, flags: &str, timeout: Duration) -> Result<Vec<(String, String)>, Failure> {
    let mut child = crate::process::blocking_command(shell)
        .args([flags, &capture_command(shell)])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| Failure::Other(e.to_string()))?;
    // Read on a thread, chunk by chunk, so the markers are seen as they come.
    let mut stdout = child.stdout.take().ok_or_else(|| Failure::Other("no stdout".into()))?;
    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
    std::thread::spawn(move || {
        let mut chunk = [0u8; 4096];
        while let Ok(n) = stdout.read(&mut chunk) {
            if n == 0 || tx.send(chunk[..n].to_vec()).is_err() {
                break;
            }
        }
    });
    let last = end_marker(NAMES[NAMES.len() - 1]);
    let deadline = Instant::now() + timeout;
    let mut out = Vec::new();
    loop {
        match rx.recv_timeout(Duration::from_millis(20)) {
            Ok(chunk) => out.extend_from_slice(&chunk),
            // stdout closed: whatever it printed is all there is.
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
        }
        if String::from_utf8_lossy(&out).contains(&last) {
            break;
        }
        if matches!(child.try_wait(), Ok(Some(_))) {
            // Gone; a grandchild may still hold stdout, so drain briefly.
            let grace = Instant::now() + Duration::from_millis(200);
            while let Some(left) = grace.checked_duration_since(Instant::now()) {
                match rx.recv_timeout(left) {
                    Ok(chunk) => out.extend_from_slice(&chunk),
                    Err(_) => break,
                }
            }
            break;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Failure::TimedOut);
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    Ok(parse(&String::from_utf8_lossy(&out)))
}

fn launchctl_path() -> Option<String> {
    let out = crate::process::output_blocking(crate::process::blocking_command("/bin/launchctl").args(["getenv", "PATH"]), crate::process::QUICK).ok()?;
    let s = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    (!s.is_empty()).then_some(s)
}

/// Where CLIs usually live, for a profile that sets no PATH or failed.
/// Only folders that exist.
pub fn well_known_dirs(home: &Path) -> Vec<String> {
    let candidates: Vec<PathBuf> = vec![
        home.join(".local/bin"),
        "/opt/homebrew/bin".into(),
        "/usr/local/bin".into(),
        home.join(".bun/bin"),
        home.join(".cargo/bin"),
        home.join(".npm-global/bin"),
        home.join(".volta/bin"),
        home.join(".nix-profile/bin"),
        "/home/linuxbrew/.linuxbrew/bin".into(),
        "/snap/bin".into(),
    ];
    candidates.into_iter().filter(|p| p.is_dir()).map(|p| p.to_string_lossy().into_owned()).collect()
}

fn split(path: &str) -> Vec<&str> {
    path.split(':').map(str::trim).filter(|e| !e.is_empty()).collect()
}

/// Joins PATH values in order, each folder once.
pub fn merge(paths: &[&str]) -> String {
    let mut seen = std::collections::HashSet::new();
    let mut out: Vec<&str> = Vec::new();
    for p in paths {
        for e in split(p) {
            if seen.insert(e) {
                out.push(e);
            }
        }
    }
    out.join(":")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_between_markers_ignore_profile_output() {
        let out = format!(
            "Welcome!\nlast login\n{}\n/a/bin:/usr/bin\n{}\n{}\n{}\n{}\n/opt/jdk\n{}\nbye\n",
            start_marker("PATH"),
            end_marker("PATH"),
            start_marker("__MISE_DIFF"),
            end_marker("__MISE_DIFF"),
            start_marker("JAVA_HOME"),
            end_marker("JAVA_HOME")
        );
        let v = parse(&out);
        assert_eq!(value(&v, "PATH"), Some("/a/bin:/usr/bin"));
        assert_eq!(value(&v, "__MISE_DIFF"), None, "an empty value is no value");
        assert_eq!(value(&v, "JAVA_HOME"), Some("/opt/jdk"));
        assert!(parse("no markers at all").is_empty());
    }

    #[test]
    fn merge_keeps_order_and_drops_repeats() {
        assert_eq!(merge(&["/opt/homebrew/bin:/usr/bin", "/usr/bin:/bin: ", "", "/home/u/.local/bin:/bin"]), "/opt/homebrew/bin:/usr/bin:/bin:/home/u/.local/bin");
    }

    #[test]
    fn a_real_shell_reports_its_path() {
        let v = read_login_shell("/bin/sh", TIMEOUT).unwrap();
        assert!(value(&v, "PATH").is_some_and(|p| p.contains("/bin")));
    }

    #[test]
    fn a_shell_that_lingers_after_printing_is_not_waited_for() {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("domovoi-shellenv-{}", crate::util::new_uuid()));
        std::fs::create_dir_all(&dir).unwrap();
        // Prints like a shell, then hangs the way a slow exit hook would.
        let shell = dir.join("sh");
        std::fs::write(&shell, "#!/bin/sh\neval \"$2\"\nsleep 30\n").unwrap();
        std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o755)).unwrap();
        let started = Instant::now();
        let v = read_shell(&shell.to_string_lossy(), "-ilc", TIMEOUT);
        let _ = std::fs::remove_dir_all(&dir);
        assert!(value(&v.unwrap(), "PATH").is_some());
        assert!(started.elapsed() < Duration::from_secs(3), "took {:?}", started.elapsed());
    }

    #[test]
    fn well_known_dirs_only_lists_folders_that_exist() {
        let home = std::env::temp_dir().join(format!("domovoi-shellenv-{}", crate::util::new_uuid()));
        std::fs::create_dir_all(home.join(".local/bin")).unwrap();
        let dirs = well_known_dirs(&home);
        let _ = std::fs::remove_dir_all(&home);
        assert!(dirs[0].ends_with(".local/bin"));
        assert!(!dirs.iter().any(|d| d.ends_with(".bun/bin")));
    }
}
