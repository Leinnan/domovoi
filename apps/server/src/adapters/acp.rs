//! Agents that speak the Agent Client Protocol (t3code `provider-acp` and
//! `effect-acp`): newline-delimited JSON-RPC 2.0 over the agent's stdio,
//! one process per thread.
//!
//! - `initialize` (protocol 1; no file system or terminal of the client's),
//!   then `session/new` in the thread's folder, or `session/load` /
//!   `session/resume` of the thread's ACP session. An agent that needs
//!   signing in is sent `authenticate` with the instance's method (or its
//!   first agent-side one) and asked again.
//! - Domovoi's MCP tools go along: over HTTP when the agent takes HTTP MCP
//!   servers, else through `domovoi-server acp-mcp-bridge` (stdio).
//! - `session/prompt` runs a turn; `session/update` notifications are its
//!   messages, thoughts, tool calls, plan and context use; `session/cancel`
//!   stops it. `session/request_permission` asks as an approval, unless the
//!   runtime mode decides it (full access allows; auto-accept-edits allows
//!   edits; reads are allowed).
//! - The model is the session's model option or `session/set_model`; plan
//!   mode is a mode whose id ends in `plan` or `architect`.
//! - The first prompt of a session carries the instructions about Domovoi's
//!   tools (ACP has no system prompt field).
//!
//! Google's Antigravity (`antigravity` driver) is a flavor of this, after
//! t3code's `AntigravityAdapterV2`: its agent starts in the instance's own
//! profile and signs in at every launch ([`crate::antigravity::launch`]);
//! sessions resume rather than load (no history replay); the model is the
//! account's (an older id of a listed family still taken) and its thinking
//! level a model option; the session mode stays `default`, with approvals
//! answered from the thread's runtime mode; native questions are question
//! forms; the agent may read files of the thread's folder; a stop waits a
//! while for the agent to finish before it is killed.

#![expect(clippy::disallowed_types, clippy::disallowed_macros, reason = "the Agent Client Protocol's JSON-RPC messages, read and written as ACP agents send them")]

use super::turn::{Plan, Turn, TurnEnd, WindowUse, Work};
use super::{Control, TurnInput};
use crate::antigravity::launch::SignIn;
use crate::jsonrpc::{Incoming, RpcClient};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{Mutex, mpsc};
use domovoi_model::typed::{
    BackgroundTaskKind, FileChangeDetail, OptionValue, PendingBackgroundTask, PlanStep, PlanStepStatus, ProviderRequestKind,
    ProviderSessionStatus, ProviderThreadStatus, ThreadId, TurnItemStatus,
};

/// How long a stopped Antigravity turn may take to end before its agent is
/// killed (`DOMOVOI_ANTIGRAVITY_CANCEL_GRACE_MS` shortens it for tests).
fn cancel_grace() -> std::time::Duration {
    let ms = std::env::var("DOMOVOI_ANTIGRAVITY_CANCEL_GRACE_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(15_000);
    std::time::Duration::from_millis(ms)
}
/// How often Antigravity background subagents are polled from disk.
fn subagent_poll_interval() -> std::time::Duration {
    let ms = std::env::var("DOMOVOI_ANTIGRAVITY_POLL_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(1_000);
    std::time::Duration::from_millis(ms.max(20))
}
/// How long to wait for Antigravity to wake itself after its last background subagent finishes.
fn wake_start_deadline() -> std::time::Duration {
    let ms = std::env::var("DOMOVOI_ANTIGRAVITY_WAKE_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(15_000);
    std::time::Duration::from_millis(ms.max(50))
}
/// How long of quiet after Antigravity's wake-up activity ends (when DB settlement wasn't seen yet).
fn wake_settle_deadline() -> std::time::Duration {
    let ms = std::env::var("DOMOVOI_ANTIGRAVITY_SETTLE_MS").ok().and_then(|v| v.parse().ok()).unwrap_or(4_000);
    std::time::Duration::from_millis(ms.max(50))
}
/// Antigravity can leave a prompt open with nothing sent; approvals pause this.
const INACTIVITY_LIMIT: std::time::Duration = std::time::Duration::from_secs(15 * 60);

/// What starts an agent: the program, its arguments and environment.
#[derive(Debug, Clone, PartialEq)]
pub struct Spawn {
    pub program: String,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
}

/// An instance's agent: a local command (`source: "local"`), or the
/// registry agent it names, installed (`crate::acp_registry`).
pub fn spawn_for(instance_id: &str) -> Result<Spawn, String> {
    let config = crate::providers::instance(instance_id).ok_or_else(|| format!("No ACP instance {instance_id}."))?;
    let c = config.config.clone().unwrap_or(Value::Null);
    let mut spawn = if c["source"] == "local" || c["agentId"].as_str().is_none_or(str::is_empty) {
        let program = c["commandPath"].as_str().filter(|p| !p.trim().is_empty()).ok_or("The ACP agent has no command (commandPath).")?.to_owned();
        let args = c["commandArgs"].as_array().map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect()).unwrap_or_default();
        Spawn { program, args, env: vec![] }
    } else {
        crate::acp_registry::installed_spawn(c["agentId"].as_str().unwrap_or_default(), c["distribution"].as_str())?
    };
    for v in config.environment.unwrap_or_default() {
        if let Some(value) = v.value {
            spawn.env.push((v.name, value));
        }
    }
    Ok(spawn)
}

/// The instance's authentication method, when its settings name one.
fn auth_method(instance_id: &str) -> Option<String> {
    crate::providers::instance(instance_id)?.config?["authMethodId"].as_str().filter(|m| !m.is_empty()).map(str::to_owned)
}

const PROTOCOL_VERSION: u64 = 1;

/// The client side of `initialize`.
pub fn initialize_params() -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "clientCapabilities": { "fs": { "readTextFile": false, "writeTextFile": false }, "terminal": false },
        "clientInfo": { "name": "t3-code", "title": "T3 Code (Domovoi)", "version": domovoi_contracts::environment::SERVER_VERSION },
    })
}

/// Whether an error answer is ACP's "authentication required" (-32000).
fn auth_required(e: &crate::jsonrpc::RpcError) -> bool {
    match e {
        crate::jsonrpc::RpcError::Remote(v) => v["code"] == -32000 || v["message"].as_str().is_some_and(|m| m.to_lowercase().contains("auth")),
        _ => false,
    }
}

/// The method `authenticate` is sent: the instance's, else the agent's first
/// that it carries out itself (`type` `agent`, or none).
fn pick_auth(methods: &Value, configured: Option<&str>) -> Option<String> {
    let methods = methods.as_array()?;
    if let Some(id) = configured {
        return methods.iter().find(|m| m["id"] == id).map(|_| id.to_owned());
    }
    methods.iter().find(|m| m["type"].is_null() || m["type"] == "agent").and_then(|m| m["id"].as_str().map(str::to_owned))
}

/// Domovoi's MCP tools as ACP `mcpServers`.
fn mcp_servers(thread_id: &str, caps: &Value) -> Value {
    let Some((url, token)) = crate::mcp::session_for(thread_id) else { return json!([]) };
    if caps["mcpCapabilities"]["http"] == true {
        return json!([{ "type": "http", "name": crate::mcp::SERVER_NAME, "url": url, "headers": [{ "name": "Authorization", "value": format!("Bearer {token}") }] }]);
    }
    let exe = std::env::current_exe().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| "domovoi-server".into());
    json!([{ "name": crate::mcp::SERVER_NAME, "command": exe, "args": ["acp-mcp-bridge"],
        "env": [{ "name": "DOMOVOI_MCP_URL", "value": url }, { "name": "DOMOVOI_MCP_TOKEN", "value": token }] }])
}

/// An agent process and its session, for one thread.
pub struct AcpSession {
    rpc: Arc<RpcClient>,
    events: Mutex<mpsc::UnboundedReceiver<Incoming>>,
    pub session_id: String,
    pub cwd: String,
    pub runtime_mode: String,
    instance: String,
    /// What `session/new` (or load) said: `models`, `modes`, `configOptions`.
    session: std::sync::Mutex<Value>,
    prompt_caps: Value,
    appends_system_prompt: bool,
    broken: AtomicBool,
    /// The instructions about Domovoi's tools went with a prompt already.
    instructed: AtomicBool,
    /// The mode plan mode replaced, to go back to.
    mode_before_plan: std::sync::Mutex<Option<String>>,
    /// Antigravity's part, for an `antigravity` instance.
    antigravity: Option<Antigravity>,
}

/// What an Antigravity session keeps beyond a generic one.
struct Antigravity {
    method: crate::antigravity::AuthMethod,
    /// The settings it was started with: other settings need another process.
    launch_key: String,
    /// Where the agent may read files: the thread's folder, the attachments.
    roots: Vec<std::path::PathBuf>,
    /// The launch's temp folder, removed with the session.
    _temp: crate::antigravity::launch::TempDir,
    /// `GEMINI_HOME`, where the agent keeps its session logs.
    profile_home: std::path::PathBuf,
    /// Keeps Domovoi's install of the agent while it runs.
    _lease: Option<crate::antigravity::install::Lease>,
    auth_url: Arc<crate::antigravity::launch::AuthUrl>,
    /// Brain plan artifacts seen in this session, so later edits stay captured.
    plan_files: std::sync::Mutex<std::collections::HashSet<std::path::PathBuf>>,
}

impl AcpSession {
    pub async fn start(input: &TurnInput, native_session: Option<String>, log_dir: std::path::PathBuf, attachments: &std::path::Path) -> Result<Arc<Self>, String> {
        let instance = input.model_selection.instance_id.as_str().to_owned();
        if crate::antigravity::is_instance(&instance) {
            return Self::start_antigravity(input, native_session, log_dir, attachments).await;
        }
        let mut spawn = spawn_for(&instance)?;
        spawn.env.push(("T3CODE_THREAD_ID".into(), input.thread_id.clone()));
        let log = crate::logging::protocol_log(log_dir.join(format!("acp-{}-{}.ndjson", crate::orchestration::events::enc(&instance), crate::util::new_uuid())));
        let args: Vec<&str> = spawn.args.iter().map(String::as_str).collect();
        let (rpc, events) = RpcClient::spawn_with(&spawn.program, &args, Some(&input.cwd), &spawn.env, true, log).map_err(|e| format!("could not start the ACP agent: {e}"))?;
        let init = rpc.request("initialize", initialize_params()).await.map_err(|e| format!("The ACP agent did not initialize: {e}"))?;
        let caps = init["agentCapabilities"].clone();
        let servers = mcp_servers(&input.thread_id, &caps);
        let configured = auth_method(&instance);
        let open = |method: &'static str, params: Value| {
            let rpc = rpc.clone();
            async move { rpc.request(method, params).await }
        };
        let (session_id, session) = 'open: {
            if let Some(id) = native_session.filter(|id| !id.starts_with("pending:")) {
                let params = json!({ "sessionId": id, "cwd": input.cwd, "mcpServers": servers });
                let method = if caps["loadSession"] == true {
                    Some("session/load")
                } else if !caps["sessionCapabilities"]["resume"].is_null() {
                    Some("session/resume")
                } else {
                    None
                };
                if let Some(method) = method
                    && let Ok(answer) = open(method, params).await
                {
                    break 'open (id, answer);
                }
            }
            let params = json!({ "cwd": input.cwd, "mcpServers": servers });
            let answer = match open("session/new", params.clone()).await {
                Ok(a) => a,
                Err(e) if auth_required(&e) => {
                    let method = pick_auth(&init["authMethods"], configured.as_deref()).ok_or("The ACP agent needs signing in, and offers no way Domovoi can do it here.")?;
                    rpc.request("authenticate", json!({ "methodId": method })).await.map_err(|e| format!("The ACP agent did not sign in: {e}"))?;
                    open("session/new", params).await.map_err(|e| format!("The ACP agent started no session: {e}"))?
                }
                Err(e) => return Err(format!("The ACP agent started no session: {e}")),
            };
            let id = answer["sessionId"].as_str().ok_or("The ACP agent's session has no id.")?.to_owned();
            (id, answer)
        };
        // A loaded session replays its history first: not the next turn's.
        let mut events = events;
        while events.try_recv().is_ok() {}
        Ok(Arc::new(AcpSession {
            rpc,
            events: Mutex::new(events),
            session_id,
            cwd: input.cwd.clone(),
            runtime_mode: input.runtime_mode.clone(),
            instance,
            session: std::sync::Mutex::new(session),
            prompt_caps: caps["promptCapabilities"].clone(),
            appends_system_prompt: false,
            broken: AtomicBool::new(false),
            instructed: AtomicBool::new(false),
            mode_before_plan: std::sync::Mutex::new(None),
            antigravity: None,
        }))
    }

    /// An Antigravity instance's agent: launched in its profile and signed
    /// in, then the thread's session resumed (never loaded: that replays
    /// it), else a new one.
    async fn start_antigravity(input: &TurnInput, native_session: Option<String>, log_dir: std::path::PathBuf, attachments: &std::path::Path) -> Result<Arc<Self>, String> {
        use crate::antigravity as ag;
        let instance = input.model_selection.instance_id.as_str().to_owned();
        let log = crate::logging::protocol_log(log_dir.join(format!("antigravity-{}-{}.ndjson", crate::orchestration::events::enc(&instance), crate::util::new_uuid())));
        let launch = ag::launch::start(&instance, &input.cwd, vec![("T3CODE_THREAD_ID".into(), input.thread_id.clone())], log).await?;
        let failed = |launch: ag::launch::Launch, message: String| async move {
            launch.kill().await;
            Err::<Arc<Self>, String>(message)
        };
        match launch.authenticate().await {
            Ok(answer) => ag::authenticated(&instance, &answer),
            Err(SignIn::Required) => {
                ag::signed_out(&instance, launch.method());
                return failed(launch, ag::SIGN_IN_REQUIRED.to_owned()).await;
            }
            Err(e) => return failed(launch, e.message()).await,
        }
        let caps = launch.init["agentCapabilities"].clone();
        let servers = mcp_servers(&input.thread_id, &caps);
        // Domovoi's tool instructions go in the system prompt where the agent takes an addition.
        let mut instructions = crate::agent_instructions::question_guidance("ask_question", None);
        if crate::mcp::session_for(&input.thread_id).is_some() {
            instructions.push_str("\n\n");
            instructions.push_str(&crate::agent_instructions::system_prompt());
        }
        let meta = ag::acp::session_meta(&launch.init, Some(&instructions));
        let instructed = meta.is_some();
        let params = launch.session_params(&input.cwd, servers, attachments, meta);
        let mut opened = None;
        if let Some(id) = native_session.filter(|id| !id.starts_with("pending:"))
            && !caps["sessionCapabilities"]["resume"].is_null()
        {
            let mut resume = params.clone();
            resume["sessionId"] = json!(id);
            match launch.rpc.request("session/resume", resume).await {
                Ok(answer) => opened = Some((id, answer)),
                Err(e) if ag::launch::sign_in_required(&e) => {
                    ag::signed_out(&instance, launch.method());
                    return failed(launch, ag::SIGN_IN_REQUIRED.to_owned()).await;
                }
                Err(e) => tracing::warn!(thread = %input.thread_id, "Antigravity did not resume its session, starting a new one: {e}"),
            }
        }
        let (session_id, session) = match opened {
            Some(o) => o,
            None => match launch.rpc.request("session/new", params).await {
                Ok(answer) => match answer["sessionId"].as_str() {
                    Some(id) => (id.to_owned(), answer),
                    None => return failed(launch, "Antigravity's session has no id.".into()).await,
                },
                Err(e) if ag::launch::sign_in_required(&e) || launch.auth_url.get().is_some() => {
                    ag::signed_out(&instance, launch.method());
                    return failed(launch, ag::SIGN_IN_REQUIRED.to_owned()).await;
                }
                Err(crate::jsonrpc::RpcError::Remote(e)) if e["code"] == -32603 => {
                    let message = "Antigravity authenticated, but could not initialize a session. Check the account's access, then refresh provider status.".to_owned();
                    ag::update_account(&instance, |a| a.message = Some(message.clone()));
                    return failed(launch, message).await;
                }
                Err(e) => return failed(launch, format!("Antigravity started no session: {}", ag::launch::explain(&e))).await,
            },
        };
        ag::session_started(&instance, launch.method(), launch.exe.version.as_deref(), &session, Some(&input.cwd));
        let ag::launch::Launch { rpc, mut events, init, config, temp, auth_url, profile, lease, .. } = launch;
        while events.try_recv().is_ok() {}
        Ok(Arc::new(AcpSession {
            rpc,
            events: Mutex::new(events),
            session_id,
            cwd: input.cwd.clone(),
            runtime_mode: input.runtime_mode.clone(),
            instance,
            session: std::sync::Mutex::new(session),
            prompt_caps: init["agentCapabilities"]["promptCapabilities"].clone(),
            appends_system_prompt: ag::acp::appends_system_prompt(&init),
            broken: AtomicBool::new(false),
            instructed: AtomicBool::new(instructed),
            mode_before_plan: std::sync::Mutex::new(None),
            antigravity: Some(Antigravity {
                method: config.auth_method,
                launch_key: config.launch_key(),
                roots: vec![std::path::PathBuf::from(&input.cwd), attachments.to_path_buf()],
                _temp: temp,
                profile_home: profile.home,
                _lease: lease,
                auth_url,
                plan_files: Default::default(),
            }),
        }))
    }

    /// Whether a running session still fits the instance's settings (an
    /// Antigravity sign-in method or credential changed: it does not).
    pub fn current(&self) -> bool {
        self.antigravity.as_ref().is_none_or(|a| crate::antigravity::config(&self.instance).launch_key() == a.launch_key)
    }

    /// The provider instance whose agent this is.
    pub fn instance(&self) -> &str {
        &self.instance
    }

    pub fn usable(&self) -> bool {
        !self.broken.load(Ordering::SeqCst)
    }

    pub async fn kill(&self) {
        self.rpc.kill().await;
        if let Some(a) = &self.antigravity {
            a._temp.remove();
        }
    }

    /// The model, its options and the mode an Antigravity turn wants (t3code
    /// `applyAntigravityAcpModelSelection`, the ACP adapter's option
    /// selections and `antigravityCliSessionMode`).
    async fn apply_antigravity_selection(&self, input: &TurnInput) -> Result<(), String> {
        use crate::antigravity::acp::{self as aga, ModelChoice};
        let options = || crate::util::locked(&self.session)["configOptions"].clone();
        let current = options();
        let model_id = aga::model_option(&current).and_then(|o| o["id"].as_str()).map(str::to_owned);
        match aga::choose_model(&current, input.model_selection.model.as_str(), Some(crate::antigravity::DEFAULT_CHAT_MODEL)) {
            ModelChoice::Keep => {}
            ModelChoice::Set(model) | ModelChoice::SetUnlisted(model) => match &model_id {
                Some(id) => self.set_option(id, &model).await.map_err(|e| format!("The agent did not accept the model \"{model}\": {e}"))?,
                None => {
                    self.rpc
                        .request("session/set_model", json!({ "sessionId": self.session_id, "modelId": model }))
                        .await
                        .map_err(|e| format!("The agent did not accept the model \"{model}\": {}", crate::antigravity::launch::explain(&e)))?;
                }
            },
            ModelChoice::Unavailable(message) => return Err(message),
        }
        for selection in input.model_selection.options.iter().flatten() {
            let OptionValue::Choice(value) = &selection.value else { continue };
            let now = options();
            let is_mode = now.as_array().is_some_and(|o| o.iter().any(|o| o["id"] == selection.id.as_str() && o["category"] == "mode"));
            if is_mode {
                continue;
            }
            if let Some(value) = aga::option_value(&now, &selection.id, value).map(str::to_owned)
                && let Err(e) = self.set_option(&selection.id, &value).await
            {
                tracing::warn!(option = %selection.id, "Antigravity did not take a model option: {e}");
            }
        }
        let now = options();
        let mode_option = now.as_array().and_then(|o| o.iter().find(|o| o["category"] == "mode" && o["type"] == "select")).cloned();
        if let Some(mode) = mode_option {
            let want = aga::resolved_mode_for_option(&mode, &input.interaction_mode);
            if mode["currentValue"] != want
                && let Some(id) = mode["id"].as_str()
            {
                self.set_option(id, want).await.map_err(|e| format!("Antigravity did not take its session mode: {e}"))?;
            }
        } else {
            let session = crate::util::locked(&self.session).clone();
            let want = aga::resolved_mode_for_session(&session, &input.interaction_mode);
            let offered = session["modes"]["availableModes"].as_array().is_some_and(|m| m.iter().any(|m| m["id"] == want));
            if offered && session["modes"]["currentModeId"] != want && self.rpc.request("session/set_mode", json!({ "sessionId": self.session_id, "modeId": want })).await.is_ok() {
                crate::util::locked(&self.session)["modes"]["currentModeId"] = json!(want);
            }
        }
        Ok(())
    }

    /// `session/set_config_option`; the session's options as the agent answers them.
    async fn set_option(&self, id: &str, value: &str) -> Result<(), String> {
        let answer = self
            .rpc
            .request("session/set_config_option", json!({ "sessionId": self.session_id, "configId": id, "value": value }))
            .await
            .map_err(|e| crate::antigravity::launch::explain(&e))?;
        let options = {
            let mut session = crate::util::locked(&self.session);
            if let Some(o) = answer["configOptions"].as_array() {
                session["configOptions"] = Value::Array(o.clone());
            } else if let Some(list) = session["configOptions"].as_array_mut()
                && let Some(option) = list.iter_mut().find(|o| o["id"] == id)
            {
                option["currentValue"] = json!(value);
            }
            session["configOptions"].clone()
        };
        if self.antigravity.is_some() {
            crate::antigravity::config_options_changed(&self.instance, &options);
        }
        Ok(())
    }

    /// The model and mode the turn wants, set on the session.
    async fn apply_selection(&self, input: &TurnInput) -> Result<(), String> {
        if self.antigravity.is_some() {
            return self.apply_antigravity_selection(input).await;
        }
        let session = crate::util::locked(&self.session).clone();
        let model = input.model_selection.model.as_str();
        if !model.is_empty() && model != "default" {
            let option = session["configOptions"].as_array().and_then(|o| o.iter().find(|o| o["category"] == "model" && o["type"] == "select").cloned());
            if let Some(option) = option {
                if option["currentValue"] != model {
                    let _ = self.rpc.request("session/set_config_option", json!({ "sessionId": self.session_id, "configId": option["id"], "value": model })).await;
                }
            } else if session["models"]["currentModelId"] != model {
                let _ = self.rpc.request("session/set_model", json!({ "sessionId": self.session_id, "modelId": model })).await;
            }
        }
        let modes = session["modes"]["availableModes"].as_array().cloned().unwrap_or_default();
        let current = session["modes"]["currentModeId"].as_str().map(str::to_owned);
        if input.interaction_mode == "plan" {
            let plan = modes.iter().filter_map(|m| m["id"].as_str()).find(|id| id.ends_with("plan") || id.ends_with("architect")).map(str::to_owned);
            if let Some(plan) = plan.filter(|p| Some(p) != current.as_ref()) {
                if self.rpc.request("session/set_mode", json!({ "sessionId": self.session_id, "modeId": plan })).await.is_ok() {
                    *crate::util::locked(&self.mode_before_plan) = current;
                    crate::util::locked(&self.session)["modes"]["currentModeId"] = json!(plan);
                }
            }
        } else {
            let before = crate::util::locked(&self.mode_before_plan).take();
            let Some(before) = before else { return Ok(()) };
            if self.rpc.request("session/set_mode", json!({ "sessionId": self.session_id, "modeId": before })).await.is_ok() {
                crate::util::locked(&self.session)["modes"]["currentModeId"] = json!(before);
            }
        }
        Ok(())
    }

    /// The prompt's content blocks: images the agent takes, then the text
    /// (with the instructions about Domovoi's tools on the session's first).
    fn prompt(&self, input: &TurnInput) -> Vec<Value> {
        use base64::Engine as _;
        let mut blocks = vec![];
        if self.antigravity.is_some() {
            blocks = crate::antigravity::protocol::attachment_blocks(&input.images, &input.files, &self.prompt_caps);
        } else if self.prompt_caps["image"] == true {
            for (mime, path) in &input.images {
                if let Ok(bytes) = std::fs::read(path) {
                    blocks.push(json!({ "type": "image", "mimeType": mime, "data": base64::engine::general_purpose::STANDARD.encode(bytes) }));
                }
            }
        }
        let leading_command = input.text.trim_start().starts_with('/');
        let mut instructions = Vec::new();
        let legacy_antigravity = self.antigravity.is_some() && !self.appends_system_prompt;
        if legacy_antigravity && !leading_command {
            instructions.push(crate::agent_instructions::question_guidance("ask_question", Some(&input.interaction_mode)));
        }
        if crate::mcp::session_for(&input.thread_id).is_some() && !leading_command && !self.instructed.swap(true, Ordering::SeqCst) {
            instructions.push(crate::agent_instructions::system_prompt());
        }
        let text = if instructions.is_empty() {
            input.text.clone()
        } else {
            format!("<t3_code_instructions>\n{}\n</t3_code_instructions>\n\n<user_request>\n{}\n</user_request>", instructions.join("\n\n"), input.text)
        };
        blocks.push(json!({ "type": "text", "text": text }));
        blocks
    }

    /// Runs one turn. Returns the turn and whether the agent is still usable.
    pub async fn run_turn(&self, mut turn: Turn, input: TurnInput, mut controls: mpsc::UnboundedReceiver<Control>) -> (Turn, bool) {
        turn.session(ProviderSessionStatus::Ready, &input.cwd, &input.model_selection.model, None);
        turn.thread_status(ProviderThreadStatus::Active, Some(&self.session_id));
        let mut events = self.events.lock().await;
        while let Ok(ev) = events.try_recv() {
            if matches!(ev, Incoming::Closed) {
                self.broken.store(true, Ordering::SeqCst);
                turn.complete(TurnEnd::Failed, Some("The ACP agent exited."));
                return (turn, false);
            }
        }
        // `/logout` alone: Domovoi signs no account out (the Google one is the
        // machine's shared agy sign-in); the turn says where to do it.
        if self.antigravity.is_some() && crate::antigravity::auth::is_logout_prompt(&input.text, !input.images.is_empty()) {
            turn.turn_started(&format!("pending:{}", crate::util::new_uuid()));
            turn.item("logout", Work::Notice { message: crate::antigravity::auth::sign_out_answer(&self.instance) }, TurnItemStatus::Completed);
            turn.complete(TurnEnd::Completed, None);
            return (turn, self.usable());
        }
        if self.antigravity.is_some()
            && let Some(refused) = crate::antigravity::protocol::goal_control(&input.text, !input.images.is_empty() || !input.files.is_empty())
        {
            turn.turn_started(&format!("pending:{}", crate::util::new_uuid()));
            turn.complete(TurnEnd::Failed, Some(refused));
            return (turn, self.usable());
        }
        if let Err(e) = self.apply_selection(&input).await {
            turn.turn_started(&format!("pending:{}", crate::util::new_uuid()));
            turn.complete(TurnEnd::Failed, Some(&e));
            return (turn, self.usable());
        }
        turn.turn_started(&format!("pending:{}", crate::util::new_uuid()));
        let prompt = self.prompt(&input);
        // Where the agent's session log stands, to read what this prompt adds to it.
        let log_mark = self.antigravity.as_ref().and_then(|a| crate::antigravity::protocol::log_mark(&a.profile_home, &self.session_id));
        let rpc = self.rpc.clone();
        let session_id = self.session_id.clone();
        let (done_tx, mut done) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let _ = done_tx.send(rpc.request("session/prompt", json!({ "sessionId": session_id, "prompt": prompt })).await);
        });
        let antigravity = self.antigravity.is_some();
        let mut state = TurnState {
            antigravity,
            interaction_mode: input.interaction_mode.clone(),
            plan_key: format!("antigravity-plan:{}", turn.run.id),
            ..TurnState::default()
        };
        let mut interrupted = false;
        let mut prompt_done = false;
        let mut message_questions_ready = false;
        let mut awaiting_background = false;
        let mut wake_activity_seen = false;
        let mut parent_step_before_wake: Option<i64> = None;
        let mut wake_deadline: Option<tokio::time::Instant> = None;
        let mut settle_deadline: Option<tokio::time::Instant> = None;
        let mut poll = tokio::time::interval(subagent_poll_interval());
        poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // Antigravity: how long the agent may stay silent, and how long a stop may take.
        let quiet = tokio::time::sleep(INACTIVITY_LIMIT);
        tokio::pin!(quiet);
        let grace = tokio::time::sleep(std::time::Duration::MAX / 4);
        tokio::pin!(grace);
        let sign_in = async {
            match &self.antigravity {
                Some(a) => a.auth_url.wait().await,
                None => std::future::pending().await,
            }
        };
        tokio::pin!(sign_in);
        let (end, error) = loop {
            tokio::select! {
                answer = &mut done, if !prompt_done => {
                    // The reader passed on what came before the answer first: take it in.
                    while let Ok(ev) = events.try_recv() {
                        if let Incoming::Notification { method, params } = ev
                            && method == "session/update"
                        {
                            self.update(&mut state, &mut turn, &params["update"]);
                        }
                    }
                    if let Ok(Ok(r)) = &answer
                        && antigravity
                        && let Some((input_tokens, cached, output, reasoning)) = crate::antigravity::acp::prompt_usage(&r["usage"])
                    {
                        let window = state.window.clone();
                        turn.token_usage(WindowUse {
                            used_tokens: window.as_ref().map_or(input_tokens, |w| w.0),
                            max_tokens: window.and_then(|w| w.1),
                            input_tokens,
                            cached_input_tokens: cached,
                            output_tokens: output,
                            reasoning_output_tokens: reasoning,
                        });
                    }
                    if let (Ok(Ok(r)), Some(a)) = (&answer, &self.antigravity)
                        && !interrupted
                        && r["stopReason"] != "cancelled"
                        && let Some(failure) = crate::antigravity::protocol::outcome(&a.profile_home, &self.session_id, log_mark, r["stopReason"].as_str())
                    {
                        if failure.usage_limit {
                            turn.usage_limited(failure.reset_at.map(domovoi_model::typed::Timestamp::from));
                        }
                        break (TurnEnd::Failed, Some(failure.message));
                    }
                    message_questions_ready = matches!(&answer, Ok(Ok(r)) if r["stopReason"] == "end_turn");
                    let outcome = match answer {
                        Ok(Ok(r)) => match r["stopReason"].as_str() {
                            Some("cancelled") => (TurnEnd::Interrupted, None),
                            Some("refusal") => (TurnEnd::Failed, Some("The agent refused to go on.".to_owned())),
                            _ if interrupted => (TurnEnd::Interrupted, None),
                            _ => (TurnEnd::Completed, None),
                        },
                        Ok(Err(_)) if interrupted => (TurnEnd::Interrupted, None),
                        Ok(Err(e)) if antigravity && crate::antigravity::launch::sign_in_required(&e) => {
                            self.signed_out();
                            (TurnEnd::Failed, Some(crate::antigravity::SIGN_IN_REQUIRED.to_owned()))
                        }
                        Ok(Err(e)) if antigravity => (TurnEnd::Failed, Some(format!("Antigravity failed the turn: {}", crate::antigravity::launch::explain(&e)))),
                        Ok(Err(e)) => (TurnEnd::Failed, Some(format!("The ACP agent failed the turn: {e}"))),
                        Err(_) => (TurnEnd::Failed, Some("The ACP agent stopped answering.".to_owned())),
                    };
                    if outcome.0 == TurnEnd::Completed
                        && !interrupted
                        && let Some(ag) = &self.antigravity
                    {
                        let _ = state.poll_antigravity_subagents(&mut turn, ag, &self.session_id);
                        if state.has_awaitable_background(ag, &self.session_id) {
                            prompt_done = true;
                            awaiting_background = true;
                            parent_step_before_wake = crate::antigravity::child_work::latest_step_idx(&ag.profile_home, &self.session_id);
                            quiet.as_mut().reset(tokio::time::Instant::now() + INACTIVITY_LIMIT);
                            continue;
                        }
                    }
                    break outcome;
                }
                _ = poll.tick(), if antigravity && (!state.bg_subagents.is_empty() || awaiting_background) => {
                    if let Some(ag) = &self.antigravity {
                        if state.poll_antigravity_subagents(&mut turn, ag, &self.session_id) {
                            quiet.as_mut().reset(tokio::time::Instant::now() + INACTIVITY_LIMIT);
                        }
                        if awaiting_background {
                            if !state.bg_subagents.is_empty() {
                                wake_deadline = None;
                                settle_deadline = None;
                            } else if !wake_activity_seen {
                                wake_deadline.get_or_insert_with(|| tokio::time::Instant::now() + wake_start_deadline());
                            } else if state.active_tools.is_empty() && state.asked.is_empty() && state.questions.is_empty() {
                                if crate::antigravity::child_work::parent_turn_settled(&ag.profile_home, &self.session_id, parent_step_before_wake) {
                                    while let Ok(ev) = events.try_recv() {
                                        if let Incoming::Notification { method, params } = ev
                                            && method == "session/update"
                                        {
                                            self.update(&mut state, &mut turn, &params["update"]);
                                        }
                                    }
                                    break (TurnEnd::Completed, None);
                                }
                                settle_deadline.get_or_insert_with(|| tokio::time::Instant::now() + wake_settle_deadline());
                            } else {
                                settle_deadline = None;
                            }
                        }
                    }
                }
                _ = tokio::time::sleep_until(wake_deadline.unwrap_or_else(tokio::time::Instant::now)), if wake_deadline.is_some() => {
                    while let Ok(ev) = events.try_recv() {
                        if let Incoming::Notification { method, params } = ev
                            && method == "session/update"
                        {
                            self.update(&mut state, &mut turn, &params["update"]);
                        }
                    }
                    break (TurnEnd::Completed, None);
                }
                _ = tokio::time::sleep_until(settle_deadline.unwrap_or_else(tokio::time::Instant::now)), if settle_deadline.is_some() => {
                    while let Ok(ev) = events.try_recv() {
                        if let Incoming::Notification { method, params } = ev
                            && method == "session/update"
                        {
                            self.update(&mut state, &mut turn, &params["update"]);
                        }
                    }
                    break (TurnEnd::Completed, None);
                }
                Some(c) = controls.recv() => match c {
                    Control::Respond { request_id, decision, answers } => {
                        if let Some((rpc_id, params)) = state.questions.remove(&request_id) {
                            let reply = crate::antigravity::acp::question_answer(&params, answers.as_ref()).filter(|_| decision != "cancel" && decision != "decline");
                            let _ = self.rpc.respond(rpc_id, reply.unwrap_or_else(|| json!({ "outcome": { "outcome": "cancelled" } }))).await;
                            turn.resolve_request(&request_id, &decision, answers.as_ref());
                        } else if let Some((rpc_id, options)) = state.asked.remove(&request_id) {
                            let _ = self.rpc.respond(rpc_id, permission_outcome(&options, &decision)).await;
                            turn.resolve_request(&request_id, &decision, None);
                        }
                        quiet.as_mut().reset(tokio::time::Instant::now() + INACTIVITY_LIMIT);
                    }
                    Control::Interrupt => {
                        interrupted = true;
                        let _ = self.rpc.notify("session/cancel", Some(json!({ "sessionId": self.session_id }))).await;
                        // A cancelled turn answers what it asked as cancelled (ACP).
                        for (id, (rpc_id, _)) in std::mem::take(&mut state.asked).into_iter().chain(std::mem::take(&mut state.questions)) {
                            let _ = self.rpc.respond(rpc_id, json!({ "outcome": { "outcome": "cancelled" } })).await;
                            turn.resolve_request(&id, "cancel", None);
                        }
                        if awaiting_background {
                            break (TurnEnd::Interrupted, None);
                        }
                        if antigravity {
                            grace.as_mut().reset(tokio::time::Instant::now() + cancel_grace());
                        }
                    }
                    Control::Steer { reply, .. } => {
                        let _ = reply.send(Err("ACP agents take more input once the turn ends.".into()));
                    }
                    Control::StopSubagent { reply, .. } => {
                        let _ = reply.send(Err("ACP agents' subagents can't be stopped one by one.".into()));
                    }
                },
                ev = events.recv() => {
                    quiet.as_mut().reset(tokio::time::Instant::now() + INACTIVITY_LIMIT);
                    if awaiting_background {
                        wake_activity_seen = true;
                        wake_deadline = None;
                        settle_deadline = None;
                    }
                    match ev {
                        None | Some(Incoming::Closed) => {
                            self.broken.store(true, Ordering::SeqCst);
                            break if interrupted { (TurnEnd::Interrupted, None) } else { (TurnEnd::Failed, Some("The ACP agent exited.".to_owned())) };
                        }
                        Some(Incoming::Notification { method, params }) if method == "session/update" => {
                            self.update(&mut state, &mut turn, &params["update"]);
                        }
                        Some(Incoming::Notification { .. }) => {}
                        Some(Incoming::Request { id, method, params }) if method == "session/request_permission" => {
                            self.ask(&mut turn, &mut state, id, &params, &input).await;
                        }
                        Some(Incoming::Request { id, method, params }) if method == "fs/read_text_file" && antigravity => {
                            let roots = self.antigravity.as_ref().map(|a| a.roots.clone()).unwrap_or_default();
                            let read = crate::antigravity::acp::read_text_file(&params, &roots);
                            let served = state.repair.attempted(params["path"].as_str().unwrap_or_default(), read.is_ok());
                            let _ = match read {
                                Ok(content) => self.rpc.respond(id, content).await,
                                Err((code, message)) => self.rpc.respond_error(id, code, &message).await,
                            };
                            // The agent never completes a read it had the client serve: Domovoi does.
                            if let Some(call) = served {
                                state.update(
                                    &mut turn,
                                    &json!({ "sessionUpdate": "tool_call_update", "toolCallId": call, "status": "completed" }),
                                    self.antigravity.as_ref().map(|a| (a, self.session_id.as_str())),
                                    None,
                                );
                            }
                        }
                        Some(Incoming::Request { id, method, .. }) => {
                            let _ = self.rpc.respond_error(id, -32601, &format!("domovoi-server does not offer {method}")).await;
                        }
                    }
                }
                () = &mut quiet, if antigravity && state.asked.is_empty() && state.questions.is_empty() => {
                    self.broken.store(true, Ordering::SeqCst);
                    break (TurnEnd::Failed, Some("Antigravity sent nothing for 15 minutes; the turn was stopped.".to_owned()));
                }
                () = &mut grace, if interrupted => {
                    // It did not end the stopped turn: the process goes, so the next turn starts clean.
                    self.broken.store(true, Ordering::SeqCst);
                    break (TurnEnd::Interrupted, Some("The ACP agent did not finish cancellation".to_owned()));
                }
                _ = &mut sign_in, if !interrupted => {
                    self.signed_out();
                    self.broken.store(true, Ordering::SeqCst);
                    break (TurnEnd::Failed, Some(crate::antigravity::SIGN_IN_REQUIRED.to_owned()));
                }
            }
        };
        drop(events);
        for (id, (rpc_id, _)) in std::mem::take(&mut state.asked).into_iter().chain(std::mem::take(&mut state.questions)) {
            let _ = self.rpc.respond(rpc_id, json!({ "outcome": { "outcome": "cancelled" } })).await;
            turn.resolve_request(&id, "cancel", None);
        }
        state.finish(&mut turn);
        if end == TurnEnd::Completed
            && let Some(ag) = &self.antigravity
        {
            let _ = state.poll_antigravity_subagents(&mut turn, ag, &self.session_id);
            state.finalize_antigravity_plan(&mut turn, ag, &self.session_id);
        }
        state.bg_subagents.clear();
        state.bg_commands.clear();
        turn.background_tasks(vec![]);
        // Subagents still open end with the turn.
        state.subagents.end(&turn, end == TurnEnd::Completed);
        let commands = std::mem::take(&mut state.commands);
        if antigravity {
            if let Some(list) = commands {
                crate::antigravity::commands_listed(&self.instance, &self.cwd, list.into_iter().map(|(name, description)| domovoi_model::typed::ServerProviderSlashCommand { name, description, input: None }).collect());
            }
        } else {
            crate::providers::remember_acp_commands(&self.instance, commands);
        }
        if end == TurnEnd::Completed && message_questions_ready {
            for (id, questions) in std::mem::take(&mut state.message_questions) {
                turn.open_message_question(&id, questions);
            }
        }
        turn.complete(end, error.as_deref());
        if antigravity {
            // What the turn used of the account's quota (the agent is still running to say).
            crate::antigravity::limits::refresh(&self.instance);
        }
        if !self.usable() {
            self.kill().await;
        }
        (turn, self.usable())
    }

    /// The account needs signing in again.
    fn signed_out(&self) {
        if let Some(a) = &self.antigravity {
            crate::antigravity::signed_out(&self.instance, a.method);
        }
    }

    /// One `session/update`, recorded; Antigravity's config options kept.
    fn update(&self, state: &mut TurnState, turn: &mut Turn, u: &Value) {
        if u["sessionUpdate"] == "config_option_update"
            && let Some(options) = u["configOptions"].as_array()
        {
            let options = Value::Array(options.clone());
            crate::util::locked(&self.session)["configOptions"] = options.clone();
            if self.antigravity.is_some() {
                crate::antigravity::config_options_changed(&self.instance, &options);
            }
            return;
        }
        if let Some(ag) = &self.antigravity {
            let signal = crate::antigravity::plan::extract_signal(u);
            let mut u = u.clone();
            crate::antigravity::protocol::normalize_update(&mut u);
            state.repair.repair(&mut u);
            state.update(turn, &u, Some((ag, &self.session_id)), signal);
            return;
        }
        state.update(turn, u, None, None);
    }

    /// A permission request: decided by the runtime mode, or asked. An
    /// Antigravity question is a question form (always the user's).
    async fn ask(&self, turn: &mut Turn, state: &mut TurnState, rpc_id: Value, params: &Value, input: &TurnInput) {
        let call = &params["toolCall"];
        let item = call["toolCallId"].as_str().unwrap_or_default().to_owned();
        if self.antigravity.is_some() && crate::antigravity::acp::is_question(params) {
            match crate::antigravity::acp::question(params) {
                Some(q) => {
                    let native = format!("question:{item}:{}", crate::util::new_uuid());
                    let rid = turn.open_user_input(&native, &item, vec![q]);
                    state.questions.insert(rid, (rpc_id, params.clone()));
                }
                None => {
                    let _ = self.rpc.respond(rpc_id, json!({ "outcome": { "outcome": "cancelled" } })).await;
                }
            }
            return;
        }
        let options = params["options"].clone();
        let kind = tool_kind(state.kinds.get(call["toolCallId"].as_str().unwrap_or_default()).map_or(call["kind"].as_str().unwrap_or("other"), String::as_str));
        let decided = match (input.runtime_mode.as_str(), kind) {
            (_, "read" | "search" | "fetch") => Some("accept"),
            ("full-access", _) => Some("accept"),
            ("auto-accept-edits", "edit" | "delete" | "move") => Some("accept"),
            _ => None,
        };
        if let Some(decision) = decided {
            let _ = self.rpc.respond(rpc_id, permission_outcome(&options, decision)).await;
            return;
        }
        let request_kind = match kind {
            "execute" => ProviderRequestKind::Command,
            "edit" | "delete" | "move" => ProviderRequestKind::FileChange,
            "read" | "search" | "fetch" => ProviderRequestKind::FileRead,
            _ => ProviderRequestKind::Permission,
        };
        let prompt = call["title"].as_str().filter(|t| !t.is_empty()).map_or_else(|| "Allow this?".to_owned(), str::to_owned);
        let native = format!("permission:{item}:{}", crate::util::new_uuid());
        let offered = self.antigravity.is_some().then(|| crate::antigravity::acp::approval_options(params));
        let rid = turn.open_request_with(&native, &item, request_kind, &prompt, offered);
        state.asked.insert(rid, (rpc_id, options));
    }
}

/// The option a decision picks among the agent's (ACP `PermissionOption`):
/// accept is `allow_once`, accept for the session `allow_always`, decline
/// `reject_once`; anything else, or no such option, cancels.
pub fn permission_outcome(options: &Value, decision: &str) -> Value {
    let wanted: &[&str] = match decision {
        "accept" => &["allow_once", "allow_always"],
        "acceptForSession" | "acceptAlways" => &["allow_always", "allow_once"],
        "decline" => &["reject_once", "reject_always"],
        _ => &[],
    };
    let chosen = wanted.iter().find_map(|kind| options.as_array()?.iter().find(|o| o["kind"] == *kind).and_then(|o| o["optionId"].as_str()));
    match chosen {
        Some(id) => json!({ "outcome": { "outcome": "selected", "optionId": id } }),
        None => json!({ "outcome": { "outcome": "cancelled" } }),
    }
}

fn tool_kind(kind: &str) -> &str {
    match kind {
        "execute" | "edit" | "delete" | "move" | "read" | "search" | "fetch" | "think" => kind,
        _ => "other",
    }
}

/// An Antigravity subagent currently tracked in the background during a turn.
struct BgSubagent {
    title: String,
    prompt: String,
    conversation_id: Option<String>,
    log_uri: Option<String>,
    /// Whether this call launched an async subagent (`agy` 1.5.x single `invoke_subagent`).
    is_async: bool,
    /// Last seen signature `(status, text_len, tool_calls_len)` per transcript step index.
    synced_steps: HashMap<u64, (Option<String>, usize, usize)>,
    /// Child tool call ids already emitted to the child thread.
    started_tools: HashSet<String>,
    /// Child tool calls started and waiting on their `GENERIC` result step: `(id, name, input)`.
    open_tools: std::collections::VecDeque<(String, String, Value)>,
}

/// Short human-readable progress label for a tool a child subagent is running.
fn tool_progress_label(name: &str, input: &Value) -> String {
    fn short_path(p: &str) -> &str {
        std::path::Path::new(p).file_name().and_then(|n| n.to_str()).unwrap_or(p)
    }
    let pick = |keys: &[&str]| keys.iter().find_map(|k| input[*k].as_str().map(str::trim).filter(|s| !s.is_empty()));
    match name {
        "read_file" | "view_file" => match pick(&["Path", "AbsolutePath", "path"]) {
            Some(p) => format!("Reading {}", short_path(p)),
            None => "Reading file".into(),
        },
        "write_file" | "edit_file" | "write_to_file" | "replace_file_content" => match pick(&["Path", "TargetFile", "path"]) {
            Some(p) => format!("Editing {}", short_path(p)),
            None => "Editing file".into(),
        },
        "run_command" | "Bash" => match pick(&["toolSummary", "CommandLine", "command"]) {
            Some(s) => {
                let first = s.lines().next().unwrap_or(s).trim();
                if first.chars().count() <= 48 { format!("Running {first}") } else { format!("Running {}…", first.chars().take(47).collect::<String>()) }
            }
            None => "Running command".into(),
        },
        "generate_image" => match pick(&["ImageName"]) {
            Some(n) => format!("Generating {n}"),
            None => "Generating image".into(),
        },
        "search_web" => "Searching the web".into(),
        "send_message" => "Sending result".into(),
        other => format!("Running {other}"),
    }
}

/// What one turn has received.
#[derive(Default)]
struct TurnState {
    /// The assistant message being written, and its text.
    message: Option<(String, String)>,
    messages: u32,
    /// The thought being written, and its text.
    thought: Option<(String, String)>,
    /// Tool calls by id: what is known of each so far.
    tools: HashMap<String, Value>,
    kinds: HashMap<String, String>,
    /// Non-subagent tool calls currently running in this turn.
    active_tools: HashSet<String>,
    /// Permissions asked, by Domovoi request id: the JSON-RPC id and the options.
    asked: HashMap<String, (Value, Value)>,
    /// Antigravity's questions asked, by Domovoi request id: the JSON-RPC id and the request.
    questions: HashMap<String, (Value, Value)>,
    message_questions: std::collections::BTreeMap<String, Vec<domovoi_model::typed::UserInputQuestion>>,
    /// The context window as the agent last reported it (used, size).
    window: Option<(u64, Option<u64>)>,
    /// Antigravity's turn: its tool calls read its way.
    antigravity: bool,
    /// Antigravity's reads the client served, reported done.
    repair: crate::antigravity::protocol::ReadRepair,
    /// Antigravity's edits, placed when first seen (before they change the file).
    patches: HashMap<String, Option<crate::antigravity::protocol::EditPatch>>,
    /// Antigravity's subagents, each a subagent of the turn.
    subagents: super::subagents::Subagents,
    /// Antigravity's currently active background/in-flight subagents, by tool call id.
    bg_subagents: BTreeMap<String, BgSubagent>,
    /// Antigravity background shell commands still running while the turn moved on (`tool_call_id -> command`).
    bg_commands: BTreeMap<String, String>,
    /// The agent's slash commands, when it said.
    commands: Option<Vec<(String, Option<String>)>>,
    /// The thread's interaction mode for this turn (`default` or `plan`).
    interaction_mode: String,
    /// Native item key for an Antigravity proposed plan in this run.
    plan_key: String,
    /// Un-truncated plan signals seen per tool call id (merged across updates).
    plan_signals: HashMap<String, crate::antigravity::plan::PlanSignal>,
    /// The latest Antigravity plan artifact captured in this turn: (path, markdown).
    plan: Option<(Option<std::path::PathBuf>, String)>,
    /// Completed assistant messages in this turn, scanned for brain plan links on finish.
    assistant_texts: Vec<String>,
}

/// The text of a content block or a list of them.
fn text_of(content: &Value) -> String {
    match content {
        Value::Array(items) => items.iter().map(text_of).collect::<Vec<_>>().join(""),
        Value::Object(_) if content["type"] == "content" => text_of(&content["content"]),
        Value::Object(_) if content["type"] == "text" => content["text"].as_str().unwrap_or_default().to_owned(),
        _ => String::new(),
    }
}

impl TurnState {
    fn close_message(&mut self, turn: &mut Turn) {
        if let Some((id, text)) = self.message.take() {
            if !text.trim().is_empty() {
                self.assistant_texts.push(text.clone());
            }
            turn.assistant(&id, &text, false, None);
        }
    }

    fn close_thought(&mut self, turn: &mut Turn) {
        if let Some((id, text)) = self.thought.take() {
            turn.item(&id, Work::Reasoning { text, streaming: false }, TurnItemStatus::Completed);
        }
    }

    fn capture_antigravity_plan(
        &mut self,
        turn: &mut Turn,
        id: &str,
        status: &TurnItemStatus,
        ag: &Antigravity,
        session_id: &str,
        signal: Option<crate::antigravity::plan::PlanSignal>,
    ) -> bool {
        use crate::antigravity::plan;
        if let Some(sig) = signal {
            self.plan_signals.entry(id.to_owned()).and_modify(|e| e.merge(sig.clone())).or_insert(sig);
        }
        let Some(sig) = self.plan_signals.get(id).cloned() else { return false };
        if !sig.is_edit_tool {
            return false;
        }
        let Some(raw_path) = sig.path.as_deref() else { return false };
        let Some(scoped) = plan::in_brain_scope(&ag.profile_home, session_id, raw_path) else { return false };
        let canonical = std::fs::canonicalize(&scoped).ok();
        let tracked = {
            let files = crate::util::locked(&ag.plan_files);
            files.contains(&scoped) || canonical.as_ref().is_some_and(|c| files.contains(c))
        };
        if !plan::should_capture(&scoped, Some(&sig), &self.interaction_mode, tracked) {
            return false;
        }
        match status {
            TurnItemStatus::Completed => {
                let captured = plan::read_brain_markdown(&ag.profile_home, session_id, raw_path)
                    .map(|(path, md)| (Some(path), md))
                    .or_else(|| sig.inline_markdown.clone().map(|md| (Some(scoped.clone()), md)));
                if let Some((path, markdown)) = captured {
                    if let Some(p) = &path {
                        crate::util::locked(&ag.plan_files).insert(p.clone());
                    }
                    self.plan = Some((path, markdown.clone()));
                    turn.plan(&self.plan_key, Plan::Proposed { markdown }, false);
                }
            }
            TurnItemStatus::Running => {
                if let Some(markdown) = sig.inline_markdown.clone() {
                    self.plan = Some((Some(scoped), markdown.clone()));
                    turn.plan(&self.plan_key, Plan::Proposed { markdown }, false);
                }
            }
            _ => {}
        }
        true
    }

    fn finalize_antigravity_plan(&mut self, turn: &mut Turn, ag: &Antigravity, session_id: &str) {
        use crate::antigravity::plan;
        if self.plan.is_none() {
            'scan: for text in self.assistant_texts.iter().rev() {
                for linked in plan::markdown_file_links(text) {
                    let Some((canonical, markdown)) = plan::read_brain_markdown(&ag.profile_home, session_id, &linked.to_string_lossy()) else {
                        continue;
                    };
                    let tracked = crate::util::locked(&ag.plan_files).contains(&canonical);
                    if plan::should_capture(&canonical, None, &self.interaction_mode, tracked) {
                        crate::util::locked(&ag.plan_files).insert(canonical.clone());
                        self.plan = Some((Some(canonical), markdown));
                        break 'scan;
                    }
                }
            }
        }
        if let Some((path, fallback)) = self.plan.take() {
            let markdown = path
                .as_deref()
                .and_then(|p| plan::read_brain_markdown(&ag.profile_home, session_id, &p.to_string_lossy()))
                .map_or(fallback, |(_, md)| md);
            if !markdown.trim().is_empty() {
                turn.plan(&self.plan_key, Plan::Proposed { markdown }, true);
            }
        }
    }

    fn publish_background_tasks(&self, turn: &mut Turn) {
        let mut tasks: Vec<PendingBackgroundTask> = self
            .bg_subagents
            .iter()
            .map(|(tool_id, bg)| PendingBackgroundTask {
                command: None,
                item_id: None,
                source_thread_id: None,
                task_id: bg.conversation_id.clone().unwrap_or_else(|| tool_id.clone()),
                description: Some(bg.title.trim().to_owned()).filter(|d| !d.is_empty()),
                parent_task_id: None,
                native_item_id: Some(tool_id.clone()),
                awaited: Some(bg.is_async),
                kind: Some(BackgroundTaskKind::Subagent),
                provider_instance_id: None,
                child_thread_id: self.subagents.child_of(tool_id).map(ThreadId::from),
            })
            .collect();
        for (tool_id, command) in &self.bg_commands {
            tasks.push(PendingBackgroundTask {
                command: Some(command.clone()),
                item_id: None,
                source_thread_id: None,
                task_id: tool_id.clone(),
                description: Some(command.clone()),
                parent_task_id: None,
                native_item_id: Some(tool_id.clone()),
                awaited: Some(false),
                kind: Some(BackgroundTaskKind::Command),
                provider_instance_id: None,
                child_thread_id: None,
            });
        }
        turn.background_tasks(tasks);
    }

    fn has_awaitable_background(&self, ag: &Antigravity, session_id: &str) -> bool {
        let has_db = crate::antigravity::child_work::session_db_exists(&ag.profile_home, session_id);
        self.bg_subagents.values().any(|bg| bg.is_async && (bg.conversation_id.is_some() || has_db))
    }

    /// Reads Antigravity's session DB and child `transcript.jsonl` files for
    /// active background subagents, mirroring their tool calls and messages
    /// into their child threads and completing any that finished.
    fn poll_antigravity_subagents(&mut self, turn: &mut Turn, ag: &Antigravity, session_id: &str) -> bool {
        use crate::antigravity::child_work::{self, ChildState};
        if self.bg_subagents.is_empty() {
            return false;
        }
        let mut changed = false;
        let mut finished: Vec<(String, &'static str, Option<String>)> = vec![];
        let mut tasks_dirty = false;
        let ids: Vec<String> = self.bg_subagents.keys().cloned().collect();
        for tool_id in ids {
            let Some(bg) = self.bg_subagents.get_mut(&tool_id) else { continue };
            if bg.conversation_id.is_none()
                && let Some(found) = child_work::lookup_spawned_child(&ag.profile_home, session_id, &tool_id)
            {
                bg.conversation_id = Some(found.conversation_id);
                bg.log_uri = Some(found.log_uri);
                tasks_dirty = true;
                changed = true;
            }
            let (Some(conv_id), Some(log_uri)) = (bg.conversation_id.clone(), bg.log_uri.clone()) else { continue };
            let Some(snapshot) = child_work::inspect_child(&ag.profile_home, session_id, &conv_id, &log_uri, &bg.prompt) else { continue };

            let terminal_result = snapshot.result.as_deref().map(str::trim);
            for step in &snapshot.steps {
                let sig = (step.status.clone(), step.text.as_deref().map_or(0, str::len), step.tool_calls.len());
                if bg.synced_steps.get(&step.index) == Some(&sig) {
                    continue;
                }
                bg.synced_steps.insert(step.index, sig);
                changed = true;
                match step.kind.as_str() {
                    "PLANNER_RESPONSE" => {
                        for (ti, tc) in step.tool_calls.iter().enumerate() {
                            let tid = format!("{tool_id}:s{}:t{ti}", step.index);
                            if bg.started_tools.insert(tid.clone()) {
                                self.subagents.tool(turn, &tool_id, &tid, &tc.name, &tc.input, None, "running");
                                bg.open_tools.push_back((tid, tc.name.clone(), tc.input.clone()));
                                self.subagents.progress(turn, &tool_id, &tool_progress_label(&tc.name, &tc.input));
                            }
                        }
                        if let Some(text) = step.text.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
                            let is_final_dup = snapshot.status.is_terminal() && terminal_result == Some(text);
                            if !is_final_dup {
                                self.subagents.text(turn, &tool_id, &format!("{tool_id}:s{}", step.index), text);
                            }
                        }
                    }
                    "GENERIC" => {
                        if let Some((tid, tname, tinput)) = bg.open_tools.pop_front() {
                            let tstatus = if matches!(step.status.as_deref(), Some("ERROR" | "FAILED")) { "failed" } else { "completed" };
                            self.subagents.tool(turn, &tool_id, &tid, &tname, &tinput, step.text.as_deref(), tstatus);
                        }
                    }
                    _ => {}
                }
            }

            if snapshot.status.is_terminal() {
                let close_status = if snapshot.status == ChildState::Failed { "failed" } else { "completed" };
                while let Some((tid, tname, tinput)) = bg.open_tools.pop_front() {
                    self.subagents.tool(turn, &tool_id, &tid, &tname, &tinput, None, close_status);
                }
                finished.push((tool_id, snapshot.status.as_str(), snapshot.result));
            }
        }
        for (tool_id, status, result) in finished {
            self.bg_subagents.remove(&tool_id);
            self.subagents.finish(turn, &tool_id, status, result.as_deref());
            tasks_dirty = true;
            changed = true;
        }
        if tasks_dirty {
            self.publish_background_tasks(turn);
        }
        changed
    }

    fn update(&mut self, turn: &mut Turn, u: &Value, ag: Option<(&Antigravity, &str)>, signal: Option<crate::antigravity::plan::PlanSignal>) {
        match u["sessionUpdate"].as_str().unwrap_or_default() {
            "agent_message_chunk" => {
                self.close_thought(turn);
                let delta = text_of(&u["content"]);
                let named = u["messageId"].as_str().map(str::to_owned);
                if named.is_some() && self.message.as_ref().map(|(id, _)| id) != named.as_ref() {
                    self.close_message(turn);
                }
                if self.message.is_none() {
                    self.messages += 1;
                    let id = named.unwrap_or_else(|| format!("message-{}", self.messages));
                    self.message = Some((id, String::new()));
                }
                if let Some((id, text)) = self.message.as_mut() {
                    turn.assistant_delta(id, text, &delta);
                }
            }
            "agent_thought_chunk" => {
                self.close_message(turn);
                let delta = text_of(&u["content"]);
                if self.thought.is_none() {
                    self.messages += 1;
                    self.thought = Some((format!("thought-{}", self.messages), String::new()));
                }
                if let Some((id, text)) = self.thought.as_mut() {
                    text.push_str(&delta);
                    let len = text.len();
                    turn.item_streaming(id, Work::Reasoning { text: text.clone(), streaming: true }, len);
                }
            }
            "tool_call" | "tool_call_update" => {
                self.close_message(turn);
                self.close_thought(turn);
                let Some(id) = u["toolCallId"].as_str().map(str::to_owned) else { return };
                let known = self.tools.entry(id.clone()).or_insert_with(|| json!({}));
                if let (Some(known), Some(fields)) = (known.as_object_mut(), u.as_object()) {
                    for (k, v) in fields {
                        if !v.is_null() {
                            known.insert(k.clone(), v.clone());
                        }
                    }
                }
                let known = known.clone();
                if self.antigravity && known["name"] == "ask_question" {
                    if let Some(questions) = crate::antigravity::acp::message_questions(&known) {
                        self.message_questions.insert(id.clone(), questions);
                    } else if matches!(known["status"].as_str(), Some("completed" | "failed")) {
                        self.message_questions.remove(&id);
                    }
                }
                if let Some(k) = known["kind"].as_str() {
                    self.kinds.insert(id.clone(), k.to_owned());
                }
                let status = match known["status"].as_str() {
                    Some("completed") => TurnItemStatus::Completed,
                    Some("failed") => TurnItemStatus::Failed,
                    _ => TurnItemStatus::Running,
                };
                if let Some((ag, session_id)) = ag
                    && self.capture_antigravity_plan(turn, &id, &status, ag, session_id, signal)
                {
                    return;
                }
                if self.antigravity && crate::antigravity::child_work::is_subagent_call(&known) {
                    use crate::antigravity::child_work;
                    let (title, prompt) = child_work::batch(&known);
                    let model = child_work::model_of(&known);
                    let is_async = !child_work::is_batch_call(&known);
                    self.subagents.start(turn, &id, &prompt, Some(&title), model.as_deref());
                    if status == TurnItemStatus::Failed {
                        self.bg_subagents.remove(&id);
                        self.subagents.finish(turn, &id, "failed", child_work::failure_output(&known).as_deref());
                        self.publish_background_tasks(turn);
                        return;
                    }
                    if is_async {
                        let newly_added = !self.bg_subagents.contains_key(&id);
                        if newly_added {
                            self.bg_subagents.insert(
                                id.clone(),
                                BgSubagent {
                                    title: title.clone(),
                                    prompt: prompt.clone(),
                                    conversation_id: None,
                                    log_uri: None,
                                    is_async: true,
                                    synced_steps: HashMap::new(),
                                    started_tools: HashSet::new(),
                                    open_tools: std::collections::VecDeque::new(),
                                },
                            );
                        }
                        if status == TurnItemStatus::Completed {
                            self.subagents.progress(turn, &id, "Running in the background");
                            self.publish_background_tasks(turn);
                            if let Some((ag, session_id)) = ag {
                                let _ = self.poll_antigravity_subagents(turn, ag, session_id);
                            }
                        } else if newly_added {
                            self.publish_background_tasks(turn);
                        }
                    } else if status == TurnItemStatus::Completed {
                        self.bg_subagents.remove(&id);
                        self.subagents.finish(turn, &id, "completed", None);
                        self.publish_background_tasks(turn);
                    } else if !self.bg_subagents.contains_key(&id) {
                        self.bg_subagents.insert(
                            id.clone(),
                            BgSubagent {
                                title,
                                prompt,
                                conversation_id: None,
                                log_uri: None,
                                is_async: false,
                                synced_steps: HashMap::new(),
                                started_tools: HashSet::new(),
                                open_tools: std::collections::VecDeque::new(),
                            },
                        );
                        self.publish_background_tasks(turn);
                    }
                    return;
                }
                if status == TurnItemStatus::Running {
                    self.active_tools.insert(id.clone());
                } else {
                    self.active_tools.remove(&id);
                    if self.bg_commands.remove(&id).is_some() {
                        self.publish_background_tasks(turn);
                    }
                }
                // When Antigravity calls `manage_task`, any other `run_command` still in progress is running in the background.
                if self.antigravity && known["name"].as_str() == Some("manage_task") {
                    let mut promoted = false;
                    for active_id in &self.active_tools {
                        if active_id == &id || self.bg_commands.contains_key(active_id) {
                            continue;
                        }
                        if let Some(tool) = self.tools.get(active_id)
                            && (tool["name"].as_str() == Some("run_command") || tool["kind"].as_str() == Some("execute"))
                        {
                            let cmd = tool["rawInput"]["CommandLine"]
                                .as_str()
                                .or_else(|| tool["args"]["CommandLine"].as_str())
                                .or_else(|| tool["title"].as_str())
                                .unwrap_or("Background command")
                                .to_owned();
                            self.bg_commands.insert(active_id.clone(), cmd);
                            promoted = true;
                        }
                    }
                    if promoted {
                        self.publish_background_tasks(turn);
                    }
                }
                let work = if self.antigravity {
                    let has_diff = known["content"].as_array().is_some_and(|c| c.iter().any(|b| b["type"] == "diff"));
                    if has_diff && !self.patches.contains_key(&id) {
                        self.patches.insert(id.clone(), crate::antigravity::protocol::patch_for(&known));
                    }
                    crate::antigravity::protocol::tool_work(&known, self.patches.get(&id).and_then(Option::as_ref))
                } else {
                    tool_work(&known)
                };
                turn.item(&id, work, status);
            }
            "plan" => {
                let steps = u["entries"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .enumerate()
                    .map(|(i, e)| PlanStep {
                        id: format!("step-{}", i + 1),
                        text: e["content"].as_str().unwrap_or_default().to_owned(),
                        status: match e["status"].as_str() {
                            Some("completed") => PlanStepStatus::Completed,
                            Some("in_progress") => PlanStepStatus::Running,
                            _ => PlanStepStatus::Pending,
                        },
                        duration_anchor_at: None,
                        duration_ms: None,
                    })
                    .collect();
                turn.plan("acp-plan", Plan::Todo { steps, explanation: None }, false);
            }
            "usage_update" => {
                if let Some(used) = u["used"].as_u64() {
                    self.window = Some((used, u["size"].as_u64()));
                    turn.token_usage(WindowUse { used_tokens: used, max_tokens: u["size"].as_u64(), input_tokens: 0, cached_input_tokens: 0, output_tokens: 0, reasoning_output_tokens: 0 });
                }
            }
            "available_commands_update" => {
                self.commands = Some(
                    u["availableCommands"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|c| Some((c["name"].as_str()?.trim_start_matches('/').to_owned(), c["description"].as_str().map(str::to_owned))))
                        .collect(),
                );
            }
            _ => {}
        }
    }

    fn finish(&mut self, turn: &mut Turn) {
        self.close_message(turn);
        self.close_thought(turn);
    }
}

/// The file a `read` tool call reads: its location, or a path in its arguments.
fn read_path(call: &Value) -> Option<&str> {
    let raw = &call["rawInput"];
    [&call["locations"][0]["path"], &raw["path"], &raw["Path"], &raw["AbsolutePath"], &raw["file_path"]]
        .into_iter()
        .find_map(Value::as_str)
        .filter(|p| !p.trim().is_empty())
}

fn is_image_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    [".png", ".jpg", ".jpeg", ".gif", ".webp", ".bmp", ".heic", ".tif", ".tiff"].iter().any(|ext| lower.ends_with(ext))
}

/// A tool call as a turn item, by its ACP kind.
pub fn tool_work(call: &Value) -> Work {
    let title = call["title"].as_str().unwrap_or("Tool").to_owned();
    let output = text_of(&call["content"]);
    let raw_output = call.get("rawOutput").filter(|v| !v.is_null()).cloned();
    match call["kind"].as_str().unwrap_or("other") {
        "execute" => {
            let input = call["rawInput"]["command"].as_str().map(str::to_owned).unwrap_or(title);
            let text = if output.is_empty() { raw_output.as_ref().and_then(|o| o["output"].as_str().or(o.as_str()).map(str::to_owned)).unwrap_or_default() } else { output };
            Work::Command { input, output: (!text.is_empty()).then_some(text), exit_code: raw_output.as_ref().and_then(|o| o["exitCode"].as_i64()) }
        }
        kind @ ("edit" | "delete" | "move") => {
            let diff = call["content"].as_array().and_then(|c| c.iter().find(|b| b["type"] == "diff").cloned());
            let path = diff.as_ref().and_then(|d| d["path"].as_str()).or(call["locations"][0]["path"].as_str()).unwrap_or("file").to_owned();
            let file_name = std::path::Path::new(&path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| path.clone());
            let operation = match kind {
                "delete" => "delete",
                _ if diff.as_ref().is_some_and(|d| d["oldText"].is_null()) => "add",
                _ => "update",
            };
            let changes = vec![FileChangeDetail { operation: operation.into(), path: path.clone(), old_path: None, file_type: None, mime_type: None }];
            let texts = diff.as_ref().map(|d| (d["oldText"].as_str().unwrap_or_default().to_owned(), d["newText"].as_str().unwrap_or_default().to_owned()));
            Work::FileChange { file_name, changes, texts, patch: None, error: None }
        }
        "search" | "fetch" if call["rawInput"]["query"].is_string() => Work::WebSearch { patterns: vec![call["rawInput"]["query"].as_str().unwrap_or_default().to_owned()], results: None },
        // A file read is named for what it is, not by its title ("File src/a.rs").
        "read" if read_path(call).is_some() => {
            let path = read_path(call).unwrap_or_default().to_owned();
            let mut input = match &call["rawInput"] {
                Value::Object(map) => map.clone(),
                _ => serde_json::Map::new(),
            };
            input.insert("file_path".into(), Value::String(path.clone()));
            let viewed_image_path = is_image_path(&path).then(|| path.clone());
            Work::Tool {
                name: "Read".into(),
                input: Value::Object(input),
                output: if output.is_empty() { raw_output.filter(|o| !o.is_object() || o.as_object().is_some_and(|m| !m.is_empty())) } else { Some(Value::String(output)) },
                viewed_image_path,
            }
        }
        _ => Work::Tool {
            name: title,
            input: call["rawInput"].clone(),
            output: if output.is_empty() { raw_output } else { Some(Value::String(output)) },
            viewed_image_path: None,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decisions_pick_the_agents_options() {
        let options = json!([{ "optionId": "a1", "kind": "allow_once" }, { "optionId": "a2", "kind": "allow_always" }, { "optionId": "r1", "kind": "reject_once" }]);
        assert_eq!(permission_outcome(&options, "accept"), json!({ "outcome": { "outcome": "selected", "optionId": "a1" } }));
        assert_eq!(permission_outcome(&options, "acceptForSession"), json!({ "outcome": { "outcome": "selected", "optionId": "a2" } }));
        assert_eq!(permission_outcome(&options, "decline"), json!({ "outcome": { "outcome": "selected", "optionId": "r1" } }));
        assert_eq!(permission_outcome(&options, "cancel"), json!({ "outcome": { "outcome": "cancelled" } }));
        assert_eq!(permission_outcome(&json!([{ "optionId": "x", "kind": "allow_always" }]), "accept")["outcome"]["optionId"], "x");
    }

    #[test]
    fn tool_calls_become_items_by_kind() {
        let cmd = tool_work(&json!({ "kind": "execute", "title": "Run ls", "rawInput": { "command": "ls" }, "content": [{ "type": "content", "content": { "type": "text", "text": "a.txt" } }] }));
        assert!(matches!(cmd, Work::Command { ref input, output: Some(ref out), .. } if input == "ls" && out == "a.txt"));
        let edit = tool_work(&json!({ "kind": "edit", "title": "Edit", "content": [{ "type": "diff", "path": "/w/a.txt", "oldText": "a", "newText": "b" }] }));
        assert!(matches!(edit, Work::FileChange { ref file_name, texts: Some((ref o, ref n)), .. } if file_name == "a.txt" && o == "a" && n == "b"));
        assert!(matches!(tool_work(&json!({ "kind": "other", "title": "Ask" })), Work::Tool { ref name, .. } if name == "Ask"));
        assert_eq!(pick_auth(&json!([{ "id": "env", "type": "env_var" }, { "id": "login" }]), None).as_deref(), Some("login"));
        assert_eq!(pick_auth(&json!([{ "id": "login" }]), Some("other")), None);
    }
}
