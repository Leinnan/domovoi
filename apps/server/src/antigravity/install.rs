//! Domovoi's own install of Google's Antigravity CLI (t3code
//! `AntigravityInstallation.ts`, `antigravityCliRelease.ts`), for
//! `provider.install.*`:
//! - Google's release channel names the current CLI for this platform
//!   (`manifests/<os>_<arch>[_musl].json`: version, URL, SHA-512); it is
//!   downloaded, checked, unpacked (one regular file) and started once to
//!   prove it is the release it claims before it becomes the active one;
//! - releases live under `<userdata>/tools/antigravity-acp/<platform>-<arch>/`
//!   as `versions/<release id>/`, and `active.json` names the one in use,
//!   replaced at once: a failed update leaves the previous one running;
//! - running agents hold a lease on the install, which is not removed under them.
//!
//! An instance's own binary path, then this install, then `agy` on the PATH
//! is what an instance runs. `DOMOVOI_ANTIGRAVITY_RELEASES` replaces Google's
//! release host (for the manifest and the download), for tests.

#![expect(clippy::disallowed_types, clippy::disallowed_macros, reason = "the release channel's manifest and the install's own JSON records")]

use crate::config_failure::Failure;
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

const MANIFEST_BASE: &str = "https://antigravity-cli-auto-updater-974169037036.us-central1.run.app/manifests";
const DOWNLOAD_BASE: &str = "https://storage.googleapis.com/antigravity-public/antigravity-cli/";
const MAX_DOWNLOAD_BYTES: u64 = 512 * 1024 * 1024;
const MAX_EXECUTABLE_BYTES: u64 = 512 * 1024 * 1024;
const FREE_SPACE_MARGIN: u64 = 256 * 1024 * 1024;
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(45 * 60);
const VALIDATION_TIMEOUT: Duration = Duration::from_secs(90);
const UPDATE_CHECK_EVERY: Duration = Duration::from_secs(3600);

/// `ProviderInstallState` of the Antigravity driver.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallState {
    pub driver: &'static str,
    pub operation_id: Option<String>,
    pub phase: &'static str,
    pub downloaded_bytes: u64,
    pub total_bytes: Option<u64>,
    /// The release the channel offers (or being installed).
    pub version: Option<String>,
    pub installed_version: Option<String>,
    pub executable_path: Option<String>,
    pub source: Option<&'static str>,
    pub can_remove: bool,
    pub message: Option<String>,
}

impl InstallState {
    fn running(&self) -> bool {
        matches!(self.phase, "downloading" | "extracting" | "verifying")
    }
}

fn releases_base() -> Option<String> {
    std::env::var("DOMOVOI_ANTIGRAVITY_RELEASES").ok().filter(|v| !v.is_empty()).map(|v| v.trim_end_matches('/').to_owned())
}

/// Node's names for this machine, as Google's channel uses them.
fn platform() -> (&'static str, &'static str) {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        other => other,
    };
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        other => other,
    };
    (os, arch)
}

/// A musl Linux (its loader is there).
fn musl() -> bool {
    cfg!(target_os = "linux") && ["/lib/libc.musl-aarch64.so.1", "/lib/libc.musl-x86_64.so.1"].iter().any(|p| Path::new(p).exists())
}

/// The channel's name of this platform (`darwin_arm64`, `linux_amd64_musl`…).
pub fn target() -> Option<String> {
    let (os, arch) = platform();
    if !["darwin", "linux", "win32"].contains(&os) || !["x64", "arm64"].contains(&arch) {
        return None;
    }
    let os = if os == "win32" { "windows" } else { os };
    Some(format!("{os}_{}{}", if arch == "x64" { "amd64" } else { arch }, if os == "linux" && musl() { "_musl" } else { "" }))
}

/// A release the channel offers for this platform.
#[derive(Debug, Clone, PartialEq)]
pub struct Release {
    pub version: String,
    pub url: String,
    pub sha512: String,
    pub executable: &'static str,
    pub archive: bool,
}

/// The channel's manifest as a release of the native CLI (1.5 or newer) at
/// the URL this platform's file has, or `None`.
pub fn parse_release(manifest: &Value, os: &str, arch: &str, is_musl: bool, download_base: &str) -> Option<Release> {
    let version = manifest["version"].as_str().filter(|v| regex::Regex::new(r"^\d+\.\d+\.\d+$").is_ok_and(|r| r.is_match(v)))?;
    let sha = manifest["sha512"].as_str().filter(|s| s.len() == 128 && s.chars().all(|c| c.is_ascii_hexdigit()))?;
    let url = manifest["url"].as_str()?;
    if super::version_tuple(version)? < (1, 5, 0) {
        return None;
    }
    let windows = os == "win32";
    let suffix = if os == "linux" && is_musl { "_musl" } else { "" };
    let name_os = match os {
        "darwin" => "mac",
        "win32" => "windows",
        _ => "linux",
    };
    let folder = format!("{}-{}{}", if windows { "windows" } else { os }, if arch == "arm64" { "arm" } else { "x64" }, if suffix.is_empty() { "" } else { "-musl" });
    let file = format!("{folder}/cli_{name_os}_{}{suffix}.{}", if arch == "x64" { "x64" } else { "arm64" }, if windows { "exe" } else { "tar.gz" });
    let rest = url.strip_prefix(&format!("{download_base}{version}-"))?;
    let (build, path) = rest.split_once('/')?;
    if build.is_empty() || !build.chars().all(|c| c.is_ascii_digit()) || path != file {
        return None;
    }
    Some(Release { version: version.to_owned(), url: url.to_owned(), sha512: sha.to_lowercase(), executable: if windows { "agy.exe" } else { "antigravity" }, archive: !windows })
}

// ---------------------------------------------------------------------------
// Where installs are
// ---------------------------------------------------------------------------

fn root() -> PathBuf {
    let (os, arch) = platform();
    super::state_dir().join("tools").join("antigravity-acp").join(format!("{os}-{arch}"))
}

/// An installed release's record (`.install-complete.json`).
fn record(version_dir: &Path) -> Option<(String, String, u64)> {
    let v: Value = serde_json::from_str(&std::fs::read_to_string(version_dir.join(".install-complete.json")).ok()?).ok()?;
    Some((v["version"].as_str()?.to_owned(), v["executable"]["name"].as_str()?.to_owned(), v["executable"]["bytes"].as_u64()?))
}

/// The active release: its folder, version and executable, checked whole.
fn active() -> Option<(PathBuf, String, PathBuf)> {
    let root = root();
    let pointer: Value = serde_json::from_str(&std::fs::read_to_string(root.join("active.json")).ok()?).ok()?;
    let id = pointer["releaseId"].as_str().filter(|id| id.len() == 64 && id.chars().all(|c| c.is_ascii_hexdigit()))?;
    let dir = root.join("versions").join(id);
    let (version, name, bytes) = record(&dir)?;
    let exe = dir.join(&name);
    let meta = std::fs::metadata(&exe).ok()?;
    (meta.len() == bytes && crate::providers::is_executable(&exe)).then_some((dir, version, exe))
}

/// The installed executable Domovoi manages, when there is one.
pub fn active_executable() -> Option<PathBuf> {
    active().map(|(_, _, exe)| exe)
}

/// Whether Domovoi can install the CLI on this platform.
pub fn supported() -> bool {
    target().is_some()
}

// ---------------------------------------------------------------------------
// The installation's state
// ---------------------------------------------------------------------------

fn idle_state() -> InstallState {
    let installed = active();
    InstallState {
        driver: super::DRIVER,
        operation_id: None,
        phase: "idle",
        downloaded_bytes: 0,
        total_bytes: None,
        version: installed.as_ref().map(|(_, v, _)| v.clone()),
        installed_version: installed.as_ref().map(|(_, v, _)| v.clone()),
        executable_path: installed.as_ref().map(|(_, _, e)| e.to_string_lossy().into_owned()),
        source: installed.as_ref().map(|_| "managed"),
        can_remove: root().exists(),
        message: None,
    }
}

fn state() -> &'static tokio::sync::watch::Sender<InstallState> {
    static STATE: OnceLock<tokio::sync::watch::Sender<InstallState>> = OnceLock::new();
    STATE.get_or_init(|| tokio::sync::watch::Sender::new(idle_state()))
}

fn update(change: impl FnOnce(&mut InstallState)) {
    state().send_modify(change);
    crate::providers::CLAUDE_COMMANDS_CHANGED.notify_one();
}

/// The install under way (its operation and task).
static RUNNING: Mutex<Option<(String, tokio::task::AbortHandle)>> = Mutex::new(None);

/// Whether an installation is under way.
pub fn busy() -> bool {
    state().borrow().running()
}

pub fn current() -> InstallState {
    state().borrow().clone()
}

/// Every state from now on.
pub fn subscribe() -> impl futures::Stream<Item = InstallState> + Send + 'static {
    let mut rx = state().subscribe();
    async_stream::stream! {
        loop {
            let s = rx.borrow_and_update().clone();
            yield s;
            if rx.changed().await.is_err() {
                break;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Leases: running agents of the install
// ---------------------------------------------------------------------------

static LEASES: Mutex<Option<HashMap<PathBuf, usize>>> = Mutex::new(None);

/// An agent runs from the install while this is held.
pub struct Lease(PathBuf);

impl Drop for Lease {
    fn drop(&mut self) {
        let mut leases = crate::util::locked(&LEASES);
        if let Some(n) = leases.get_or_insert_with(HashMap::new).get_mut(&self.0) {
            *n = n.saturating_sub(1);
        }
    }
}

/// A lease when `exe` is inside the install.
pub fn acquire(exe: &Path) -> Option<Lease> {
    let root = std::fs::canonicalize(root()).ok()?;
    let exe = std::fs::canonicalize(exe).ok()?;
    if !exe.starts_with(&root) {
        return None;
    }
    *crate::util::locked(&LEASES).get_or_insert_with(HashMap::new).entry(exe.clone()).or_default() += 1;
    Some(Lease(exe))
}

fn leased() -> bool {
    crate::util::locked(&LEASES).as_ref().is_some_and(|l| l.values().any(|n| *n > 0))
}

// ---------------------------------------------------------------------------
// The release channel
// ---------------------------------------------------------------------------

async fn fetch_release() -> Result<Release, String> {
    let target = target().ok_or_else(|| {
        let (os, arch) = platform();
        format!("Google does not publish an Antigravity runtime for {os}-{arch}. Use a custom executable.")
    })?;
    let base = releases_base();
    let manifest_url = base.as_ref().map_or_else(|| format!("{MANIFEST_BASE}/{target}.json"), |b| format!("{b}/manifests/{target}.json"));
    let download_base = base.as_ref().map_or_else(|| DOWNLOAD_BASE.to_owned(), |b| format!("{b}/antigravity-public/antigravity-cli/"));
    let client = reqwest::Client::builder().timeout(Duration::from_secs(30)).build().map_err(|e| e.to_string())?;
    let unreachable = "Could not find a native Antigravity CLI release. Check the network connection and try again.".to_owned();
    let answer = client.get(&manifest_url).send().await.map_err(|_| unreachable.clone())?;
    if !answer.status().is_success() {
        return Err(unreachable);
    }
    let manifest: Value = answer.json().await.map_err(|_| unreachable.clone())?;
    let (os, arch) = platform();
    parse_release(&manifest, os, arch, musl(), &download_base).ok_or(unreachable)
}

static LAST_CHECK: Mutex<Option<std::time::Instant>> = Mutex::new(None);

/// Looks for a newer release (at most hourly, when update checks are on):
/// the state then offers it as `version`.
pub fn check_for_update() {
    let enabled = match crate::state::STATE.get() {
        Some(s) => s.settings.try_read().map_or(true, |settings| settings.enable_provider_update_checks != Some(false)),
        None => false,
    };
    if !enabled || active().is_none() || busy() {
        return;
    }
    {
        let mut last = crate::util::locked(&LAST_CHECK);
        if last.is_some_and(|t| t.elapsed() < UPDATE_CHECK_EVERY) {
            return;
        }
        *last = Some(std::time::Instant::now());
    }
    tokio::spawn(async {
        if let Ok(release) = fetch_release().await {
            update(|s| {
                if !s.running() {
                    s.version = Some(release.version);
                }
            });
        }
    });
}

// ---------------------------------------------------------------------------
// Installing
// ---------------------------------------------------------------------------

fn free_space(dir: &Path) -> Option<u64> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let path = std::ffi::CString::new(dir.as_os_str().as_bytes()).ok()?;
        let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
        // SAFETY: `path` is a NUL-terminated string and `stat` a statvfs to fill.
        let ok = unsafe { libc::statvfs(path.as_ptr(), &mut stat) } == 0;
        ok.then(|| stat.f_bavail as u64 * stat.f_frsize as u64)
    }
    #[cfg(windows)]
    {
        crate::win::free_space(dir)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = dir;
        None
    }
}

/// Starts the agent once in a throwaway profile: it must say it is the
/// release it was installed as, with what Domovoi uses of it.
async fn validate(exe: &Path, version: &str) -> Result<(), String> {
    let scratch = std::env::temp_dir().join(format!("domovoi-antigravity-check-{}", crate::util::new_uuid()));
    let profile = super::Profile { home: scratch.join("profile"), acp: scratch.join("profile/antigravity-acp"), temp: scratch.join("tmp") };
    let cwd = scratch.join("cwd");
    for d in [&profile.acp, &profile.temp, &cwd] {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    let env = super::environment(&profile, &super::Config::default(), &profile.temp, &super::browser_command().unwrap_or_default());
    let args = super::acp_args(&profile);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let options = crate::jsonrpc::SpawnOptions { env, remove_env: super::REMOVED_ENV.iter().map(|s| (*s).to_owned()).collect(), jsonrpc2: true, ..Default::default() };
    let result = async {
        let (rpc, _events) = crate::jsonrpc::RpcClient::spawn_opts(&exe.to_string_lossy(), &args, Some(&cwd.to_string_lossy()), options, |_, _| {}).map_err(|e| format!("The installed Antigravity runtime did not start: {e}"))?;
        let init = tokio::time::timeout(VALIDATION_TIMEOUT, rpc.request("initialize", super::acp::initialize_params(Some(version)))).await;
        rpc.kill().await;
        let init = init.map_err(|_| "The installed Antigravity runtime did not answer in time.".to_owned())?.map_err(|_| "The installed Antigravity runtime did not initialize.".to_owned())?;
        let caps = &init["agentCapabilities"];
        let ok = init["agentInfo"]["name"] == "Antigravity ACP Server"
            && init["agentInfo"]["version"] == version
            && matches!(init["protocolVersion"].as_u64(), Some(1 | 2))
            && caps["loadSession"] == true
            && !caps["sessionCapabilities"]["resume"].is_null()
            && !caps["auth"]["logout"].is_null()
            && init["authMethods"].as_array().is_some_and(|m| m.iter().any(|m| m["id"] == "oauth-personal"));
        if ok { Ok(()) } else { Err("The downloaded Antigravity runtime is not the release it claims to be. Nothing was installed.".to_owned()) }
    }
    .await;
    let _ = std::fs::remove_dir_all(&scratch);
    result
}

/// Downloads, checks, unpacks, proves and activates a release.
async fn install(release: Release) -> Result<(), String> {
    let root = root();
    let versions = root.join("versions");
    std::fs::create_dir_all(&versions).map_err(|e| format!("Could not create the Antigravity install folder: {e}"))?;
    update(|s| s.can_remove = true);
    let release_id = release.sha512[..64].to_owned();
    let destination = versions.join(&release_id);
    if destination.exists() {
        // Installed before: proved again, then made active.
        let (version, name, _) = record(&destination).ok_or("The existing managed release is incomplete. Remove it before reinstalling.")?;
        if version != release.version {
            return Err("The existing managed release has the wrong version. Remove it before reinstalling.".into());
        }
        update(|s| {
            s.phase = "verifying";
            s.message = Some("Checking the installed runtime.".into());
        });
        validate(&destination.join(name), &release.version).await?;
        return activate(&root, &release_id, &release.version);
    }
    let required = MAX_DOWNLOAD_BYTES + MAX_EXECUTABLE_BYTES + FREE_SPACE_MARGIN;
    if free_space(&versions).is_some_and(|free| free < required) {
        return Err(format!("Antigravity needs at least {} MiB of free space to install.", required / 1024 / 1024));
    }
    let staging = versions.join(format!(".install-{}", crate::util::new_uuid()));
    let runtime = staging.join("runtime");
    std::fs::create_dir_all(&runtime).map_err(|e| e.to_string())?;
    let result = async {
        let archive = staging.join("download");
        download(&release, &archive).await?;
        update(|s| {
            s.phase = "extracting";
            s.message = Some("Extracting the verified runtime.".into());
        });
        let exe = runtime.join(release.executable);
        if release.archive {
            extract(&archive, release.executable, &exe).await?;
        } else {
            std::fs::copy(&archive, &exe).map_err(|e| format!("Could not copy the Antigravity executable: {e}"))?;
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
        }
        update(|s| {
            s.phase = "verifying";
            s.message = Some("Checking the downloaded runtime.".into());
        });
        validate(&exe, &release.version).await?;
        let bytes = std::fs::metadata(&exe).map_err(|e| e.to_string())?.len();
        let record = json!({ "releaseId": release_id, "version": release.version, "executable": { "name": release.executable, "bytes": bytes }, "harness": null });
        std::fs::write(runtime.join(".install-complete.json"), format!("{record}\n")).map_err(|e| e.to_string())?;
        std::fs::rename(&runtime, &destination).map_err(|e| format!("Could not move the runtime into place: {e}"))?;
        activate(&root, &release_id, &release.version)
    }
    .await;
    let _ = std::fs::remove_dir_all(&staging);
    result
}

/// Points `active.json` at the release, replacing it at once.
fn activate(root: &Path, release_id: &str, version: &str) -> Result<(), String> {
    let pointer = root.join(format!("active.json.{}", crate::util::new_uuid()));
    std::fs::write(&pointer, json!({ "releaseId": release_id }).to_string()).map_err(|e| e.to_string())?;
    std::fs::rename(&pointer, root.join("active.json")).map_err(|_| "Could not activate Antigravity. The previous runtime is unchanged. Check for locked files and try again.".to_owned())?;
    let exe = active_executable();
    update(|s| {
        s.phase = "succeeded";
        s.installed_version = Some(version.to_owned());
        s.executable_path = exe.map(|e| e.to_string_lossy().into_owned());
        s.source = Some("managed");
        s.message = None;
    });
    Ok(())
}

async fn download(release: &Release, to: &Path) -> Result<(), String> {
    use sha2::Digest;
    use std::io::Write;
    let failed = "Could not download Antigravity from Google. Check the network connection and any proxy settings, then try again.".to_owned();
    let client = reqwest::Client::builder().timeout(DOWNLOAD_TIMEOUT).build().map_err(|_| failed.clone())?;
    let mut answer = client.get(&release.url).send().await.map_err(|_| failed.clone())?;
    if !answer.status().is_success() {
        return Err(failed);
    }
    let total = answer.content_length();
    update(|s| s.total_bytes = total);
    let mut file = std::fs::File::create(to).map_err(|e| e.to_string())?;
    let mut hash = sha2::Sha512::new();
    let (mut downloaded, mut shown) = (0u64, std::time::Instant::now());
    while let Some(chunk) = answer.chunk().await.map_err(|_| failed.clone())? {
        downloaded += chunk.len() as u64;
        if downloaded > MAX_DOWNLOAD_BYTES {
            return Err("The Antigravity download exceeded its allowed size.".into());
        }
        hash.update(&chunk);
        file.write_all(&chunk).map_err(|e| e.to_string())?;
        if shown.elapsed() >= Duration::from_millis(250) {
            shown = std::time::Instant::now();
            update(|s| s.downloaded_bytes = downloaded);
        }
    }
    update(|s| s.downloaded_bytes = downloaded);
    if hex::encode(hash.finalize()) != release.sha512 {
        return Err("The Antigravity download failed its size or checksum check. Nothing was installed.".into());
    }
    Ok(())
}

/// The archive's one regular file, unpacked to `to`.
async fn extract(archive: &Path, name: &str, to: &Path) -> Result<(), String> {
    let wrong = || "The Antigravity archive must contain one regular executable.".to_owned();
    let tar = |args: &[&str]| {
        let mut cmd = crate::process::command("tar");
        cmd.args(args).arg(archive);
        cmd
    };
    let list = crate::process::output(tar(&["-tzf"]), Duration::from_secs(300)).await.map_err(|_| wrong())?;
    let types = crate::process::output(tar(&["-tvzf"]), Duration::from_secs(300)).await.map_err(|_| wrong())?;
    let types = String::from_utf8_lossy(&types.stdout).into_owned();
    if String::from_utf8_lossy(&list.stdout).trim() != name || types.trim().lines().count() != 1 || !types.starts_with('-') {
        return Err(wrong());
    }
    let failed = || "Could not extract the Antigravity executable.".to_owned();
    let out = std::fs::File::create(to).map_err(|e| e.to_string())?;
    let mut cmd = tar(&["-xOzf"]);
    // Straight into the file (`output` would collect it in memory instead).
    cmd.arg(name).stdout(out).stderr(std::process::Stdio::null()).kill_on_drop(true);
    let mut child = cmd.spawn().map_err(|_| failed())?;
    let status = tokio::time::timeout(Duration::from_secs(600), child.wait()).await.map_err(|_| failed())?.map_err(|_| failed())?;
    let size = std::fs::metadata(to).map(|m| m.len()).unwrap_or(0);
    if !status.success() || size == 0 || size > MAX_EXECUTABLE_BYTES {
        return Err("Could not extract the Antigravity executable.".into());
    }
    Ok(())
}

fn error(instance: &str, operation: &'static str, detail: &str) -> Failure {
    Failure::setup(instance, operation, detail)
}

/// `provider.install.start`: installs the channel's release (or proves the
/// installed one, when it is as new).
pub async fn start(instance: &str) -> Result<InstallState, Failure> {
    if busy() {
        return Ok(current());
    }
    let release = fetch_release().await.map_err(|e| error(instance, "install", &e))?;
    let installed = active();
    let operation_id = crate::util::new_uuid();
    let reuse = installed.filter(|(_, v, _)| super::version_tuple(v) >= super::version_tuple(&release.version));
    {
        let mut running = crate::util::locked(&RUNNING);
        if running.is_some() {
            return Ok(current());
        }
        update(|s| {
            s.operation_id = Some(operation_id.clone());
            s.phase = if reuse.is_some() { "verifying" } else { "downloading" };
            s.downloaded_bytes = 0;
            s.total_bytes = None;
            s.version = Some(reuse.as_ref().map_or_else(|| release.version.clone(), |(_, v, _)| v.clone()));
            s.message = Some(if reuse.is_some() { "Checking the installed runtime." } else { "Downloading Google's official Antigravity runtime." }.into());
        });
        let op = operation_id.clone();
        let task = tokio::spawn(async move {
            let result = match reuse {
                Some((_, version, exe)) => validate(&exe, &version).await.map(|()| {
                    update(|s| {
                        s.phase = "succeeded";
                        s.installed_version = Some(version.clone());
                        s.message = None;
                    });
                }),
                None => install(release).await,
            };
            if let Err(message) = result {
                update(|s| {
                    if s.operation_id.as_deref() == Some(op.as_str()) && s.phase != "succeeded" {
                        s.phase = "failed";
                        s.message = Some(message);
                    }
                });
            }
            let mut running = crate::util::locked(&RUNNING);
            if running.as_ref().is_some_and(|(id, _)| *id == op) {
                *running = None;
            }
        });
        *running = Some((operation_id, task.abort_handle()));
    }
    Ok(current())
}

/// `provider.install.cancel`.
pub fn cancel(instance: &str, operation_id: &str) -> Result<InstallState, Failure> {
    if current().operation_id.as_deref() != Some(operation_id) {
        return Err(error(instance, "cancel-install", "This installation is no longer current. Refresh its status before cancelling."));
    }
    if let Some((id, task)) = crate::util::locked(&RUNNING).take() {
        if id == operation_id && busy() {
            task.abort();
            update(|s| {
                s.phase = "cancelled";
                s.message = Some("Installation cancelled. The previous runtime is unchanged.".into());
            });
            // What the cancelled download left.
            if let Ok(entries) = std::fs::read_dir(root().join("versions")) {
                for e in entries.flatten().filter(|e| e.file_name().to_string_lossy().starts_with(".install-")) {
                    let _ = std::fs::remove_dir_all(e.path());
                }
            }
        }
    }
    Ok(current())
}

/// `provider.install.remove`: not while it installs or an agent runs from
/// it, nor while an instance's binary path leads into it.
pub fn remove(instance: &str) -> Result<InstallState, Failure> {
    if busy() || leased() {
        return Err(error(instance, "remove-install", "Stop Antigravity sessions and sign-in flows before removing its managed runtime."));
    }
    let root = root();
    if let Ok(real) = std::fs::canonicalize(&root) {
        for id in super::instances() {
            let path = super::config(&id).binary_path;
            if path.is_empty() {
                continue;
            }
            let resolved = std::fs::canonicalize(super::expand_tilde(&path)).unwrap_or_else(|_| PathBuf::from(&path));
            if resolved.starts_with(&real) {
                return Err(error(instance, "remove-install", "A provider instance has a custom path inside this managed runtime. Clear that path before removing it."));
            }
        }
    }
    std::fs::remove_dir_all(&root).or_else(|e| if e.kind() == std::io::ErrorKind::NotFound { Ok(()) } else { Err(e) }).map_err(|_| {
        error(instance, "remove-install", "Could not remove the managed Antigravity runtime. Check for open processes and try again.")
    })?;
    update(|s| {
        *s = InstallState { version: None, ..idle_state() };
    });
    Ok(current())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_release_this_platform_gets_is_taken() {
        let sha = "ab".repeat(64);
        let manifest = |version: &str, url: &str| json!({ "version": version, "url": url, "sha512": sha });
        let good = manifest("1.5.14", "https://storage.googleapis.com/antigravity-public/antigravity-cli/1.5.14-123/darwin-arm/cli_mac_arm64.tar.gz");
        let r = parse_release(&good, "darwin", "arm64", false, DOWNLOAD_BASE).unwrap();
        assert_eq!((r.version.as_str(), r.executable, r.archive), ("1.5.14", "antigravity", true));
        assert!(parse_release(&good, "linux", "arm64", false, DOWNLOAD_BASE).is_none(), "another platform's file");
        let musl = manifest("1.5.14", "https://storage.googleapis.com/antigravity-public/antigravity-cli/1.5.14-9/linux-x64-musl/cli_linux_x64_musl.tar.gz");
        assert!(parse_release(&musl, "linux", "x64", true, DOWNLOAD_BASE).is_some());
        let windows = manifest("1.6.0", "https://storage.googleapis.com/antigravity-public/antigravity-cli/1.6.0-1/windows-x64/cli_windows_x64.exe");
        assert_eq!(parse_release(&windows, "win32", "x64", false, DOWNLOAD_BASE).map(|r| r.executable), Some("agy.exe"));
        assert!(parse_release(&manifest("1.4.5", "https://storage.googleapis.com/antigravity-public/antigravity-cli/1.4.5-1/darwin-arm/cli_mac_arm64.tar.gz"), "darwin", "arm64", false, DOWNLOAD_BASE).is_none(), "older than the CLI");
        assert!(parse_release(&manifest("1.5.14", "https://evil.example/antigravity-cli/1.5.14-1/darwin-arm/cli_mac_arm64.tar.gz"), "darwin", "arm64", false, DOWNLOAD_BASE).is_none());
        let bad_sha = json!({ "version": "1.5.14", "url": good["url"], "sha512": "xyz" });
        assert!(parse_release(&bad_sha, "darwin", "arm64", false, DOWNLOAD_BASE).is_none());
    }
}
