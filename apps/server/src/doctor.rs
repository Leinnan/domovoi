//! `domovoi-server doctor [--json]`: what a server using this home directory
//! needs, checked without starting one or changing anything. Each check is
//! `ok`, `warn` or `fail` with a one-line detail; any `fail` makes the exit
//! code nonzero. t3code's `t3 doctor` (`cli/doctor.ts`) inspects and
//! compacts the database; Domovoi's checks the whole setup instead and
//! changes nothing (compaction is `server.reclaimDatabaseSpace`).
//!
//! Checked: the home directory (exists, is a directory, is writable, who else
//! may read or write it); the folders and files that must be private; the database (opens read-only, `PRAGMA integrity_check`);
//! each provider CLI (found as the server finds it, `--version` with a hard
//! timeout); `git`, `git lfs`, `gh` and its sign-in; the iroh key and whether
//! remote access is on; launch at login and (Linux) the systemd user service;
//! the descriptor apps on this machine sign in by; whether a server already
//! answers on the port, and whether it is this home's and ready.
//!
//! `--records` also reads every stored thread, record and project the way the
//! server does (`orchestration/audit.rs`), and with `--events` the logged
//! events: the rows this version cannot decode are counted by table and kind
//! and the first few named. The server leaves such a row out, so they are a
//! warning, not a failure.
//!
//! Limits: no database compaction or legacy-database report (t3code's
//! `--fix`); the iroh relays are not dialled (that needs the network and a
//! started endpoint); provider sign-in is not probed (the server's provider
//! list shows it); provider instances from the settings are not checked
//! one by one, only the CLIs the built-in drivers run.

use crate::orchestration::audit::Group;
use crate::providers::Tool;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// How long one probe (`--version`, `gh auth status`) may take.
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);
/// How long the server on the port gets to answer.
const PORT_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Ok,
    Warn,
    Fail,
}

impl Status {
    fn word(self) -> &'static str {
        match self {
            Status::Ok => "ok",
            Status::Warn => "warn",
            Status::Fail => "fail",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Check {
    pub name: String,
    pub status: Status,
    pub detail: String,
}

impl Check {
    fn new(name: impl Into<String>, status: Status, detail: impl AsRef<str>) -> Self {
        Check { name: name.into(), status, detail: one_line(detail.as_ref()) }
    }
}

/// What `doctor --json` prints.
#[derive(Debug, Serialize)]
pub struct Report {
    /// No check failed.
    pub ok: bool,
    pub checks: Vec<Check>,
    /// With `--records`: the stored records by table and kind.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub records: Option<Vec<Group>>,
}

/// `doctor --records`: what is read beside the threads, their records and
/// the projects.
#[derive(Debug, Clone, Copy)]
pub struct Records {
    pub events: bool,
}

impl Report {
    pub fn new(checks: Vec<Check>) -> Self {
        Report { ok: checks.iter().all(|c| c.status != Status::Fail), checks, records: None }
    }

    /// The report with the stored records read: a row that does not decode
    /// is a warning (the server leaves it out and serves the rest).
    fn with_records(mut self, read: Result<Vec<Group>, String>) -> Self {
        let name = "records";
        match read {
            Ok(groups) => {
                let (rows, unread): (u64, u64) = groups.iter().fold((0, 0), |(rows, unread), g| (rows + g.rows, unread + g.unread));
                let status = if unread == 0 { Status::Ok } else { Status::Warn };
                let detail = if unread == 0 { format!("{rows} stored records decode") } else { format!("{unread} of {rows} stored records do not decode") };
                self.checks.push(Check::new(name, status, detail));
                self.records = Some(groups);
            }
            Err(e) => {
                self.checks.push(Check::new(name, Status::Fail, format!("the stored records could not be read: {e}")));
                self.ok = false;
            }
        }
        self
    }

    /// One aligned line per check, then a summary.
    pub fn text(&self) -> String {
        let width = self.checks.iter().map(|c| c.name.len()).max().unwrap_or(0);
        let mut out = String::new();
        for c in &self.checks {
            out.push_str(&format!("{:<4}  {:<width$}  {}\n", c.status.word(), c.name, c.detail));
        }
        let count = |s: Status| self.checks.iter().filter(|c| c.status == s).count();
        let (failed, warned) = (count(Status::Fail), count(Status::Warn));
        out.push_str(&match (failed, warned) {
            (0, 0) => "All checks passed.".to_owned(),
            (0, w) => format!("No failures; {w} warning(s)."),
            (f, w) => format!("{f} check(s) failed; {w} warning(s)."),
        });
        if let Some(groups) = &self.records {
            let kind = groups.iter().map(|g| g.kind.len()).max().unwrap_or(0);
            out.push_str("\n\nStored records:\n");
            for g in groups {
                out.push_str(&format!("  {:<15}  {:<kind$}  {:>8} rows  {:>9.1} MB  {} unread\n", g.table, g.kind, g.rows, g.bytes as f64 / 1_048_576.0, g.unread));
                for sample in &g.samples {
                    out.push_str(&format!("      {}\n", one_line(sample)));
                }
            }
        }
        out
    }
}

/// The error `main` exits nonzero for without printing anything more.
#[derive(Debug)]
pub struct Failed;

impl std::fmt::Display for Failed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("doctor found failures")
    }
}

impl std::error::Error for Failed {}

/// Collapses whitespace runs (newlines included) to single spaces.
fn one_line(text: &str) -> String {
    let line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if line.is_empty() { "-".to_owned() } else { line }
}

/// `doctor`: prints the report; `Err(Failed)` when a check failed.
pub async fn run(home: &Path, host: &str, port: u16, relays: &[String], json: bool, records: Option<Records>) -> crate::Fallible {
    let mut report = Report::new(checks(home, host, port, relays).await);
    if let Some(records) = records {
        let database = home.join("userdata").join("domovoi.sqlite");
        let read = tokio::task::spawn_blocking(move || read_records(&database, records)).await.unwrap_or_else(|e| Err(e.to_string()));
        report = report.with_records(read);
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("{}", report.text());
    }
    if report.ok { Ok(()) } else { Err(Box::new(Failed)) }
}

async fn checks(home: &Path, host: &str, port: u16, relays: &[String]) -> Vec<Check> {
    let database = home.join("userdata").join("domovoi.sqlite");
    let mut out = vec![check_home(home)];
    let (db_check, environment_id) = {
        let database = database.clone();
        tokio::task::spawn_blocking(move || check_database(&database)).await.unwrap_or_else(|e| (Check::new("database", Status::Fail, e.to_string()), None))
    };
    out.push(db_check);
    let tools = [
        (crate::providers::CLAUDE, crate::providers::CLAUDE_TOOL),
        (crate::providers::CODEX, crate::providers::CODEX_TOOL),
        (crate::providers::OPENCODE, crate::adapters::opencode::OPENCODE_TOOL),
        (crate::providers::PI, crate::pi_rpc::PI_TOOL),
        (crate::antigravity::DRIVER, crate::antigravity::TOOL),
    ];
    let providers = futures::future::join_all(tools.into_iter().map(|(name, tool)| check_provider(name, tool)));
    let (providers, git, lfs, gh, listening) = tokio::join!(providers, check_git(), check_lfs(), check_gh(), check_port(host, port, environment_id.as_deref()));
    out.extend(providers);
    out.extend([git, lfs, gh]);
    out.push(check_remote(home, relays));
    out.push(check_secrets(home));
    out.push(check_private(home));
    out.push(check_local(home, environment_id.as_deref()));
    out.push(check_autostart(home, host, port, relays));
    if let Some(service) = crate::service::summary() {
        out.push(Check::new("service", Status::Ok, format!("systemd user service {service}")));
    }
    out.push(listening);
    out
}

fn check_home(home: &Path) -> Check {
    let name = "home";
    let shown = home.display();
    let meta = match std::fs::metadata(home) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Check::new(name, Status::Warn, format!("{shown} does not exist yet (the server creates it)")),
        Err(e) => return Check::new(name, Status::Fail, format!("{shown}: {e}")),
    };
    if !meta.is_dir() {
        return Check::new(name, Status::Fail, format!("{shown} is not a directory"));
    }
    // Writable in fact, not by its mode bits: the owner may be another user.
    let probe = home.join(format!(".doctor-{}", std::process::id()));
    if let Err(e) = std::fs::write(&probe, b"") {
        return Check::new(name, Status::Fail, format!("{shown} is not writable: {e}"));
    }
    let _ = std::fs::remove_file(&probe);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = meta.permissions().mode() & 0o777;
        if mode & 0o022 != 0 {
            return Check::new(name, Status::Warn, format!("{shown} is writable by other users (mode {mode:03o}); run `chmod go-w {shown}`"));
        }
        if crate::state::open_to_others(mode) {
            // The server closes it at its next start, unless it is not its to close.
            return Check::new(name, Status::Warn, format!("{shown} is readable by other users (mode {mode:03o}); run `chmod 700 {shown}`"));
        }
        Check::new(name, Status::Ok, format!("{shown} (mode {mode:03o})"))
    }
    #[cfg(not(unix))]
    Check::new(name, Status::Ok, shown.to_string())
}

/// The database check, and the environment id the database names.
fn check_database(path: &Path) -> (Check, Option<String>) {
    let name = "database";
    if !path.exists() {
        return (Check::new(name, Status::Warn, format!("{} does not exist yet (the server creates it)", path.display())), None);
    }
    use rusqlite::OpenFlags;
    let conn = match rusqlite::Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX) {
        Ok(conn) => conn,
        Err(e) => return (Check::new(name, Status::Fail, format!("{} does not open: {e}", path.display())), None),
    };
    let _ = conn.busy_timeout(Duration::from_secs(5));
    let verdict = conn
        .prepare("PRAGMA integrity_check")
        .and_then(|mut statement| statement.query_map([], |row| row.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<String>>>());
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let check = match verdict {
        Ok(lines) if lines == ["ok"] => Check::new(name, Status::Ok, format!("integrity ok, {:.1} MB", size as f64 / 1_048_576.0)),
        Ok(lines) => Check::new(name, Status::Fail, format!("integrity_check: {} ({} problem(s))", lines.first().map(String::as_str).unwrap_or("no answer"), lines.len())),
        Err(e) => Check::new(name, Status::Fail, format!("integrity_check did not run: {e}")),
    };
    let environment_id = conn.query_row("SELECT value FROM meta WHERE key = 'environment_id'", [], |row| row.get::<_, String>(0)).ok();
    (check, environment_id)
}

/// Every stored record read as the server reads it, on a read-only connection.
fn read_records(path: &Path, records: Records) -> Result<Vec<Group>, String> {
    use rusqlite::OpenFlags;
    let conn = rusqlite::Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)
        .map_err(|e| format!("{} does not open: {e}", path.display()))?;
    let _ = conn.busy_timeout(Duration::from_secs(5));
    crate::orchestration::audit::records(&conn, records.events).map_err(|e| e.to_string())
}

/// Runs `cmd` with the probe timeout: its trimmed output (stdout, else
/// stderr) on success, why not otherwise.
async fn probe(mut cmd: tokio::process::Command, shown: &str) -> Result<String, String> {
    cmd.stdin(std::process::Stdio::null());
    match crate::process::output(cmd, PROBE_TIMEOUT).await {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout).trim().to_owned();
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_owned();
            let text = if stdout.is_empty() { stderr } else { stdout };
            let first = text.lines().next().unwrap_or_default().trim().to_owned();
            if out.status.success() { Ok(first) } else { Err(format!("`{shown}` failed: {}", if first.is_empty() { out.status.to_string() } else { first })) }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err("not installed".into()),
        Err(e) if e.kind() == std::io::ErrorKind::TimedOut => Err(format!("`{shown}` did not answer within {} s", PROBE_TIMEOUT.as_secs())),
        Err(e) => Err(format!("`{shown}` did not run: {e}")),
    }
}

async fn check_provider(driver: &str, tool: Tool) -> Check {
    let name = format!("provider:{driver}");
    let exe = match crate::providers::executable(tool) {
        Ok(exe) => exe,
        Err(_) => return Check::new(name, Status::Warn, format!("{} not found (install it, or set {} to its path)", tool.label, tool.env)),
    };
    let mut cmd = if tool.program == crate::pi_rpc::PI_TOOL.program { crate::pi_rpc::command(&exe) } else { crate::process::command(&exe) };
    cmd.arg("--version");
    match probe(cmd, &format!("{} --version", tool.program)).await {
        Ok(version) if !version.is_empty() => Check::new(name, Status::Ok, format!("{version} at {}", exe.display())),
        Ok(_) => Check::new(name, Status::Warn, format!("{} at {} printed no version", tool.label, exe.display())),
        Err(why) => Check::new(name, Status::Warn, format!("{why} ({})", exe.display())),
    }
}

async fn check_git() -> Check {
    let mut cmd = crate::process::command("git");
    cmd.arg("--version");
    match probe(cmd, "git --version").await {
        Ok(version) => Check::new("git", Status::Ok, version),
        // Checkpoints, diffs and worktrees all need it.
        Err(why) => Check::new("git", Status::Fail, why),
    }
}

async fn check_lfs() -> Check {
    let mut cmd = crate::process::command("git");
    cmd.args(["lfs", "version"]);
    match probe(cmd, "git lfs version").await {
        Ok(version) => Check::new("git-lfs", Status::Ok, version),
        Err(why) => Check::new("git-lfs", Status::Warn, format!("{why} (Git LFS files show as pointers)")),
    }
}

async fn check_gh() -> Check {
    let program = std::env::var("DOMOVOI_GH_BINARY").ok().filter(|v| !v.trim().is_empty()).unwrap_or_else(|| "gh".into());
    let mut cmd = crate::process::command(&program);
    cmd.arg("--version");
    let version = match probe(cmd, "gh --version").await {
        Ok(version) => version,
        Err(why) => return Check::new("gh", Status::Warn, format!("{why} (pull requests and issues need the GitHub CLI)")),
    };
    let mut cmd = crate::process::command(&program);
    cmd.args(["auth", "status"]).env("GH_PROMPT_DISABLED", "1").env("GH_NO_UPDATE_NOTIFIER", "1").env("NO_COLOR", "1");
    match probe(cmd, "gh auth status").await {
        Ok(_) => Check::new("gh", Status::Ok, format!("{version}, signed in")),
        Err(_) => Check::new("gh", Status::Warn, format!("{version}, not signed in: run `gh auth login`")),
    }
}

fn check_remote(home: &Path, relays: &[String]) -> Check {
    let name = "remote";
    let key_path = home.join("remote").join("secret.key");
    let settings: crate::remote::RemoteSettings =
        std::fs::read(home.join("remote.json")).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    let relays = if relays.is_empty() { settings.relays.clone() } else { relays.to_vec() };
    if let Err(e) = crate::remote::network(&relays) {
        return Check::new(name, Status::Fail, format!("relay setting: {e}"));
    }
    let relays = match relays.as_slice() {
        [] => "n0's public relays".to_owned(),
        r if r.iter().any(|r| r == "disabled") => "local network only".to_owned(),
        r => format!("relays {}", r.join(", ")),
    };
    let state = if settings.enabled { "on" } else { "off" };
    if !key_path.exists() {
        return Check::new(name, Status::Ok, format!("remote access {state}, no iroh key yet (made at first start)"));
    }
    let Some(key) = crate::remote::read_key(&key_path) else {
        return Check::new(name, Status::Fail, format!("{} is not a valid iroh key; remove it to make a new one (paired devices must pair again)", key_path.display()));
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&key_path).map(|m| m.permissions().mode() & 0o777).unwrap_or(0);
        if mode & 0o077 != 0 {
            return Check::new(name, Status::Warn, format!("{} is readable by other users (mode {mode:03o}); run `chmod 600` on it", key_path.display()));
        }
    }
    #[cfg(windows)]
    if crate::util::is_private(&key_path) == Some(false) {
        let fix = format!("icacls \"{}\" /inheritance:r /grant:r \"%USERNAME%:F\"", key_path.display());
        return Check::new(name, Status::Warn, format!("{} can be read by other accounts (its folder's permissions); run `{fix}`", key_path.display()));
    }
    Check::new(name, Status::Ok, format!("remote access {state} ({relays}), endpoint {}", key.public()))
}

/// The files that hold credentials (provider keys, the server's own
/// secrets, secret requests, the push key) besides the iroh key, which
/// `remote` checks: each must be readable by this user only.
fn check_secrets(home: &Path) -> Check {
    let name = "secrets";
    let userdata = home.join("userdata");
    let mut files = vec![home.join("webpush-vapid.p8")];
    for dir in ["secrets", "server-secrets", "secret-requests"] {
        files_under(&userdata.join(dir), &mut files);
    }
    files.retain(|f| f.is_file());
    let open: Vec<&PathBuf> = files.iter().filter(|f| crate::util::is_private(f) == Some(false)).collect();
    let Some(first) = open.first() else {
        return Check::new(name, Status::Ok, format!("{} file(s), readable by you only", files.len()));
    };
    let fix = if cfg!(windows) {
        format!("icacls \"{}\" /inheritance:r /grant:r \"%USERNAME%:F\"", first.display())
    } else {
        format!("chmod 600 {}", first.display())
    };
    let shown: Vec<String> = open.iter().take(3).map(|f| f.display().to_string()).collect();
    let more = if open.len() > 3 { format!(" and {} more", open.len() - 3) } else { String::new() };
    Check::new(name, Status::Warn, format!("readable by other users: {}{more}; run `{fix}` on each", shown.join(", ")))
}

/// The folders and files with threads, logs and addresses in them
/// (`Paths::private_paths`): each must be readable by this user only. The
/// server makes them so at every start; this says what it could not.
fn check_private(home: &Path) -> Check {
    let name = "private";
    let paths: Vec<PathBuf> = crate::state::Paths::private_paths(home).into_iter().filter(|p| p.exists()).collect();
    let open: Vec<&PathBuf> = paths.iter().filter(|p| crate::util::is_private(p) == Some(false)).collect();
    if open.is_empty() {
        return Check::new(name, Status::Ok, format!("{} path(s), readable by you only", paths.len()));
    }
    let shown: Vec<String> = open.iter().take(3).map(|p| p.display().to_string()).collect();
    let more = if open.len() > 3 { format!(" and {} more", open.len() - 3) } else { String::new() };
    Check::new(name, Status::Warn, format!("readable by other users: {}{more}; starting the server makes them private", shown.join(", ")))
}

/// Sign-in without a code for apps on this machine (`crate::local`): what
/// the descriptor a running server wrote says, read as an app reads it.
fn check_local(home: &Path, environment_id: Option<&str>) -> Check {
    let name = "local-sign-in";
    let path = domovoi_local::descriptor_path(home);
    if std::fs::symlink_metadata(&path).is_err() {
        return Check::new(name, Status::Ok, "no descriptor: apps on this machine pair with a code (it is written while a server listens on loopback, without --no-local-sign-in)");
    }
    let Some(descriptor) = domovoi_local::read_descriptor(&path).as_deref().and_then(domovoi_local::Descriptor::parse) else {
        return Check::new(name, Status::Warn, format!("{} is not a descriptor an app accepts (not a plain file of yours, writable by others, or not this version's); restart the server", path.display()));
    };
    if environment_id.is_some_and(|id| id != descriptor.environment_id) {
        return Check::new(name, Status::Warn, format!("{} names another server; restart the server", path.display()));
    }
    Check::new(name, Status::Ok, format!("apps on this machine sign in without a code at {} (while that server runs)", descriptor.url))
}

/// Every file under `dir` (none when it is missing), links not followed.
fn files_under(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let Ok(kind) = entry.file_type() else { continue };
        if kind.is_dir() {
            files_under(&entry.path(), out);
        } else if kind.is_file() {
            out.push(entry.path());
        }
    }
}

fn check_autostart(home: &Path, host: &str, port: u16, relays: &[String]) -> Check {
    let name = "autostart";
    if !crate::autostart::is_enabled() {
        return Check::new(name, Status::Ok, "launch at login is off");
    }
    match crate::autostart::Entry::current(home, host, port, relays, &[]) {
        Ok(entry) => match entry.problem() {
            Some(problem) => Check::new(name, Status::Warn, format!("launch at login is on, but {problem}")),
            None => Check::new(name, Status::Ok, "launch at login is on"),
        },
        Err(e) => Check::new(name, Status::Warn, format!("launch at login is on; this executable's path is unknown: {e}")),
    }
}

/// `/ready`'s body.
#[derive(serde::Deserialize)]
struct Ready {
    status: String,
}

async fn check_port(host: &str, port: u16, environment_id: Option<&str>) -> Check {
    let name = "port";
    let host = match host {
        "0.0.0.0" | "::" | "[::]" => "127.0.0.1",
        other => other,
    };
    let address = format!("{host}:{port}");
    match tokio::time::timeout(PORT_TIMEOUT, tokio::net::TcpStream::connect(&address)).await {
        Ok(Ok(_)) => {}
        Ok(Err(_)) => return Check::new(name, Status::Ok, format!("{address} is free; no server is running")),
        Err(_) => return Check::new(name, Status::Warn, format!("{address} did not answer within {} s", PORT_TIMEOUT.as_secs())),
    }
    let Ok(http) = reqwest::Client::builder().timeout(PORT_TIMEOUT).build() else {
        return Check::new(name, Status::Warn, format!("{address} is in use"));
    };
    let origin = format!("http://{address}");
    let environment = match http.get(format!("{origin}/.well-known/t3/environment")).send().await {
        Ok(res) => res.json::<domovoi_contracts::environment::EnvironmentDescriptor>().await.ok(),
        Err(_) => None,
    };
    let Some(environment) = environment else {
        return Check::new(name, Status::Warn, format!("{address} is in use by another program; pass --port"));
    };
    if environment_id != Some(environment.environment_id.as_str()) {
        return Check::new(name, Status::Warn, format!("{address} is used by another server ({}, {}); pass its --home-dir or another --port", environment.label, environment.server_version));
    }
    let ready = match http.get(format!("{origin}/ready")).send().await {
        Ok(res) => res.json::<Ready>().await.map(|r| r.status).unwrap_or_else(|_| "unknown".into()),
        Err(_) => "unknown".into(),
    };
    Check::new(name, Status::Ok, format!("this home's server is running on {address} ({}, {ready})", environment.server_version))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn database_path(home: &Path) -> PathBuf {
        home.join("userdata").join("domovoi.sqlite")
    }

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("domovoi-doctor-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_file(&dir);
        dir
    }

    #[test]
    fn the_report_is_one_line_per_check_and_fails_on_any_failure() {
        let report = Report::new(vec![
            Check::new("home", Status::Ok, "/h (mode 700)"),
            Check::new("provider:codex", Status::Warn, "`codex --version`\ndid not answer"),
            Check::new("git", Status::Fail, "not installed"),
        ]);
        assert!(!report.ok);
        assert_eq!(
            report.text(),
            "ok    home            /h (mode 700)\nwarn  provider:codex  `codex --version` did not answer\nfail  git             not installed\n1 check(s) failed; 1 warning(s)."
        );
        assert_eq!(
            serde_json::to_value(&report).unwrap(),
            serde_json::json!({ "ok": false, "checks": [
                { "name": "home", "status": "ok", "detail": "/h (mode 700)" },
                { "name": "provider:codex", "status": "warn", "detail": "`codex --version` did not answer" },
                { "name": "git", "status": "fail", "detail": "not installed" },
            ]})
        );
        let fine = Report::new(vec![Check::new("home", Status::Ok, ""), Check::new("gh", Status::Warn, "x")]);
        assert!(fine.ok);
        assert_eq!(fine.text(), "ok    home  -\nwarn  gh    x\nNo failures; 1 warning(s).");
        assert_eq!(Report::new(vec![]).text(), "All checks passed.");
    }

    #[test]
    fn the_home_and_database_checks() {
        let home = temp("home");
        assert_eq!(check_home(&home).status, Status::Warn, "missing");
        std::fs::write(&home, "x").unwrap();
        assert_eq!(check_home(&home).status, Status::Fail, "a file");
        std::fs::remove_file(&home).unwrap();
        std::fs::create_dir_all(home.join("userdata")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
            assert_eq!(check_home(&home).status, Status::Ok);
            std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o777)).unwrap();
            assert_eq!(check_home(&home).status, Status::Warn, "writable by others");
            std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o700)).unwrap();
        }

        let db = database_path(&home);
        assert_eq!(check_database(&db).0.status, Status::Warn, "not made yet");
        let made = crate::db::Db::open(&db).unwrap();
        made.meta_set("environment_id", "env-1").unwrap();
        drop(made);
        let (check, environment_id) = check_database(&db);
        assert_eq!(check.status, Status::Ok, "{check:?}");
        assert!(check.detail.starts_with("integrity ok"), "{check:?}");
        assert_eq!(environment_id.as_deref(), Some("env-1"));
        for suffix in ["-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", db.display()));
        }
        std::fs::write(&db, b"this is not a database, it is long enough to look like a header to nobody").unwrap();
        assert_eq!(check_database(&db).0.status, Status::Fail);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn the_private_and_local_sign_in_checks() {
        let home = temp("private");
        std::fs::create_dir_all(home.join("userdata")).unwrap();
        assert_eq!(check_local(&home, None).status, Status::Ok, "no descriptor");
        let local = crate::local::Local::new(&home);
        local.publish("127.0.0.1:13773".parse().unwrap(), "env-1", "Studio").unwrap();
        let check = check_local(&home, Some("env-1"));
        assert_eq!(check.status, Status::Ok, "{check:?}");
        assert!(check.detail.contains("http://127.0.0.1:13773"), "{check:?}");
        assert_eq!(check_local(&home, Some("env-2")).status, Status::Warn, "another server's");
        std::fs::write(domovoi_local::descriptor_path(&home), "{}").unwrap();
        assert_eq!(check_local(&home, Some("env-1")).status, Status::Warn, "not a descriptor");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(home.join("userdata"), std::fs::Permissions::from_mode(0o700)).unwrap();
            assert_eq!(check_private(&home).status, Status::Ok);
            std::fs::set_permissions(home.join("userdata"), std::fs::Permissions::from_mode(0o755)).unwrap();
            let check = check_private(&home);
            assert_eq!(check.status, Status::Warn);
            assert!(check.detail.contains("userdata"), "{check:?}");
            std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o755)).unwrap();
            assert_eq!(check_home(&home).status, Status::Warn, "readable by others");
            std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o711)).unwrap();
            assert_eq!(check_home(&home).status, Status::Ok, "others may only pass through");
        }
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn the_remote_check_reads_the_key_without_making_one() {
        let home = temp("remote");
        std::fs::create_dir_all(&home).unwrap();
        let check = check_remote(&home, &[]);
        assert_eq!(check.status, Status::Ok);
        assert!(check.detail.contains("no iroh key yet"), "{check:?}");
        assert!(!home.join("remote").exists(), "doctor makes no key");
        std::fs::create_dir_all(home.join("remote")).unwrap();
        std::fs::write(home.join("remote").join("secret.key"), "nothex").unwrap();
        assert_eq!(check_remote(&home, &[]).status, Status::Fail);
        std::fs::remove_file(home.join("remote").join("secret.key")).unwrap();
        let key = crate::remote::load_or_create_key(&home.join("remote").join("secret.key")).unwrap();
        std::fs::write(home.join("remote.json"), r#"{"enabled":true,"relays":["disabled"]}"#).unwrap();
        let check = check_remote(&home, &[]);
        assert_eq!(check.status, Status::Ok, "{check:?}");
        assert_eq!(check.detail, format!("remote access on (local network only), endpoint {}", key.public()));
        let _ = std::fs::remove_dir_all(&home);
    }
}
