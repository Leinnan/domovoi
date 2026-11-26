//! Agent CLIs t3code drives over ACP as drivers of their own (t3code
//! `provider-grok`, `provider-kiro` and `provider-fx`): xAI's `grok agent
//! stdio`, Kiro's `kiro-cli acp` and Vercel's `fx acp`. Their turns run in
//! [`crate::adapters::acp`]; this module holds what is each CLI's own:
//! - its instances: the built-in one (`grok`, `kiro`, `fx`, t3code's default
//!   instance of the driver) when the CLI is found or settings name it, and
//!   those settings add. Off unless settings turn them on, as in t3code;
//! - its settings: `binaryPath`, `customModels` and the instance's
//!   environment;
//! - its launch: the program, its arguments for the runtime mode, what
//!   `initialize` and `session/new` add ([`Kind::spawn`]);
//! - its status probe: the CLI found (the setting, else `DOMOVOI_<NAME>_BINARY`,
//!   else the search folders), `--version` under a 10 s limit, then the
//!   sign-in and models the way each CLI reports them, never opening a
//!   session (the probe's agent only answers `initialize`).
//!
//! Limits: Domovoi installs and updates none of them; they write no titles,
//! branch names or commits (`supportsTextGeneration: false`); probes are kept
//! 15 minutes unless refreshed for the instance; fx's per-backend
//! sub-providers are not listed (Domovoi's snapshot has no `subProviders`).

#![expect(clippy::disallowed_types, clippy::disallowed_macros, reason = "the CLIs' JSON output and ACP `initialize` answers, read as they print them")]

pub mod fx;
pub mod grok;
pub mod kiro;

use crate::providers::Tool;
use domovoi_model::typed::{
    ModelCapabilities, ProviderInstanceConfig, ServerProvider, ServerProviderAuth, ServerProviderAuthStatus, ServerProviderModel,
    ServerProviderSetup, ServerProviderSlashCommand, ServerProviderState,
};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// `--version`'s limit.
const VERSION_TIMEOUT: Duration = Duration::from_secs(10);
/// A sign-in or model listing's limit.
const AUTH_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a probe is kept.
const PROBE_FRESH: Duration = Duration::from_secs(15 * 60);

pub fn compact_command() -> ServerProviderSlashCommand {
    ServerProviderSlashCommand { name: "compact".into(), description: Some("Summarize the conversation and reduce context usage".into()), input: None }
}

/// One of the CLIs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Grok,
    Kiro,
    Fx,
}

impl Kind {
    pub const ALL: [Kind; 3] = [Kind::Grok, Kind::Kiro, Kind::Fx];

    /// The driver's wire name, also its built-in instance's id.
    pub const fn driver(self) -> &'static str {
        match self {
            Kind::Grok => "grok",
            Kind::Kiro => "kiro",
            Kind::Fx => "fx",
        }
    }

    pub fn from_driver(driver: &str) -> Option<Kind> {
        Kind::ALL.into_iter().find(|k| k.driver() == driver)
    }

    /// The name t3code shows (`displayName`).
    pub const fn label(self) -> &'static str {
        match self {
            Kind::Grok => "Grok",
            Kind::Kiro => "Kiro",
            Kind::Fx => "fx",
        }
    }

    pub const fn tool(self) -> Tool {
        match self {
            Kind::Grok => Tool { program: "grok", label: "Grok CLI", env: "DOMOVOI_GROK_BINARY" },
            Kind::Kiro => Tool { program: "kiro-cli", label: "Kiro CLI", env: "DOMOVOI_KIRO_BINARY" },
            Kind::Fx => Tool { program: "fx", label: "fx", env: "DOMOVOI_FX_BINARY" },
        }
    }

    /// The agent's command line for `runtime_mode` (`None`: the probe's).
    pub fn spawn(self, exe: &Path, config: &Config, runtime_mode: Option<&str>) -> crate::adapters::acp::Spawn {
        let program = exe.to_string_lossy().into_owned();
        let mut env = config.env.clone();
        let args: Vec<&str> = match self {
            Kind::Grok => {
                env.push(("GROK_OAUTH2_REFERRER".into(), "t3code".into()));
                grok::spawn_args(runtime_mode).to_vec()
            }
            Kind::Kiro => kiro::ACP_ARGS.to_vec(),
            Kind::Fx => vec!["acp"],
        };
        crate::adapters::acp::Spawn { program, args: args.into_iter().map(str::to_owned).collect(), env }
    }

    /// `initialize`'s parameters: the ACP client's, with what this CLI wants added.
    pub fn initialize_params(self, client_name: &str) -> Value {
        let mut params = crate::adapters::acp::initialize_params();
        params["clientInfo"]["name"] = json!(client_name);
        match self {
            // The generic client type silently denies Auto refusals.
            Kind::Grok => params["_meta"] = json!({ "clientType": "extension" }),
            // Kiro V3 runs the user's hooks itself only when the client opts in.
            Kind::Kiro => params["clientCapabilities"]["_meta"] = json!({ "kiro": { "hooks": { "enabled": true, "v2": true } } }),
            Kind::Fx => {}
        }
        params
    }

    /// `_meta` of `session/new`, `session/load` and `session/resume`.
    pub fn session_meta(self) -> Option<Value> {
        // fx inherits the user's `~/.fx/mcp.json` servers only when asked.
        (self == Kind::Fx).then(|| json!({ "fx": { "profileMcpServers": true } }))
    }

    /// `_meta` of `session/cancel`.
    pub fn cancel_meta(self) -> Option<Value> {
        // Grok's Ctrl+C blocks stale background wakes until the next user turn.
        (self == Kind::Grok).then(|| json!({ "cancelTrigger": "ctrl_c" }))
    }

    /// The sign-in method sent when the agent asks for one.
    pub fn auth_method(self, config: &Config) -> Option<String> {
        match self {
            Kind::Grok => Some(grok::auth_method(config).to_owned()),
            Kind::Kiro | Kind::Fx => None,
        }
    }
}

/// The driver of instance `id`, when it is one of these CLIs: the driver
/// settings name for it, else the built-in instance's.
pub fn kind_of(id: &str) -> Option<Kind> {
    match crate::providers::instance(id) {
        Some(c) => Kind::from_driver(c.driver.as_str()),
        None => Kind::from_driver(id),
    }
}

/// An instance's settings (t3code `GrokSettings`, `KiroSettings`, `FxSettings`).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Config {
    /// Off unless settings turn it on.
    pub enabled: bool,
    pub display_name: Option<String>,
    pub binary_path: Option<String>,
    /// `customModels`: (slug, name).
    pub custom_models: Vec<(String, String)>,
    /// The instance's environment, over the server's.
    pub env: Vec<(String, String)>,
}

impl Config {
    /// A variable of the instance's environment, else of the server's.
    pub fn var(&self, name: &str) -> Option<String> {
        self.env.iter().rev().find(|(n, _)| n == name).map(|(_, v)| v.clone()).or_else(|| std::env::var(name).ok()).filter(|v| !v.trim().is_empty())
    }
}

pub fn config(id: &str) -> Config {
    config_from(crate::providers::instance(id).as_ref())
}

fn config_from(instance: Option<&ProviderInstanceConfig>) -> Config {
    let c = instance.and_then(|i| i.config.clone()).unwrap_or(Value::Null);
    let enabled = match (instance.and_then(|i| i.enabled), c["enabled"].as_bool()) {
        (Some(false), _) | (_, Some(false)) => false,
        (Some(e), _) | (None, Some(e)) => e,
        (None, None) => false,
    };
    let mut seen = std::collections::HashSet::new();
    let custom_models = c["customModels"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| {
            let slug = m.as_str().or(m["slug"].as_str())?.trim().to_owned();
            let name = m["name"].as_str().map(str::trim).filter(|n| !n.is_empty()).map_or_else(|| slug.clone(), str::to_owned);
            (!slug.is_empty() && seen.insert(slug.clone())).then_some((slug, name))
        })
        .collect();
    let env = instance.and_then(|i| i.environment.clone()).unwrap_or_default().into_iter().filter_map(|v| Some((v.name, v.value?))).collect();
    Config {
        enabled,
        display_name: instance.and_then(|i| i.display_name.clone()).filter(|n| !n.trim().is_empty()),
        binary_path: c["binaryPath"].as_str().map(str::trim).filter(|p| !p.is_empty()).map(str::to_owned),
        custom_models,
        env,
    }
}

/// Every instance of these CLIs: each built-in one whose CLI is found (or
/// that settings name), then those settings add.
pub fn instances() -> Vec<(String, Kind)> {
    let mut out = vec![];
    for kind in Kind::ALL {
        let id = kind.driver();
        let shown = match crate::providers::instance(id) {
            Some(c) => c.driver.as_str() == id,
            None => resolve(kind, &Config::default()).is_ok(),
        };
        if shown {
            out.push((id.to_owned(), kind));
        }
        out.extend(crate::providers::instances_of(id).into_iter().map(|(other, _)| other).filter(|other| other != id).map(|other| (other, kind)));
    }
    out
}

/// The CLI's executable: the instance's `binaryPath`, else the tool's
/// variable, else the first in the search folders.
pub fn resolve(kind: Kind, config: &Config) -> Result<PathBuf, String> {
    let tool = kind.tool();
    let overridden = config.binary_path.clone().or_else(|| std::env::var(tool.env).ok().filter(|v| !v.trim().is_empty()));
    crate::providers::executable_in(tool, overridden, &crate::providers::search_dirs())
}

/// How a CLI run went.
#[derive(Debug)]
pub(crate) enum Ran {
    /// It ran to its end: the exit code (none when a signal ended it) and its output.
    Done { code: Option<i32>, stdout: String, stderr: String },
    /// No such program.
    Missing,
    /// Still running at the limit: it was killed.
    TimedOut,
    /// It could not be started.
    Failed,
}

impl Ran {
    /// Standard output of a run that succeeded.
    pub(crate) fn ok_stdout(&self) -> Option<&str> {
        match self {
            Ran::Done { code: Some(0), stdout, .. } => Some(stdout),
            _ => None,
        }
    }
}

/// `exe args…` with the probe's environment, killed at `limit`.
pub(crate) async fn run(exe: &Path, args: &[&str], env: &[(String, String)], cwd: Option<&Path>, limit: Duration) -> Ran {
    let mut cmd = crate::process::command(exe);
    cmd.args(args).envs(env.iter().map(|(k, v)| (k, v))).stdin(std::process::Stdio::null());
    if let Some(cwd) = cwd {
        cmd.current_dir(cwd);
    }
    match crate::process::output(cmd, limit).await {
        Ok(out) => Ran::Done { code: out.status.code(), stdout: String::from_utf8_lossy(&out.stdout).into_owned(), stderr: String::from_utf8_lossy(&out.stderr).into_owned() },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ran::Missing,
        Err(e) if e.kind() == std::io::ErrorKind::TimedOut => Ran::TimedOut,
        Err(_) => Ran::Failed,
    }
}

/// The environment probes run with:
/// the instance's, offline version managers, and no Grok self-update.
pub(crate) fn probe_env(kind: Kind, config: &Config) -> Vec<(String, String)> {
    let mut env = config.env.clone();
    env.push(("MISE_OFFLINE".into(), "1".into()));
    if kind == Kind::Grok {
        env.push(("GROK_DISABLE_AUTOUPDATER".into(), "1".into()));
    }
    env
}

/// The agent started for its `initialize` answer (and `extra`'s, a request
/// sent next), then stopped: no session is opened, so no MCP server starts
/// and nothing is saved.
pub(crate) async fn acp_initialize(kind: Kind, exe: &Path, config: &Config, extra: Option<(&str, Value)>, limit: Duration) -> Result<(Value, Option<Value>), String> {
    let mut spawn = kind.spawn(exe, config, None);
    spawn.env.extend(probe_env(kind, config));
    let cwd = std::env::current_dir().ok().map(|d| d.to_string_lossy().into_owned());
    let args: Vec<&str> = spawn.args.iter().map(String::as_str).collect();
    let (rpc, _events) = crate::jsonrpc::RpcClient::spawn_with(&spawn.program, &args, cwd.as_deref(), &spawn.env, true, |_, _| {}).map_err(|e| e.to_string())?;
    let asked = tokio::time::timeout(limit, async {
        let init = rpc.request("initialize", kind.initialize_params("domovoi-provider-probe")).await.map_err(|e| e.to_string())?;
        let more = match extra {
            Some((method, params)) => rpc.request(method, params).await.ok(),
            None => None,
        };
        Ok::<_, String>((init, more))
    })
    .await
    .unwrap_or_else(|_| Err("it did not answer in time".into()));
    rpc.kill().await;
    asked
}

/// What a probe starts from: the instance's name and presentation.
fn base_entry(id: &str, kind: Kind, config: &Config) -> ServerProvider {
    let mut v = crate::providers::entry(id, kind.driver(), config.display_name.as_deref().unwrap_or(kind.label()));
    v.enabled = config.enabled;
    v.show_interaction_mode_toggle = Some(false);
    v.supported_runtime_modes = Some(
        match kind {
            Kind::Grok => grok::RUNTIME_MODES.to_vec(),
            Kind::Kiro => kiro::RUNTIME_MODES.to_vec(),
            Kind::Fx => fx::RUNTIME_MODES.to_vec(),
        }
        .into(),
    );
    if kind == Kind::Grok {
        v.supports_conversation_rollback = Some(false);
    }
    v.setup = Some(ServerProviderSetup { can_authenticate: false, can_install: false, documentation_url: None });
    v.supports_text_generation = Some(false);
    v.status = ServerProviderState::Warning;
    v
}

/// A model the CLI offers.
pub(crate) fn model(slug: &str, name: &str, capabilities: Option<ModelCapabilities>) -> ServerProviderModel {
    ServerProviderModel {
        slug: slug.to_owned(),
        name: name.to_owned(),
        short_name: None,
        sub_provider: None,
        aliases: None,
        badge: None,
        is_custom: false,
        is_default: None,
        is_legacy: None,
        supports_text_generation: None,
        capabilities: Some(capabilities.unwrap_or(ModelCapabilities { option_descriptors: Some(vec![]) })),
    }
}

/// `models` with the instance's custom models: a custom name renames a
/// listed model, the rest follow.
pub(crate) fn with_custom_models(mut models: Vec<ServerProviderModel>, config: &Config, capabilities: Option<ModelCapabilities>) -> Vec<ServerProviderModel> {
    for (slug, name) in &config.custom_models {
        match models.iter_mut().find(|m| &m.slug == slug) {
            Some(listed) if name != slug => {
                listed.name = name.clone();
                listed.short_name = None;
            }
            Some(_) => {}
            None => {
                let mut custom = model(slug, name, capabilities.clone());
                custom.is_custom = true;
                models.push(custom);
            }
        }
    }
    models
}

pub(crate) fn auth(status: ServerProviderAuthStatus, kind: Option<&str>, label: Option<&str>) -> ServerProviderAuth {
    crate::providers::auth(status, kind, label)
}

/// The CLI found and run with `--version`: its path and version, or the
/// entry as it stands (missing, timed out, failing).
async fn version_check(kind: Kind, config: &Config, v: &mut ServerProvider) -> Option<PathBuf> {
    let (program, label) = (kind.tool().program, kind.label());
    let missing = match kind {
        Kind::Grok => format!("Grok CLI (`{program}`) is not installed or not on PATH."),
        Kind::Kiro => format!("Kiro CLI (`{program}`) is not installed or not on PATH."),
        Kind::Fx => "fx is not installed or not on PATH.".to_owned(),
    };
    let fail = |v: &mut ServerProvider, installed: bool, message: String| {
        v.installed = installed;
        v.status = ServerProviderState::Error;
        v.message = Some(message);
        None
    };
    let exe = match resolve(kind, config) {
        Ok(exe) => exe,
        Err(_) => return fail(v, false, missing),
    };
    match run(&exe, &["--version"], &probe_env(kind, config), None, VERSION_TIMEOUT).await {
        Ran::Missing => fail(v, false, missing),
        Ran::TimedOut if kind == Kind::Grok => fail(v, true, format!("Grok CLI is installed but timed out while running `{program} --version`.")),
        Ran::TimedOut | Ran::Failed => fail(v, true, if kind == Kind::Grok { "Failed to execute Grok CLI health check.".to_owned() } else { format!("Failed to run `{program} --version`.") }),
        Ran::Done { code, stdout, stderr } => {
            v.installed = true;
            if code != Some(0) {
                let what = if kind == Kind::Fx { label.to_owned() } else { format!("{label} CLI") };
                return fail(v, true, format!("{what} is installed but failed to run."));
            }
            v.version = crate::providers::first_version(&format!("{stdout}\n{stderr}"));
            Some(exe)
        }
    }
}

/// Probes kept: by instance, with the settings they were made for.
static PROBES: std::sync::Mutex<std::collections::BTreeMap<String, (std::time::Instant, Config, ServerProvider)>> = std::sync::Mutex::new(std::collections::BTreeMap::new());

/// Instance `id`'s entry; `force` probes again within the 15 minutes.
pub async fn probe(id: &str, kind: Kind, force: bool) -> ServerProvider {
    let config = config(id);
    if !force
        && let Some((at, was, kept)) = crate::util::locked(&PROBES).get(id).cloned()
        && at.elapsed() < PROBE_FRESH
        && was == config
    {
        return kept;
    }
    let mut v = base_entry(id, kind, &config);
    v.models = with_custom_models(
        match kind {
            Kind::Grok => grok::built_in_models(),
            Kind::Kiro => kiro::built_in_models(),
            Kind::Fx => fx::models(None, &[]),
        },
        &config,
        kind_custom_capabilities(kind),
    );
    if kind == Kind::Kiro {
        v.slash_commands = Some(vec![compact_command()]);
    }
    if !config.enabled {
        // t3code `buildServerProvider`: a disabled provider is `disabled`, its CLI not looked at.
        v.status = ServerProviderState::Disabled;
        v.message = Some(format!("{} is disabled in Domovoi's settings.", kind.label()));
    } else if let Some(exe) = version_check(kind, &config, &mut v).await {
        match kind {
            Kind::Grok => grok::probe(&exe, &config, &mut v).await,
            Kind::Kiro => kiro::probe(&exe, &config, &mut v).await,
            Kind::Fx => fx::probe(&exe, &config, &mut v).await,
        }
    }
    crate::util::locked(&PROBES).insert(id.to_owned(), (std::time::Instant::now(), config, v.clone()));
    v
}

/// What a custom model of `kind` can set.
fn kind_custom_capabilities(kind: Kind) -> Option<ModelCapabilities> {
    (kind == Kind::Fx).then(fx::model_capabilities)
}

/// The latest probe of instance `id`, if one was made.
pub fn last_probe(id: &str) -> Option<ServerProvider> {
    crate::util::locked(&PROBES).get(id).map(|(_, _, v)| v.clone())
}

/// The ACP permission policy t3code applies when no sandbox or approval
/// policy overrides the runtime mode (`acpPermissionDisposition`): reads
/// never ask; Supervised asks; Auto-accept edits takes edits and asks for
/// the rest; anything else is allowed.
pub fn disposition(runtime_mode: &str, tool_kind: &str) -> Disposition {
    match (tool_kind, runtime_mode) {
        ("read" | "search" | "think", _) => Disposition::Allow,
        (_, "approval-required") => Disposition::Ask,
        ("edit" | "delete" | "move", "auto-accept-edits") => Disposition::Allow,
        (_, "auto-accept-edits") => Disposition::Ask,
        _ => Disposition::Allow,
    }
}

/// What a permission request gets without asking.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Disposition {
    Allow,
    Ask,
}

/// A flavor's disposition of a permission request (`kind` is the tool's).
pub fn flavor_disposition(flavor: Option<Kind>, runtime_mode: &str, tool_kind: &str, options: &Value) -> Disposition {
    match flavor {
        // Grok Auto asks only about classifier refusals: they reach the user.
        Some(Kind::Grok) if grok::launch_mode(runtime_mode) == "auto" => Disposition::Ask,
        // Kiro's allow_always saves a workspace rule: without allow_once, the user decides.
        Some(Kind::Kiro) => match disposition(runtime_mode, tool_kind) {
            Disposition::Allow if !offers(options, "allow_once") => Disposition::Ask,
            d => d,
        },
        Some(Kind::Fx) => fx::disposition(runtime_mode, tool_kind),
        _ => disposition(runtime_mode, tool_kind),
    }
}

/// Whether the request offers a non-empty option of `kind`.
pub(crate) fn offers(options: &Value, kind: &str) -> bool {
    options.as_array().is_some_and(|o| o.iter().any(|o| o["kind"] == kind && o["optionId"].as_str().is_some_and(|id| !id.trim().is_empty())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use domovoi_model::typed::{ProviderDriverKind, ProviderInstanceEnvironmentVariable};

    fn instance(enabled: Option<bool>, config: Value) -> ProviderInstanceConfig {
        ProviderInstanceConfig {
            driver: ProviderDriverKind::from("grok"),
            display_name: None,
            accent_color: None,
            environment: Some(vec![serde_json::from_value::<ProviderInstanceEnvironmentVariable>(json!({ "name": "XAI_API_KEY", "value": "k" })).unwrap()]),
            enabled,
            config: Some(config),
        }
    }

    #[test]
    fn instances_are_off_until_settings_turn_them_on() {
        assert!(!config_from(None).enabled);
        assert!(config_from(Some(&instance(Some(true), json!({})))).enabled);
        assert!(config_from(Some(&instance(None, json!({ "enabled": true })))).enabled);
        assert!(!config_from(Some(&instance(Some(true), json!({ "enabled": false })))).enabled, "either off is off");
        let c = config_from(Some(&instance(Some(true), json!({ "binaryPath": " /opt/grok ", "customModels": ["a", { "slug": "b", "name": "Bee" }, "a", ""] }))));
        assert_eq!(c.binary_path.as_deref(), Some("/opt/grok"));
        assert_eq!(c.custom_models, [("a".to_owned(), "a".to_owned()), ("b".to_owned(), "Bee".to_owned())]);
        assert_eq!(c.var("XAI_API_KEY").as_deref(), Some("k"));
    }

    #[tokio::test]
    async fn a_disabled_instance_is_listed_without_running_its_cli() {
        let v = probe("kiro-unconfigured-test", Kind::Kiro, true).await;
        let sent = serde_json::to_value(&v).unwrap();
        assert_eq!((sent["driver"].as_str(), sent["enabled"].as_bool(), sent["installed"].as_bool(), sent["status"].as_str()), (Some("kiro"), Some(false), Some(false), Some("disabled")), "{sent}");
        assert_eq!(sent["message"], "Kiro is disabled in Domovoi's settings.");
        assert_eq!(sent["models"][0]["slug"], "default");
        assert_eq!(sent["supportedRuntimeModes"], json!(["approval-required", "full-access"]));
        assert_eq!((sent["showInteractionModeToggle"].as_bool(), sent["supportsTextGeneration"].as_bool()), (Some(false), Some(false)));
    }

    #[test]
    fn custom_models_rename_or_follow_the_listed_ones() {
        let c = Config { custom_models: vec![("grok-build".into(), "Mine".into()), ("x".into(), "x".into())], ..Config::default() };
        let models = with_custom_models(vec![model("grok-build", "Grok Build", None)], &c, None);
        assert_eq!(models.iter().map(|m| (m.slug.as_str(), m.name.as_str(), m.is_custom)).collect::<Vec<_>>(), [("grok-build", "Mine", false), ("x", "x", true)]);
    }

    #[test]
    fn the_permission_policy_is_as_listed() {
        use Disposition::*;
        for (mode, kind, want) in [
            ("approval-required", "read", Allow),
            ("approval-required", "think", Allow),
            ("approval-required", "fetch", Ask),
            ("approval-required", "edit", Ask),
            ("auto-accept-edits", "edit", Allow),
            ("auto-accept-edits", "execute", Ask),
            ("auto", "execute", Allow),
            ("full-access", "execute", Allow),
        ] {
            assert_eq!(disposition(mode, kind), want, "{mode} {kind}");
        }
        let once = json!([{ "optionId": "o", "kind": "allow_once" }]);
        let always = json!([{ "optionId": "a", "kind": "allow_always" }]);
        assert_eq!(flavor_disposition(Some(Kind::Kiro), "full-access", "execute", &once), Allow);
        assert_eq!(flavor_disposition(Some(Kind::Kiro), "full-access", "execute", &always), Ask, "no one-time choice: the user decides");
        assert_eq!(flavor_disposition(Some(Kind::Grok), "auto", "read", &once), Ask, "Grok Auto asks about refusals only");
        assert_eq!(flavor_disposition(Some(Kind::Grok), "full-access", "execute", &once), Allow);
    }

    #[test]
    fn launches_add_what_each_cli_wants() {
        let c = Config::default();
        let grok = Kind::Grok.spawn(Path::new("/bin/grok"), &c, Some("approval-required"));
        assert_eq!(grok.args, ["--permission-mode", "default", "agent", "stdio"]);
        assert!(grok.env.contains(&("GROK_OAUTH2_REFERRER".into(), "t3code".into())));
        assert_eq!(Kind::Kiro.spawn(Path::new("kiro-cli"), &c, None).args, ["acp", "--agent-engine=v3", "--auth-method=cli"]);
        assert_eq!(Kind::Fx.spawn(Path::new("fx"), &c, None).args, ["acp"]);
        assert_eq!(Kind::Grok.initialize_params("domovoi")["_meta"], json!({ "clientType": "extension" }));
        assert_eq!(Kind::Kiro.initialize_params("domovoi")["clientCapabilities"]["_meta"]["kiro"]["hooks"]["v2"], true);
        assert_eq!(Kind::Fx.session_meta(), Some(json!({ "fx": { "profileMcpServers": true } })));
    }
}
