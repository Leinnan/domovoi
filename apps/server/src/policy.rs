//! The administrator's machine-wide policy file (t3code `policy/ManagedPolicy.ts`,
//! `managedSettings.ts`, `managedLimits.ts`, `managedProviders.ts` and
//! `@t3tools/shared/managedPolicyFile`), read once at startup. A file this
//! build cannot fully enforce stops the server (exit code 77, t3code's
//! `SERVER_EXIT_CODE_MANAGED_POLICY`) instead of leaving it looking managed.
//!
//! Where it is read: `/Library/Application Support/Domovoi/policy.json` on
//! macOS, `C:\Program Files\Domovoi\policy.json` on Windows and
//! `/etc/domovoi/policy.json` elsewhere (t3code's paths with Domovoi's name;
//! a missing file is no policy). Debug builds read
//! `DOMOVOI_DEV_MANAGED_POLICY_PATH` instead when it is set, as t3code's dev
//! builds read `T3CODE_DEV_MANAGED_POLICY_PATH`; release builds never do,
//! since any override a user can set is a way around the policy.
//!
//! The file is `ManagedPolicyDocument`, decoded with excess properties
//! rejected at every level: a key this build does not know might be a
//! restriction it cannot enforce.
//!
//! What it does:
//! - `settings`: a `locked: false` entry replaces the built-in default, a
//!   `locked: true` one is laid over the settings on every load, inside each
//!   project's overrides too, and `server.updateSettings` refuses moving it
//!   (`ServerSettingsError`, `write-managed-setting`). Policy values are never
//!   written to the user's settings file: a save puts the user's stored value
//!   back at each locked path, and writes the built-in default where a value
//!   equals the policy's default, so removing the policy brings the user's
//!   own settings back. Entries are vetted as t3code vets them (unknown or
//!   unmanageable keys, `observability`, `compactIdleThreads`,
//!   `storageCleanup.worktree` without `worktreeCleanup`, one field of a
//!   provider instance, a path inside a setting that can be `null`, a value
//!   the settings do not read back, a credential, a default that adds
//!   entries);
//! - `limits.clients`: Domovoi serves `loopback-browser`, so a list without
//!   `local-browser` stops the server; without `remote` the server refuses a
//!   `--host` beyond loopback and `--remote`, remote access over iroh does not
//!   start, and sessions bound to an iroh device are signed out at startup;
//! - `limits.connect` (`false`, or no `remote`): `cloud.installRelayClient` is
//!   refused; Domovoi has no T3 Connect otherwise;
//! - `limits.webhookTasks: false`: webhook tasks show `blockedByPolicy`,
//!   enabling one is refused, and their URLs answer 403 `blocked_by_policy`;
//! - `limits.providers.allowedDrivers`: other drivers' instances show as
//!   unavailable, their turns do not start, and enabling one is refused
//!   (`use-disallowed-provider`);
//! - `limits.telemetry`, `browserCookieImport`, `previewTunnel`, `updates`:
//!   reported to clients; Domovoi sends no analytics, imports no browser
//!   cookies, has no preview tunnel and never updates itself, so there is
//!   nothing else to switch off.
//!
//! Clients get `ManagedPolicySummary` in `ServerConfig.managedPolicy`.
//!
//! Limits (each refused, so the server does not start, rather than ignored):
//! `limits.maxRuntimeMode` (thread, task and agent mode ceilings with every
//! driver's launch configuration taken over) and `limits.providers.instances`
//! (pinned binary paths, launch arguments and variables). Not read: macOS
//! configuration profiles (`com.t3tools.t3code.plist`) and the Windows
//! Policies registry key; no `policy show` command; the file is not watched
//! (t3code reads it once too).

#![expect(clippy::disallowed_types, reason = "a policy's `settings` entries hold any setting's value, laid over the settings document as JSON")]

use domovoi_model::typed::{ManagedPolicyClient, ManagedPolicySummary, ManagedPolicySummaryLimits, ManagedPolicySummaryProviders, ManagedPolicySummaryUpdates, ManagedUpdatesMode};
use serde::{Deserialize, Deserializer};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// The exit code of a server that will not start under its policy (t3code `SERVER_EXIT_CODE_MANAGED_POLICY`).
pub const EXIT_CODE: u8 = 77;
/// The variable a debug build reads the policy path from.
pub const DEV_PATH_ENV: &str = "DOMOVOI_DEV_MANAGED_POLICY_PATH";

/// Why the policy stops the server (t3code `ManagedPolicyFileError`).
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("Domovoi cannot use the managed policy at {path}: {reason}. It will not start until an administrator fixes this.")]
pub struct PolicyFileError {
    pub path: String,
    pub reason: String,
}

/// A request the policy refuses (t3code `ManagedPolicyRefusedError`).
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
#[error("{organization}'s policy does not allow {action}{}.", detail.as_ref().map(|d| format!(", {d}")).unwrap_or_default())]
pub struct Refused {
    pub organization: String,
    /// What the policy does not allow, for example "remote clients".
    pub action: String,
    /// What that means for this request.
    pub detail: Option<String>,
}

// ---- the document (`ManagedPolicyDocument`) ----

/// A value that, when its key is present, must be there: `null` is no value
/// (Effect's `optionalKey` takes no `null`).
fn present<'de, D: Deserializer<'de>, T: Deserialize<'de>>(d: D) -> Result<Option<T>, D::Error> {
    T::deserialize(d).map(Some)
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Document {
    schema_version: u64,
    organization: String,
    #[serde(default, deserialize_with = "present")]
    support_url: Option<String>,
    #[serde(default, deserialize_with = "present")]
    settings: Option<BTreeMap<String, Setting>>,
    #[serde(default, deserialize_with = "present")]
    limits: Option<Limits>,
}

/// `ManagedPolicySetting`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
struct Setting {
    value: Value,
    locked: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Mode {
    ApprovalRequired,
    AutoAcceptEdits,
    Auto,
    FullAccess,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Client {
    Desktop,
    LocalBrowser,
    Remote,
}

/// `ManagedPolicyLimits`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Limits {
    #[serde(default, deserialize_with = "present")]
    max_runtime_mode: Option<Mode>,
    #[serde(default, deserialize_with = "present")]
    clients: Option<Vec<Client>>,
    #[serde(default, deserialize_with = "present")]
    connect: Option<bool>,
    #[serde(default, deserialize_with = "present")]
    webhook_tasks: Option<bool>,
    #[serde(default, deserialize_with = "present")]
    telemetry: Option<bool>,
    #[serde(default, deserialize_with = "present")]
    browser_cookie_import: Option<bool>,
    #[serde(default, deserialize_with = "present")]
    preview_tunnel: Option<bool>,
    #[serde(default, deserialize_with = "present")]
    providers: Option<ProviderLimits>,
    #[serde(default, deserialize_with = "present")]
    updates: Option<Updates>,
}

/// `ManagedProviderLimits`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ProviderLimits {
    #[serde(default, deserialize_with = "present")]
    allowed_drivers: Option<Vec<String>>,
    #[serde(default, deserialize_with = "present")]
    instances: Option<BTreeMap<String, ProviderInstancePin>>,
}

/// `ManagedProviderInstance`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ProviderInstancePin {
    locked: bool,
    #[serde(default, deserialize_with = "present")]
    environment: Option<Vec<PinnedVariable>>,
    #[serde(default, deserialize_with = "present")]
    binary_path: Option<String>,
    #[serde(default, deserialize_with = "present")]
    launch_args: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
struct PinnedVariable {
    name: String,
    value: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
enum UpdatesMode {
    Auto,
    Off,
}

/// `ManagedUpdatesPolicy`.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Updates {
    mode: UpdatesMode,
    #[serde(default, deserialize_with = "present")]
    feed_url: Option<String>,
    #[serde(default, deserialize_with = "present")]
    allow_nightly: Option<bool>,
}

/// What the schema checks beyond the shape: literals, trimmed non-empty
/// strings, non-empty lists, an https feed. `Err` names the offending key.
fn check_schema(d: &mut Document) -> Result<(), String> {
    fn trimmed(value: &mut String, at: &str) -> Result<(), String> {
        let t = value.trim();
        if t.is_empty() {
            return Err(format!("Expected a non-empty string at {at}"));
        }
        *value = t.to_owned();
        Ok(())
    }
    if d.schema_version != 1 {
        return Err("Expected 1 at [\"schemaVersion\"]".into());
    }
    trimmed(&mut d.organization, "[\"organization\"]")?;
    if let Some(url) = &mut d.support_url {
        trimmed(url, "[\"supportUrl\"]")?;
    }
    let Some(limits) = &mut d.limits else { return Ok(()) };
    if limits.clients.as_ref().is_some_and(Vec::is_empty) {
        return Err("Expected a non-empty array at [\"limits\"][\"clients\"]".into());
    }
    if let Some(providers) = &mut limits.providers {
        if let Some(drivers) = &mut providers.allowed_drivers {
            if drivers.is_empty() {
                return Err("Expected a non-empty array at [\"limits\"][\"providers\"][\"allowedDrivers\"]".into());
            }
            for driver in drivers.iter_mut() {
                trimmed(driver, "[\"limits\"][\"providers\"][\"allowedDrivers\"]")?;
            }
        }
        for (driver, pin) in providers.instances.iter_mut().flatten() {
            let at = format!("[\"limits\"][\"providers\"][\"instances\"][\"{driver}\"]");
            for field in [&mut pin.binary_path, &mut pin.launch_args].into_iter().flatten() {
                trimmed(field, &at)?;
            }
            for variable in pin.environment.iter().flatten() {
                let mut chars = variable.name.chars();
                let valid = chars.next().is_some_and(|c| c.is_ascii_alphabetic() || c == '_') && chars.all(|c| c.is_ascii_alphanumeric() || c == '_');
                if !valid {
                    return Err(format!("Expected an environment variable name at {at}[\"environment\"]"));
                }
            }
        }
    }
    if let Some(feed) = limits.updates.as_mut().and_then(|u| u.feed_url.as_mut()) {
        trimmed(feed, "[\"limits\"][\"updates\"][\"feedUrl\"]")?;
        let host = feed.get(..8).filter(|scheme| scheme.eq_ignore_ascii_case("https://")).map(|_| &feed[8..]);
        if !host.and_then(|h| h.chars().next()).is_some_and(|c| !c.is_whitespace() && c != '/') {
            return Err("Expected an https URL at [\"limits\"][\"updates\"][\"feedUrl\"]".into());
        }
    }
    Ok(())
}

/// The document in `text`, or why it is none (`decodeDocument`).
fn decode(text: &str) -> Result<Document, String> {
    let schema = |detail: String| format!("it is not valid JSON or does not match the policy schema ({})", detail.replace('\n', " "));
    let mut document: Document = serde_json::from_str(text).map_err(|e| schema(e.to_string()))?;
    check_schema(&mut document).map_err(schema)?;
    Ok(document)
}

// ---- where the file is ----

/// The system path an administrator deploys the policy to (`managedPolicyPath`).
pub fn system_path() -> PathBuf {
    if cfg!(target_os = "macos") {
        PathBuf::from("/Library/Application Support/Domovoi/policy.json")
    } else if cfg!(windows) {
        // Not ProgramData, where a user can create the folder first and own
        // it. Program Files from the registry, which only administrators
        // change (`%ProgramFiles%` is any user's to set), on whichever drive.
        #[cfg(windows)]
        let program_files = crate::win::registry_string(
            windows_sys::Win32::System::Registry::HKEY_LOCAL_MACHINE,
            r"SOFTWARE\Microsoft\Windows\CurrentVersion",
            "ProgramFilesDir",
        );
        #[cfg(not(windows))]
        let program_files: Option<String> = None;
        PathBuf::from(program_files.unwrap_or_else(|| r"C:\Program Files".into())).join("Domovoi").join("policy.json")
    } else {
        PathBuf::from("/etc/domovoi/policy.json")
    }
}

/// Where this server reads its policy: the system path, or in a debug build
/// the one `DOMOVOI_DEV_MANAGED_POLICY_PATH` names.
pub fn startup_path() -> PathBuf {
    if cfg!(debug_assertions) {
        if let Some(path) = std::env::var_os(DEV_PATH_ENV).filter(|p| !p.is_empty()) {
            return PathBuf::from(path);
        }
    }
    system_path()
}

// ---- the policy ----

/// One `settings` entry: its dot path and the value as written.
#[derive(Debug, Clone, PartialEq)]
struct Entry {
    path: String,
    segments: Vec<String>,
    value: Value,
}

/// The policy this server runs under (t3code's `ManagedPolicy` service).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Policy {
    document: Option<Document>,
    defaults: Vec<Entry>,
    locks: Vec<Entry>,
}

/// Why a settings write is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteRefusal {
    /// It moves `path` off its locked value.
    Locked(String),
    /// It turns on an instance of a driver the policy does not allow.
    Disallowed { instance_id: String, label: String },
}

/// Settings keys the server reads (`ServerSettings.fields`), beyond those the
/// bundled defaults hold.
const OPTIONAL_SETTINGS: [&str; 2] = ["idleCompaction", "usageModelFilter"];
/// Per-project stores; administrators lock the top-level key, and the lock reaches every project.
const UNMANAGEABLE_KEYS: [&str; 4] = ["projectSettingsOverrides", "projectAgentBrowserAccessOverrides", "projectAutoPullOverrides", "projectScriptOverrides"];
/// Resolved at startup from the user's environment, which a policy cannot override.
const ENVIRONMENT_OVERRIDDEN_KEYS: [&str; 1] = ["observability"];
/// Legacy settings derived from another on every write; manage that one instead.
const DERIVED_FROM: [(&str, &str); 1] = [("compactIdleThreads", "idleCompaction")];
/// Settings another one overrides when it is set.
const OVERRIDDEN_BY: [(&str, &str); 1] = [("storageCleanup.worktree", "worktreeCleanup")];
/// Project overrides mirror these top-level keys (`ProjectSettingsOverrides.fields`), so a lock on one applies inside each project.
const PROJECT_OVERRIDE_KEYS: [&str; 29] = [
    "worktreeCleanup",
    "defaultModelSelection",
    "defaultRuntimeMode",
    "defaultThreadEnvMode",
    "newWorktreesStartFromOrigin",
    "worktreeSubmodules",
    "defaultAutoPull",
    "defaultProjectScripts",
    "enableAgentBrowserAccess",
    "enableAgentDeviceAccess",
    "textGenerationModelSelection",
    "threadTitleInstructions",
    "sourceControlWriterModelSelection",
    "reviewGuideModelSelection",
    "reviewGuideInstructions",
    "sourceControlWritingStyle",
    "removeAgentCreditsOnMerge",
    "branchNamingMode",
    "branchNamePrefix",
    "branchNameInstructions",
    "pullRequestMergeMethod",
    "sidebarAutoSettleOnMerge",
    "sidebarAutoSettleAfterDays",
    "sidebarAutoSettleKeepPullRequests",
    "continueThreadsAfterServerUpdate",
    "responseStreamingMode",
    "speechProjectCustomWords",
    "providerInstances",
    "idleCompaction",
];

/// Drivers a policy may name, with the labels users see (t3code `KNOWN_DRIVERS`); any other is refused.
const KNOWN_DRIVERS: [(&str, &str); 11] = [
    ("codex", "Codex"),
    ("claudeAgent", "Claude"),
    ("antigravity", "Antigravity"),
    ("cursor", "Cursor"),
    ("grok", "Grok"),
    ("fx", "fx"),
    ("kiro", "Kiro"),
    ("muse", "Muse Code"),
    ("opencode", "OpenCode"),
    ("pi", "Pi"),
    ("acpRegistry", "ACP Registry"),
];

fn driver_label(driver: &str) -> String {
    KNOWN_DRIVERS.iter().find(|(d, _)| *d == driver).map_or_else(|| driver.to_owned(), |(_, label)| (*label).to_owned())
}

/// "Example Corp's policy does not allow webhook tasks." (`managedPolicyDeniedMessage`)
pub fn denied_message(organization: &str, capability: &str) -> String {
    format!("{organization}'s policy does not allow {capability}.")
}

/// `disallowedProviderMessage`.
pub fn disallowed_provider_message(organization: Option<&str>, label: &str) -> String {
    match organization {
        Some(org) => format!("{org}'s policy does not allow the {label} provider."),
        None => format!("Your organization's policy does not allow the {label} provider."),
    }
}

impl Policy {
    /// Read and validate the policy at `path`; a missing file is no policy (`ManagedPolicy.load`).
    pub fn load(path: &Path) -> Result<Policy, PolicyFileError> {
        let fail = |reason: String| PolicyFileError { path: path.to_string_lossy().into_owned(), reason };
        let text = match std::fs::read(path) {
            Ok(bytes) => String::from_utf8(bytes).map_err(|_| fail("it is not valid JSON or does not match the policy schema (not UTF-8)".into()))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Policy::default()),
            Err(_) => return Err(fail("the file cannot be read".into())),
        };
        Policy::from_text(&text).map_err(fail)
    }

    /// The policy a document's text describes, or why it cannot be enforced (`fromDocument`).
    pub fn from_text(text: &str) -> Result<Policy, String> {
        let document = decode(text)?;
        let limits = document.limits.as_ref();
        if limits.is_some_and(|l| l.max_runtime_mode.is_some()) {
            return Err("\"limits.maxRuntimeMode\" is a restriction this Domovoi build cannot enforce".into());
        }
        if let Some(providers) = limits.and_then(|l| l.providers.as_ref()) {
            for driver in providers.allowed_drivers.iter().flatten() {
                if !KNOWN_DRIVERS.iter().any(|(d, _)| d == driver) {
                    return Err(format!("\"{driver}\" in allowedDrivers is not a known provider"));
                }
            }
            if let Some(driver) = providers.instances.as_ref().and_then(|i| i.keys().next()) {
                if !KNOWN_DRIVERS.iter().any(|(d, _)| d == driver) {
                    return Err(format!("\"{driver}\" in instances is not a known provider"));
                }
                return Err("\"limits.providers.instances\" is a restriction this Domovoi build cannot enforce".into());
            }
        }
        let (defaults, locks) = vet_settings(document.settings.as_ref())?;
        Ok(Policy { document: Some(document), defaults, locks })
    }

    pub fn is_managed(&self) -> bool {
        self.document.is_some()
    }

    pub fn organization(&self) -> Option<&str> {
        self.document.as_ref().map(|d| d.organization.as_str())
    }

    fn limits(&self) -> Option<&Limits> {
        self.document.as_ref().and_then(|d| d.limits.as_ref())
    }

    fn refuse(&self, action: &str, detail: Option<String>) -> Refused {
        Refused { organization: self.organization().unwrap_or("Your organization").to_owned(), action: action.to_owned(), detail }
    }

    /// Whether clients of `client` kind may connect (`allowsAuthPolicy`).
    fn allows_client(&self, client: Client) -> bool {
        self.limits().and_then(|l| l.clients.as_ref()).is_none_or(|clients| clients.contains(&client))
    }

    /// Whether this server may run as it does, serving browsers on this machine (`loopback-browser`).
    pub fn check_local_clients(&self) -> Result<(), Refused> {
        if self.allows_client(Client::LocalBrowser) { Ok(()) } else { Err(self.refuse("browsers on this machine", None)) }
    }

    /// Whether the desktop app on this machine may sign in by itself, with
    /// no pairing code (`crate::local`): the `desktop` client kind.
    pub fn allows_desktop_client(&self) -> bool {
        self.allows_client(Client::Desktop)
    }

    /// Whether anything off this machine may pair (`allowsRemoteClients`).
    pub fn allows_remote_clients(&self) -> bool {
        self.allows_client(Client::Remote)
    }

    /// `limits.previewTunnel !== false` (half of t3code's `allowsPreviewTunnel`; the setting is the other).
    pub fn allows_preview_tunnel(&self) -> bool {
        self.limits().and_then(|l| l.preview_tunnel) != Some(false)
    }

    /// T3 Connect is a way for remote clients in, so a policy without `remote` turns it off too (`allowsConnect`).
    pub fn allows_connect(&self) -> bool {
        self.limits().and_then(|l| l.connect) != Some(false) && self.allows_remote_clients()
    }

    /// The refusal of remote clients (`refuseRemoteClients`).
    pub fn refuse_remote_clients(&self, detail: Option<String>) -> Refused {
        self.refuse("remote clients", detail)
    }

    /// The refusal of T3 Connect (`refuseConnect`).
    pub fn refuse_connect(&self) -> Refused {
        self.refuse("T3 Connect", None)
    }

    /// Fails when the server would listen where the policy's clients leave out
    /// (`checkServerReachability`): beyond loopback, or over iroh, without `remote`.
    pub fn check_reachability(&self, host: &str, remote_flag: bool) -> Result<(), Refused> {
        self.check_local_clients()?;
        if self.allows_remote_clients() {
            return Ok(());
        }
        let loopback = host.eq_ignore_ascii_case("localhost") || host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback());
        if !loopback {
            let what = if host.is_empty() || host == "0.0.0.0" || host == "::" { "every interface".to_owned() } else { host.to_owned() };
            return Err(self.refuse_remote_clients(Some(format!("so the server cannot listen on {what}"))));
        }
        if remote_flag {
            return Err(self.refuse_remote_clients(Some("so remote access over iroh cannot be turned on".into())));
        }
        Ok(())
    }

    /// The organization whose policy switches `webhookTasks` off (`deniedBy`).
    pub fn webhook_block(&self) -> Option<String> {
        let org = self.organization()?;
        (self.limits().and_then(|l| l.webhook_tasks) == Some(false)).then(|| denied_message(org, "webhook tasks"))
    }

    /// Whether the policy lets `driver` run (`isManagedDriverAllowed`).
    pub fn is_driver_allowed(&self, driver: &str) -> bool {
        self.limits().and_then(|l| l.providers.as_ref()).and_then(|p| p.allowed_drivers.as_ref()).is_none_or(|allowed| allowed.iter().any(|d| d == driver))
    }

    /// Why `driver` may not run here, or `None` (`disallowedDriverReason`).
    pub fn disallowed_driver_reason(&self, driver: &str) -> Option<String> {
        (!self.is_driver_allowed(driver)).then(|| disallowed_provider_message(self.organization(), &driver_label(driver)))
    }

    /// What clients are told (`ManagedPolicy.summary`); `None` when unmanaged.
    pub fn summary(&self) -> Option<ManagedPolicySummary> {
        let document = self.document.as_ref()?;
        let limits = document.limits.as_ref().map(|l| {
            let connect = self.allows_connect();
            ManagedPolicySummaryLimits {
                max_runtime_mode: None,
                clients: l.clients.as_ref().map(|clients| {
                    clients
                        .iter()
                        .map(|c| match c {
                            Client::Desktop => ManagedPolicyClient::Desktop,
                            Client::LocalBrowser => ManagedPolicyClient::LocalBrowser,
                            Client::Remote => ManagedPolicyClient::Remote,
                        })
                        .collect()
                }),
                connect: (!connect).then_some(false),
                webhook_tasks: l.webhook_tasks,
                telemetry: l.telemetry,
                browser_cookie_import: l.browser_cookie_import,
                preview_tunnel: l.preview_tunnel,
                providers: l.providers.as_ref().and_then(|p| p.allowed_drivers.as_ref()).map(|drivers| ManagedPolicySummaryProviders {
                    allowed_drivers: Some(drivers.iter().map(|d| d.as_str().into()).collect()),
                    locked_instance_fields: None,
                }),
                updates: l.updates.as_ref().map(|u| ManagedPolicySummaryUpdates {
                    mode: match u.mode {
                        UpdatesMode::Auto => ManagedUpdatesMode::Auto,
                        UpdatesMode::Off => ManagedUpdatesMode::Off,
                    },
                    allow_nightly: u.allow_nightly,
                }),
            }
        });
        Some(ManagedPolicySummary {
            organization: document.organization.clone(),
            support_url: document.support_url.clone(),
            locked_settings: self.locked_paths(),
            limits,
        })
    }

    /// Dot paths into the settings users cannot change.
    pub fn locked_paths(&self) -> Vec<String> {
        self.locks.iter().map(|l| l.path.clone()).collect()
    }

    // ---- settings ----

    /// The settings document as loaded: `builtin` (the built-in defaults) with
    /// the policy's defaults over it, then `saved` (the file) over that, then
    /// the locks. A saved value equal to the built-in default at a path the
    /// policy defaults reads as unset: Domovoi's file holds every setting.
    pub fn loaded(&self, builtin: &Map<String, Value>, saved: Option<&Map<String, Value>>) -> Map<String, Value> {
        let mut fields = builtin.clone();
        for entry in &self.defaults {
            set_path(&mut fields, &entry.segments, Some(entry.value.clone()));
        }
        if let Some(saved) = saved {
            let mut saved = saved.clone();
            for entry in &self.defaults {
                let built_in = get_path(builtin, &entry.segments);
                if built_in.is_some() && get_path(&saved, &entry.segments).is_some_and(|v| Some(v).zip(built_in).is_some_and(|(a, b)| json_equal(a, b))) {
                    set_path(&mut saved, &entry.segments, None);
                }
            }
            merge(&mut fields, &saved);
        }
        self.apply_locks(&mut fields);
        fields
    }

    /// Lays the locked values over a settings document, inside each project's overrides too (`applyLocks`).
    pub fn apply_locks(&self, fields: &mut Map<String, Value>) {
        for lock in &self.locks {
            for target in lock_targets(fields, lock) {
                set_path(fields, &target, Some(lock.value.clone()));
            }
        }
        // The legacy switch follows `idleCompaction` (`withIdleCompaction`).
        if let Some(mode) = self.locks.iter().find(|l| l.path == "idleCompaction").and_then(|l| l.value.as_str()) {
            fields.insert("compactIdleThreads".into(), Value::Bool(mode == "beforeCacheExpires"));
        }
    }

    /// The document a save writes: the user's `stored` values back at every
    /// locked path, and the built-in default where a value is the policy's default.
    pub fn persisted(&self, fields: &mut Map<String, Value>, stored: &Map<String, Value>, builtin: &Map<String, Value>) {
        for lock in &self.locks {
            for target in lock_targets(fields, lock) {
                // A parent the write adds holds the locked value already; removing only the locked key would leave a partial object.
                let added = (1..target.len()).any(|n| get_path(stored, &target[..n]).is_none());
                if !added {
                    set_path(fields, &target, get_path(stored, &target).cloned());
                }
            }
        }
        for entry in &self.defaults {
            if get_path(fields, &entry.segments).is_some_and(|v| json_equal(v, &entry.value)) {
                set_path(fields, &entry.segments, get_path(builtin, &entry.segments).cloned());
            }
        }
    }

    /// Vets a write from `before` to `after` asked by `patch` (`vetWrite` of
    /// settings and providers): refused when it moves a locked path off its
    /// value, set in the patch or changed by another route, or turns on an
    /// instance of a driver the policy leaves out.
    pub fn vet_write(&self, before: &Map<String, Value>, after: &Map<String, Value>, patch: &Map<String, Value>) -> Result<(), WriteRefusal> {
        for lock in &self.locks {
            let sent = lock_targets(patch, lock).into_iter().any(|t| get_path(patch, &t).is_some_and(|v| !matches_lock(v, &lock.value)));
            let changed = lock_targets(after, lock).into_iter().any(|t| {
                get_path(after, &t).is_some_and(|v| get_path(before, &t).is_none_or(|b| !json_equal(v, b)) && !matches_lock(v, &lock.value))
            });
            if sent || changed {
                return Err(WriteRefusal::Locked(lock.path.clone()));
            }
        }
        if self.limits().and_then(|l| l.providers.as_ref()).and_then(|p| p.allowed_drivers.as_ref()).is_none() {
            return Ok(());
        }
        let instances = |fields: &Map<String, Value>| fields.get("providerInstances").and_then(Value::as_object).cloned().unwrap_or_default();
        let (stored, next) = (instances(before), instances(after));
        let enabled = |instance: &Value| instance.get("enabled") != Some(&Value::Bool(false));
        let driver_of = |instance: &Value| instance.get("driver").and_then(Value::as_str).unwrap_or_default().to_owned();
        for (id, instance) in &next {
            let driver = driver_of(instance);
            // A driver's default instance runs before it is configured.
            let was_enabled = match stored.get(id) {
                Some(s) => driver_of(s) == driver && enabled(s),
                None => *id == driver,
            };
            if !self.is_driver_allowed(&driver) && enabled(instance) && !was_enabled {
                return Err(WriteRefusal::Disallowed { instance_id: id.clone(), label: driver_label(&driver) });
            }
        }
        // A project can switch an instance on for itself; that is enabling it too.
        for (project, overrides) in after.get("projectSettingsOverrides").and_then(Value::as_object).into_iter().flatten() {
            for (id, value) in overrides.get("providerInstances").and_then(Value::as_object).into_iter().flatten() {
                let stored_on = before
                    .get("projectSettingsOverrides")
                    .and_then(|p| p.get(project))
                    .and_then(|o| o.get("providerInstances"))
                    .and_then(|i| i.get(id))
                    .and_then(|i| i.get("enabled"))
                    == Some(&Value::Bool(true));
                let driver = next.get(id).map_or_else(|| id.clone(), driver_of);
                if value.get("enabled") == Some(&Value::Bool(true)) && !stored_on && !self.is_driver_allowed(&driver) {
                    return Err(WriteRefusal::Disallowed { instance_id: id.clone(), label: driver_label(&driver) });
                }
            }
        }
        Ok(())
    }
}

/// The settings keys the server reads.
fn known_setting(head: &str, builtin: &Map<String, Value>) -> bool {
    builtin.contains_key(head) || OPTIONAL_SETTINGS.contains(&head)
}

/// What the settings read back of `fields`: the record written out again.
fn read_back(fields: Map<String, Value>) -> Value {
    serde_json::to_value(&*crate::settings::Settings::from_document(fields)).unwrap_or(Value::Null)
}

/// Vets a policy's `settings` (`makeManagedSettings`): its defaults and locks,
/// or the first entry this build cannot apply.
fn vet_settings(settings: Option<&BTreeMap<String, Setting>>) -> Result<(Vec<Entry>, Vec<Entry>), String> {
    let builtin = crate::settings::default_document();
    let (mut defaults, mut locks) = (Vec::new(), Vec::new());
    for (path, setting) in settings.into_iter().flatten() {
        let segments: Vec<String> = path.split('.').map(str::to_owned).collect();
        if segments.iter().any(String::is_empty) {
            return Err(format!("\"{path}\" is not a settings path"));
        }
        let head = segments[0].as_str();
        if !known_setting(head, &builtin) || UNMANAGEABLE_KEYS.contains(&head) {
            return Err(format!("\"{path}\" is not a setting this version can manage"));
        }
        if ENVIRONMENT_OVERRIDDEN_KEYS.contains(&head) {
            return Err(format!("\"{path}\" is resolved at startup from each user's environment and settings file, which a policy cannot override"));
        }
        if let Some((_, source)) = DERIVED_FROM.iter().find(|(legacy, _)| *legacy == head) {
            return Err(format!("\"{head}\" follows \"{source}\"; manage \"{source}\" instead"));
        }
        let entry = Entry { path: path.clone(), segments, value: setting.value.clone() };
        if setting.locked { locks.push(entry) } else { defaults.push(entry) }
    }
    for lock in &locks {
        let unheld = OVERRIDDEN_BY.iter().find(|(overridden, by)| {
            (lock.path.starts_with(overridden) || overridden.starts_with(&format!("{}.", lock.path))) && !locks.iter().any(|other| other.path == *by)
        });
        if let Some((_, by)) = unheld {
            return Err(format!("\"{}\" can be overridden by \"{by}\"; lock \"{by}\" too", lock.path));
        }
        // An instance's fields override each other.
        if lock.segments[0] == "providerInstances" && lock.segments.len() > 2 {
            let id = &lock.segments[1];
            return Err(format!(
                "\"{}\" is one field of a provider instance whose fields override each other; lock \"providerInstances.{id}\" as a whole",
                lock.path
            ));
        }
    }
    let all: Vec<&Entry> = defaults.iter().chain(&locks).collect();
    // A user's stored null would leave a nested lock building a partial object.
    for entry in &all {
        for n in 1..entry.segments.len() {
            let mut nulled = builtin.clone();
            set_path(&mut nulled, &entry.segments[..n], Some(Value::Null));
            if get_path_value(&read_back(nulled), &entry.segments[..n]) == Some(&Value::Null) {
                return Err(format!("\"{}\" is inside a setting that can be empty; manage \"{}\" as a whole", entry.path, entry.segments[0]));
            }
        }
    }
    let mut applied = builtin.clone();
    for entry in &all {
        set_path(&mut applied, &entry.segments, Some(entry.value.clone()));
    }
    let round_trip = read_back(applied);
    if let Some(lost) = all.iter().find(|e| get_path_value(&round_trip, &e.segments).is_none_or(|v| !json_equal(v, &e.value))) {
        return Err(format!("\"{}\" is not a setting this version can manage, or its value is invalid", lost.path));
    }
    if holds_credential(&round_trip) {
        return Err("a managed setting holds a credential, which every user can read in the policy; deploy credentials another way".into());
    }
    // A default that adds an entry to a record would come back each time the user removed it.
    if let Some(adds) = defaults.iter().find(|entry| match get_path(&builtin, &entry.segments) {
        None => entry.segments.len() > 1,
        Some(existing) => adds_keys(existing, &entry.value),
    }) {
        return Err(format!("\"{}\" adds entries users could not remove; lock it instead", adds.path));
    }
    Ok((defaults, locks))
}

/// Whether settings hold a credential the server would move to the secret
/// store (t3code `mapSettingsSecrets`): built-in defaults hold none, so any
/// found is the policy's.
fn holds_credential(settings: &Value) -> bool {
    let secret = |v: Option<&Value>| v.and_then(Value::as_str).is_some_and(|s| !s.is_empty() && s != crate::secrets::REDACTED);
    let entries = |key: &str| settings.get(key).and_then(Value::as_object).into_iter().flat_map(|m| m.values());
    let items = |key: &str| settings.get(key).and_then(Value::as_array).into_iter().flatten();
    let speech = settings.get("cloudSpeechCredentials").and_then(Value::as_object).into_iter().flat_map(|m| m.values()).any(|v| secret(Some(v)));
    speech
        || entries("usageLimitSources").any(|s| secret(s.get("managementKey")))
        || entries("textGenerators").any(|g| secret(g.get("apiKey")))
        || items("easDeviceHosts").any(|h| secret(h.get("accessToken")))
        || items("simlockDeviceHosts").any(|h| secret(h.get("agentToken")))
        || entries("providerInstances").any(|instance| {
            secret(instance.get("config").and_then(|c| c.get("apiKey")))
                || instance.get("environment").and_then(Value::as_array).into_iter().flatten().any(|v| v.get("sensitive") == Some(&Value::Bool(true)) && secret(v.get("value")))
        })
}

// ---- JSON paths ----

fn get_path<'a>(fields: &'a Map<String, Value>, segments: &[String]) -> Option<&'a Value> {
    let (first, rest) = segments.split_first()?;
    let value = fields.get(first)?;
    if rest.is_empty() { Some(value) } else { get_path_value(value, rest) }
}

fn get_path_value<'a>(value: &'a Value, segments: &[String]) -> Option<&'a Value> {
    segments.iter().try_fold(value, |node, segment| node.as_object()?.get(segment))
}

/// Sets (or with `None` removes) the value at `segments`, making the objects on the way.
fn set_path(fields: &mut Map<String, Value>, segments: &[String], value: Option<Value>) {
    let Some((last, parents)) = segments.split_last() else { return };
    let mut node = fields;
    for segment in parents {
        let child = node.entry(segment.clone()).or_insert(Value::Null);
        if !child.is_object() {
            if value.is_none() {
                return;
            }
            *child = Value::Object(Map::new());
        }
        node = child.as_object_mut().expect("made an object above");
    }
    match value {
        Some(value) => {
            node.insert(last.clone(), value);
        }
        None => {
            node.remove(last);
        }
    }
}

/// `deepMerge`: objects merge key by key; anything else replaces.
fn merge(current: &mut Map<String, Value>, patch: &Map<String, Value>) {
    for (key, value) in patch {
        match (current.get_mut(key), value) {
            (Some(Value::Object(existing)), Value::Object(value)) => merge(existing, value),
            _ => {
                current.insert(key.clone(), value.clone());
            }
        }
    }
}

/// JSON equality with numbers compared by value (`7` is `7.0`).
fn json_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x == y || x.as_f64().zip(y.as_f64()).is_some_and(|(x, y)| x == y),
        (Value::Array(x), Value::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(x, y)| json_equal(x, y)),
        (Value::Object(x), Value::Object(y)) => x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| json_equal(v, w))),
        _ => a == b,
    }
}

/// Whether `value` agrees with a lock on every key it holds: patches and
/// project overrides carry only some of a locked object's keys.
fn matches_lock(value: &Value, lock: &Value) -> bool {
    match (value, lock) {
        (Value::Object(value), Value::Object(lock)) => value.iter().all(|(k, child)| lock.get(k).is_some_and(|l| matches_lock(child, l))),
        _ => json_equal(value, lock),
    }
}

/// Whether `value` holds a key `existing` does not, at any depth.
fn adds_keys(existing: &Value, value: &Value) -> bool {
    match (existing, value) {
        (Value::Object(existing), Value::Object(value)) => value.iter().any(|(k, child)| existing.get(k).is_none_or(|e| adds_keys(e, child))),
        _ => false,
    }
}

/// The paths a lock holds in `fields`: its own, and the same key in each
/// project's overrides that sets it.
fn lock_targets(fields: &Map<String, Value>, lock: &Entry) -> Vec<Vec<String>> {
    let mut targets = vec![lock.segments.clone()];
    let head = lock.segments[0].as_str();
    if PROJECT_OVERRIDE_KEYS.contains(&head) {
        for (project, overrides) in fields.get("projectSettingsOverrides").and_then(Value::as_object).into_iter().flatten() {
            if overrides.as_object().is_some_and(|o| o.contains_key(head)) {
                let mut target = vec!["projectSettingsOverrides".to_owned(), project.clone()];
                target.extend(lock.segments.iter().cloned());
                targets.push(target);
            }
        }
    }
    targets
}

// ---- the running server's policy ----

static POLICY: OnceLock<Policy> = OnceLock::new();

/// Reads the policy from [`startup_path`] and makes it this process's.
pub fn install_from_system() -> Result<&'static Policy, PolicyFileError> {
    let path = startup_path();
    let policy = Policy::load(&path)?;
    if let Some(org) = policy.organization() {
        tracing::info!(path = %path.display(), organization = org, locked_settings = ?policy.locked_paths(), "Managed policy applies.");
    }
    Ok(POLICY.get_or_init(|| policy))
}

/// The policy this process runs under: unmanaged until one is installed.
pub fn current() -> &'static Policy {
    static UNMANAGED: Policy = Policy { document: None, defaults: Vec::new(), locks: Vec::new() };
    POLICY.get().unwrap_or(&UNMANAGED)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/policy").join(name)).expect("fixture exists")
    }

    fn obj(v: Value) -> Map<String, Value> {
        v.as_object().expect("an object").clone()
    }

    fn reason(text: &str) -> String {
        Policy::from_text(text).expect_err("refused")
    }

    #[test]
    fn a_missing_file_is_no_policy_and_a_broken_one_stops_the_server() {
        let dir = std::env::temp_dir().join(format!("domovoi-policy-{}", crate::util::new_uuid()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("policy.json");
        let none = Policy::load(&path).unwrap();
        assert!(!none.is_managed() && none.summary().is_none());
        std::fs::write(&path, "{ not json").unwrap();
        let e = Policy::load(&path).unwrap_err();
        assert!(e.reason.starts_with("it is not valid JSON or does not match the policy schema ("), "{e}");
        assert!(e.to_string().ends_with("It will not start until an administrator fixes this."), "{e}");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unknown_keys_fail_closed_at_every_level() {
        for text in [
            r#"{"schemaVersion":1,"organization":"Acme","future":true}"#,
            r#"{"schemaVersion":1,"organization":"Acme","limits":{"newRestriction":false}}"#,
            r#"{"schemaVersion":1,"organization":"Acme","limits":{"updates":{"mode":"off","channel":"x"}}}"#,
            r#"{"schemaVersion":1,"organization":"Acme","settings":{"defaultAutoPull":{"value":true,"locked":true,"why":"x"}}}"#,
            r#"{"schemaVersion":2,"organization":"Acme"}"#,
            r#"{"schemaVersion":1,"organization":"  "}"#,
            r#"{"schemaVersion":1,"organization":"Acme","supportUrl":null}"#,
            r#"{"schemaVersion":1,"organization":"Acme","limits":{"clients":[]}}"#,
            r#"{"schemaVersion":1,"organization":"Acme","limits":{"clients":["phone"]}}"#,
            r#"{"schemaVersion":1,"organization":"Acme","limits":{"updates":{"mode":"auto","feedUrl":"http://mirror"}}}"#,
            r#"{"organization":"Acme"}"#,
        ] {
            assert!(reason(text).starts_with("it is not valid JSON or does not match the policy schema ("), "{text}");
        }
        assert!(reason(r#"{"schemaVersion":1,"organization":"Acme","future":true}"#).contains("future"), "the offending key is named");
    }

    #[test]
    fn restrictions_this_build_cannot_enforce_are_refused() {
        assert_eq!(reason(&fixture("max-runtime-mode.json")), "\"limits.maxRuntimeMode\" is a restriction this Domovoi build cannot enforce");
        assert_eq!(
            reason(r#"{"schemaVersion":1,"organization":"Acme","limits":{"providers":{"instances":{"codex":{"locked":true,"binaryPath":"/opt/codex"}}}}}"#),
            "\"limits.providers.instances\" is a restriction this Domovoi build cannot enforce"
        );
        assert_eq!(reason(r#"{"schemaVersion":1,"organization":"Acme","limits":{"providers":{"allowedDrivers":["gemini"]}}}"#), "\"gemini\" in allowedDrivers is not a known provider");
    }

    #[test]
    fn settings_entries_are_vetted_as_t3code_vets_them() {
        let settings = |entries: Value| reason(&json!({ "schemaVersion": 1, "organization": "Acme", "settings": entries }).to_string());
        assert_eq!(settings(json!({ "noSuchSetting": { "value": 1, "locked": true } })), "\"noSuchSetting\" is not a setting this version can manage");
        assert_eq!(settings(json!({ "a..b": { "value": 1, "locked": true } })), "\"a..b\" is not a settings path");
        assert_eq!(settings(json!({ "projectSettingsOverrides": { "value": {}, "locked": true } })), "\"projectSettingsOverrides\" is not a setting this version can manage");
        assert!(settings(json!({ "observability.otlpTracesUrl": { "value": "https://x", "locked": true } })).contains("which a policy cannot override"));
        assert_eq!(settings(json!({ "compactIdleThreads": { "value": true, "locked": true } })), "\"compactIdleThreads\" follows \"idleCompaction\"; manage \"idleCompaction\" instead");
        assert_eq!(
            settings(json!({ "storageCleanup.worktreeAfterDays": { "value": 3, "locked": true } })),
            "\"storageCleanup.worktreeAfterDays\" can be overridden by \"worktreeCleanup\"; lock \"worktreeCleanup\" too"
        );
        assert!(settings(json!({ "providerInstances.codex.enabled": { "value": false, "locked": true } })).contains("lock \"providerInstances.codex\" as a whole"));
        assert_eq!(settings(json!({ "defaultAutoPull": { "value": "yes", "locked": true } })), "\"defaultAutoPull\" is not a setting this version can manage, or its value is invalid");
        assert_eq!(
            settings(json!({ "storageCleanup.futureRule": { "value": 1, "locked": false } })),
            "\"storageCleanup.futureRule\" is not a setting this version can manage, or its value is invalid"
        );
        assert!(settings(json!({ "textGenerators": { "value": { "x": { "kind": "anthropic", "baseUrl": "https://api", "apiKey": "sk-1" } }, "locked": true } })).contains("holds a credential"));
        assert_eq!(
            settings(json!({ "textGenerators": { "value": { "x": { "kind": "anthropic", "baseUrl": "https://api" } }, "locked": false } })),
            "\"textGenerators\" adds entries users could not remove; lock it instead"
        );
        assert!(settings(json!({ "defaultModelSelection.model": { "value": "m", "locked": true } })).contains("inside a setting that can be empty"));
    }

    #[test]
    fn locks_are_laid_over_reads_and_refused_in_writes() {
        let policy = Policy::from_text(&fixture("acme.json")).unwrap();
        assert_eq!(policy.locked_paths(), ["enableAgentBrowserAccess", "storageCleanup.logsAfterDays"]);
        let builtin = crate::settings::default_document();
        let saved = obj(json!({ "enableAgentBrowserAccess": true, "defaultAutoPull": true, "branchNamePrefix": builtin["branchNamePrefix"].clone(),
            "projectSettingsOverrides": { "p1": { "enableAgentBrowserAccess": true }, "p2": { "defaultAutoPull": false } } }));
        let loaded = policy.loaded(&builtin, Some(&saved));
        assert_eq!(loaded["enableAgentBrowserAccess"], false, "the lock wins over the file");
        assert_eq!(loaded["projectSettingsOverrides"]["p1"]["enableAgentBrowserAccess"], false, "inside each project that sets it");
        assert!(loaded["projectSettingsOverrides"]["p2"].get("enableAgentBrowserAccess").is_none());
        assert_eq!(loaded["storageCleanup"]["logsAfterDays"], 14);
        assert_eq!(loaded["branchNamePrefix"], "acme", "a saved built-in default reads as unset");
        assert_eq!(loaded["defaultAutoPull"], true, "the user's own choice stays");

        // Re-sending the locked value is allowed; moving it is not, however it is sent.
        let patch = |p: Value| obj(p);
        let after = |p: &Map<String, Value>| {
            let mut a = loaded.clone();
            merge(&mut a, p);
            a
        };
        let same = patch(json!({ "enableAgentBrowserAccess": false }));
        assert_eq!(policy.vet_write(&loaded, &after(&same), &same), Ok(()));
        let moved = patch(json!({ "storageCleanup": { "logsAfterDays": 30 } }));
        assert_eq!(policy.vet_write(&loaded, &after(&moved), &moved), Err(WriteRefusal::Locked("storageCleanup.logsAfterDays".into())));
        let project = patch(json!({ "projectSettingsOverrides": { "p2": { "enableAgentBrowserAccess": true } } }));
        assert_eq!(policy.vet_write(&loaded, &after(&project), &project), Err(WriteRefusal::Locked("enableAgentBrowserAccess".into())));
        let other = patch(json!({ "defaultAutoPull": false, "storageCleanup": { "worktreeOnMerge": true } }));
        assert_eq!(policy.vet_write(&loaded, &after(&other), &other), Ok(()));

        // A save writes the user's own values back (what the file holds over the built-in defaults).
        let mut stored = builtin.clone();
        merge(&mut stored, &saved);
        let mut written = after(&other);
        policy.persisted(&mut written, &stored, &builtin);
        assert_eq!(written["enableAgentBrowserAccess"], true);
        assert_eq!(written["projectSettingsOverrides"]["p1"]["enableAgentBrowserAccess"], true);
        assert_eq!(written["storageCleanup"]["logsAfterDays"], Value::Null, "the user never set it");
        assert_eq!(written["storageCleanup"]["worktreeOnMerge"], true, "beside it, the user's write stays");
        assert_eq!(written["branchNamePrefix"], builtin["branchNamePrefix"], "the policy's default is not written as the user's");
        assert_eq!(written["defaultAutoPull"], false);
    }

    #[test]
    fn the_summary_tells_clients_what_is_enforced() {
        let policy = Policy::from_text(&fixture("acme.json")).unwrap();
        assert_eq!(
            json!(policy.summary()),
            json!({
                "organization": "Acme Corp", "supportUrl": "https://help.acme.test", "lockedSettings": ["enableAgentBrowserAccess", "storageCleanup.logsAfterDays"],
                "limits": { "clients": ["local-browser"], "connect": false, "webhookTasks": false, "previewTunnel": false,
                    "providers": { "allowedDrivers": ["codex", "claudeAgent"] }, "updates": { "mode": "off" } }
            })
        );
        assert!(!policy.allows_remote_clients() && !policy.allows_connect());
        assert_eq!(policy.webhook_block().as_deref(), Some("Acme Corp's policy does not allow webhook tasks."));
        assert_eq!(policy.disallowed_driver_reason("opencode").as_deref(), Some("Acme Corp's policy does not allow the OpenCode provider."));
        assert_eq!(policy.disallowed_driver_reason("codex"), None);
        assert_eq!(policy.check_reachability("127.0.0.1", false), Ok(()));
        assert_eq!(
            policy.check_reachability("0.0.0.0", false).unwrap_err().to_string(),
            "Acme Corp's policy does not allow remote clients, so the server cannot listen on every interface."
        );
        assert!(policy.check_reachability("localhost", true).is_err(), "nor serve over iroh");
        let desktop_only = Policy::from_text(r#"{"schemaVersion":1,"organization":"Acme","limits":{"clients":["desktop"]}}"#).unwrap();
        assert_eq!(desktop_only.check_local_clients().unwrap_err().to_string(), "Acme's policy does not allow browsers on this machine.");
        let open = Policy::from_text(r#"{"schemaVersion":1,"organization":"Acme"}"#).unwrap();
        assert_eq!(json!(open.summary()), json!({ "organization": "Acme", "lockedSettings": [] }));
        assert!(open.allows_connect() && open.webhook_block().is_none());
    }

    #[test]
    fn instances_of_disallowed_drivers_cannot_be_turned_on() {
        let policy = Policy::from_text(&fixture("acme.json")).unwrap();
        let before = obj(json!({ "providerInstances": { "opencode-old": { "driver": "opencode" } } }));
        let refused = |after: Value| policy.vet_write(&before, &obj(after), &Map::new());
        assert_eq!(
            refused(json!({ "providerInstances": { "opencode-old": { "driver": "opencode" }, "opencode-2": { "driver": "opencode" } } })),
            Err(WriteRefusal::Disallowed { instance_id: "opencode-2".into(), label: "OpenCode".into() })
        );
        assert_eq!(refused(json!({ "providerInstances": { "opencode-old": { "driver": "opencode", "displayName": "Old" } } })), Ok(()), "one already on stays as it is");
        assert_eq!(refused(json!({ "providerInstances": { "opencode-2": { "driver": "opencode", "enabled": false } } })), Ok(()));
        assert_eq!(refused(json!({ "providerInstances": { "codex-2": { "driver": "codex" } } })), Ok(()));
        assert_eq!(
            refused(json!({ "projectSettingsOverrides": { "p": { "providerInstances": { "pi": { "enabled": true } } } } })),
            Err(WriteRefusal::Disallowed { instance_id: "pi".into(), label: "Pi".into() })
        );
    }

    #[test]
    fn a_locked_idle_compaction_carries_the_legacy_switch() {
        let policy = Policy::from_text(r#"{"schemaVersion":1,"organization":"Acme","settings":{"idleCompaction":{"value":"beforeCacheExpires","locked":true}}}"#).unwrap();
        let loaded = policy.loaded(&crate::settings::default_document(), None);
        assert_eq!((&loaded["idleCompaction"], &loaded["compactIdleThreads"]), (&json!("beforeCacheExpires"), &json!(true)));
    }
}
