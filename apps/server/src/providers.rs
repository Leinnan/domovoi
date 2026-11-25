//! Provider status probes (`ServerProvider` entries) for the Codex, Claude,
//! OpenCode, Pi, ACP and Antigravity drivers (Grok, Kiro and fx in
//! [`crate::acp_cli`]). Probes only read version, login state and what
//! the provider offers; they never open a session that is kept (t3code:
//! "setup must not happen as a health-check side effect").

#![expect(clippy::disallowed_types, clippy::disallowed_macros, reason = "the Claude CLI's `system/init` message, `claude auth status` output and pi's RPC answers, read into records here")]

use crate::util::expand_home;
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use domovoi_model::typed::{
    ModelCapabilities, ProviderOptionChoice, ProviderOptionDescriptor, RuntimeMode, SelectProviderOptionDescriptor, ServerProvider, ServerProviderAuth,
    ServerProviderAuthStatus, ServerProviderContinuation, ServerProviderModel, ServerProviderSetup, ServerProviderSkill, ServerProviderSlashCommand,
    ServerProviderSlashCommandInput, ServerProviderState,
};
use tokio::sync::RwLock;

pub(crate) mod codex;

pub const CODEX: &str = "codex";
pub const OPENCODE: &str = "opencode";
/// OpenCode's oldest version with the v2 API Domovoi speaks.
const OPENCODE_MIN_VERSION: (u64, u64, u64) = (2, 0, 18);
pub const CLAUDE: &str = "claudeAgent";
pub const PI: &str = "pi";
/// The oldest pi with everything Domovoi uses: `agent_settled`, `get_entries`,
/// usage on `message_update`, `clear_queue` and `pi auth check`.
const PI_MIN_VERSION: (u64, u64, u64) = (0, 84, 4);
const PI_DISCOVERY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
const PI_TEXT_GENERATION_DISCOVERY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

#[derive(Clone, Default)]
pub struct ProviderRegistry {
    statuses: Arc<RwLock<Vec<ServerProvider>>>,
}

/// A provider's command line tool.
#[derive(Clone, Copy)]
pub struct Tool {
    pub program: &'static str,
    pub label: &'static str,
    /// Names the executable to use instead of a PATH search.
    pub env: &'static str,
}

/// Claude's slash commands as its last session started (`system/init`):
/// what the CLI runs headless (not its terminal-only ones), skills included.
/// Kept in a file so a restarted server offers them before a new session.
static CLAUDE_COMMANDS: std::sync::RwLock<Vec<ServerProviderSlashCommand>> = std::sync::RwLock::new(Vec::new());
static CLAUDE_COMMANDS_FILE: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
/// Fires when Claude's commands change (main sends clients a new snapshot).
pub static CLAUDE_COMMANDS_CHANGED: tokio::sync::Notify = tokio::sync::Notify::const_new();

/// Loads the commands remembered in `dir` and remembers new ones there.
pub fn init_claude_commands(dir: &Path) {
    let file = dir.join("claude-slash-commands.json");
    if let Some(list) = std::fs::read_to_string(&file).ok().and_then(|t| serde_json::from_str::<Vec<ServerProviderSlashCommand>>(&t).ok()) {
        *CLAUDE_COMMANDS.write().unwrap_or_else(|e| e.into_inner()) = list;
    }
    let _ = CLAUDE_COMMANDS_FILE.set(file);
}

/// The commands a `system/init` message of the Claude CLI lists, as t3code's `slashCommands`.
pub fn claude_commands_from_init(init: &Value) -> Vec<ServerProviderSlashCommand> {
    let terminal: std::collections::HashSet<&str> = init["terminal_slash_commands"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
    let mut seen = std::collections::HashSet::new();
    init["slash_commands"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(|n| n.trim().trim_start_matches('/'))
        .filter(|n| !n.is_empty() && !terminal.contains(n) && !n.starts_with("__") && seen.insert(n.to_owned()))
        .map(|n| ServerProviderSlashCommand { name: n.to_owned(), description: crate::skills::description(n), input: None })
        .collect()
}

/// Remembers the commands of a session's `system/init`.
pub fn remember_claude_commands(init: &Value) {
    let list = claude_commands_from_init(init);
    if list.is_empty() {
        return;
    }
    {
        let mut current = CLAUDE_COMMANDS.write().unwrap_or_else(|e| e.into_inner());
        if *current == list {
            return;
        }
        *current = list.clone();
    }
    if let Some(file) = CLAUDE_COMMANDS_FILE.get() {
        let _ = std::fs::write(file, serde_json::to_string(&list).unwrap_or_default());
    }
    CLAUDE_COMMANDS_CHANGED.notify_one();
}

fn claude_commands() -> Vec<ServerProviderSlashCommand> {
    CLAUDE_COMMANDS.read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// The provider instances settings add (`providerInstances`), by id: kept
/// here, outside the settings lock, for the code that only asks which driver
/// an instance runs.
static INSTANCES: std::sync::RwLock<std::collections::BTreeMap<String, domovoi_model::typed::ProviderInstanceConfig>> = std::sync::RwLock::new(std::collections::BTreeMap::new());

/// ACP agents' slash commands as each instance's agent last listed them.
static ACP_COMMANDS: std::sync::RwLock<std::collections::BTreeMap<String, Vec<ServerProviderSlashCommand>>> = std::sync::RwLock::new(std::collections::BTreeMap::new());

/// Remembers an ACP agent's commands (`available_commands_update`).
pub fn remember_acp_commands(instance: &str, commands: Option<Vec<(String, Option<String>)>>) {
    let Some(commands) = commands else { return };
    let list: Vec<ServerProviderSlashCommand> = commands.into_iter().map(|(name, description)| ServerProviderSlashCommand { name, description, input: None }).collect();
    let changed = ACP_COMMANDS.write().unwrap_or_else(|e| e.into_inner()).insert(instance.to_owned(), list.clone()).as_ref() != Some(&list);
    if changed {
        CLAUDE_COMMANDS_CHANGED.notify_one();
    }
}

/// Remembers the instances of settings as loaded or saved.
pub fn remember_instances(settings: &domovoi_model::typed::ServerSettings) {
    let instances = settings.provider_instances.clone().unwrap_or_default().into_iter().map(|(id, c)| (id.as_str().to_owned(), c)).collect();
    *INSTANCES.write().unwrap_or_else(|e| e.into_inner()) = instances;
}

/// A settings instance's configuration.
pub fn instance(id: &str) -> Option<domovoi_model::typed::ProviderInstanceConfig> {
    INSTANCES.read().unwrap_or_else(|e| e.into_inner()).get(id).cloned()
}

/// Every settings instance of `driver`.
pub fn instances_of(driver: &str) -> Vec<(String, domovoi_model::typed::ProviderInstanceConfig)> {
    INSTANCES.read().unwrap_or_else(|e| e.into_inner()).iter().filter(|(_, c)| c.driver.as_str() == driver).map(|(id, c)| (id.clone(), c.clone())).collect()
}

pub const CLAUDE_TOOL: Tool = Tool { program: "claude", label: "Claude Code", env: "DOMOVOI_CLAUDE_BINARY" };
pub const CODEX_TOOL: Tool = Tool { program: "codex", label: "Codex", env: "DOMOVOI_CODEX_BINARY" };

/// The folders searched for a tool: PATH (already merged with the login
/// shell's at startup), then where CLIs usually live.
pub(crate) fn search_dirs() -> Vec<PathBuf> {
    let path = std::env::var("PATH").unwrap_or_default();
    let known = crate::shell_env::well_known_dirs(&crate::util::home_dir()).join(":");
    std::env::split_paths(&crate::shell_env::merge(&[&path, &known])).collect()
}

pub(crate) fn is_executable(p: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        p.metadata().is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    p.is_file()
}

/// Where a tool is: its env override, else the first match in the search
/// folders. Resolved to the real file, so a version manager's per-shell
/// folder (fnm) going away later does not lose it.
pub fn executable(tool: Tool) -> Result<PathBuf, String> {
    executable_in(tool, std::env::var(tool.env).ok().filter(|v| !v.trim().is_empty()), &search_dirs())
}

pub(crate) fn executable_in(tool: Tool, overridden: Option<String>, dirs: &[PathBuf]) -> Result<PathBuf, String> {
    let resolve = |p: PathBuf| std::fs::canonicalize(&p).unwrap_or(p);
    if let Some(given) = overridden {
        let given = expand_home(given.trim());
        if given.contains('/') {
            let p = PathBuf::from(&given);
            return if is_executable(&p) { Ok(resolve(p)) } else { Err(format!("{} was not found at {given} ({} names it).", tool.label, tool.env)) };
        }
        if let Some(p) = dirs.iter().map(|d| d.join(&given)).find(|p| is_executable(p)) {
            return Ok(resolve(p));
        }
    }
    if let Some(p) = dirs.iter().map(|d| d.join(tool.program)).find(|p| is_executable(p)) {
        return Ok(resolve(p));
    }
    let home = crate::util::home_dir();
    let shown: Vec<String> = dirs
        .iter()
        .map(|d| match d.strip_prefix(&home) {
            Ok(rest) => format!("~/{}", rest.display()),
            Err(_) => d.display().to_string(),
        })
        .collect();
    Err(format!(
        "{label} isn't installed on the server, or the server can't find `{program}`. It looked in {dirs}. Install {label} on the server (then open a new terminal there to check `{program} --version` works), or start the server with {env}=/path/to/{program}.",
        label = tool.label,
        program = tool.program,
        dirs = shown.join(", "),
        env = tool.env,
    ))
}

async fn run(exe: &Path, args: &[&str]) -> Option<String> {
    let mut cmd = crate::process::command(exe);
    cmd.args(args);
    let out = crate::process::output(cmd, crate::process::QUICK).await.ok()?;
    let text = String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    out.status.success().then_some(text)
}

pub(crate) fn first_version(s: &str) -> Option<String> {
    s.split_whitespace()
        .find(|w| w.chars().next().is_some_and(|c| c.is_ascii_digit()) && w.contains('.'))
        .map(|w| w.trim_matches(|c: char| !c.is_ascii_alphanumeric() && c != '.' && c != '-').to_owned())
}

/// A sign-in state, with the kind of account and its name when signed in.
pub(crate) fn auth(status: ServerProviderAuthStatus, kind: Option<&str>, label: Option<&str>) -> ServerProviderAuth {
    ServerProviderAuth {
        status,
        r#type: kind.map(str::to_owned),
        label: label.map(str::to_owned),
        email: None,
        action: None,
        can_logout: None,
        subscription_sharing: None,
        profile_id: None,
        workspace_id: None,
    }
}

fn signed_in(kind: &str, label: &str) -> ServerProviderAuth {
    auth(ServerProviderAuthStatus::Authenticated, Some(kind), Some(label))
}

/// A provider entry before anything is known of its CLI: not installed,
/// sign-in unknown, no models, checked now.
pub(crate) fn entry(instance_id: &str, driver: &str, display_name: &str) -> ServerProvider {
    ServerProvider {
        instance_id: instance_id.into(),
        driver: driver.into(),
        display_name: Some(display_name.to_owned()),
        accent_color: None,
        icon_url: None,
        badge_label: None,
        continuation: None,
        show_interaction_mode_toggle: None,
        reports_context_window: None,
        supported_runtime_modes: None,
        supports_conversation_rollback: None,
        supports_text_generation: None,
        setup: None,
        native_sessions: None,
        configurable_providers: None,
        runtime_paths: None,
        enabled: true,
        installed: false,
        version: None,
        installed_version: None,
        status: ServerProviderState::Error,
        auth: auth(ServerProviderAuthStatus::Unknown, None, None),
        checked_at: domovoi_contracts::iso_now().into(),
        message: None,
        availability: None,
        unavailable_reason: None,
        models: vec![],
        update_required_models: None,
        slash_commands: Some(vec![]),
        skills: Some(vec![]),
        workspace_snapshots: None,
        usage_limits: None,
        version_advisory: None,
        compatibility_advisory: None,
        update_state: None,
        catalog_digest: None,
        loaded_catalog_digest: None,
    }
}

/// The entry of a built-in provider (Codex, Claude) with what no probe decides.
fn base_entry(instance_id: &str) -> ServerProvider {
    let defaults = domovoi_contracts::defaults::providers().remove(instance_id).expect("a built-in provider has bundled defaults");
    let mut v = entry(instance_id, defaults.driver.as_str(), &defaults.display_name);
    v.continuation = Some(ServerProviderContinuation { group_key: expand_home(&defaults.continuation.group_key) });
    v.show_interaction_mode_toggle = Some(defaults.show_interaction_mode_toggle);
    v.reports_context_window = Some(defaults.reports_context_window);
    v.setup = Some(defaults.setup);
    v.models = defaults.models;
    if instance_id == CLAUDE {
        v.slash_commands = Some(claude_commands());
    }
    v
}

fn finish(mut v: ServerProvider, version: Option<String>, auth: ServerProviderAuth, unauth_message: &str, missing_message: &str) -> ServerProvider {
    let installed = version.is_some();
    let authenticated = auth.status == ServerProviderAuthStatus::Authenticated;
    v.installed = installed;
    v.version = version;
    v.auth = auth;
    v.status = if installed && authenticated { ServerProviderState::Ready } else { ServerProviderState::Error };
    if !installed {
        v.message = Some(missing_message.to_owned());
    } else if !authenticated {
        v.message = Some(unauth_message.to_owned());
    }
    v
}

pub async fn probe_codex() -> ServerProvider {
    let unknown = || auth(ServerProviderAuthStatus::Unknown, None, None);
    let signed_out = || auth(ServerProviderAuthStatus::Unauthenticated, None, None);
    let exe = match executable(CODEX_TOOL) {
        Ok(exe) => exe,
        Err(e) => return finish(base_entry(CODEX), None, unknown(), "", &e),
    };
    let version = run(&exe, &["--version"]).await.and_then(|s| first_version(&s));
    let auth = match run(&exe, &["login", "status"]).await {
        Some(s) if s.contains("ChatGPT") => signed_in("chatgpt", "ChatGPT"),
        Some(s) if s.to_lowercase().contains("api key") => signed_in("apiKey", "API key"),
        Some(_) => signed_out(),
        None if version.is_some() => signed_out(),
        None => unknown(),
    };
    if auth.status == ServerProviderAuthStatus::Unauthenticated {
        // Signed out: another account may sign in.
        crate::usage_limits::clear(CODEX);
    }
    let mut provider = finish(base_entry(CODEX), version, auth, "Codex is not authenticated. Run `codex login` and try again.", "Codex CLI is not installed.");
    if provider.status == ServerProviderState::Ready {
        match codex::discover(&exe).await {
            Ok(models) => provider.models = models,
            Err(error) => tracing::warn!(%error, "Could not discover Codex models; using bundled defaults"),
        }
    }
    provider
}

/// What `claude auth status` prints.
#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeAuthStatus {
    #[serde(default, deserialize_with = "flag")]
    logged_in: bool,
    #[serde(default, deserialize_with = "text")]
    subscription_type: Option<String>,
    #[serde(default, deserialize_with = "text")]
    auth_method: Option<String>,
}

/// `true`, or `false` for anything else.
fn flag<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<bool, D::Error> {
    Ok(<Value as serde::Deserialize>::deserialize(deserializer)? == Value::Bool(true))
}

/// A string, or nothing for anything else.
fn text<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    Ok(<Value as serde::Deserialize>::deserialize(deserializer)?.as_str().map(str::to_owned))
}

/// How old an account's limits can be before a probe (or the periodic refresh) reads them again.
pub const LIMITS_FRESH: std::time::Duration = std::time::Duration::from_secs(5 * 60);

/// Reads again the limits of each signed-in account whose limits are older
/// than [`LIMITS_FRESH`], in the background (the periodic refresh while a
/// client watches the provider list).
pub async fn refresh_stale_limits(providers: &[ServerProvider]) {
    for p in providers.iter().filter(|p| p.enabled && p.auth.status == ServerProviderAuthStatus::Authenticated) {
        let id = p.instance_id.as_str();
        if !crate::usage_limits::stale(id, LIMITS_FRESH) {
            continue;
        }
        if p.driver.as_str() == crate::antigravity::DRIVER {
            crate::antigravity::limits::refresh(id, LIMITS_FRESH);
            continue;
        }
        match id {
            CLAUDE => crate::claude_limits::refresh(),
            CODEX => {
                let live = match crate::adapters::HOST.get() {
                    Some(h) => h.live_codex().await,
                    None => None,
                };
                match live {
                    Some(server) => {
                        tokio::spawn(async move { server.read_limits().await });
                    }
                    None => {
                        if let Ok(exe) = executable(CODEX_TOOL) {
                            tokio::spawn(async move { codex::refresh_limits(&exe).await });
                        }
                    }
                }
            }
            OPENCODE => {
                if let Some(h) = crate::adapters::HOST.get() {
                    let host = h.clone();
                    tokio::spawn(async move { host.read_opencode_limits().await });
                }
            }
            _ => {}
        }
    }
}

pub async fn probe_claude() -> ServerProvider {
    let unknown = || auth(ServerProviderAuthStatus::Unknown, None, None);
    let exe = match executable(CLAUDE_TOOL) {
        Ok(exe) => exe,
        Err(e) => return finish(base_entry(CLAUDE), None, unknown(), "", &e),
    };
    let version = run(&exe, &["--version"]).await.and_then(|s| first_version(&s));
    // Any JSON is an answer; only an object can say it is signed in.
    let status: Option<Value> = run(&exe, &["auth", "status"]).await.and_then(|s| serde_json::from_str(&s).ok());
    let auth = match status.map(|answer| <ClaudeAuthStatus as serde::Deserialize>::deserialize(answer).ok()) {
        Some(Some(s)) if s.logged_in => {
            let label = match s.subscription_type.as_deref() {
                Some("max") => "Claude Max Subscription".to_owned(),
                Some("pro") => "Claude Pro Subscription".to_owned(),
                Some(other) => format!("Claude {other} subscription"),
                None => "Claude".to_owned(),
            };
            // The account's limits, read in the background when not fresh.
            if crate::usage_limits::stale(CLAUDE, LIMITS_FRESH) {
                crate::claude_limits::refresh();
            }
            signed_in(s.auth_method.as_deref().unwrap_or("claude.ai"), &label)
        }
        Some(_) => {
            // Signed out: another account may sign in.
            crate::usage_limits::clear(CLAUDE);
            auth(ServerProviderAuthStatus::Unauthenticated, None, None)
        }
        None => unknown(),
    };
    finish(base_entry(CLAUDE), version, auth, "Claude Code is not authenticated. Run `claude auth login` and try again.", "Claude Code CLI is not installed.")
}

/// `1.14.19` (or `v2.1.3`) as numbers.
fn version_tuple(v: &str) -> Option<(u64, u64, u64)> {
    let mut parts = v.trim_start_matches('v').split(['.', '-']).map(|p| p.parse::<u64>().ok());
    Some((parts.next()??, parts.next()??, parts.next().flatten().unwrap_or(0)))
}

/// OpenCode, when installed: its version (2.0.18 or newer: the v2 API) and
/// the models it can run, read from its server (started for this).
pub async fn probe_opencode() -> Option<ServerProvider> {
    let exe = executable(crate::adapters::opencode::OPENCODE_TOOL).ok()?;
    let version = run(&exe, &["--version"]).await.and_then(|s| s.split_whitespace().find_map(|w| version_tuple(w).map(|_| w.trim_start_matches('v').to_owned())));
    let mut v = entry(OPENCODE, OPENCODE, "OpenCode");
    v.show_interaction_mode_toggle = Some(false);
    v.reports_context_window = Some(false);
    v.setup = Some(ServerProviderSetup { can_authenticate: false, can_install: false, documentation_url: None });
    v.installed = true;
    v.version = version.clone();
    v.auth = auth(ServerProviderAuthStatus::Authenticated, None, Some("OpenCode"));
    v.status = ServerProviderState::Ready;
    let failed = |v: &mut ServerProvider, message: String| {
        v.status = ServerProviderState::Error;
        v.message = Some(message);
    };
    if version.as_deref().and_then(version_tuple).is_none_or(|t| t < OPENCODE_MIN_VERSION) {
        failed(&mut v, format!("Domovoi needs OpenCode 2.0.18 or newer (found {}). Install it with `npm install -g @opencode/cli`.", version.as_deref().unwrap_or("an unknown version")));
        return Some(v);
    }
    let host = crate::adapters::HOST.get()?;
    match host.opencode().await {
        Ok(server) => match server.models().await {
            Ok((models, _)) if !models.is_empty() => {
                v.models = models;
                // The workspace's usage limits (t3code reads them with the provider's status).
                if crate::usage_limits::stale(OPENCODE, LIMITS_FRESH) {
                    let host = host.clone();
                    tokio::spawn(async move { host.read_opencode_limits().await });
                }
            }
            Ok(_) => failed(&mut v, "OpenCode has no models. Connect a provider with `opencode auth login`, or set one up in OpenCode.".to_owned()),
            Err(e) => failed(&mut v, format!("Could not list OpenCode's models: {e}")),
        },
        Err(e) => failed(&mut v, e),
    }
    Some(v)
}

// ---------------------------------------------------------------------------
// Pi (after t3code `provider-pi` `status.ts`, `modelCapabilities.ts`, `commands.ts`)
// ---------------------------------------------------------------------------

/// pi's thinking levels, lowest first. `xhigh` and `max` need a model's
/// `thinkingLevelMap` to name them; offering them otherwise makes
/// `set_thinking_level` fail.
pub const PI_THINKING_LEVELS: [(&str, &str); 7] =
    [("off", "Off"), ("minimal", "Minimal"), ("low", "Low"), ("medium", "Medium"), ("high", "High"), ("xhigh", "Extra High"), ("max", "Max")];
/// The service tier that sends none, leaving the endpoint's default.
pub const PI_STANDARD_SERVICE_TIER: &str = "default";
/// The model that leaves pi's own choice (its settings.json) alone.
pub const PI_DEFAULT_MODEL: &str = "default";

/// The thinking levels a pi model takes.
pub fn pi_thinking_levels(model: &Value) -> Vec<&'static str> {
    if model["reasoning"] != true {
        return vec![];
    }
    let map = model["thinkingLevelMap"].as_object();
    PI_THINKING_LEVELS
        .iter()
        .map(|(id, _)| *id)
        .filter(|level| match map.and_then(|m| m.get(*level)) {
            Some(Value::Null) => false,
            Some(_) => true,
            None => !matches!(*level, "xhigh" | "max"),
        })
        .collect()
}

/// `wanted` if the model has it, else the nearest level above, else below.
pub fn clamp_pi_thinking_level<'a>(wanted: &str, levels: &[&'a str]) -> Option<&'a str> {
    let all: Vec<&str> = PI_THINKING_LEVELS.iter().map(|(id, _)| *id).collect();
    let at = all.iter().position(|l| *l == wanted)?;
    let has = |l: &str| levels.iter().copied().find(|x| *x == l);
    has(wanted).or_else(|| all[at + 1..].iter().find_map(|l| has(l))).or_else(|| all[..at].iter().rev().find_map(|l| has(l))).or(levels.first().copied())
}

pub(crate) fn select(id: &str, label: &str, options: Vec<ProviderOptionChoice>, current_value: Option<String>) -> ProviderOptionDescriptor {
    ProviderOptionDescriptor::Select(SelectProviderOptionDescriptor {
        id: id.into(),
        label: label.into(),
        description: None,
        options,
        current_value,
        prompt_injected_values: None,
        role: None,
    })
}

pub(crate) fn choice(id: &str, label: &str, description: Option<&str>, is_default: bool) -> ProviderOptionChoice {
    ProviderOptionChoice { id: id.into(), label: label.into(), description: description.map(str::to_owned), is_default: is_default.then_some(true) }
}

/// What can be set on a pi model: its thinking level (defaulting to
/// `default_level` as it fits the model) and the service tiers an
/// extension advertises.
pub fn pi_model_capabilities(model: &Value, default_level: Option<&str>) -> ModelCapabilities {
    let mut descriptors = vec![];
    let levels = pi_thinking_levels(model);
    if !levels.is_empty() {
        let default = default_level.and_then(|d| clamp_pi_thinking_level(d, &levels));
        let label = |l: &str| PI_THINKING_LEVELS.iter().find(|(id, _)| *id == l).map_or(l, |(_, label)| *label).to_owned();
        descriptors.push(select("thinking", "Thinking", levels.iter().map(|l| choice(l, &label(l), None, Some(*l) == default)).collect(), None));
    }
    let mut seen = std::collections::HashSet::from([PI_STANDARD_SERVICE_TIER.to_owned()]);
    let tiers: Vec<ProviderOptionChoice> = model["serviceTiers"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|t| {
            let id = t["id"].as_str().filter(|id| !id.trim().is_empty())?;
            seen.insert(id.to_owned()).then(|| {
                let name = t["name"].as_str().filter(|n| !n.trim().is_empty()).unwrap_or(id);
                choice(id, name, t["description"].as_str().filter(|d| !d.trim().is_empty()), false)
            })
        })
        .collect();
    if !tiers.is_empty() {
        let mut options = vec![choice(PI_STANDARD_SERVICE_TIER, "Standard", None, true)];
        options.extend(tiers);
        descriptors.push(select("serviceTier", "Service Tier", options, Some(PI_STANDARD_SERVICE_TIER.into())));
    }
    ModelCapabilities { option_descriptors: Some(descriptors) }
}

fn pi_default_model() -> ServerProviderModel {
    ServerProviderModel {
        slug: PI_DEFAULT_MODEL.into(),
        name: "Pi default".into(),
        short_name: None,
        sub_provider: None,
        aliases: None,
        badge: None,
        is_custom: false,
        is_default: None,
        is_legacy: None,
        supports_text_generation: None,
        capabilities: Some(ModelCapabilities { option_descriptors: Some(vec![]) }),
    }
}

/// `get_available_models` as provider models (`provider/id` slugs), the
/// active model's thinking level as its default. `None` if a model has no
/// identity: pi's answer is not what Domovoi reads.
pub fn pi_models(data: &Value, state: &Value) -> Option<Vec<ServerProviderModel>> {
    let active = (state["model"]["provider"].as_str(), state["model"]["id"].as_str());
    let mut seen = std::collections::HashSet::new();
    let mut out = vec![];
    for m in data["models"].as_array()? {
        let (provider, id) = (m["provider"].as_str().filter(|p| !p.trim().is_empty())?, m["id"].as_str().filter(|i| !i.trim().is_empty())?);
        let slug = format!("{provider}/{id}");
        if !seen.insert(slug.clone()) {
            continue;
        }
        let level = (active == (Some(provider), Some(id))).then(|| state["thinkingLevel"].as_str()).flatten();
        out.push(ServerProviderModel {
            name: m["name"].as_str().filter(|n| !n.trim().is_empty()).unwrap_or(&slug).to_owned(),
            sub_provider: Some(provider.to_owned()),
            capabilities: Some(pi_model_capabilities(m, level)),
            slug,
            ..pi_default_model()
        });
    }
    Some(out)
}

/// `get_commands` as slash commands and skills, with `/compact` first (pi's
/// RPC leaves out its terminal built-ins; Domovoi runs it as `compact`).
pub fn pi_commands(data: &Value) -> (Vec<ServerProviderSlashCommand>, Vec<ServerProviderSkill>) {
    let text = |v: &Value, k: &str| v[k].as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned);
    let mut commands = vec![ServerProviderSlashCommand {
        name: "compact".into(),
        description: Some("Summarize the conversation and reduce context usage".into()),
        input: Some(ServerProviderSlashCommandInput { hint: "Optional instructions".into() }),
    }];
    let mut skills = vec![];
    for c in data["commands"].as_array().into_iter().flatten() {
        let Some(name) = text(c, "name") else { continue };
        let description = text(c, "description");
        if c["source"] == "skill" {
            let name = name.strip_prefix("skill:").unwrap_or(&name).to_owned();
            if name.is_empty() {
                continue;
            }
            let (info, iface) = (&c["sourceInfo"], &c["interface"]);
            let scope = text(info, "scope").or_else(|| text(c, "location")).map(|s| match s.to_lowercase().as_str() {
                "global" | "personal" => "user".to_owned(),
                "workspace" | "local" => "project".to_owned(),
                _ => s,
            });
            skills.push(ServerProviderSkill {
                path: text(info, "path").or_else(|| text(c, "path")).unwrap_or_else(|| format!("pi:skill:{name}")),
                display_name: text(c, "displayName").or_else(|| text(info, "displayName")).or_else(|| text(iface, "displayName")),
                short_description: text(c, "shortDescription").or_else(|| text(info, "shortDescription")).or_else(|| text(iface, "shortDescription")),
                name,
                description,
                scope,
                enabled: true,
                user_invocation_only: None,
                user_invocable: None,
            });
        } else if name != "compact" {
            commands.push(ServerProviderSlashCommand { name, description, input: None });
        }
    }
    (commands, skills)
}

/// One short-lived `pi --mode rpc --no-session --offline`: its state, models and commands.
async fn pi_discover(exe: &Path, user_args: &[String], without_extensions: bool, want_commands: bool) -> Result<(Value, Value, Option<Value>), String> {
    use crate::pi_rpc::{Launch, PiRpc};
    let launch = Launch { ephemeral: true, short_lived: true, no_extensions: without_extensions, no_tools: without_extensions, user_args: user_args.to_vec(), ..Launch::default() };
    let cwd = crate::util::home_dir().to_string_lossy().into_owned();
    let (rpc, mut events) = PiRpc::spawn(exe, &launch, &cwd, &crate::pi_rpc::environment("approval-required", None), |_, _| {}).map_err(|e| format!("could not start pi: {e}"))?;
    // Nothing reads the events of a probe: keep the channel drained.
    tokio::spawn(async move { while events.recv().await.is_some() {} });
    let answers = async {
        let state = rpc.request(json!({ "type": "get_state" }), PI_DISCOVERY_TIMEOUT).await?;
        let models = rpc.request(json!({ "type": "get_available_models" }), PI_DISCOVERY_TIMEOUT).await?;
        let commands = if want_commands { rpc.request(json!({ "type": "get_commands" }), PI_DISCOVERY_TIMEOUT).await.ok() } else { None };
        Ok::<_, crate::pi_rpc::RpcError>((state, models, commands))
    }
    .await;
    rpc.terminate().await;
    answers.map_err(String::from)
}

/// Why pi's own default model can't start a turn (its provider lists no models).
async fn pi_unavailable_model_message(exe: &Path, version: (u64, u64, u64), user_args: &[String], provider: &str, id: &str) -> String {
    let label = format!("Pi default ({provider}/{id})");
    let unavailable = format!("{label} uses {provider}, which has no available models in Pi. Choose another model, or check that provider's setup and the Pi launch arguments.");
    // `auth check` can't see a runtime --api-key; it arrived in 0.84.1.
    if version < (0, 84, 1) || user_args.iter().any(|a| a == "--api-key") {
        return unavailable;
    }
    let mut cmd = crate::pi_rpc::command(exe);
    cmd.args(["auth", "check", "--provider", provider, "--json", "--no-refresh"]);
    let out = crate::process::output(cmd, std::time::Duration::from_secs(10)).await.ok();
    let reason = out.and_then(|o| serde_json::from_slice::<Value>(&o.stdout).ok()).and_then(|v| v["reason"].as_str().map(str::to_owned));
    if reason.as_deref() == Some("credentials_not_configured") {
        format!("{label} has no credentials configured for {provider}. Choose another model, or run `pi` in a terminal and use /login or configure its API key.")
    } else {
        unavailable
    }
}

/// Pi, when installed: its version, and from a short-lived RPC session the
/// models it can run (those of providers it holds credentials for) and its
/// commands and skills.
pub async fn probe_pi() -> Option<ServerProvider> {
    let exe = executable(crate::pi_rpc::PI_TOOL).ok()?;
    let mut v = entry(PI, PI, "Pi");
    v.show_interaction_mode_toggle = Some(false);
    v.reports_context_window = Some(true);
    v.supported_runtime_modes = Some(vec![RuntimeMode::ApprovalRequired, RuntimeMode::AutoAcceptEdits, RuntimeMode::FullAccess].into());
    v.setup = Some(ServerProviderSetup { can_authenticate: false, can_install: false, documentation_url: None });
    v.installed = true;
    v.models = vec![pi_default_model()];
    let failed = |v: &mut ServerProvider, message: String| {
        v.status = ServerProviderState::Error;
        v.message = Some(message);
    };
    let mut cmd = crate::pi_rpc::command(&exe);
    cmd.arg("--version");
    let out = crate::process::output(cmd, crate::process::QUICK).await.ok().filter(|o| o.status.success());
    let version = out.map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).and_then(|s| s.split_whitespace().find_map(|w| version_tuple(w).map(|_| w.trim_start_matches('v').to_owned())));
    v.version = version.clone();
    let Some(tuple) = version.as_deref().and_then(version_tuple) else {
        failed(&mut v, "Pi is installed, but Domovoi could not read its version (`pi --version`).".into());
        return Some(v);
    };
    if tuple < PI_MIN_VERSION {
        let (a, b, c) = PI_MIN_VERSION;
        failed(&mut v, format!("Domovoi needs Pi {a}.{b}.{c} or newer (found {}). Update it with `pi update` or `npm install -g --ignore-scripts @earendil-works/pi-coding-agent`.", version.as_deref().unwrap_or_default()));
        return Some(v);
    }
    let user_args = match crate::pi_rpc::user_launch_args() {
        Ok(a) => a,
        Err(e) => {
            failed(&mut v, e);
            return Some(v);
        }
    };
    let text_gen = tokio::time::timeout(PI_TEXT_GENERATION_DISCOVERY_TIMEOUT, pi_discover(&exe, &user_args, true, false));
    let (text_gen, discovered) = tokio::join!(text_gen, tokio::time::timeout(PI_DISCOVERY_TIMEOUT, pi_discover(&exe, &user_args, false, true)));
    let (state, models_data, commands) = match discovered {
        Ok(Ok(d)) => d,
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "pi discovery failed");
            v.status = ServerProviderState::Ready;
            v.message = Some("Pi is available, but Domovoi could not refresh its models and commands. The live session will retry startup.".into());
            return Some(v);
        }
        Err(_) => {
            v.status = ServerProviderState::Ready;
            v.message = Some("Pi is available, but model and command discovery needs interactive input. The live session will handle it.".into());
            return Some(v);
        }
    };
    let Some(mut models) = pi_models(&models_data, &state) else {
        failed(&mut v, "Pi answered `get_available_models` with a model list Domovoi can't read.".into());
        return Some(v);
    };
    // Models only an extension registers can't be loaded by text generation (run without extensions).
    if let Ok(Ok((_, tg, _))) = &text_gen {
        let usable: std::collections::HashSet<String> = pi_models(tg, &Value::Null).unwrap_or_default().into_iter().map(|m| m.slug).collect();
        if usable.is_empty() {
            v.supports_text_generation = Some(false);
        }
        for m in models.iter_mut().filter(|m| !usable.contains(&m.slug)) {
            m.supports_text_generation = Some(false);
        }
    }
    let (commands, skills) = pi_commands(&commands.unwrap_or(Value::Null));
    v.slash_commands = Some(commands);
    v.skills = Some(skills);
    let authenticated = !models.is_empty();
    let active = (state["model"]["provider"].as_str(), state["model"]["id"].as_str());
    v.message = if !authenticated {
        Some("Pi has no usable models. Run `pi` in a terminal and use /login, or configure an API key in ~/.pi/agent.".into())
    } else if let (Some(provider), Some(id)) = active
        && !models.iter().any(|m| m.sub_provider.as_deref() == Some(provider))
    {
        Some(pi_unavailable_model_message(&exe, tuple, &user_args, provider, id).await)
    } else {
        None
    };
    // Other models still work when only pi's default can't: keep the picker usable.
    v.status = if authenticated { ServerProviderState::Ready } else { ServerProviderState::Warning };
    v.auth = auth(if authenticated { ServerProviderAuthStatus::Authenticated } else { ServerProviderAuthStatus::Unauthenticated }, Some("pi"), None);
    v.models.extend(models.drain(..));
    Some(v)
}

/// What changes between probes: Claude's commands and skills as of its
/// latest session, Codex's skills, and each account's usage limits.
fn overlay(current: &mut [ServerProvider]) {
    // Claude's commands as of its latest session, not of the last probe.
    for p in current.iter_mut() {
        if let Some(limits) = crate::usage_limits::get(p.instance_id.as_str()) {
            p.usage_limits = Some(limits);
        }
        if let Some(update) = crate::provider_update::state(p.instance_id.as_str()) {
            p.update_state = Some(update);
        }
        // A driver the managed policy leaves out shows why instead of running (`blockedReason`).
        if let Some(reason) = crate::policy::current().disallowed_driver_reason(p.driver.as_str()) {
            p.enabled = false;
            p.installed = false;
            p.version = None;
            p.status = ServerProviderState::Error;
            p.auth = auth(ServerProviderAuthStatus::Unknown, None, None);
            p.models = vec![];
            p.skills = Some(vec![]);
            p.slash_commands = Some(vec![]);
            p.setup = None;
            p.message = Some(reason.clone());
            p.availability = Some(domovoi_model::typed::ServerProviderAvailability::Unavailable);
            p.unavailable_reason = Some(reason);
            continue;
        }
        if p.driver.as_str() == crate::antigravity::DRIVER {
            crate::antigravity::overlay(p);
            continue;
        }
        match p.instance_id.as_str() {
            CLAUDE => {
                p.slash_commands = Some(claude_commands());
                p.skills = Some(crate::skills::claude());
            }
            // Read each time: a skill installed since shows at once.
            CODEX if p.installed => p.skills = Some(crate::skills::codex()),
            id => {
                if let Some(commands) = ACP_COMMANDS.read().unwrap_or_else(|e| e.into_inner()).get(id) {
                    p.slash_commands = Some(commands.clone());
                }
                // A URL sign-in an ACP agent waits for.
                if p.driver.as_str() == "acpRegistry" {
                    p.auth.action = crate::adapters::acp::url_auth::action(id);
                }
            }
        }
    }
}

/// ACP probes, kept 15 minutes (starting an agent can take a while).
static ACP_PROBES: std::sync::Mutex<std::collections::BTreeMap<String, (std::time::Instant, ServerProvider)>> = std::sync::Mutex::new(std::collections::BTreeMap::new());
const ACP_PROBE_FRESH: std::time::Duration = std::time::Duration::from_secs(15 * 60);

fn acp_model(slug: &str, name: &str) -> ServerProviderModel {
    ServerProviderModel { slug: slug.to_owned(), name: name.to_owned(), short_name: None, sub_provider: None, aliases: None, badge: None, is_custom: false,
        is_default: None, is_legacy: None, supports_text_generation: None, capabilities: None }
}

/// An ACP instance (t3code `acpRegistry` probe): its agent started in a
/// scratch folder, `initialize`, then `session/new` for its models; then
/// stopped. `force` probes again within the 15 minutes.
pub async fn probe_acp(id: &str, config: &domovoi_model::typed::ProviderInstanceConfig, force: bool) -> ServerProvider {
    if !force && let Some((at, kept)) = crate::util::locked(&ACP_PROBES).get(id).cloned() && at.elapsed() < ACP_PROBE_FRESH {
        return kept;
    }
    let c = config.config.clone().unwrap_or(Value::Null);
    let name = config.display_name.clone().or_else(|| c["agentId"].as_str().map(str::to_owned)).unwrap_or_else(|| id.to_owned());
    let mut v = entry(id, "acpRegistry", &name);
    v.enabled = config.enabled != Some(false);
    let probed = tokio::time::timeout(std::time::Duration::from_secs(60), probe_acp_agent(id)).await.unwrap_or_else(|_| Err("The ACP agent did not answer within a minute.".into()));
    match probed {
        Ok((agent, models, authenticated)) => {
            v.installed = true;
            v.version = agent["version"].as_str().map(str::to_owned);
            if let Some(title) = agent["title"].as_str().or(agent["name"].as_str()).filter(|_| config.display_name.is_none()) {
                v.display_name = Some(title.to_owned());
            }
            v.models = if models.is_empty() { vec![acp_model("default", "Default")] } else { models };
            if authenticated {
                v.status = ServerProviderState::Ready;
                v.auth = auth(ServerProviderAuthStatus::Authenticated, None, None);
            } else {
                v.status = ServerProviderState::Error;
                v.auth = auth(ServerProviderAuthStatus::Unauthenticated, None, None);
                v.message = Some(format!("{name} needs signing in. Sign in with its own command, then refresh."));
            }
        }
        Err(e) => {
            v.status = ServerProviderState::Error;
            v.message = Some(e);
        }
    }
    crate::util::locked(&ACP_PROBES).insert(id.to_owned(), (std::time::Instant::now(), v.clone()));
    v
}

/// `initialize` and `session/new` of an instance's agent: its info, models, and whether it is signed in.
async fn probe_acp_agent(id: &str) -> Result<(Value, Vec<ServerProviderModel>, bool), String> {
    let spawn = crate::adapters::acp::spawn_for(id)?;
    let scratch = std::env::temp_dir().join(format!("domovoi-acp-probe-{}", crate::util::new_uuid()));
    std::fs::create_dir_all(&scratch).map_err(|e| e.to_string())?;
    let cwd = scratch.to_string_lossy().into_owned();
    let args: Vec<&str> = spawn.args.iter().map(String::as_str).collect();
    let (rpc, _events) = crate::jsonrpc::RpcClient::spawn_with(&spawn.program, &args, Some(&cwd), &spawn.env, true, |_, _| {}).map_err(|e| format!("Could not start the ACP agent: {e}"))?;
    let result = async {
        let init = rpc.request("initialize", crate::adapters::acp::initialize_params()).await.map_err(|e| format!("The ACP agent did not initialize: {e}"))?;
        let session = match rpc.request("session/new", serde_json::json!({ "cwd": cwd, "mcpServers": [] })).await {
            Ok(s) => s,
            Err(crate::jsonrpc::RpcError::Remote(e)) if e["code"] == -32000 => return Ok((init["agentInfo"].clone(), vec![], false)),
            Err(e) => return Err(format!("The ACP agent started no session: {e}")),
        };
        let mut models: Vec<ServerProviderModel> = session["models"]["availableModels"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|m| Some(acp_model(m["modelId"].as_str()?, m["name"].as_str().or(m["modelId"].as_str())?)))
            .collect();
        if models.is_empty() {
            let option = session["configOptions"].as_array().and_then(|o| o.iter().find(|o| o["category"] == "model").cloned());
            models = option
                .and_then(|o| o["options"].as_array().cloned())
                .into_iter()
                .flatten()
                .filter_map(|m| Some(acp_model(m["value"].as_str()?, m["name"].as_str().or(m["value"].as_str())?)))
                .collect();
        }
        Ok((init["agentInfo"].clone(), models, true))
    }
    .await;
    rpc.kill().await;
    let _ = std::fs::remove_dir_all(&scratch);
    result
}

impl ProviderRegistry {
    pub async fn refresh(&self, only: Option<&str>) -> Vec<ServerProvider> {
        let mut current = self.statuses.read().await.clone();
        let wanted = |id: &str| only.is_none_or(|o| o == id);
        let mut fresh = Vec::new();
        if wanted(CODEX) {
            fresh.push(probe_codex().await);
        }
        if wanted(CLAUDE) {
            fresh.push(probe_claude().await);
        }
        if wanted(OPENCODE) {
            if let Some(o) = probe_opencode().await {
                fresh.push(o);
            }
        }
        if wanted(PI) {
            if let Some(p) = probe_pi().await {
                fresh.push(p);
            }
        }
        // ACP instances settings add; one removed from settings leaves the list.
        let acp = instances_of("acpRegistry");
        current.retain(|p| p.driver.as_str() != "acpRegistry" || acp.iter().any(|(id, _)| id == p.instance_id.as_str()));
        for (id, config) in &acp {
            if wanted(id) {
                fresh.push(probe_acp(id, config, only == Some(id.as_str())).await);
            }
        }
        // Antigravity: the built-in instance (when installed) and those settings add.
        let antigravity = crate::antigravity::instances();
        current.retain(|p| p.driver.as_str() != crate::antigravity::DRIVER || antigravity.contains(&p.instance_id.as_str().to_owned()));
        for id in &antigravity {
            if wanted(id) {
                match crate::antigravity::probe(id).await {
                    Some(p) => fresh.push(p),
                    None => current.retain(|p| p.instance_id.as_str() != id),
                }
            }
        }
        // Grok, Kiro and fx: the built-in instances whose CLI is found, and those settings add.
        let cli = crate::acp_cli::instances();
        current.retain(|p| crate::acp_cli::Kind::from_driver(p.driver.as_str()).is_none() || cli.iter().any(|(id, _)| id == p.instance_id.as_str()));
        for (id, kind) in &cli {
            if wanted(id) {
                fresh.push(crate::acp_cli::probe(id, *kind, only == Some(id.as_str())).await);
            }
        }
        for f in fresh {
            match current.iter_mut().find(|c| c.instance_id == f.instance_id) {
                Some(slot) => *slot = f,
                None => current.push(f),
            }
        }
        current.sort_by_key(|p| match p.instance_id.as_str() {
            CODEX => 0,
            CLAUDE => 1,
            // After OpenCode, which recorded answers already list third.
            PI => 3,
            _ if p.driver.as_str() == crate::antigravity::DRIVER => 4,
            _ if crate::acp_cli::Kind::from_driver(p.driver.as_str()).is_some() => 5,
            _ => 2,
        });
        *self.statuses.write().await = current.clone();
        overlay(&mut current);
        current
    }

    pub async fn snapshot(&self) -> Vec<ServerProvider> {
        let mut current = self.statuses.read().await.clone();
        if current.is_empty() {
            current = self.refresh(None).await;
        }
        overlay(&mut current);
        // macOS stopped this process reading a project folder.
        crate::folder_access::apply(&mut current);
        current
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn claude_commands_leave_out_terminal_ones() {
        let init = json!({ "slash_commands": ["review", "compact", "config", "/clear", "review", "__remote-workflow", "deep-research"], "terminal_slash_commands": ["config", "clear"] });
        let names: Vec<String> = claude_commands_from_init(&init).into_iter().map(|c| c.name).collect();
        assert_eq!(names, ["review", "compact", "deep-research"]);
    }

    /// The entries as clients are sent them: the keys of each state, no more.
    #[test]
    fn entries_are_written_as_t3code_provider_statuses() {
        let missing = finish(base_entry(CODEX), None, auth(ServerProviderAuthStatus::Unknown, None, None), "", "Codex CLI is not installed.");
        let sent = json!(missing);
        let bundled: Value = serde_json::from_str(include_str!("../../../crates/domovoi-contracts/defaults/providers.json")).unwrap();
        let mut expected = bundled["codex"].clone();
        expected["continuation"]["groupKey"] = json!(expand_home("codex:home:/HOME/.codex"));
        for (key, value) in [
            ("instanceId", json!("codex")), ("enabled", json!(true)), ("slashCommands", json!([])), ("skills", json!([])), ("checkedAt", sent["checkedAt"].clone()),
            ("installed", json!(false)), ("version", Value::Null), ("auth", json!({ "status": "unknown" })), ("status", json!("error")), ("message", json!("Codex CLI is not installed.")),
        ] {
            expected[key] = value;
        }
        assert_eq!(sent, expected);

        let ready = finish(base_entry(CLAUDE), Some("2.1.319".into()), signed_in("claude.ai", "Claude Max Subscription"), "unauthenticated", "missing");
        let sent = json!(ready);
        assert_eq!(sent["auth"], json!({ "status": "authenticated", "type": "claude.ai", "label": "Claude Max Subscription" }));
        assert_eq!((&sent["status"], &sent["version"], &sent["installed"]), (&json!("ready"), &json!("2.1.319"), &json!(true)));
        assert!(sent.get("message").is_none());
        assert_eq!(sent["models"], bundled["claudeAgent"]["models"]);

        let signed_out = finish(base_entry(CODEX), Some("0.1.0".into()), auth(ServerProviderAuthStatus::Unauthenticated, None, None), "Run `codex login`.", "missing");
        assert_eq!((signed_out.status, signed_out.message.as_deref()), (ServerProviderState::Error, Some("Run `codex login`.")));
    }

    fn scratch() -> PathBuf {
        let d = std::env::temp_dir().join(format!("domovoi-exe-{}", crate::util::new_uuid()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn fake(dir: &Path, name: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join(name);
        std::fs::write(&p, "#!/bin/sh\necho 1.0.0\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    #[test]
    fn executables_are_found_in_order_and_resolved() {
        let (a, b) = (scratch(), scratch());
        std::fs::write(a.join("claude"), "not executable").unwrap();
        let real = fake(&b, "claude-2.1");
        std::os::unix::fs::symlink(&real, b.join("claude")).unwrap();
        let found = executable_in(CLAUDE_TOOL, None, &[a.clone(), b.clone()]).unwrap();
        assert_eq!(found, std::fs::canonicalize(&real).unwrap(), "skips a file that can't run, follows the link");

        let other = fake(&a, "my-claude");
        assert_eq!(executable_in(CLAUDE_TOOL, Some(other.display().to_string()), &[]).unwrap(), std::fs::canonicalize(&other).unwrap());
        assert_eq!(executable_in(CLAUDE_TOOL, Some("my-claude".into()), &[a.clone()]).unwrap(), std::fs::canonicalize(&other).unwrap());
        assert!(executable_in(CLAUDE_TOOL, Some("/nope/claude".into()), &[b.clone()]).unwrap_err().contains("/nope/claude"));
        let _ = (std::fs::remove_dir_all(a), std::fs::remove_dir_all(b));
    }

    #[test]
    fn a_missing_executable_says_where_it_looked() {
        let home = crate::util::home_dir();
        let err = executable_in(CODEX_TOOL, None, &[home.join(".local/bin"), "/usr/bin".into()]).unwrap_err();
        assert!(err.starts_with("Codex isn't installed on the server"), "{err}");
        assert!(err.contains("~/.local/bin, /usr/bin"), "{err}");
        assert!(err.contains("DOMOVOI_CODEX_BINARY"), "{err}");
    }

    #[test]
    fn pi_thinking_levels_follow_the_model() {
        let ids = |caps: &ModelCapabilities| -> Vec<(String, bool)> {
            caps.option_descriptors.as_ref().unwrap()[0].options().iter().map(|o| (o.id.clone(), o.is_default == Some(true))).collect()
        };
        // As pi 1.3.0 reports DeepSeek Flash: a null mapping removes a level; max is named.
        let flash = json!({ "reasoning": true, "thinkingLevelMap": { "minimal": null, "low": "low", "medium": null, "high": "high", "max": "max" } });
        assert_eq!(pi_thinking_levels(&flash), ["off", "low", "high", "max"]);
        let caps = pi_model_capabilities(&flash, Some("medium"));
        assert_eq!(ids(&caps).into_iter().filter(|(_, d)| *d).map(|(id, _)| id).collect::<Vec<_>>(), ["high"], "medium clamps up to high");
        assert_eq!(pi_thinking_levels(&json!({ "reasoning": true })), ["off", "minimal", "low", "medium", "high"], "xhigh and max need the map");
        assert!(pi_model_capabilities(&json!({ "reasoning": false }), None).option_descriptors.unwrap().is_empty());
        assert_eq!(clamp_pi_thinking_level("max", &["off", "low"]), Some("low"), "nothing above: the nearest below");
        assert_eq!(clamp_pi_thinking_level("bogus", &["off"]), None);
    }

    #[test]
    fn pi_service_tiers_start_with_standard() {
        let caps = pi_model_capabilities(&json!({ "serviceTiers": [{ "id": "priority", "name": "Fast", "description": "2x" }, { "id": "default" }, { "id": "" }] }), None);
        let d = &caps.option_descriptors.unwrap()[0];
        assert_eq!(d.id(), Some("serviceTier"));
        assert_eq!(d.options().iter().map(|o| (o.id.as_str(), o.label.as_str())).collect::<Vec<_>>(), [("default", "Standard"), ("priority", "Fast")]);
    }

    #[test]
    fn pi_models_and_commands_are_read_from_rpc_answers() {
        let state = json!({ "model": { "provider": "zai", "id": "glm-5.3" }, "thinkingLevel": "high" });
        let data = json!({ "models": [
            { "provider": "zai", "id": "glm-5.3", "name": "GLM 5.3", "reasoning": true },
            { "provider": "openrouter", "id": "a/b", "reasoning": false },
            { "provider": "zai", "id": "glm-5.3" },
        ] });
        let models = pi_models(&data, &state).unwrap();
        assert_eq!(models.iter().map(|m| (m.slug.as_str(), m.name.as_str(), m.sub_provider.as_deref())).collect::<Vec<_>>(),
            [("zai/glm-5.3", "GLM 5.3", Some("zai")), ("openrouter/a/b", "openrouter/a/b", Some("openrouter"))]);
        let default = models[0].capabilities.as_ref().unwrap().option_descriptors.as_ref().unwrap()[0].options().iter().find(|o| o.is_default == Some(true)).map(|o| o.id.clone());
        assert_eq!(default.as_deref(), Some("high"), "the active model defaults to pi's level");
        assert!(pi_models(&json!({ "models": [{ "id": "x" }] }), &state).is_none());

        let (commands, skills) = pi_commands(&json!({ "commands": [
            { "name": "websearch", "description": "Search", "source": "extension" },
            { "name": "compact", "source": "prompt" },
            { "name": "skill:review", "source": "skill", "sourceInfo": { "path": "/s/review/SKILL.md", "scope": "global" } },
            { "name": "  ", "source": "prompt" },
        ] }));
        assert_eq!(commands.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["compact", "websearch"]);
        assert_eq!((skills[0].name.as_str(), skills[0].path.as_str(), skills[0].scope.as_deref()), ("review", "/s/review/SKILL.md", Some("user")));
        assert_eq!(pi_commands(&Value::Null).0.len(), 1, "/compact even without an answer");
    }

    /// `cargo test -p domovoi-server providers::tests::the_installed_pi -- --ignored --nocapture`
    #[tokio::test]
    #[ignore = "runs the pi on this machine"]
    async fn the_installed_pi_is_probed() {
        let p = probe_pi().await.expect("pi installed");
        println!("{} {:?} {:?} models={} commands={} skills={} tg={:?}", p.version.clone().unwrap_or_default(), p.status, p.message, p.models.len(),
            p.slash_commands.as_ref().map_or(0, Vec::len), p.skills.as_ref().map_or(0, Vec::len), p.supports_text_generation);
        assert_eq!(p.models[0].slug, PI_DEFAULT_MODEL);
        assert!(serde_json::to_value(&p).is_ok());
    }

    #[test]
    fn extracts_versions() {
        assert_eq!(super::first_version("2.1.319 (Claude Code)").as_deref(), Some("2.1.319"));
        assert_eq!(super::first_version("codex-cli 0.166.0").as_deref(), Some("0.166.0"));
    }
}
