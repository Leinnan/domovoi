//! Google Antigravity (t3code's `antigravity` driver): Google's `agy` CLI,
//! driven over ACP (`agy acp`, see [`crate::adapters::acp`]), each provider
//! instance in a profile of its own.
//!
//! After t3code `provider/AntigravityProvider.ts`, `antigravityAuthSupport.ts`,
//! `Drivers/AntigravityDriver.ts` and `acp/AntigravityAcpSupport.ts`:
//! - an instance's agent runs with `GEMINI_HOME` set to its own profile under
//!   `<userdata>/providers/antigravity/<sha256(instance id)>`, so its Google
//!   sign-in, hooks and MCP servers are its own; the user's global skills
//!   (`~/.gemini/config/skills`, `~/.gemini/antigravity-cli/skills`) are
//!   linked in;
//! - Google credentials in the server's environment are not passed on: the
//!   instance's sign-in method and credentials are the only ones it uses;
//! - the status probe never starts the agent (each start is expensive): it
//!   finds the executable and its version, and shows the account as the last
//!   session (or sign-in) left it, kept in the profile across restarts.

#![expect(clippy::disallowed_types, clippy::disallowed_macros, reason = "the ACP agent's `session/new` answer and config options, read as the agent sends them")]

pub mod acp;
pub mod auth;
pub mod child_work;
pub mod launch;
pub mod limits;
pub mod plan;
pub mod protocol;
pub mod skills;
pub mod usage;

use crate::providers::Tool;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use domovoi_model::typed::{
    ModelCapabilities, ProviderOptionChoice, ProviderOptionDescriptor, ProviderOptionRole, RuntimeMode, SelectProviderOptionDescriptor, ServerProvider,
    ServerProviderAuthStatus, ServerProviderModel, ServerProviderSetup, ServerProviderSlashCommand, ServerProviderState,
};

pub const DRIVER: &str = "antigravity";
/// The built-in instance's id (t3code's default instance of the driver).
pub const BUILT_IN: &str = "antigravity";
pub const DISPLAY_NAME: &str = "Antigravity";
/// The model that leaves the choice to the account's default; never sent to the agent.
pub const DEFAULT_MODEL: &str = "antigravity-default";
/// The catalog's default chat model (t3code `model-manifest.json`).
pub const DEFAULT_CHAT_MODEL: &str = "gemini-3.8-flash-high";
pub const TOOL: Tool = Tool { program: "agy", label: "Antigravity", env: "DOMOVOI_ANTIGRAVITY_BINARY" };
/// The agent's own name of its executable on older installs.
const ALTERNATE_PROGRAM: &str = "antigravity";
/// The oldest `agy` with the native `acp` subcommand.
const MIN_VERSION: (u64, u64, u64) = (1, 5, 0);
/// The range t3code supports for the native CLI (`>=1.5.1 <2`).
const SUPPORTED: ((u64, u64, u64), (u64, u64, u64)) = ((1, 5, 1), (2, 0, 0));

pub const SIGN_IN_REQUIRED: &str = "Sign in to Antigravity in Settings before you continue.";
pub const AUTH_STDOUT_PREFIX: &str = "Open the following link to authenticate the ACP server: ";
pub const AUTH_BROWSER_MARKER: &str = "__T3_ANTIGRAVITY_AUTH_URL__";
const SIGN_IN_MESSAGE: &str = "Sign in with Google to use Antigravity.";
const AUTH_UNCHECKED_MESSAGE: &str = "Antigravity is installed. Google account access is not checked yet.";
const MISSING_MESSAGE: &str = "Antigravity ACP runtime is not installed or its executable could not be found.";

/// Inherited variables the agent must not see (t3code `removedEnvironmentKeys`):
/// Google credentials of the server's environment, and what Domovoi sets itself.
pub const REMOVED_ENV: [&str; 18] = [
    "GEMINI_API_KEY",
    "GOOGLE_API_KEY",
    "GOOGLE_APPLICATION_CREDENTIALS",
    "GOOGLE_CLOUD_PROJECT",
    "GOOGLE_CLOUD_LOCATION",
    "GOOGLE_CLOUD_QUOTA_PROJECT",
    "GOOGLE_GENAI_USE_VERTEXAI",
    "GCLOUD_PROJECT",
    "CLOUDSDK_CORE_PROJECT",
    "AGY_ACP_CCPA_PROJECT",
    "AGY_ACP_ENABLE_OAUTH",
    "GEMINI_HOME",
    "AGY_ACP_FORCE_FILE_STORAGE",
    "ANTIGRAVITY_HARNESS_PATH",
    "BROWSER",
    "PYTHONUNBUFFERED",
    "ELECTRON_RUN_AS_NODE",
    // An inherited pipe whose owner exited makes the agent print crash-reporter errors.
    "CHROME_CRASHPAD_PIPE_NAME",
];

/// How an instance signs in to Google (`AntigravitySettings.authMethod`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AuthMethod {
    #[default]
    OauthPersonal,
    OauthBusiness,
    GeminiApiKey,
    AgentPlatform,
}

impl AuthMethod {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "oauth-personal" => Self::OauthPersonal,
            "oauth-business" => Self::OauthBusiness,
            "gemini-api-key" => Self::GeminiApiKey,
            "agent-platform" => Self::AgentPlatform,
            _ => return None,
        })
    }

    /// The id the agent and the settings use.
    pub fn id(self) -> &'static str {
        match self {
            Self::OauthPersonal => "oauth-personal",
            Self::OauthBusiness => "oauth-business",
            Self::GeminiApiKey => "gemini-api-key",
            Self::AgentPlatform => "agent-platform",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::OauthPersonal => "Google account",
            Self::OauthBusiness => "Gemini Enterprise",
            Self::GeminiApiKey => "Gemini API key",
            Self::AgentPlatform => "Agent Platform",
        }
    }

    /// Signs in through Google in a browser (else with credentials).
    pub fn uses_browser(self) -> bool {
        matches!(self, Self::OauthPersonal | Self::OauthBusiness)
    }
}

/// An instance's settings (its `providerInstances` entry's `config`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Config {
    pub auth_method: AuthMethod,
    /// The API key, its secret resolved; empty when none is set.
    pub api_key: String,
    /// The API key is a secret reference that could not be read.
    pub api_key_unresolved: Option<String>,
    pub gcp_project: String,
    pub gcp_location: String,
    pub binary_path: String,
    pub enabled: bool,
    pub display_name: Option<String>,
    /// An instance settings add beside the built-in one.
    pub extra: bool,
}

/// Why an added instance can't use a Google sign-in: the native CLI keeps
/// one in the user's keychain (found through `HOME`), which every agent of
/// the user shares, terminal and IDE included.
pub const SHARED_LOGIN: &str = "Antigravity keeps one Google sign-in for this user on the server's machine (in the keychain, shared with the agy command and the Antigravity IDE), so an added Antigravity instance can't sign in to another Google account. Use the built-in Antigravity instance for the Google account, or choose the Gemini API key or Agent Platform method here.";

/// What a sign-out from Domovoi says instead of signing the shared account out.
pub const SIGN_OUT_ELSEWHERE: &str = "Antigravity uses this machine's agy sign-in, which the agy command and the Antigravity IDE share, so Domovoi doesn't sign it out of Google. To change accounts, run `agy` on the server's machine and use /logout there, then sign in again.";

impl Config {
    /// Why the instance can't start with these settings (t3code `antigravityAuthConfigIssue`).
    pub fn issue(&self) -> Option<String> {
        if let Some(provider) = &self.api_key_unresolved {
            return Some(format!("Could not read the credential from the secret provider \"{provider}\". Unlock or fix it in Settings, then try again."));
        }
        if self.extra && self.auth_method.uses_browser() {
            return Some(SHARED_LOGIN.into());
        }
        let gcp = !self.gcp_project.is_empty() && !self.gcp_location.is_empty();
        match self.auth_method {
            AuthMethod::OauthPersonal => None,
            AuthMethod::OauthBusiness if !gcp => Some("Gemini Enterprise needs a GCP project and location in the Antigravity provider settings.".into()),
            AuthMethod::GeminiApiKey if self.api_key.is_empty() => Some("Enter a Gemini API key in the Antigravity provider settings.".into()),
            AuthMethod::AgentPlatform if self.api_key.is_empty() && !gcp => {
                Some("Agent Platform needs an API key, or a GCP project and location, in the Antigravity provider settings.".into())
            }
            _ => None,
        }
    }

    /// What the agent's own process sees of these settings: changing any of
    /// it stops the instance's sessions (they run with the old ones).
    pub fn launch_key(&self) -> String {
        crate::util::sha256_hex(format!("{}\0{}\0{}\0{}\0{}", self.auth_method.id(), self.api_key, self.gcp_project, self.gcp_location, self.binary_path).as_bytes())
    }
}

/// The settings of instance `id`: its configuration, or the defaults for the
/// built-in instance no settings name.
pub fn config(id: &str) -> Config {
    let configured = crate::providers::instance(id);
    let c = configured.as_ref().and_then(|i| i.config.clone()).unwrap_or(Value::Null);
    let text = |key: &str| c[key].as_str().map(str::trim).unwrap_or_default().to_owned();
    let (api_key, api_key_unresolved) = match crate::secrets::resolve(id, "apiKey", &c["apiKey"]) {
        Ok(key) => (key, None),
        Err(provider) => (String::new(), Some(provider)),
    };
    Config {
        auth_method: c["authMethod"].as_str().and_then(AuthMethod::parse).unwrap_or_default(),
        api_key,
        api_key_unresolved,
        gcp_project: text("gcpProject"),
        gcp_location: text("gcpLocation"),
        binary_path: text("binaryPath"),
        enabled: configured.as_ref().and_then(|i| i.enabled).or(c["enabled"].as_bool()).unwrap_or(true),
        display_name: configured.and_then(|i| i.display_name).filter(|n| !n.trim().is_empty()),
        extra: id != BUILT_IN,
    }
}

/// Every Antigravity instance: the built-in one, and those settings add.
pub fn instances() -> Vec<String> {
    let mut ids = vec![BUILT_IN.to_owned()];
    ids.extend(crate::providers::instances_of(DRIVER).into_iter().map(|(id, _)| id).filter(|id| id != BUILT_IN));
    ids
}

/// Whether `id` names an Antigravity instance.
pub fn is_instance(id: &str) -> bool {
    id == BUILT_IN && crate::providers::instance(id).is_none_or(|c| c.driver.as_str() == DRIVER)
        || crate::providers::instance(id).is_some_and(|c| c.driver.as_str() == DRIVER)
}

// ---------------------------------------------------------------------------
// The executable
// ---------------------------------------------------------------------------

/// The agent's executable and its version.
#[derive(Debug, Clone, PartialEq)]
pub struct Executable {
    pub path: PathBuf,
    pub version: Option<String>,
}

/// `1.5.14` as numbers.
pub fn version_tuple(v: &str) -> Option<(u64, u64, u64)> {
    let mut parts = v.trim().trim_start_matches('v').split(['.', '-', ' ']).map(|p| p.parse::<u64>().ok());
    Some((parts.next()??, parts.next()??, parts.next().flatten().unwrap_or(0)))
}

/// `~/x` and `~` as the user's home.
pub(crate) fn expand_tilde(p: &str) -> PathBuf {
    match p.strip_prefix("~") {
        Some("") => crate::util::home_dir(),
        Some(rest) if rest.starts_with('/') => crate::util::home_dir().join(&rest[1..]),
        _ => PathBuf::from(p),
    }
}

/// Where the agent is: the instance's binary path (a path, or a name looked
/// up like a CLI), else Domovoi's managed install, else `agy` (or
/// `antigravity`) where CLIs live.
pub fn locate(config: &Config) -> Result<PathBuf, String> {
    let dirs = crate::providers::search_dirs();
    let resolve = |p: PathBuf| std::fs::canonicalize(&p).unwrap_or(p);
    if !config.binary_path.is_empty() {
        let given = expand_tilde(&config.binary_path);
        if given.components().count() > 1 || given.is_absolute() {
            return if crate::providers::is_executable(&given) { Ok(resolve(given)) } else { Err(format!("Antigravity was not found at {} (its binary path setting).", given.display())) };
        }
        return dirs.iter().map(|d| d.join(&given)).find(|p| crate::providers::is_executable(p)).map(resolve).ok_or_else(|| format!("Antigravity's binary path names `{}`, which is not on the server's PATH.", config.binary_path));
    }
    if let Some(managed) = install::active_executable() {
        return Ok(managed);
    }
    crate::providers::executable(TOOL).or_else(|e| dirs.iter().map(|d| d.join(ALTERNATE_PROGRAM)).find(|p| crate::providers::is_executable(p)).map(resolve).ok_or(e))
}

/// The executable and its version (`agy --version`).
pub async fn resolve(config: &Config) -> Result<Executable, String> {
    let path = locate(config)?;
    let mut cmd = crate::process::command(&path);
    cmd.arg("--version");
    let out = crate::process::output(cmd, std::time::Duration::from_secs(20)).await.ok().filter(|o| o.status.success());
    let version = out.and_then(|o| String::from_utf8_lossy(&o.stdout).split_whitespace().find(|w| version_tuple(w).is_some()).map(|w| w.trim_start_matches('v').to_owned()));
    if let Some(v) = version.as_deref().and_then(version_tuple)
        && v < MIN_VERSION
    {
        return Err(format!(
            "Domovoi needs the Antigravity CLI 1.5 or newer (found {}). Update it with `agy update`, or install it from https://antigravity.google/docs/getting-started.",
            version.as_deref().unwrap_or_default()
        ));
    }
    Ok(Executable { path, version })
}

/// A note when the version is outside the range t3code supports.
fn version_advice(version: Option<&str>) -> Option<String> {
    let v = version_tuple(version?)?;
    let ((lo, hi), shown) = (SUPPORTED, version.unwrap_or_default());
    (v < lo || v >= hi).then(|| format!("Antigravity {shown} has not been checked with Domovoi (it supports 1.5.1 up to 2.0); it may still work."))
}

// ---------------------------------------------------------------------------
// The profile
// ---------------------------------------------------------------------------

/// An instance's own `GEMINI_HOME`, and where its agents unpack themselves.
#[derive(Debug, Clone, PartialEq)]
pub struct Profile {
    /// `GEMINI_HOME`.
    pub home: PathBuf,
    /// `<home>/antigravity-acp`: the agent's data (sign-in, conversations).
    pub acp: PathBuf,
    /// Each launch gets its own folder here (it unpacks about 1 GB).
    pub temp: PathBuf,
}

/// The server's state folder (`<home>/userdata`).
pub fn state_dir() -> PathBuf {
    if let Some(state) = crate::state::STATE.get() {
        return state.paths.userdata.clone();
    }
    crate::managed::HOME.get().map_or_else(|| std::env::temp_dir().join("domovoi-antigravity"), |h| h.join("userdata"))
}

/// Instance `id`'s profile. Its id is hashed, so ids that differ only in
/// case stay apart on a case-insensitive file system.
pub fn profile(id: &str) -> Profile {
    profile_in(&state_dir(), id)
}

pub fn profile_in(state: &Path, id: &str) -> Profile {
    let key = crate::util::sha256_hex(id.as_bytes());
    let home = state.join("providers").join("antigravity").join(&key);
    Profile { acp: home.join("antigravity-acp"), temp: state.join("antigravity-tmp").join(&key[..12]), home }
}

/// `<acp>/settings.json`: the sign-in method (so a native logout clears only
/// its credentials) and the GCP project and location. Never a credential.
pub fn profile_settings(config: &Config) -> String {
    let mut v = json!({ "auth": { "type": config.auth_method.id() } });
    let mut gcp = serde_json::Map::new();
    if !config.gcp_project.is_empty() {
        gcp.insert("project".into(), json!(config.gcp_project));
    }
    if !config.gcp_location.is_empty() {
        gcp.insert("location".into(), json!(config.gcp_location));
    }
    if !gcp.is_empty() {
        v["gcp"] = Value::Object(gcp);
    }
    format!("{v}\n")
}

fn private_dir(dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// The user's global skill folders, as linked into a profile.
pub fn user_skill_dirs(gemini_home: &Path) -> [PathBuf; 2] {
    [gemini_home.join("config").join("skills"), gemini_home.join("antigravity-cli").join("skills")]
}

/// Links the user's global skills into the profile (its private
/// `GEMINI_HOME` would hide them). A missing target unlinks; an empty
/// placeholder folder the agent made is replaced; anything else is kept.
fn link_user_skills(profile: &Profile, user_home: &Path) {
    let links = user_skill_dirs(&profile.home);
    let targets = user_skill_dirs(&user_home.join(".gemini"));
    for (link, target) in links.iter().zip(targets.iter()) {
        let existing = std::fs::read_link(link).ok().map(|t| link.parent().map_or(t.clone(), |p| p.join(&t)));
        if !target.exists() {
            if existing.is_some() {
                let _ = std::fs::remove_file(link);
            }
            continue;
        }
        if existing.as_deref() == Some(target.as_path()) {
            continue;
        }
        if existing.is_some() {
            let _ = std::fs::remove_file(link);
        } else if link.exists() && std::fs::remove_dir(link).is_err() {
            // A real folder with content: the user's, left alone.
            continue;
        }
        if let Some(parent) = link.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        #[cfg(unix)]
        if let Err(e) = std::os::unix::fs::symlink(target, link) {
            tracing::warn!(link = %link.display(), "Antigravity user skills are not linked into the profile: {e}");
        }
    }
}

/// Makes the profile ready for a launch: its folders (private to the user),
/// its settings file and the links to the user's skills.
pub fn prepare(profile: &Profile, config: &Config) -> Result<(), String> {
    for dir in [&profile.home, &profile.acp, &profile.temp] {
        private_dir(dir).map_err(|e| format!("The Antigravity profile directory could not be created: {e}"))?;
    }
    std::fs::write(profile.acp.join("settings.json"), profile_settings(config)).map_err(|e| format!("The Antigravity profile settings could not be written: {e}"))?;
    link_user_skills(profile, &crate::skills::agent_home());
    Ok(())
}

/// Removes what force-killed launches left in the profile's temp folder.
pub fn sweep_temp(profile: &Profile) {
    let Ok(entries) = std::fs::read_dir(&profile.temp) else { return };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().starts_with("run-") {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// Removes a throwaway session's files from the profile (sign-in checks,
/// text generation): its conversation database and its `brain` folder, only
/// when the session's own record says it ran in `cwd` (a scratch folder no
/// other session has) and its id is the agent's kind (a UUID v4).
pub fn remove_session_files(profile: &Profile, session_id: Option<&str>, cwd: &str) {
    let Some(id) = session_id.filter(|id| uuid::Uuid::parse_str(id).is_ok_and(|u| u.get_version_num() == 4)) else { return };
    let base = profile.acp.join("conversations").join(id);
    let meta = std::fs::read_to_string(base.with_extension("meta")).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok());
    if meta.as_ref().and_then(|m| m["cwd"].as_str()) != Some(cwd) {
        return;
    }
    for suffix in ["db", "db-wal", "db-shm", "db-journal", "meta"] {
        let _ = std::fs::remove_file(base.with_extension(suffix));
    }
    let _ = std::fs::remove_dir_all(profile.acp.join("brain").join(id));
}

/// A new temp folder for one launch.
pub fn launch_temp(profile: &Profile) -> Result<PathBuf, String> {
    let dir = profile.temp.join(format!("run-{}", crate::util::new_uuid()));
    private_dir(&dir).map_err(|e| format!("Could not create an Antigravity runtime temp directory: {e}"))?;
    Ok(dir)
}

/// `BROWSER` for the agent: this server's `antigravity-browser-url`, which
/// writes the sign-in link to stderr for the server to read, so no browser
/// opens on the server's machine. Quoted as a shell word; Python's
/// `webbrowser` splits `BROWSER` on `:` first, so a path with one can't be used.
pub fn browser_command() -> Result<String, String> {
    let exe = std::env::current_exe().map_err(|e| format!("The server's own path is unknown: {e}"))?.to_string_lossy().into_owned();
    if exe.contains(':') || exe.contains(['\r', '\n', '\0']) || exe.contains("%s") {
        return Err("The server's path cannot be used to keep Antigravity from opening a browser.".into());
    }
    let quote = |s: &str| format!("'{}'", s.replace('\'', "'\"'\"'"));
    Ok([quote(&exe), quote(BROWSER_SUBCOMMAND), quote("%s")].join(" "))
}

/// The hidden subcommand `BROWSER` runs.
pub const BROWSER_SUBCOMMAND: &str = "antigravity-browser-url";

/// The file a launch's `open` hands Google sign-in links to.
pub fn signin_link_file(launch_temp: &Path) -> PathBuf {
    launch_temp.join("signin-link")
}

/// `open` and `xdg-open` for one launch, first on the agent's PATH: the
/// native CLI opens its Google sign-in link with them (it reads no
/// `BROWSER`), so a link goes to the server (to [`signin_link_file`])
/// instead of a browser on the server's machine; anything else is opened
/// by the real command. Returns the folder to put first on PATH.
pub fn write_open_shims(launch_temp: &Path) -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("The server's own path is unknown: {e}"))?;
    let dir = launch_temp.join("bin");
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let quote = |s: &str| format!("'{}'", s.replace('\'', "'\"'\"'"));
    let link_file = signin_link_file(launch_temp);
    for name in ["open", "xdg-open"] {
        let real = crate::providers::search_dirs().into_iter().map(|d| d.join(name)).find(|p| crate::providers::is_executable(p));
        let fallback = real.map_or_else(|| "exit 1".to_owned(), |r| format!("exec {} \"$@\"", quote(&r.to_string_lossy())));
        let script = format!(
            "#!/bin/sh\ncase \"$1\" in\n  https://accounts.google.com/*) exec {} {} --to {} \"$1\" ;;\nesac\n{fallback}\n",
            quote(&exe.to_string_lossy()),
            BROWSER_SUBCOMMAND,
            quote(&link_file.to_string_lossy()),
        );
        let path = dir.join(name);
        std::fs::write(&path, script).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
        }
    }
    Ok(dir)
}

/// What `antigravity-browser-url <url>` does: one marker line on stderr.
pub fn browser_url_line(url: &str) -> String {
    format!("{AUTH_BROWSER_MARKER}{}\n", Value::String(url.to_owned()))
}

/// A certificate bundle for the agent's bundled Python on Linux, which may
/// look where this distribution has none.
fn ca_bundle() -> Option<String> {
    if !cfg!(target_os = "linux") || std::env::var_os("SSL_CERT_FILE").is_some() || std::env::var_os("SSL_CERT_DIR").is_some() {
        return None;
    }
    let nix = std::env::var("NIX_SSL_CERT_FILE").ok();
    nix.into_iter()
        .chain(["/etc/ssl/certs/ca-bundle.crt", "/etc/ssl/certs/ca-certificates.crt", "/etc/pki/tls/certs/ca-bundle.crt", "/etc/ssl/cert.pem"].map(str::to_owned))
        .find(|c| Path::new(c).is_absolute() && Path::new(c).exists())
}

/// The variables set for the agent (over the server's own, less [`REMOVED_ENV`]).
pub fn environment(profile: &Profile, config: &Config, launch_temp: &Path, browser: &str) -> Vec<(String, String)> {
    let mut env = vec![];
    // The GCP pair goes in settings.json: an API key in the environment takes precedence.
    match config.auth_method {
        AuthMethod::GeminiApiKey if !config.api_key.is_empty() => env.push(("GEMINI_API_KEY".into(), config.api_key.clone())),
        AuthMethod::AgentPlatform if !config.api_key.is_empty() => env.push(("GOOGLE_API_KEY".into(), config.api_key.clone())),
        _ => {}
    }
    if let Some(bundle) = ca_bundle() {
        env.push(("SSL_CERT_FILE".into(), bundle));
    }
    let temp = launch_temp.to_string_lossy().into_owned();
    env.extend([
        ("GEMINI_HOME".to_owned(), profile.home.to_string_lossy().into_owned()),
        // Keeps the sign-in in the profile, not in an entry the macOS keychain shares.
        ("AGY_ACP_FORCE_FILE_STORAGE".to_owned(), "1".to_owned()),
        ("BROWSER".to_owned(), browser.to_owned()),
        ("PYTHONUNBUFFERED".to_owned(), "1".to_owned()),
        ("ELECTRON_RUN_AS_NODE".to_owned(), "1".to_owned()),
    ]);
    if cfg!(windows) {
        env.extend([("TEMP".to_owned(), temp.clone()), ("TMP".to_owned(), temp)]);
    } else {
        env.push(("TMPDIR".to_owned(), temp));
    }
    env
}

/// `agy acp`'s arguments. The data folder is relative: the CLI joins it
/// under `--gemini-dir`, absolute or not.
pub fn acp_args(profile: &Profile) -> Vec<String> {
    vec!["acp".into(), "--gemini-dir".into(), profile.home.to_string_lossy().into_owned(), "--app-data-dir".into(), "antigravity-acp".into()]
}

// ---------------------------------------------------------------------------
// The account, as sessions and sign-ins left it
// ---------------------------------------------------------------------------

/// What is known of an instance's Google account, kept in its profile.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Account {
    /// `authenticated`, `unauthenticated`, or unknown (`None`).
    #[serde(default)]
    pub auth: Option<String>,
    /// The sign-in method the state is for: another method's is unknown.
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub models: Vec<ServerProviderModel>,
    /// The thinking option each model showed when selected (`null`: none).
    #[serde(default)]
    pub thinking: BTreeMap<String, Value>,
    #[serde(default)]
    pub slash_commands: Vec<ServerProviderSlashCommand>,
    /// The last thing that went wrong signing in or starting, until a session starts.
    #[serde(default)]
    pub message: Option<String>,
    /// The folders sessions ran in lately (each has its own project skills), newest last.
    #[serde(default)]
    pub cwds: Vec<String>,
    /// The account's plan as `authenticate` names it (`Google AI Pro`).
    #[serde(default)]
    pub tier: Option<String>,
    /// The commands the agent listed in each folder of [`Account::cwds`].
    #[serde(default)]
    pub commands_by_cwd: BTreeMap<String, Vec<ServerProviderSlashCommand>>,
}

/// `authenticate` answered: the account's plan, when it names one
/// (`_meta.antigravity.tier.name`).
pub fn authenticated(id: &str, answer: &Value) {
    let tier = answer["_meta"]["antigravity"]["tier"]["name"].as_str().map(str::trim).filter(|t| !t.is_empty()).map(str::to_owned);
    if tier.is_some() {
        update_account(id, |a| a.tier = tier);
    }
}

/// How many folders an instance's entry describes (t3code's workspace snapshots).
const MAX_WORKSPACES: usize = 32;

static ACCOUNTS: std::sync::Mutex<BTreeMap<String, Account>> = std::sync::Mutex::new(BTreeMap::new());

fn account_file(id: &str) -> PathBuf {
    profile(id).home.join("domovoi-account.json")
}

/// Instance `id`'s account, read from its profile the first time.
pub fn account(id: &str) -> Account {
    let mut accounts = crate::util::locked(&ACCOUNTS);
    if let Some(a) = accounts.get(id) {
        return a.clone();
    }
    let a: Account = std::fs::read_to_string(account_file(id)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
    accounts.insert(id.to_owned(), a.clone());
    a
}

/// Changes instance `id`'s account; clients get a new snapshot when it changed.
pub fn update_account(id: &str, change: impl FnOnce(&mut Account)) {
    let mut a = account(id);
    let before = a.clone();
    change(&mut a);
    if a == before {
        return;
    }
    crate::util::locked(&ACCOUNTS).insert(id.to_owned(), a.clone());
    let file = account_file(id);
    if let Some(dir) = file.parent()
        && private_dir(dir).is_ok()
    {
        let _ = std::fs::write(&file, serde_json::to_string(&a).unwrap_or_default());
    }
    crate::providers::CLAUDE_COMMANDS_CHANGED.notify_one();
}

/// A session started (in `cwd`, a thread's folder): the account is signed in, with these models.
pub fn session_started(id: &str, method: AuthMethod, version: Option<&str>, session: &Value, cwd: Option<&str>) {
    update_account(id, |a| {
        if let Some(cwd) = cwd {
            a.cwds.retain(|c| c != cwd);
            a.cwds.push(cwd.to_owned());
            let excess = a.cwds.len().saturating_sub(MAX_WORKSPACES);
            a.cwds.drain(..excess);
        }
        a.auth = Some("authenticated".into());
        a.method = Some(method.id().into());
        a.message = None;
        if let Some(v) = version {
            a.version = Some(v.to_owned());
        }
        acp::observe_thinking(&mut a.thinking, &session["configOptions"]);
        let models = acp::models_from_session(session, &a.thinking);
        if !models.is_empty() {
            a.models = models;
        }
    });
    // A running agent can read the quota now.
    if crate::usage_limits::get(id).is_none_or(|l| l.unavailable.is_some()) || crate::usage_limits::stale(id, crate::providers::LIMITS_FRESH) {
        limits::refresh(id);
    }
}

/// The agent's config options changed (a model was chosen): its thinking levels.
pub fn config_options_changed(id: &str, options: &Value) {
    update_account(id, |a| {
        acp::observe_thinking(&mut a.thinking, options);
        if a.auth.as_deref() == Some("authenticated") {
            let models = acp::models_from_session(&json!({ "configOptions": options }), &a.thinking);
            if !models.is_empty() {
                a.models = models;
            }
        }
    });
}

/// The agent needs signing in (or signed out): nothing of the account is known.
pub fn signed_out(id: &str, method: AuthMethod) {
    update_account(id, |a| {
        *a = Account { auth: Some("unauthenticated".into()), method: Some(method.id().into()), version: a.version.clone(), ..Account::default() };
    });
    limits::forget(id);
    crate::usage_limits::clear(id);
}

/// The agent's slash commands, as it last listed them.
pub fn commands_listed(id: &str, cwd: &str, commands: Vec<ServerProviderSlashCommand>) {
    update_account(id, |a| {
        if a.auth.as_deref() != Some("unauthenticated") {
            // A folder's own commands (its skills and workflows); the latest are the instance's.
            a.commands_by_cwd.insert(cwd.to_owned(), commands.clone());
            a.commands_by_cwd.retain(|c, _| a.cwds.contains(c) || c == cwd);
            a.slash_commands = commands;
        }
    });
}

/// Each instance's launch settings ([`Config::launch_key`]), to compare after a settings change.
pub fn launch_keys() -> BTreeMap<String, String> {
    instances().into_iter().map(|id| {
        let key = config(&id).launch_key();
        (id, key)
    }).collect()
}

/// Stops the sessions of instances whose launch settings differ from `before`
/// (they run with the old sign-in method or credentials).
pub async fn stop_changed(before: &BTreeMap<String, String>) {
    let Some(host) = crate::adapters::HOST.get() else { return };
    for (id, key) in launch_keys() {
        if before.get(&id).is_some_and(|old| *old != key) {
            host.stop_instance(&id).await;
        }
    }
}

// ---------------------------------------------------------------------------
// The status entry
// ---------------------------------------------------------------------------

fn base_entry(id: &str, config: &Config) -> ServerProvider {
    let mut v = crate::providers::entry(id, DRIVER, config.display_name.as_deref().unwrap_or(DISPLAY_NAME));
    v.enabled = config.enabled;
    v.show_interaction_mode_toggle = Some(true);
    // No automatic review: Auto falls back to Supervised rather than approving everything.
    v.supported_runtime_modes = Some(vec![RuntimeMode::ApprovalRequired, RuntimeMode::AutoAcceptEdits, RuntimeMode::FullAccess].into());
    v.setup = Some(ServerProviderSetup { can_authenticate: true, can_install: install::supported(), documentation_url: None });
    v.supports_conversation_rollback = Some(false);
    v.supports_text_generation = Some(false);
    v.status = ServerProviderState::Warning;
    v
}

/// Instance `id`'s entry: `None` for the built-in instance when no agent is
/// installed and no settings name it (it isn't offered).
pub async fn probe(id: &str) -> Option<ServerProvider> {
    let config = config(id);
    let mut v = base_entry(id, &config);
    let configured = crate::providers::instance(id).is_some();
    if !config.enabled {
        v.message = Some("Antigravity is disabled in Domovoi's settings.".into());
        return Some(v);
    }
    match resolve(&config).await {
        Ok(exe) => {
            v.installed = true;
            v.version = exe.version.clone();
            install::check_for_update();
            v.version_advisory = None;
            if let Some(advice) = version_advice(exe.version.as_deref()) {
                v.message = Some(advice);
            }
        }
        Err(e) => {
            if !configured && id == BUILT_IN && !install::busy() {
                return None;
            }
            v.status = ServerProviderState::Error;
            v.message = Some(if e.contains("not found") || e.contains("isn't installed") { MISSING_MESSAGE.to_owned() } else { e });
            return Some(v);
        }
    }
    if let Some(issue) = config.issue() {
        v.status = ServerProviderState::Error;
        v.message = Some(issue);
    } else if account(id).auth.as_deref() == Some("authenticated") {
        // Limits refresh with provider status, in the background.
        limits::refresh(id);
    }
    Some(v)
}

/// The account's part of an entry: sign-in, models and commands as last
/// seen, and the readiness they give. Applied to every snapshot.
pub fn overlay(v: &mut ServerProvider) {
    let id = v.instance_id.as_str().to_owned();
    let config = config(&id);
    let a = account(&id);
    let current = a.method.as_deref() == Some(config.auth_method.id());
    let status = if current { a.auth.as_deref() } else { None };
    let credentials = !config.auth_method.uses_browser();
    let label = config.auth_method.label();
    // A Google account shows its plan where the agent names it.
    let shown = a.tier.clone().filter(|_| !credentials).unwrap_or_else(|| label.to_owned());
    v.auth = crate::providers::auth(
        match status {
            Some("authenticated") => ServerProviderAuthStatus::Authenticated,
            Some("unauthenticated") => ServerProviderAuthStatus::Unauthenticated,
            _ => ServerProviderAuthStatus::Unknown,
        },
        Some(config.auth_method.id()),
        (status == Some("authenticated")).then_some(shown.as_str()),
    );
    // Domovoi signs no account out: the Google one is shared with agy, a key is the settings'.
    v.auth.can_logout = Some(false);
    if !v.enabled || !v.installed || v.status == ServerProviderState::Error {
        return;
    }
    if v.version.is_none() {
        v.version = a.version.clone();
    }
    match status {
        Some("authenticated") => {
            v.status = ServerProviderState::Ready;
            v.models = a.models.clone();
            v.slash_commands = Some(a.slash_commands.clone());
            // Read each time: a skill added since shows at once.
            let home = crate::skills::agent_home();
            v.skills = Some(skills::discover(None, &home));
            v.workspace_snapshots = Some(
                a.cwds
                    .iter()
                    .map(|cwd| domovoi_model::typed::ServerProviderWorkspaceSnapshot {
                        cwd: cwd.clone(),
                        checked_at: v.checked_at.clone(),
                        slash_commands: a.commands_by_cwd.get(cwd).cloned().unwrap_or_else(|| a.slash_commands.clone()),
                        slash_commands_pending: None,
                        skills: skills::discover(Some(Path::new(cwd)), &home),
                        option_descriptors: None,
                        models: None,
                        models_pending: None,
                    })
                    .collect(),
            );
            v.supports_text_generation = Some(crate::antigravity::text_generation_available(&id));
        }
        Some("unauthenticated") => {
            v.status = ServerProviderState::Warning;
            v.message = Some(if credentials { format!("Connect with {label} to use Antigravity and load models.") } else { SIGN_IN_MESSAGE.to_owned() });
        }
        _ => {
            v.status = ServerProviderState::Warning;
            // Models of an earlier session still show, so a thread can start (and check access).
            v.models = if current { a.models.clone() } else { vec![] };
            v.message = Some(if credentials { format!("Antigravity is installed. Connect with {label} to load models.") } else { AUTH_UNCHECKED_MESSAGE.to_owned() });
        }
    }
    if let Some(message) = a.message.filter(|_| current && status != Some("authenticated")) {
        v.message = Some(message);
    }
    if v.models.is_empty() && status != Some("unauthenticated") {
        v.models = vec![default_model()];
    }
}

/// The account's default, before any session listed models.
fn default_model() -> ServerProviderModel {
    ServerProviderModel {
        slug: DEFAULT_MODEL.into(),
        name: "Antigravity default".into(),
        short_name: None,
        sub_provider: None,
        aliases: None,
        badge: None,
        is_custom: false,
        is_default: Some(true),
        is_legacy: None,
        supports_text_generation: None,
        capabilities: Some(ModelCapabilities { option_descriptors: Some(vec![]) }),
    }
}

/// A select option of the agent's (`configOptions` entry) as a model option
/// (t3code `acpProviderOptionDescriptors`); `with_current` keeps its value.
pub fn select_descriptor(option: &Value, with_current: bool) -> Option<ProviderOptionDescriptor> {
    let id = option["id"].as_str().filter(|id| !id.is_empty() && id.trim() == *id && id.len() <= 256)?;
    let mut seen = std::collections::HashSet::new();
    let choices: Vec<ProviderOptionChoice> = acp::flatten_choices(&option["options"])
        .into_iter()
        .filter_map(|c| {
            let value = c["value"].as_str()?;
            let id = if value.is_empty() { "_t3/empty".to_owned() } else { value.to_owned() };
            seen.insert(id.clone()).then(|| {
                let label = c["name"].as_str().map(str::trim).filter(|n| !n.is_empty()).unwrap_or(&id).chars().take(256).collect();
                let description = c["description"].as_str().map(str::trim).filter(|d| !d.is_empty()).map(|d| d.chars().take(1024).collect());
                ProviderOptionChoice { id, label, description, is_default: None }
            })
        })
        .take(64)
        .collect();
    if choices.is_empty() {
        return None;
    }
    let current = option["currentValue"].as_str().map(|v| if v.is_empty() { "_t3/empty".to_owned() } else { v.to_owned() });
    let current_value = current.filter(|c| with_current && choices.iter().any(|choice| &choice.id == c));
    let label = option["name"].as_str().map(str::trim).filter(|n| !n.is_empty()).unwrap_or(id).chars().take(256).collect();
    let description = option["description"].as_str().map(str::trim).filter(|d| !d.is_empty()).map(|d| d.chars().take(1024).collect());
    Some(ProviderOptionDescriptor::Select(SelectProviderOptionDescriptor {
        id: id.to_owned(),
        label,
        description,
        options: choices,
        current_value,
        prompt_injected_values: None,
        role: (option["category"] == "thought_level").then_some(ProviderOptionRole::Reasoning),
    }))
}

pub mod install;

/// Whether text generation can run on instance `id` (filled in with its module).
pub fn text_generation_available(id: &str) -> bool {
    crate::text_gen::antigravity_available(id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_check_what_each_sign_in_method_needs() {
        let base = Config::default();
        assert_eq!(base.issue(), None);
        let business = Config { auth_method: AuthMethod::OauthBusiness, gcp_project: "p".into(), ..Config::default() };
        assert!(business.issue().unwrap().contains("GCP project and location"));
        assert_eq!(Config { gcp_location: "us".into(), ..business.clone() }.issue(), None);
        let key = Config { auth_method: AuthMethod::GeminiApiKey, ..Config::default() };
        assert!(key.issue().unwrap().contains("API key"));
        let platform = Config { auth_method: AuthMethod::AgentPlatform, gcp_project: "p".into(), gcp_location: "l".into(), ..Config::default() };
        assert_eq!(platform.issue(), None);
        assert_eq!(Config { api_key: "k".into(), gcp_project: String::new(), ..platform }.issue(), None);
    }

    #[test]
    fn the_profile_names_only_the_method_and_project() {
        let c = Config { auth_method: AuthMethod::OauthBusiness, gcp_project: "proj".into(), api_key: "secret".into(), ..Config::default() };
        assert_eq!(profile_settings(&c), "{\"auth\":{\"type\":\"oauth-business\"},\"gcp\":{\"project\":\"proj\"}}\n");
        assert_eq!(profile_settings(&Config::default()), "{\"auth\":{\"type\":\"oauth-personal\"}}\n");
    }

    #[test]
    fn profiles_are_per_instance_and_hashed() {
        let p = profile_in(Path::new("/s"), "antigravity");
        let key = crate::util::sha256_hex(b"antigravity");
        assert_eq!(p.home, Path::new("/s/providers/antigravity").join(&key));
        assert_eq!(p.acp, p.home.join("antigravity-acp"));
        assert_eq!(p.temp, Path::new("/s/antigravity-tmp").join(&key[..12]));
        assert_ne!(profile_in(Path::new("/s"), "Antigravity").home, p.home);
        assert_eq!(acp_args(&p)[..2], ["acp".to_owned(), "--gemini-dir".to_owned()]);
        assert_eq!(acp_args(&p)[3..], ["--app-data-dir".to_owned(), "antigravity-acp".to_owned()]);
    }

    #[test]
    fn the_environment_carries_only_the_instances_credential() {
        let p = profile_in(Path::new("/s"), "a");
        let names = |env: &[(String, String)]| env.iter().map(|(k, _)| k.clone()).collect::<Vec<_>>();
        let env = environment(&p, &Config::default(), Path::new("/t/run-1"), "'b'");
        assert!(!names(&env).iter().any(|k| k.contains("API_KEY")));
        assert!(env.contains(&("GEMINI_HOME".into(), p.home.to_string_lossy().into_owned())));
        assert!(env.contains(&("AGY_ACP_FORCE_FILE_STORAGE".into(), "1".into())));
        assert!(env.contains(&("TMPDIR".into(), "/t/run-1".into())));
        let key = Config { auth_method: AuthMethod::GeminiApiKey, api_key: "k1".into(), ..Config::default() };
        assert!(environment(&p, &key, Path::new("/t"), "b").contains(&("GEMINI_API_KEY".into(), "k1".into())));
        let platform = Config { auth_method: AuthMethod::AgentPlatform, api_key: "k2".into(), ..Config::default() };
        assert!(environment(&p, &platform, Path::new("/t"), "b").contains(&("GOOGLE_API_KEY".into(), "k2".into())));
        // Every variable Domovoi sets is also one it removes from what the agent inherits.
        for name in ["GEMINI_HOME", "BROWSER", "AGY_ACP_FORCE_FILE_STORAGE", "GEMINI_API_KEY", "GOOGLE_API_KEY"] {
            assert!(REMOVED_ENV.contains(&name), "{name}");
        }
    }

    #[test]
    fn the_browser_helper_line_is_json_after_the_marker() {
        assert_eq!(browser_url_line("https://a/b?c=\"d\""), "__T3_ANTIGRAVITY_AUTH_URL__\"https://a/b?c=\\\"d\\\"\"\n");
    }

    #[test]
    fn user_skills_are_linked_and_placeholders_replaced() {
        let dir = std::env::temp_dir().join(format!("domovoi-ag-skills-{}", crate::util::new_uuid()));
        let user = dir.join("user");
        std::fs::create_dir_all(user.join(".gemini/config/skills")).unwrap();
        let p = profile_in(&dir.join("state"), "a");
        // The agent's own empty placeholder for the second folder.
        std::fs::create_dir_all(p.home.join("antigravity-cli/skills")).unwrap();
        link_user_skills(&p, &user);
        assert_eq!(std::fs::read_link(p.home.join("config/skills")).unwrap(), user.join(".gemini/config/skills"));
        assert!(p.home.join("antigravity-cli/skills").is_dir(), "no target: the placeholder stays");
        std::fs::create_dir_all(user.join(".gemini/antigravity-cli/skills")).unwrap();
        link_user_skills(&p, &user);
        assert_eq!(std::fs::read_link(p.home.join("antigravity-cli/skills")).unwrap(), user.join(".gemini/antigravity-cli/skills"));
        std::fs::remove_dir_all(user.join(".gemini/config/skills")).unwrap();
        link_user_skills(&p, &user);
        assert!(std::fs::symlink_metadata(p.home.join("config/skills")).is_err(), "a link to nothing goes");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn versions_outside_the_checked_range_are_noted() {
        assert_eq!(version_advice(Some("1.5.14")), None);
        assert!(version_advice(Some("2.1.0")).is_some());
        assert!(version_advice(Some("1.5.0")).is_some());
        assert_eq!(version_tuple("v1.5.14"), Some((1, 5, 14)));
    }
}
