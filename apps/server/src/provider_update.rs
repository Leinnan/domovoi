//! `server.updateProvider`: updates a provider CLI in place, after t3code's
//! `providerMaintenanceRunner.ts` and the resolution in provider-core's
//! `maintenanceResolver.ts` (`resolvePackageManagedProviderMaintenance`).
//!
//! The CLI the server runs is found as the probes find it; what installed it
//! decides the command:
//! - an npm global install (its real path is `<prefix>/lib/node_modules/<package>/…`):
//!   `npm install -g --prefix <prefix> --allow-scripts=<package> <package>@latest`;
//! - a Homebrew formula or cask whose `brew --prefix` owns it: `brew upgrade [--cask] <name>`;
//! - the CLI's own updater where its installer put it: `claude update`
//!   (`~/.local/share/claude/…`), `codex update` (`…/packages/standalone/…`,
//!   `CODEX_HOME` set to it), `opencode upgrade --method curl`
//!   (`~/.opencode/bin`, `HOME` set to it), and `pi update --self` anywhere
//!   that is not a `node_modules` folder;
//! - anything else (Nix store, MacPorts, mise, bun, pnpm, yarn, Volta, a
//!   project's `node_modules`) is manual-only and refused as t3code refuses it.
//!
//! The command runs through `crate::process` with no input, its output
//! sampled into the provider's `updateState` once a second (so every client's
//! provider list shows progress), for at most 30 minutes. One update runs per
//! installation (`lockKey`); a second one for another instance of the same
//! installation queues. Afterwards the provider is probed again: `succeeded`
//! when it is still installed and usable, `unchanged` when that cannot be told.
//!
//! Limits: only the built-in instance of each driver (`codex`, `claudeAgent`,
//! `opencode`, `pi`) is updated; Domovoi probes no others. There is no
//! latest-version lookup or version advisory (`versionAdvisory`), so an
//! up-to-date CLI is not detected before the command runs, and no model
//! manifest, so a `targetVersion` is always refused as t3code refuses an
//! unrecommended one. Running sessions are not drained first (t3code's
//! `ProviderInstallationGate`): npm, Homebrew and the native updaters replace
//! files that running processes keep open. On Windows, npm's global `.cmd`
//! shims are updated with npm and the native installers' `.exe` with the CLI
//! itself; what WinGet, Scoop and Chocolatey installed is left to them.

use crate::config_failure::Failure;
use crate::providers::Tool;
use domovoi_model::typed::{ServerProvider, ServerProviderAuthStatus, ServerProviderState, ServerProviderUpdateState, ServerProviderUpdateStatus, ServerProviderUpdatedPayload};
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// t3code `UPDATE_TIMEOUT_MINUTES`: a native updater downloads a large binary without progress.
const UPDATE_TIMEOUT_MINUTES: u64 = 30;
/// t3code `UPDATE_OUTPUT_MAX_BYTES`, per stream.
const OUTPUT_MAX_BYTES: usize = 10_000;
/// t3code `UPDATE_PROGRESS_INTERVAL`.
const PROGRESS_INTERVAL: Duration = Duration::from_secs(1);
/// t3code `UPDATE_PROGRESS_MAX_LENGTH`.
const PROGRESS_MAX_CHARS: usize = 200;
/// t3code `UPDATE_PARTIAL_LINE_MAX_LENGTH`.
const PARTIAL_LINE_MAX_CHARS: usize = 4_096;

/// A provider's own updater (t3code `PackageManagedProviderMaintenanceDefinition.nativeUpdate`).
struct Native {
    args: &'static [&'static str],
    /// The paths its installer puts the CLI at; `None`: anywhere.
    is_command_path: Option<fn(&str) -> bool>,
    /// Installs outside `is_command_path` stay manual-only.
    requires_command_path: bool,
    /// The environment it needs, from the resolved and real paths.
    env: fn(&Path, &Path) -> Vec<(String, String)>,
}

/// How a driver's CLI is distributed (t3code `PackageManagedProviderMaintenanceDefinition`).
struct Definition {
    tool: Tool,
    npm_package: &'static str,
    native: Option<Native>,
}

fn no_env(_: &Path, _: &Path) -> Vec<(String, String)> {
    vec![]
}

fn normalize(path: &str) -> String {
    path.replace('\\', "/").to_lowercase()
}

/// t3code `ClaudeMaintenance`'s native launcher paths.
fn is_claude_native(path: &str) -> bool {
    let p = normalize(path);
    p.ends_with("/.local/bin/claude.exe") || p.contains("/.local/share/claude/") || p.contains("/claude/versions/")
}

/// t3code `codexStandaloneHome`: the folder above `packages/standalone/`.
fn codex_standalone_home(path: &str) -> Option<String> {
    let slashed = path.replace('\\', "/");
    let at = slashed.to_lowercase().find("/packages/standalone/")?;
    Some(if at == 0 { "/".to_owned() } else { slashed[..at].to_owned() })
}

fn codex_env(resolved: &Path, real: &Path) -> Vec<(String, String)> {
    let home = codex_standalone_home(&real.to_string_lossy()).or_else(|| codex_standalone_home(&resolved.to_string_lossy()));
    home.map(|h| vec![("CODEX_HOME".to_owned(), h)]).unwrap_or_default()
}

/// t3code `isOpenCodeNativeCommandPath`: the curl installer's binary.
fn is_opencode_native(path: &str) -> bool {
    let p = normalize(path);
    p.ends_with("/.opencode/bin/opencode") || p.ends_with("/.opencode/bin/opencode.exe")
}

/// t3code `openCodeNativeHome`.
fn opencode_native_home(path: &str) -> Option<String> {
    let slashed = path.replace('\\', "/");
    let lower = slashed.to_lowercase();
    let at = ["/.opencode/bin/opencode.exe", "/.opencode/bin/opencode"].iter().find_map(|tail| lower.ends_with(tail).then(|| lower.len() - tail.len()))?;
    Some(if at == 0 { "/".to_owned() } else { slashed[..at].to_owned() })
}

fn opencode_env(resolved: &Path, real: &Path) -> Vec<(String, String)> {
    let home = opencode_native_home(&real.to_string_lossy()).or_else(|| opencode_native_home(&resolved.to_string_lossy()));
    home.map(|h| vec![("HOME".to_owned(), h)]).unwrap_or_default()
}

/// The drivers whose CLIs Domovoi updates.
fn definition(driver: &str) -> Option<Definition> {
    use crate::providers::{CLAUDE, CODEX, OPENCODE, PI};
    Some(match driver {
        CLAUDE => Definition {
            tool: crate::providers::CLAUDE_TOOL,
            npm_package: "@anthropic-ai/claude-code",
            native: Some(Native { args: &["update"], is_command_path: Some(is_claude_native), requires_command_path: true, env: no_env }),
        },
        CODEX => Definition {
            tool: crate::providers::CODEX_TOOL,
            npm_package: "@openai/codex",
            native: Some(Native { args: &["update"], is_command_path: Some(|p| codex_standalone_home(p).is_some()), requires_command_path: true, env: codex_env }),
        },
        // Domovoi speaks OpenCode's v2 API, which `@opencode/cli` installs.
        OPENCODE => Definition {
            tool: crate::adapters::opencode::OPENCODE_TOOL,
            npm_package: "@opencode/cli",
            native: Some(Native { args: &["upgrade", "--method", "curl"], is_command_path: Some(is_opencode_native), requires_command_path: true, env: opencode_env }),
        },
        PI => Definition {
            tool: crate::pi_rpc::PI_TOOL,
            npm_package: "@earendil-works/pi-coding-agent",
            native: Some(Native { args: &["update", "--self"], is_command_path: None, requires_command_path: false, env: no_env }),
        },
        _ => return None,
    })
}

/// A command that updates a CLI (t3code `ProviderMaintenanceCommandAction`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Action {
    pub executable: String,
    pub args: Vec<String>,
    /// The installation it replaces: one update at a time each.
    pub lock_key: String,
    pub env: Vec<(String, String)>,
}

impl Action {
    /// t3code `describeCommand`.
    fn describe(&self) -> String {
        let name = self.executable.rsplit(['/', '\\']).next().filter(|n| !n.is_empty()).unwrap_or(&self.executable);
        std::iter::once(name.to_owned()).chain(self.args.iter().cloned()).collect::<Vec<_>>().join(" ")
    }
}

/// t3code `npmGlobalPrefixFromCommandPath`.
pub fn npm_global_prefix(real: &str, package: &str) -> Option<String> {
    let slashed = real.replace('\\', "/");
    let lower = slashed.to_lowercase();
    let segment = format!("/lib/node_modules/{}/", package.to_lowercase());
    let at = lower.rfind(&segment)?;
    if lower[..at].contains("/node_modules/") {
        return None;
    }
    // A mise tool's layout looks like npm's; only globals under its Node are npm's.
    let before = &lower[..at];
    if let Some(rest) = before.rsplit_once("/mise/installs/").map(|(_, rest)| rest) {
        let mut parts = rest.split('/');
        if let (Some(tool), Some(_), None) = (parts.next(), parts.next(), parts.next()) {
            if tool != "node" {
                return None;
            }
        }
    }
    Some(if at == 0 { "/".to_owned() } else { slashed[..at].to_owned() })
}

/// t3code `homebrewOwnershipFromCommandPath`: (cask, name, prefix).
pub fn homebrew_ownership(real: &str) -> Option<(bool, String, String)> {
    let slashed = real.replace('\\', "/");
    let lower = slashed.to_lowercase();
    for (keg, cask) in [("/cellar/", false), ("/caskroom/", true)] {
        let Some(at) = lower.find(keg) else { continue };
        let rest = &slashed[at + keg.len()..];
        let mut parts = rest.splitn(3, '/');
        let (Some(name), Some(version), Some(_)) = (parts.next(), parts.next(), parts.next()) else { continue };
        if name.is_empty() || version.is_empty() {
            continue;
        }
        return Some((cask, name.to_owned(), slashed[..at].to_owned()));
    }
    None
}

/// Paths only their own manager may change, which Domovoi does not drive.
fn manual_only(path: &str) -> bool {
    let p = normalize(path);
    p.starts_with("/nix/store/")
        || p.starts_with("/opt/local/")
        || p.contains("/mise/installs/") && !p.contains("/mise/installs/node/")
        || p.contains("/mise/shims/")
        || p.contains("/.bun/bin/")
        || p.contains("/.local/share/pnpm/")
        || p.contains("/library/pnpm/")
        || p.contains("/pnpm/global/")
        || p.contains("/yarn/global/")
        || p.contains("/.volta/")
        || p.contains("/.vite-plus/")
        || p.contains("/scoop/apps/")
        || p.contains("/scoop/shims/")
        || p.contains("/microsoft/winget/")
        || p.contains("/chocolatey/")
}

/// npm's global install on Windows: `<prefix>\claude.cmd` beside
/// `<prefix>\node_modules\<package>` (no `lib\` there, and the shim is a
/// script, not a link to the package). The prefix, if `resolved` is one.
fn npm_shim_prefix(resolved: &Path, package: &str) -> Option<String> {
    let ext = resolved.extension().and_then(|e| e.to_str()).map(str::to_ascii_lowercase);
    if !matches!(ext.as_deref(), Some("cmd" | "ps1") | None) {
        return None;
    }
    let prefix = resolved.parent()?;
    let manifest = package.split('/').fold(prefix.join("node_modules"), |dir, part| dir.join(part)).join("package.json");
    manifest.is_file().then(|| prefix.to_string_lossy().into_owned())
}

/// What updates the CLI at `resolved` (as found) whose real file is `real`,
/// short of Homebrew (which needs `brew` asked): `Err(true)` when Homebrew
/// may own it, `Err(false)` when it is manual-only.
fn resolve(definition: &Definition, resolved: &Path, real: &Path, npm: impl Fn(&str) -> String) -> Result<Action, bool> {
    let paths = [resolved.to_string_lossy().into_owned(), real.to_string_lossy().into_owned()];
    if paths.iter().any(|p| manual_only(p)) {
        return Err(false);
    }
    let npm_prefix = npm_global_prefix(&paths[1], definition.npm_package).or_else(|| npm_shim_prefix(resolved, definition.npm_package));
    let native = definition.native.as_ref().map(|n| Action {
        executable: paths[0].clone(),
        args: n.args.iter().map(|a| (*a).to_owned()).collect(),
        lock_key: format!("{}-native", definition.tool.program),
        env: (n.env)(resolved, real),
    });
    // A native-looking launcher can still delegate to an npm-owned install.
    if let Some(n) = &definition.native
        && n.is_command_path.is_some_and(|is| paths.iter().any(|p| is(p)))
        && npm_prefix.is_none()
    {
        return native.ok_or(false);
    }
    if let Some(prefix) = npm_prefix {
        let package = definition.npm_package;
        return Ok(Action {
            executable: npm(&prefix),
            args: ["install", "-g", "--prefix", &prefix, &format!("--allow-scripts={package}"), &format!("{package}@latest")].map(str::to_owned).to_vec(),
            lock_key: format!("npm-global:{}", normalize(&prefix)),
            env: vec![],
        });
    }
    if homebrew_ownership(&paths[1]).is_some() {
        return Err(true);
    }
    // A `node_modules` path not proven above belongs to another package or a project.
    let requires = definition.native.as_ref().is_some_and(|n| n.requires_command_path);
    if requires || paths.iter().any(|p| normalize(p).contains("/node_modules/")) {
        return Err(false);
    }
    native.ok_or(false)
}

/// `npm` beside the prefix's Node when there is one (a version manager's), else the PATH's.
fn npm_for(prefix: &str) -> String {
    let beside = Path::new(prefix).join("bin").join("npm");
    if crate::providers::is_executable(&beside) {
        return beside.to_string_lossy().into_owned();
    }
    // Windows starts `npm.cmd` only by its full name.
    if cfg!(windows)
        && let Some(npm) = crate::providers::find_in(&crate::providers::search_dirs(), "npm")
    {
        return npm.to_string_lossy().into_owned();
    }
    "npm".to_owned()
}

/// t3code `resolveHomebrewProviderMaintenance`: `brew upgrade` once the
/// keg's prefix is confirmed by a `brew` that reports it as its own.
async fn homebrew(real: &Path) -> Option<Action> {
    let (cask, name, prefix) = homebrew_ownership(&real.to_string_lossy())?;
    let keg_brew = Path::new(&prefix).join("bin").join("brew");
    let brew = if crate::providers::is_executable(&keg_brew) {
        keg_brew
    } else {
        crate::providers::find_in(&crate::providers::search_dirs(), "brew")?
    };
    let mut cmd = crate::process::command(&brew);
    cmd.arg("--prefix").stdin(std::process::Stdio::null());
    let out = crate::process::output(cmd, Duration::from_secs(20)).await.ok().filter(|o| o.status.success())?;
    let reported = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    let reported = std::fs::canonicalize(&reported).map(|p| p.to_string_lossy().into_owned()).unwrap_or(reported);
    if reported.is_empty() || normalize(&reported) != normalize(&prefix) {
        return None;
    }
    let mut args = vec!["upgrade".to_owned()];
    if cask {
        args.push("--cask".into());
    }
    args.push(name);
    Some(Action { executable: brew.to_string_lossy().into_owned(), args, lock_key: "homebrew".into(), env: vec![] })
}

/// The CLI as found (PATH entry or override) and its real file.
fn locate(tool: Tool) -> Option<(PathBuf, PathBuf)> {
    let real = crate::providers::executable(tool).ok()?;
    let overridden = std::env::var(tool.env).ok().map(|v| crate::util::expand_home(v.trim())).filter(|v| !v.is_empty());
    let found = match overridden {
        Some(given) if given.contains('/') || cfg!(windows) && given.contains('\\') => crate::providers::runnable(Path::new(&given)),
        given => {
            let names: Vec<String> = given.into_iter().chain([tool.program.to_owned()]).collect();
            let dirs = crate::providers::search_dirs();
            names.iter().find_map(|name| crate::providers::find_in(&dirs, name))
        }
    };
    Some((found.unwrap_or_else(|| real.clone()), real))
}

/// What updates the default instance of `driver`, if anything can.
async fn capabilities(driver: &str) -> Option<Action> {
    let definition = definition(driver)?;
    let (resolved, real) = locate(definition.tool)?;
    match resolve(&definition, &resolved, &real, npm_for) {
        Ok(action) => Some(action),
        Err(true) => homebrew(&real).await,
        Err(false) => None,
    }
}

// ---------------------------------------------------------------------------
// Update states, as the provider list shows them
// ---------------------------------------------------------------------------

static STATES: Mutex<Option<HashMap<String, ServerProviderUpdateState>>> = Mutex::new(None);

/// The instance's update state, for its `ServerProvider.updateState`.
pub fn state(instance: &str) -> Option<ServerProviderUpdateState> {
    crate::util::locked(&STATES).as_ref()?.get(instance).cloned()
}

fn set_state(instance: &str, state: ServerProviderUpdateState) {
    crate::util::locked(&STATES).get_or_insert_with(HashMap::new).insert(instance.to_owned(), state);
    // Every client's provider list gets it.
    crate::providers::CLAUDE_COMMANDS_CHANGED.notify_one();
}

fn now() -> domovoi_model::typed::Timestamp {
    crate::time_fmt::iso_now().into()
}

fn update_state(status: ServerProviderUpdateStatus, started_at: Option<&domovoi_model::typed::Timestamp>, finished: bool, message: &str, output: Option<String>) -> ServerProviderUpdateState {
    ServerProviderUpdateState {
        status,
        started_at: started_at.cloned(),
        finished_at: finished.then(now),
        message: (!message.trim().is_empty()).then(|| message.to_owned()),
        output,
    }
}

/// Instances with an update running or queued (t3code's command coordinator target keys).
static TARGETS: Mutex<Option<HashSet<String>>> = Mutex::new(None);
/// One lock per installation.
static LOCKS: Mutex<Option<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> = Mutex::new(None);

/// Holds an instance's target for as long as its update runs; marks a run
/// that is dropped before it finished as failed (t3code's `onInterrupt`).
struct Running {
    instance: String,
    started_at: Option<domovoi_model::typed::Timestamp>,
    settled: bool,
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(targets) = crate::util::locked(&TARGETS).as_mut() {
            targets.remove(&self.instance);
        }
        if !self.settled {
            set_state(&self.instance, update_state(ServerProviderUpdateStatus::Failed, self.started_at.as_ref(), true, "Update stopped before it finished.", None));
        }
    }
}

// ---------------------------------------------------------------------------
// Running the command
// ---------------------------------------------------------------------------

/// t3code `ProviderMaintenanceCommandResult`.
#[derive(Debug, Default)]
struct CommandResult {
    stdout: String,
    stderr: String,
    exit_code: Option<i32>,
    timed_out: bool,
    /// The command did not start (t3code's `ProviderMaintenanceCommandError`).
    spawn_error: Option<String>,
}

/// t3code `splitOutputLines`: complete lines, and the partial one left.
fn split_output_lines(partial: &str, text: &str) -> (Vec<String>, String) {
    let joined = format!("{partial}{text}");
    // Cursor moves to the line's start are line ends too.
    let joined = joined.replace("\x1b[999D", "\r").replace("\x1b[1G", "\r").replace("\x1b[G", "\r").replace("\x1b[0G", "\r");
    let mut parts: Vec<String> = joined.split("\r\n").flat_map(|p| p.split(['\r', '\n'])).map(str::to_owned).collect();
    let last = parts.pop().unwrap_or_default();
    let chars: Vec<char> = last.chars().collect();
    let tail: String = chars[chars.len().saturating_sub(PARTIAL_LINE_MAX_CHARS)..].iter().collect();
    (parts, tail)
}

/// Removes ANSI escape sequences (Node's `stripVTControlCharacters`, near enough).
fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\x1b' {
            if !c.is_control() || c == '\t' {
                out.push(c);
            }
            continue;
        }
        match chars.peek() {
            Some('[') => {
                chars.next();
                // Parameters and intermediates, then one final byte.
                while let Some(&n) = chars.peek() {
                    chars.next();
                    if ('@'..='~').contains(&n) {
                        break;
                    }
                }
            }
            Some(']') => {
                // An OSC string, up to BEL or ESC \.
                chars.next();
                while let Some(n) = chars.next() {
                    if n == '\x07' || (n == '\x1b' && chars.peek() == Some(&'\\')) {
                        chars.next();
                        break;
                    }
                }
            }
            _ => {
                chars.next();
            }
        }
    }
    out
}

/// t3code `toProgressLine`: a line fit to show, or `None`.
fn to_progress_line(line: &str) -> Option<String> {
    // An escape sequence cut off at the end of what arrived.
    let trailing = line.rfind('\x1b').map(|at| &line[at + 1..]);
    if trailing.is_some_and(|rest| rest.is_empty() || (rest.starts_with('[') && rest[1..].chars().all(|c| c.is_ascii_digit()))) {
        return None;
    }
    let text = strip_ansi(line).split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        return None;
    }
    let chars: Vec<char> = text.chars().collect();
    Some(if chars.len() <= PROGRESS_MAX_CHARS { text } else { chars[..PROGRESS_MAX_CHARS - 1].iter().collect::<String>() + "…" })
}

/// One output stream: kept up to `OUTPUT_MAX_BYTES`, its latest line offered as progress.
async fn read_stream(mut pipe: impl tokio::io::AsyncRead + Unpin, progress: Arc<Mutex<Option<String>>>) -> String {
    use tokio::io::AsyncReadExt;
    let mut kept = Vec::new();
    let mut partial = String::new();
    let mut buf = [0u8; 8192];
    // Bytes of a UTF-8 character split across reads.
    let mut carry = Vec::new();
    loop {
        let n = match pipe.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        if kept.len() < OUTPUT_MAX_BYTES {
            kept.extend_from_slice(&buf[..n.min(OUTPUT_MAX_BYTES - kept.len())]);
        }
        carry.extend_from_slice(&buf[..n]);
        let valid = match std::str::from_utf8(&carry) {
            Ok(_) => carry.len(),
            Err(e) => e.valid_up_to(),
        };
        let text = String::from_utf8_lossy(&carry[..valid]).into_owned();
        carry.drain(..valid);
        if carry.len() > 4 {
            carry.clear();
        }
        let (lines, rest) = split_output_lines(&partial, &text);
        partial = rest;
        let line = to_progress_line(&partial).or_else(|| lines.iter().rev().find_map(|l| to_progress_line(l)));
        if let Some(line) = line {
            *crate::util::locked(&progress) = Some(line);
        }
    }
    String::from_utf8_lossy(&kept).into_owned()
}

/// Runs the action, offering its progress lines to `on_progress` at most once a second.
async fn run_command(action: &Action, on_progress: impl Fn(String)) -> CommandResult {
    let mut cmd = crate::process::command(&action.executable);
    cmd.args(&action.args)
        .envs(action.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
        // Nothing answers an update command: one that reads its input sees it end.
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => return CommandResult { spawn_error: Some(format!("Failed to run update command {}: {e}", action.executable)), ..Default::default() },
    };
    // npm and the native updaters start children of their own: they end with it.
    let _tree = crate::process::Tree::of(child.id());
    let progress = Arc::new(Mutex::new(None::<String>));
    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        return CommandResult { spawn_error: Some("Update command failed to run.".into()), ..Default::default() };
    };
    let run = async {
        let streams = futures::future::join(read_stream(stdout, progress.clone()), read_stream(stderr, progress.clone()));
        let waited = async {
            let (out, err) = streams.await;
            (out, err, child.wait().await)
        };
        tokio::pin!(waited);
        let mut tick = tokio::time::interval(PROGRESS_INTERVAL);
        loop {
            tokio::select! {
                done = &mut waited => break done,
                _ = tick.tick() => {
                    if let Some(line) = crate::util::locked(&progress).take() {
                        on_progress(line);
                    }
                }
            }
        }
    };
    match tokio::time::timeout(Duration::from_secs(UPDATE_TIMEOUT_MINUTES * 60), run).await {
        Ok((stdout, stderr, status)) => CommandResult { stdout, stderr, exit_code: status.ok().and_then(|s| s.code()), ..Default::default() },
        // The child is killed as it is dropped.
        Err(_) => CommandResult { timed_out: true, ..Default::default() },
    }
}

/// t3code `commandOutput`.
fn command_output(result: &CommandResult) -> Option<String> {
    let joined = [result.stderr.as_str(), result.stdout.as_str()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join("\n\n");
    let trimmed = joined.trim();
    if trimmed.is_empty() {
        return None;
    }
    Some(trimmed.chars().take(OUTPUT_MAX_BYTES).collect())
}

/// t3code `failureReason`: the last line naming an error, else the last line.
fn failure_reason(result: &CommandResult) -> Option<String> {
    let streams: Vec<Vec<String>> = [&result.stderr, &result.stdout].iter().map(|o| o.split(['\r', '\n']).filter_map(to_progress_line).collect()).collect();
    let names_error = |l: &String| {
        let lower = l.to_lowercase();
        lower.contains("error") || lower.contains("fail")
    };
    streams
        .iter()
        .find_map(|lines| lines.iter().rev().find(|l| names_error(l)).cloned())
        .or_else(|| streams.iter().find_map(|lines| lines.last().cloned()))
}

/// t3code `failureMessage`.
fn failure_message(result: &CommandResult) -> String {
    if result.timed_out {
        return format!("Update timed out after {UPDATE_TIMEOUT_MINUTES} minutes.");
    }
    let summary = match result.exit_code {
        Some(code) if code != 0 => format!("Update command exited with code {code}"),
        _ => "Update command failed".to_owned(),
    };
    match failure_reason(result) {
        Some(reason) => format!("{summary}: {reason}"),
        None => format!("{summary}."),
    }
}

/// The probe after the command says the CLI may not have been updated
/// (t3code's `couldNotVerify`).
fn could_not_verify(provider: Option<&ServerProvider>, target_version: Option<&str>) -> bool {
    let Some(p) = provider else { return true };
    // `installedVersion` when the snapshot has the key, else `version`.
    let version = match &p.installed_version {
        Some(installed) => installed.as_deref(),
        None => p.version.as_deref(),
    };
    !p.installed
        || (p.status == ServerProviderState::Error && (p.auth.status != ServerProviderAuthStatus::Unauthenticated || p.version.is_none()))
        || target_version.is_some_and(|t| version.map(|v| v.trim_start_matches('v')) != Some(t))
}

/// What `server.updateProvider` is asked (`ServerProviderUpdateInput`).
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInput {
    #[serde(default)]
    pub provider: Option<String>,
    #[serde(default)]
    pub target_version: Option<String>,
    #[serde(default)]
    pub instance_id: Option<String>,
}

/// `server.updateProvider`: runs the update and answers with every provider
/// as probed afterwards. A command that fails is an update state, not an error.
pub async fn update(state: &crate::state::AppState, input: UpdateInput) -> Result<ServerProviderUpdatedPayload, Failure> {
    let provider = input.provider.clone().unwrap_or_default();
    let refused = |reason: &str| Failure::ProviderUpdate { provider: input.provider.clone(), reason: reason.to_owned() };
    let instance = input.instance_id.clone().unwrap_or_else(|| provider.clone());
    // Only the built-in instance of each driver is probed, so only it is updated.
    let action = if instance == provider { capabilities(&provider).await } else { None };
    let Some(action) = action else {
        return Err(refused("This provider does not support one-click updates."));
    };
    if !crate::util::locked(&TARGETS).get_or_insert_with(HashSet::new).insert(instance.clone()) {
        return Err(refused("An update is already running for this provider."));
    }
    let mut running = Running { instance: instance.clone(), started_at: None, settled: false };
    let lock = crate::util::locked(&LOCKS).get_or_insert_with(HashMap::new).entry(action.lock_key.clone()).or_default().clone();
    let _held = match lock.try_lock() {
        Ok(held) => held,
        Err(_) => {
            set_state(&instance, update_state(ServerProviderUpdateStatus::Queued, None, false, "Waiting for another provider update to finish.", None));
            lock.lock().await
        }
    };
    let started_at = now();
    running.started_at = Some(started_at.clone());
    let finish = |running: &mut Running, state: ServerProviderUpdateState| {
        running.settled = true;
        set_state(&running.instance, state);
    };
    let running_message = |message: &str| set_state(&instance, update_state(ServerProviderUpdateStatus::Running, Some(&started_at), false, message, None));

    if input.target_version.is_some() {
        // No model manifest recommends a version, so none is installed on request.
        finish(
            &mut running,
            update_state(
                ServerProviderUpdateStatus::Failed,
                Some(&started_at),
                true,
                "This version is no longer recommended or this installer cannot install a specific version. Refresh provider settings.",
                None,
            ),
        );
        return Ok(ServerProviderUpdatedPayload { providers: state.providers.snapshot().await.into() });
    }

    running_message(&format!("Running {}", action.describe()));
    let result = run_command(&action, |line| running_message(&line)).await;
    if let Some(message) = &result.spawn_error {
        finish(&mut running, update_state(ServerProviderUpdateStatus::Failed, Some(&started_at), true, message, None));
        return Ok(ServerProviderUpdatedPayload { providers: state.providers.snapshot().await.into() });
    }
    if result.timed_out || result.exit_code != Some(0) {
        tracing::warn!(provider, command = action.describe(), exit_code = ?result.exit_code, timed_out = result.timed_out, "provider update command failed");
        let output = command_output(&result);
        finish(&mut running, update_state(ServerProviderUpdateStatus::Failed, Some(&started_at), true, &failure_message(&result), output));
        return Ok(ServerProviderUpdatedPayload { providers: state.providers.snapshot().await.into() });
    }

    running_message("Verifying the installed version");
    // Probed whole first, if never yet: the answer lists every provider.
    state.providers.snapshot().await;
    let refreshed = state.providers.refresh(Some(&instance)).await;
    let verified = refreshed.iter().find(|p| p.instance_id.as_str() == instance);
    let (status, message) = if could_not_verify(verified, None) {
        (ServerProviderUpdateStatus::Unchanged, "Update command completed, but Domovoi could not verify the provider version.")
    } else {
        (ServerProviderUpdateStatus::Succeeded, "Provider updated.")
    };
    finish(&mut running, update_state(status, Some(&started_at), true, message, command_output(&result)));
    Ok(ServerProviderUpdatedPayload { providers: state.providers.snapshot().await.into() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn action(driver: &str, resolved: &str, real: &str) -> Result<Action, bool> {
        resolve(&definition(driver).unwrap(), Path::new(resolved), Path::new(real), |prefix| format!("{prefix}/bin/npm"))
    }

    #[test]
    fn the_installer_decides_the_command() {
        // npm globals, under a version manager's Node too.
        let npm = action("codex", "/u/.local/share/fnm/aliases/default/bin/codex", "/u/.local/share/fnm/node-versions/v22/installation/lib/node_modules/@openai/codex/bin/codex.js").unwrap();
        assert_eq!(npm.executable, "/u/.local/share/fnm/node-versions/v22/installation/bin/npm");
        assert_eq!(npm.args, ["install", "-g", "--prefix", "/u/.local/share/fnm/node-versions/v22/installation", "--allow-scripts=@openai/codex", "@openai/codex@latest"]);
        assert_eq!(npm.lock_key, "npm-global:/u/.local/share/fnm/node-versions/v22/installation");
        assert_eq!(npm.describe(), "npm install -g --prefix /u/.local/share/fnm/node-versions/v22/installation --allow-scripts=@openai/codex @openai/codex@latest");
        let pi = action("pi", "/usr/local/bin/pi", "/usr/local/lib/node_modules/@earendil-works/pi-coding-agent/dist/cli.js").unwrap();
        assert_eq!(pi.args.last().map(String::as_str), Some("@earendil-works/pi-coding-agent@latest"));

        // The CLIs' own updaters, where their installers put them.
        let claude = action("claudeAgent", "/u/.local/bin/claude", "/u/.local/share/claude/versions/2.1.319").unwrap();
        assert_eq!((claude.executable.as_str(), claude.args.as_slice(), claude.lock_key.as_str()), ("/u/.local/bin/claude", &["update".to_owned()][..], "claude-native"));
        let codex = action("codex", "/u/.codex/packages/standalone/bin/codex", "/u/.codex/packages/standalone/releases/0.170.0/codex").unwrap();
        assert_eq!(codex.args, ["update"]);
        assert_eq!(codex.env, [("CODEX_HOME".to_owned(), "/u/.codex".to_owned())]);
        let opencode = action("opencode", "/u/.opencode/bin/opencode", "/u/.opencode/bin/opencode").unwrap();
        assert_eq!(opencode.args, ["upgrade", "--method", "curl"]);
        assert_eq!(opencode.env, [("HOME".to_owned(), "/u".to_owned())]);
        let pi = action("pi", "/u/bin/pi", "/u/bin/pi").unwrap();
        assert_eq!(pi.args, ["update", "--self"]);

        // Homebrew is asked; the rest is manual-only.
        assert_eq!(action("opencode", "/opt/homebrew/bin/opencode", "/opt/homebrew/Cellar/opencode/2.1.3/bin/opencode"), Err(true));
        assert_eq!(action("claudeAgent", "/usr/local/bin/claude", "/usr/local/bin/claude"), Err(false), "claude update cannot see this install");
        assert_eq!(action("codex", "/nix/store/abc-codex/bin/codex", "/nix/store/abc-codex/bin/codex"), Err(false));
        assert_eq!(action("pi", "/u/.bun/bin/pi", "/u/.bun/install/global/node_modules/pi/cli.js"), Err(false));
        assert_eq!(action("pi", "/u/project/node_modules/.bin/pi", "/u/project/node_modules/pi/cli.js"), Err(false), "a project's package");
        assert_eq!(action("codex", "/u/.local/share/mise/installs/codex/0.1/bin/codex", "/u/.local/share/mise/installs/codex/0.1/bin/codex"), Err(false));
        assert!(definition("antigravity").is_none());
    }

    #[test]
    fn package_paths_are_read_as_t3code_reads_them() {
        assert_eq!(npm_global_prefix("/usr/local/lib/node_modules/@openai/codex/bin/codex.js", "@openai/codex").as_deref(), Some("/usr/local"));
        assert_eq!(npm_global_prefix("/lib/node_modules/@openai/codex/x", "@openai/codex").as_deref(), Some("/"));
        assert_eq!(npm_global_prefix("/a/node_modules/b/lib/node_modules/@openai/codex/x", "@openai/codex"), None, "nested in a package");
        assert_eq!(npm_global_prefix("/u/mise/installs/node/22/lib/node_modules/@openai/codex/x", "@openai/codex").as_deref(), Some("/u/mise/installs/node/22"));
        assert_eq!(npm_global_prefix("/u/mise/installs/codex/1/lib/node_modules/@openai/codex/x", "@openai/codex"), None);
        assert_eq!(homebrew_ownership("/opt/homebrew/Cellar/gh/2.80.0/bin/gh"), Some((false, "gh".into(), "/opt/homebrew".into())));
        assert_eq!(homebrew_ownership("/opt/homebrew/Caskroom/codex/0.170.0/codex"), Some((true, "codex".into(), "/opt/homebrew".into())));
        assert_eq!(homebrew_ownership("/usr/local/bin/gh"), None);
    }

    #[test]
    fn progress_lines_and_failures_read_as_t3codes() {
        let (lines, partial) = split_output_lines("dow", "nloading\r\n50%\r75%\x1b[1Gdone\npart");
        assert_eq!(lines, ["downloading", "50%", "75%", "done"]);
        assert_eq!(partial, "part");
        assert_eq!(to_progress_line("\x1b[32m  added   3 packages \x1b[0m").as_deref(), Some("added 3 packages"));
        assert_eq!(to_progress_line("cut \x1b["), None);
        assert_eq!(to_progress_line("   "), None);
        assert_eq!(to_progress_line(&"x".repeat(300)).map(|l| l.chars().count()), Some(200));

        let failed = CommandResult { stderr: "npm error code EACCES\nnpm error path /usr\nTry running as root".into(), exit_code: Some(243), ..Default::default() };
        assert_eq!(failure_message(&failed), "Update command exited with code 243: npm error path /usr");
        assert_eq!(command_output(&failed).as_deref(), Some("npm error code EACCES\nnpm error path /usr\nTry running as root"));
        assert_eq!(failure_message(&CommandResult { exit_code: Some(1), ..Default::default() }), "Update command exited with code 1.");
        assert_eq!(failure_message(&CommandResult { timed_out: true, ..Default::default() }), "Update timed out after 30 minutes.");
        assert_eq!(failure_message(&CommandResult { stdout: "only this".into(), ..Default::default() }), "Update command failed: only this");
    }

    #[test]
    fn the_state_is_written_as_t3codes() {
        let started = domovoi_model::typed::Timestamp::from("2026-01-01T00:00:00.000Z");
        let state = update_state(ServerProviderUpdateStatus::Running, Some(&started), false, "Running npm install", None);
        assert_eq!(
            serde_json::to_value(&state).unwrap(),
            serde_json::json!({ "status": "running", "startedAt": "2026-01-01T00:00:00.000Z", "finishedAt": null, "message": "Running npm install", "output": null })
        );
        let queued = update_state(ServerProviderUpdateStatus::Queued, None, false, "Waiting for another provider update to finish.", None);
        assert_eq!(serde_json::to_value(&queued).unwrap()["startedAt"], serde_json::Value::Null);
    }
}
