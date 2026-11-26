//! Agents that speak the Agent Client Protocol (t3code `provider-acp` and
//! `effect-acp`): newline-delimited JSON-RPC 2.0 over the agent's stdio,
//! one process per thread.
//!
//! - `initialize` (protocol 1), then `session/new` in the thread's folder,
//!   or `session/load` / `session/resume` of the thread's ACP session. An
//!   agent that needs signing in is sent `authenticate` with the instance's
//!   method (or its first agent-side one) and asked again.
//! - What the client offers is t3code's per flavor: an `acpRegistry` agent
//!   gets URL sign-ins (`elicitation.url`: shown on the instance's entry as
//!   `auth.action` until `server.acceptAcpRegistryUrlAuth`, 10 minutes at
//!   most; [`url_auth`]) and, for Devin alone, the client's terminals
//!   ([`super::acp_terminals`]: opened only when the runtime mode runs
//!   commands or a command was approved in the turn or for the session);
//!   Antigravity reads files of the thread's folder; no agent is offered
//!   `fs/write_text_file` or elicitation forms (t3code offers writes to none).
//! - Domovoi's MCP tools go along: over HTTP when the agent takes HTTP MCP
//!   servers, else through `domovoi-server acp-mcp-bridge` (stdio).
//! - `session/prompt` runs a turn; `session/update` notifications are its
//!   messages, thoughts, tool calls (a `terminal` content shown as what the
//!   terminal printed), plan and context use; `session/cancel` stops it.
//!   `session/request_permission` asks as an approval unless t3code's policy
//!   decides it (`acpPermissionDisposition`: reads, searches and thoughts are
//!   allowed; Supervised asks; auto-accept-edits allows edits; Auto and full
//!   access allow), as each flavor adjusts it.
//! - The model is the session's model option or `session/set_model`; plan
//!   mode is a mode whose id ends in `plan` or `architect`.
//! - A message sent during a turn waits for its end (t3code restarts the
//!   turn instead), except for Grok and Kiro, which take it natively.
//! - The first prompt of a session carries the instructions about Domovoi's
//!   tools (ACP has no system prompt field).
//!
//! Grok, Kiro and fx ([`crate::acp_cli`]) are flavors of this: each CLI's
//! launch, `initialize` and session `_meta`, its model, option and mode
//! writes ([`AcpSession::apply_cli_selection`]), its approval choices and
//! policy, and its native steer (Grok `_x.ai/interject`, taken once queued;
//! Kiro `_session/steer`, text only, taken once Kiro reports it read, the
//! note its model adds left out). Grok's `ask_user_question` is a question
//! form, its `exit_plan_mode` a proposed plan, and a `prompt_complete`
//! notification ends a prompt it never answered; its `session/load` counts
//! as loaded a second after the replay goes quiet. Kiro's cloud-config tool
//! call is not the turn's.
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
//!
//! Limits: no steering of Antigravity (t3code sends it another
//! `session/prompt`); no client terminals for agents other than Devin, and a
//! terminal's folder is not held to the workspace (t3code's isn't); requests
//! an agent sends between turns are not answered, except about terminals.

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

/// The client side of `initialize`: no file system or terminal of the
/// client's (a flavor adds what it serves, [`registry_initialize_params`]).
pub fn initialize_params() -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "clientCapabilities": { "fs": { "readTextFile": false, "writeTextFile": false }, "terminal": false },
        "clientInfo": { "name": "t3-code", "title": "T3 Code (Domovoi)", "version": domovoi_contracts::environment::SERVER_VERSION },
    })
}

/// What an `acpRegistry` instance's agent is offered (t3code
/// `makeAcpRegistryAdapterV2`): URL sign-ins (`elicitation.url`), and
/// terminals for Devin, which runs its commands in the client's.
pub fn registry_initialize_params(devin: bool) -> Value {
    let mut params = initialize_params();
    params["clientCapabilities"]["elicitation"] = json!({ "url": {} });
    if devin {
        params["clientCapabilities"]["terminal"] = json!(true);
        params["clientCapabilities"]["_meta"] = json!({ "cognition.ai/subagentSupport": true, "cognition.ai/messageGrouping": true });
    }
    params
}

/// Whether an `acpRegistry` instance runs Devin (registry agent `devin`).
fn is_devin(instance_id: &str) -> bool {
    crate::providers::instance(instance_id).and_then(|c| c.config).is_some_and(|c| c["source"] != "local" && c["agentId"] == "devin")
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
    /// The CLI, for a `grok`, `kiro` or `fx` instance ([`crate::acp_cli`]).
    cli: Option<crate::acp_cli::Kind>,
    /// What `initialize` answered.
    init: Value,
    /// The terminals the agent runs, when it was offered them.
    terminals: Option<Arc<super::acp_terminals::Terminals>>,
    /// Commands approved, which also let the agent open terminals.
    grants: std::sync::Mutex<ExecGrants>,
    /// URL sign-ins are answered (an `acpRegistry` instance).
    url_auth: bool,
    /// fx: the session's mode is written at the next turn even if it is the one fx reports.
    mode_unwritten: AtomicBool,
    /// Kiro: what its session updates need across turns.
    kiro: std::sync::Mutex<KiroState>,
    /// The CLI's settings it was started with: other settings need another process.
    cli_config: Option<crate::acp_cli::Config>,
}

/// Commands approved: an approved `execute` request lets the agent open
/// terminals for the turn, or for the session when allowed for it (ACP
/// can't tie a command to its terminal; t3code `makeAcpClientPolicyGrants`).
#[derive(Default)]
struct ExecGrants {
    session: bool,
    turn: Option<String>,
}

#[derive(Default)]
struct KiroState {
    notes: crate::acp_cli::kiro::SteeringNotes,
    cloud_config: crate::acp_cli::kiro::CloudConfigCalls,
}

/// Steers sent and not yet read by the agent (Kiro's `steering_injected`),
/// by its message id; and those read before their send was answered.
#[derive(Default)]
struct SteerWaits {
    waiting: HashMap<String, tokio::sync::oneshot::Sender<Result<(), String>>>,
    injected: HashSet<String>,
}

impl SteerWaits {
    fn queued(&mut self, id: String, reply: tokio::sync::oneshot::Sender<Result<(), String>>) {
        if self.injected.remove(&id) {
            let _ = reply.send(Ok(()));
        } else {
            self.waiting.insert(id, reply);
        }
    }

    fn injected(&mut self, id: &str) {
        match self.waiting.remove(id) {
            Some(reply) => {
                let _ = reply.send(Ok(()));
            }
            None => {
                self.injected.insert(id.to_owned());
            }
        }
    }

    fn fail_all(&mut self) {
        for (_, reply) in self.waiting.drain() {
            let _ = reply.send(Err("The turn ended before the agent read the message.".into()));
        }
        self.injected.clear();
    }
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
        let cli = crate::acp_cli::kind_of(&instance);
        let devin = cli.is_none() && is_devin(&instance);
        let (mut spawn, init_params, configured, instance_env) = match cli {
            Some(kind) => {
                let config = crate::acp_cli::config(&instance);
                let exe = crate::acp_cli::resolve(kind, &config)?;
                (kind.spawn(&exe, &config, Some(&input.runtime_mode)), kind.initialize_params("t3-code"), kind.auth_method(&config), config.env)
            }
            None => {
                let spawn = spawn_for(&instance)?;
                let env = spawn.env.clone();
                (spawn, registry_initialize_params(devin), auth_method(&instance), env)
            }
        };
        spawn.env.push(("T3CODE_THREAD_ID".into(), input.thread_id.clone()));
        let log = crate::logging::protocol_log(log_dir.join(format!("acp-{}-{}.ndjson", crate::orchestration::events::enc(&instance), crate::util::new_uuid())));
        let args: Vec<&str> = spawn.args.iter().map(String::as_str).collect();
        let (rpc, mut events) = RpcClient::spawn_with(&spawn.program, &args, Some(&input.cwd), &spawn.env, true, log).map_err(|e| format!("could not start the ACP agent: {e}"))?;
        let init = rpc.request("initialize", init_params).await.map_err(|e| format!("The ACP agent did not initialize: {e}"))?;
        let caps = init["agentCapabilities"].clone();
        let servers = mcp_servers(&input.thread_id, &caps);
        let meta = cli.and_then(crate::acp_cli::Kind::session_meta);
        let with_meta = |mut params: Value| {
            if let Some(m) = &meta {
                params["_meta"] = m.clone();
            }
            params
        };
        let open = |method: &'static str, params: Value| {
            let rpc = rpc.clone();
            async move { rpc.request(method, params).await }
        };
        let (session_id, session) = 'open: {
            if let Some(id) = native_session.filter(|id| !id.starts_with("pending:")) {
                let params = with_meta(json!({ "sessionId": id, "cwd": input.cwd, "mcpServers": servers }));
                let method = if caps["loadSession"] == true {
                    Some("session/load")
                } else if !caps["sessionCapabilities"]["resume"].is_null() {
                    Some("session/resume")
                } else {
                    None
                };
                if let Some(method) = method {
                    let answer = if cli == Some(crate::acp_cli::Kind::Grok) && method == "session/load" {
                        replay_settled(open(method, params), &mut events).await
                    } else {
                        open(method, params).await
                    };
                    if let Ok(answer) = answer {
                        break 'open (id, answer);
                    }
                }
            }
            let params = with_meta(json!({ "cwd": input.cwd, "mcpServers": servers }));
            let answer = match open("session/new", params.clone()).await {
                Ok(a) => a,
                Err(e) if auth_required(&e) => {
                    // A CLI's own method goes even when the agent doesn't list it (t3code `authenticateAfterRequired`).
                    let method = pick_auth(&init["authMethods"], configured.as_deref())
                        .or_else(|| configured.clone().filter(|_| cli.is_some()))
                        .ok_or("The ACP agent needs signing in, and offers no way Domovoi can do it here.")?;
                    rpc.request("authenticate", json!({ "methodId": method })).await.map_err(|e| format!("The ACP agent did not sign in: {e}"))?;
                    open("session/new", params).await.map_err(|e| format!("The ACP agent started no session: {e}"))?
                }
                Err(e) => return Err(format!("The ACP agent started no session: {e}")),
            };
            let id = answer["sessionId"].as_str().ok_or("The ACP agent's session has no id.")?.to_owned();
            (id, answer)
        };
        // A loaded session replays its history first: not the next turn's.
        while events.try_recv().is_ok() {}
        let terminals = devin.then(|| super::acp_terminals::Terminals::new(std::path::Path::new(&input.cwd), instance_env, true));
        let cli_config = cli.map(|_| crate::acp_cli::config(&instance));
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
            cli,
            init,
            terminals,
            grants: Default::default(),
            url_auth: cli.is_none(),
            mode_unwritten: AtomicBool::new(true),
            kiro: Default::default(),
            cli_config,
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
        let (prompt_caps, appends_system_prompt) = (init["agentCapabilities"]["promptCapabilities"].clone(), ag::acp::appends_system_prompt(&init));
        Ok(Arc::new(AcpSession {
            rpc,
            events: Mutex::new(events),
            session_id,
            cwd: input.cwd.clone(),
            runtime_mode: input.runtime_mode.clone(),
            instance,
            session: std::sync::Mutex::new(session),
            prompt_caps,
            appends_system_prompt,
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
            cli: None,
            init,
            terminals: None,
            grants: Default::default(),
            url_auth: false,
            mode_unwritten: AtomicBool::new(false),
            kiro: Default::default(),
            cli_config: None,
        }))
    }

    /// Whether a running session still fits the instance's settings (an
    /// Antigravity sign-in method or credential changed: it does not).
    pub fn current(&self) -> bool {
        self.antigravity.as_ref().is_none_or(|a| crate::antigravity::config(&self.instance).launch_key() == a.launch_key)
            && self.cli_config.as_ref().is_none_or(|c| *c == crate::acp_cli::config(&self.instance))
    }

    /// The provider instance whose agent this is.
    pub fn instance(&self) -> &str {
        &self.instance
    }

    pub fn usable(&self) -> bool {
        !self.broken.load(Ordering::SeqCst)
    }

    pub async fn kill(&self) {
        if let Some(t) = &self.terminals {
            t.dispose_all();
        }
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
        if let Some(kind) = self.cli {
            return self.apply_cli_selection(kind, input).await;
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

    /// A select config option of the session, by id.
    fn config_option(&self, id: &str) -> Option<Value> {
        crate::util::locked(&self.session)["configOptions"].as_array()?.iter().find(|o| o["id"] == id && o["type"] == "select").cloned()
    }

    /// Writes a select config option unless it holds `value` already; one
    /// the session doesn't list is skipped (with `listed_only`).
    async fn write_option(&self, id: &str, value: &str, listed_only: bool) -> Result<bool, String> {
        let option = self.config_option(id);
        if option.as_ref().is_some_and(|o| o["currentValue"] == value) {
            return Ok(false);
        }
        if listed_only && !option.as_ref().is_some_and(|o| select_values(o).iter().any(|v| v == value)) {
            return Ok(false);
        }
        self.set_option(id, value).await.map(|()| true)
    }

    /// A Grok, Kiro or fx turn's model, its options and the session's mode
    /// (each flavor's `applyModelSelection`, then the options it leaves to
    /// the shared loop).
    async fn apply_cli_selection(&self, kind: crate::acp_cli::Kind, input: &TurnInput) -> Result<(), String> {
        use crate::acp_cli::{Kind, fx, grok, kiro};
        let model = input.model_selection.model.as_str();
        let owned: &[&str] = match kind {
            Kind::Grok => &["reasoningEffort"],
            Kind::Kiro => &["reasoningEffort", kiro::AGENT],
            Kind::Fx => &[fx::MODE],
        };
        match kind {
            Kind::Grok => {
                let effort = choice(input, "reasoningEffort");
                let session = crate::util::locked(&self.session).clone();
                if self.init["protocolVersion"].as_u64().unwrap_or(PROTOCOL_VERSION) <= 1 {
                    // Protocol 1: the level goes as `session/set_model` metadata.
                    let mut sent = grok::set_model_params(&self.session_id, &session, model, effort.as_deref());
                    if let Some(params) = sent.clone()
                        && let Err(e) = self.rpc.request("session/set_model", params.clone()).await
                    {
                        if params.get("_meta").is_none() {
                            return Err(format!("Grok did not accept the model: {e}"));
                        }
                        tracing::warn!("Grok skipped the requested reasoning effort: {e}");
                        sent = grok::set_model_params(&self.session_id, &session, model, None);
                        if let Some(params) = &sent {
                            self.rpc.request("session/set_model", params.clone()).await.map_err(|e| format!("Grok did not accept the model: {e}"))?;
                        }
                    }
                    if let Some(params) = sent {
                        let mut session = crate::util::locked(&self.session);
                        session["models"]["currentModelId"] = params["modelId"].clone();
                        if let Some(e) = params["_meta"]["reasoningEffort"].as_str()
                            && let Some(m) = session["models"]["availableModels"].as_array_mut().and_then(|a| a.iter_mut().find(|m| m["modelId"] == params["modelId"]))
                        {
                            m["_meta"]["reasoningEffort"] = json!(e);
                        }
                    }
                } else {
                    let wanted = Some(grok::model_id(model)).filter(|m| *m != grok::DEFAULT_MODEL);
                    let current = crate::util::locked(&self.session)["configOptions"].as_array().and_then(|o| o.iter().find(|o| o["category"] == "model").cloned());
                    if let Some(m) = wanted.filter(|m| current.as_ref().is_none_or(|c| c["currentValue"] != *m)) {
                        self.rpc.request("session/set_model", json!({ "sessionId": self.session_id, "modelId": m })).await.map_err(|e| format!("Grok did not accept the model: {e}"))?;
                    }
                    let level = crate::util::locked(&self.session)["configOptions"].as_array().and_then(|o| o.iter().find(|o| o["category"] == "thought_level" && o["type"] == "select").cloned());
                    if let (Some(e), Some(level)) = (effort, level)
                        && let Some(id) = level["id"].as_str()
                        && let Err(e) = self.write_option(id, &e, true).await
                    {
                        tracing::warn!("Grok skipped the requested reasoning effort: {e}");
                    }
                }
            }
            Kind::Kiro => {
                // The agent first: switching applies the agent's own model.
                let mut switched = false;
                if let Some(agent) = choice(input, kiro::AGENT)
                    && let Some(option) = self.config_option(kiro::AGENT)
                    && option["currentValue"] != agent.as_str()
                {
                    if select_values(&option).contains(&agent) {
                        self.set_option(kiro::AGENT, &agent).await.map_err(|e| format!("Kiro did not switch to the agent \"{agent}\": {e}"))?;
                        switched = true;
                    } else {
                        tracing::warn!("Kiro does not list the thread's agent");
                    }
                }
                let option = crate::util::locked(&self.session)["configOptions"].as_array().and_then(|o| o.iter().find(|o| o["id"] == kiro::MODEL || o["category"] == "model").cloned());
                let is_select = option.as_ref().is_some_and(|o| o["type"] == "select");
                let current = option.as_ref().filter(|_| is_select).and_then(|o| o["currentValue"].as_str()).map(str::to_owned);
                let selected = model.trim();
                let is_default = selected.is_empty() || selected == kiro::DEFAULT_MODEL;
                // A new session runs Kiro's default already, unless an agent switch moved it.
                if !(is_default && !is_select && !switched) {
                    let requested = if is_default { kiro::default_model_id(&self.instance) } else { selected.to_owned() };
                    if Some(&requested) != current.as_ref() {
                        let id = option.as_ref().and_then(|o| o["id"].as_str()).unwrap_or(kiro::MODEL).to_owned();
                        self.set_option(&id, &requested).await.map_err(|e| format!("Kiro did not accept the model \"{requested}\": {e}"))?;
                    }
                }
                if let Some(effort) = choice(input, "reasoningEffort")
                    && let Err(e) = self.write_option(kiro::EFFORT, &effort, true).await
                {
                    tracing::warn!("Kiro rejected the reasoning effort {effort}: {e}");
                }
                self.write_option(kiro::AUTOPILOT, kiro::autopilot_value(&input.runtime_mode), true).await.map_err(|e| format!("Kiro did not take its autopilot setting: {e}"))?;
            }
            Kind::Fx => {
                self.apply_fx_mode(&input.runtime_mode).await?;
                self.apply_fx_model(input).await?;
                if choice(input, fx::EFFORT).is_none() {
                    // A thread without an effort starts at medium.
                    self.write_option(fx::EFFORT, fx::DEFAULT_EFFORT, true).await.map_err(|e| format!("fx did not take its effort: {e}"))?;
                }
            }
        }
        // The thread's other options, where the session has an option of that id.
        for selection in input.model_selection.options.iter().flatten() {
            let OptionValue::Choice(value) = &selection.value else { continue };
            if owned.contains(&selection.id.as_str()) {
                continue;
            }
            if let Err(e) = self.write_option(&selection.id, value, true).await {
                tracing::warn!(option = %selection.id, "the agent did not take a model option: {e}");
            }
        }
        Ok(())
    }

    /// fx's session mode for the runtime mode (t3code `withFxSessionMode`):
    /// the first write after a session opens always goes out.
    async fn apply_fx_mode(&self, runtime_mode: &str) -> Result<(), String> {
        use crate::acp_cli::fx;
        let (advertised, current) = {
            let session = crate::util::locked(&self.session);
            let ids: Vec<String> = session["modes"]["availableModes"].as_array().into_iter().flatten().filter_map(|m| m["id"].as_str().map(str::to_owned)).collect();
            (ids, session["modes"]["currentModeId"].as_str().map(str::to_owned))
        };
        let requested = fx::session_mode(runtime_mode);
        let listed: Vec<&str> = advertised.iter().map(String::as_str).collect();
        let Some(target) = fx::mode_id(requested, &listed) else {
            let shown = if listed.is_empty() { "no modes".to_owned() } else { listed.join(", ") };
            return Err(format!("fx cannot run in mode \"{requested}\"; it advertises {shown}."));
        };
        let unwritten = self.mode_unwritten.load(Ordering::SeqCst);
        if current.as_deref() != Some(target) {
            self.rpc.request("session/set_mode", json!({ "sessionId": self.session_id, "modeId": target })).await.map_err(|e| format!("fx did not take its mode: {e}"))?;
            crate::util::locked(&self.session)["modes"]["currentModeId"] = json!(target);
        } else if unwritten {
            self.rpc
                .request("session/set_config_option", json!({ "sessionId": self.session_id, "configId": fx::MODE, "value": target }))
                .await
                .map_err(|e| format!("fx did not take its mode: {e}"))?;
        }
        self.mode_unwritten.store(false, Ordering::SeqCst);
        Ok(())
    }

    /// fx's backend and model (t3code `applyFxModelSelection`): the backend
    /// first, as its models change with it; put back if the model fails.
    async fn apply_fx_model(&self, input: &TurnInput) -> Result<(), String> {
        use crate::acp_cli::fx;
        let backend = self.config_option(fx::PROVIDER);
        let backends = backend.as_ref().map(select_values).unwrap_or_default();
        let original_backend = backend.as_ref().and_then(|b| b["currentValue"].as_str()).map(str::to_owned);
        let original_model = self.config_option(fx::MODEL).and_then(|m| m["currentValue"].as_str().map(str::to_owned));
        let requested = input.model_selection.model.trim();
        let is_default = requested.is_empty() || requested == fx::DEFAULT_MODEL;
        let target: Option<(Option<String>, String)> = if is_default {
            let config = crate::acp_cli::config(&self.instance);
            match crate::acp_cli::resolve(crate::acp_cli::Kind::Fx, &config) {
                Ok(exe) => fx::default_choice(&exe, &config, std::path::Path::new(&self.cwd)).await.map(|(b, m)| (Some(b), m)),
                Err(_) => None,
            }
        } else {
            let listed: Vec<&str> = backends.iter().map(String::as_str).collect();
            let (b, m) = fx::parse_slug(requested, &listed);
            Some((b.map(str::to_owned), m.to_owned()))
        };
        let Some((target_backend, target_model)) = target else { return Ok(()) };
        let can_select = target_backend.as_ref().is_none_or(|b| backend.is_some() && (original_backend.as_ref() == Some(b) || backends.contains(b)));
        let changing = can_select && backend.is_some() && target_backend.as_ref().is_some_and(|b| original_backend.as_ref() != Some(b));
        let result = async {
            if changing && let Some(b) = &target_backend {
                self.set_option(fx::PROVIDER, b).await?;
            }
            let model_option = self.config_option(fx::MODEL);
            let current = model_option.as_ref().and_then(|m| m["currentValue"].as_str()).map(str::to_owned);
            if current.as_ref() == Some(&target_model) || (is_default && !can_select) {
                return Ok(());
            }
            if is_default && !model_option.as_ref().is_some_and(|m| select_values(m).contains(&target_model)) {
                return Err("The default fx model is unavailable on its configured backend.".to_owned());
            }
            self.set_option(fx::MODEL, &target_model).await
        }
        .await;
        if result.is_err()
            && changing
            && let Some(b) = &original_backend
        {
            let _ = self.set_option(fx::PROVIDER, b).await;
            if let Some(m) = &original_model
                && self.config_option(fx::MODEL).is_some_and(|o| o["currentValue"] != m.as_str())
            {
                let _ = self.set_option(fx::MODEL, m).await;
            }
        }
        result.map_err(|e| format!("fx did not take the model \"{requested}\": {e}"))
    }

    /// The prompt's content blocks: images the agent takes, then the text
    /// (with the instructions about Domovoi's tools on the session's first).
    fn prompt(&self, input: &TurnInput) -> Vec<Value> {
        use base64::Engine as _;
        let mut blocks = vec![];
        if self.antigravity.is_some() {
            blocks = crate::antigravity::protocol::attachment_blocks(&input.images, &input.files, &self.prompt_caps);
        } else if self.prompt_caps["image"] == true || self.cli == Some(crate::acp_cli::Kind::Grok) {
            // Grok takes images though it says it doesn't (t3code `supportsImagePrompts`).
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
            match ev {
                Incoming::Closed => {
                    self.broken.store(true, Ordering::SeqCst);
                    turn.complete(TurnEnd::Failed, Some("The ACP agent exited."));
                    return (turn, false);
                }
                // A terminal asked about between turns is answered still.
                Incoming::Request { id, method, params } if method.starts_with("terminal/") && self.terminals.is_some() => {
                    self.terminal_request(id, &method, params, turn.run.id.as_str(), &input.runtime_mode).await;
                }
                _ => {}
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
        let steers: Arc<std::sync::Mutex<SteerWaits>> = Arc::default();
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
                            self.update(&mut state, &mut turn, &params["update"], &steers);
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
                        // Grok's and Kiro's prompt errors carry a message meant for the user (t3code `promptFailure`).
                        Ok(Err(crate::jsonrpc::RpcError::Remote(e))) if self.cli.is_some() && e["message"].as_str().is_some_and(|m| !m.trim().is_empty()) => {
                            if self.cli == Some(crate::acp_cli::Kind::Grok) && e["code"] == crate::acp_cli::grok::RATE_LIMITED {
                                turn.usage_limited(None);
                            }
                            (TurnEnd::Failed, Some(bounded(e["message"].as_str().unwrap_or_default().trim(), 2000)))
                        }
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
                                            self.update(&mut state, &mut turn, &params["update"], &steers);
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
                            self.update(&mut state, &mut turn, &params["update"], &steers);
                        }
                    }
                    break (TurnEnd::Completed, None);
                }
                _ = tokio::time::sleep_until(settle_deadline.unwrap_or_else(tokio::time::Instant::now)), if settle_deadline.is_some() => {
                    while let Ok(ev) = events.try_recv() {
                        if let Incoming::Notification { method, params } = ev
                            && method == "session/update"
                        {
                            self.update(&mut state, &mut turn, &params["update"], &steers);
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
                        } else if let Some((rpc_id, params)) = state.xai_questions.remove(&request_id) {
                            let given = answers.as_ref().filter(|_| decision != "cancel" && decision != "decline");
                            let _ = self.rpc.respond(rpc_id, crate::acp_cli::grok::question_answer(&params, given)).await;
                            turn.resolve_request(&request_id, &decision, answers.as_ref());
                        } else if let Some((rpc_id, options)) = state.asked.remove(&request_id) {
                            let (kind, session_offered) = state.asked_kinds.remove(&request_id).unwrap_or_default();
                            // A card that never offered "this session" is not answered with it.
                            let decision = if decision == "acceptForSession" && !session_offered { "accept".to_owned() } else { decision };
                            if kind == "execute" && matches!(decision.as_str(), "accept" | "acceptForSession") {
                                let mut grants = crate::util::locked(&self.grants);
                                if decision == "acceptForSession" {
                                    grants.session = true;
                                } else {
                                    grants.turn = Some(turn.run.id.as_str().to_owned());
                                }
                            }
                            let _ = self.rpc.respond(rpc_id, permission_outcome(&options, &decision)).await;
                            turn.resolve_request(&request_id, &decision, None);
                        }
                        quiet.as_mut().reset(tokio::time::Instant::now() + INACTIVITY_LIMIT);
                    }
                    Control::Interrupt => {
                        interrupted = true;
                        let mut cancel = json!({ "sessionId": self.session_id });
                        if let Some(meta) = self.cli.and_then(crate::acp_cli::Kind::cancel_meta) {
                            cancel["_meta"] = meta;
                        }
                        let _ = self.rpc.notify("session/cancel", Some(cancel)).await;
                        // A cancelled turn answers what it asked as cancelled (ACP).
                        state.cancel_requests(&self.rpc, &mut turn).await;
                        if awaiting_background {
                            break (TurnEnd::Interrupted, None);
                        }
                        if antigravity {
                            grace.as_mut().reset(tokio::time::Instant::now() + cancel_grace());
                        }
                    }
                    Control::Steer { text, images, reply } => self.steer(text, images, reply, &steers),
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
                            self.update(&mut state, &mut turn, &params["update"], &steers);
                        }
                        Some(Incoming::Notification { method, params })
                            if self.cli == Some(crate::acp_cli::Kind::Grok) && crate::acp_cli::grok::is_prompt_complete(&method) && params["sessionId"] == self.session_id.as_str() && !prompt_done =>
                        {
                            // Grok can say a prompt is done without answering it (t3code `makeXAiPromptCompletionRuntime`).
                            break match params["stopReason"].as_str() {
                                Some("cancelled") => (TurnEnd::Interrupted, None),
                                Some("refusal") => (TurnEnd::Failed, Some("The agent refused to go on.".to_owned())),
                                _ if interrupted => (TurnEnd::Interrupted, None),
                                _ => (TurnEnd::Completed, None),
                            };
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
                        Some(Incoming::Request { id, method, params }) if method.starts_with("terminal/") && self.terminals.is_some() => {
                            self.terminal_request(id, &method, params, turn.run.id.as_str(), &input.runtime_mode).await;
                        }
                        Some(Incoming::Request { id, method, params }) if method == "elicitation/create" && self.url_auth => {
                            self.url_elicitation(id, &params);
                        }
                        Some(Incoming::Request { id, method, params }) if self.cli == Some(crate::acp_cli::Kind::Grok) && crate::acp_cli::grok::is_question(&method) => {
                            match crate::acp_cli::grok::questions(&params) {
                                Some((call, questions)) => {
                                    let native = format!("{}:xai-question:{call}", self.session_id);
                                    let rid = turn.open_user_input(&native, &native, questions);
                                    state.xai_questions.insert(rid, (id, params));
                                }
                                None => {
                                    let _ = self.rpc.respond(id, json!({ "outcome": "cancelled" })).await;
                                }
                            }
                        }
                        Some(Incoming::Request { id, method, params }) if self.cli == Some(crate::acp_cli::Kind::Grok) && crate::acp_cli::grok::is_plan_exit(&method) => {
                            state.finish(&mut turn);
                            let markdown = crate::acp_cli::grok::plan_markdown(&params, None);
                            turn.plan(&format!("grok-plan:{}", turn.run.id), Plan::Proposed { markdown }, true);
                            let _ = self.rpc.respond(id, crate::acp_cli::grok::plan_captured()).await;
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
        state.cancel_requests(&self.rpc, &mut turn).await;
        crate::util::locked(&steers).fail_all();
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
        } else if self.cli.is_none() {
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
            crate::antigravity::limits::refresh(&self.instance, crate::antigravity::limits::AFTER_TURN);
        }
        if !self.usable() {
            self.kill().await;
        }
        (turn, self.usable())
    }

    /// More input for the running turn: Grok's `_x.ai/interject` (taken once
    /// queued), Kiro's `_session/steer` (text only, taken once Kiro reports
    /// it read). Other ACP agents have no way to take it: the message waits
    /// for the turn's end.
    fn steer(&self, text: String, images: Vec<(String, std::path::PathBuf)>, reply: tokio::sync::oneshot::Sender<Result<(), String>>, steers: &Arc<std::sync::Mutex<SteerWaits>>) {
        use crate::acp_cli::{Kind, grok, kiro};
        let rpc = self.rpc.clone();
        let session_id = self.session_id.clone();
        match self.cli {
            Some(Kind::Grok) => {
                use base64::Engine as _;
                let content: Vec<Value> = images
                    .iter()
                    .filter_map(|(mime, path)| Some(json!({ "type": "image", "mimeType": mime, "data": base64::engine::general_purpose::STANDARD.encode(std::fs::read(path).ok()?) })))
                    .collect();
                let mut params = json!({ "sessionId": session_id, "text": text });
                if !content.is_empty() {
                    params["content"] = json!(content);
                }
                tokio::spawn(async move {
                    let taken = match rpc.request(grok::INTERJECT, params).await {
                        Ok(answer) if answer["status"] == "queued" => Ok(()),
                        Ok(_) => Err("Grok did not queue the interjection.".to_owned()),
                        Err(e) => Err(format!("Grok did not take the message: {e}")),
                    };
                    let _ = reply.send(taken);
                });
            }
            Some(Kind::Kiro) if images.is_empty() => {
                let steers = steers.clone();
                tokio::spawn(async move {
                    match rpc.request(kiro::STEER, json!({ "sessionId": session_id, "message": text })).await {
                        Ok(answer) => match kiro::steer_queued(&answer) {
                            Some(id) => crate::util::locked(&steers).queued(id, reply),
                            None => {
                                let _ = reply.send(Err("Kiro did not queue the steering message.".into()));
                            }
                        },
                        Err(e) => {
                            let _ = reply.send(Err(format!("Kiro did not take the message: {e}")));
                        }
                    }
                });
            }
            Some(Kind::Kiro) => {
                let _ = reply.send(Err("Kiro takes a message during a turn as text only.".into()));
            }
            _ => {
                let _ = reply.send(Err("ACP agents take more input once the turn ends.".into()));
            }
        }
    }

    /// A `terminal/*` request. Opening one needs the runtime mode's leave
    /// to run commands, or a command approved in the turn (or for the
    /// session); waiting and releasing are answered when the command ends.
    async fn terminal_request(&self, id: Value, method: &str, params: Value, run_id: &str, runtime_mode: &str) {
        let Some(terminals) = self.terminals.clone() else { return };
        let answer = match method {
            "terminal/create" => {
                let allowed = crate::acp_cli::disposition(runtime_mode, "execute") == crate::acp_cli::Disposition::Allow || {
                    let grants = crate::util::locked(&self.grants);
                    grants.session || grants.turn.as_deref() == Some(run_id)
                };
                if allowed {
                    terminals.create(&params)
                } else {
                    Err(super::acp_terminals::Refused(
                        "The active T3 runtime policy requires approval for terminal/create. Request permission with session/request_permission before retrying.".into(),
                    ))
                }
            }
            "terminal/output" => terminals.output(&params),
            "terminal/kill" => terminals.kill(&params),
            "terminal/wait_for_exit" | "terminal/release" => {
                let (rpc, method) = (self.rpc.clone(), method.to_owned());
                tokio::spawn(async move {
                    let answer = if method == "terminal/release" { terminals.release(&params).await } else { terminals.wait_for_exit(&params).await };
                    let _ = match answer {
                        Ok(v) => rpc.respond(id, v).await,
                        Err(e) => rpc.respond_error(id, -32603, &e.0).await,
                    };
                });
                return;
            }
            other => Err(super::acp_terminals::Refused(format!("domovoi-server does not offer {other}"))),
        };
        let _ = match answer {
            Ok(v) => self.rpc.respond(id, v).await,
            Err(e) => self.rpc.respond_error(id, -32603, &e.0).await,
        };
    }

    /// `elicitation/create`: a URL sign-in is shown on the instance's entry
    /// until the user opens it (`server.acceptAcpRegistryUrlAuth`) or it
    /// expires; anything else is declined (Domovoi has no elicitation forms).
    fn url_elicitation(&self, id: Value, params: &Value) {
        let rpc = self.rpc.clone();
        let url = (params["mode"] == "url").then(|| params["url"].as_str().and_then(web_url)).flatten();
        let elicitation = params["elicitationId"].as_str().map(str::trim).filter(|e| !e.is_empty());
        let (Some(url), Some(elicitation)) = (url, elicitation) else {
            tokio::spawn(async move {
                let _ = rpc.respond(id, json!({ "action": "decline" })).await;
            });
            return;
        };
        let consent = url_auth::request(&self.instance, bounded(elicitation, 256), url, bounded(params["message"].as_str().unwrap_or_default().trim(), 1024));
        tokio::spawn(async move {
            let accepted = matches!(tokio::time::timeout(url_auth::TTL, consent).await, Ok(Ok(true)));
            let _ = rpc.respond(id, json!({ "action": if accepted { "accept" } else { "decline" } })).await;
        });
    }

    /// The account needs signing in again.
    fn signed_out(&self) {
        if let Some(a) = &self.antigravity {
            crate::antigravity::signed_out(&self.instance, a.method);
        }
    }

    /// One `session/update`, recorded; Antigravity's config options kept.
    fn update(&self, state: &mut TurnState, turn: &mut Turn, u: &Value, steers: &std::sync::Mutex<SteerWaits>) {
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
        if self.cli.is_some() || self.terminals.is_some() {
            let mut u = u.clone();
            if self.cli == Some(crate::acp_cli::Kind::Kiro) {
                let mut kiro = crate::util::locked(&self.kiro);
                if kiro.cloud_config.ignore(&u) {
                    return;
                }
                if let Some(("steering_injected", Some(id))) = crate::acp_cli::kiro::steering_info(&u) {
                    crate::util::locked(steers).injected(id);
                }
                kiro.notes.filter(&mut u);
            }
            if let Some(t) = &self.terminals {
                t.embed(&self.session_id, &mut u);
            }
            state.update(turn, &u, None, None);
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
        let decided = if self.antigravity.is_some() {
            match (input.runtime_mode.as_str(), kind) {
                (_, "read" | "search" | "fetch") => Some("accept"),
                ("full-access", _) => Some("accept"),
                ("auto-accept-edits", "edit" | "delete" | "move") => Some("accept"),
                _ => None,
            }
        } else {
            // t3code's ACP policy (`acpPermissionDisposition`), as each flavor adjusts it.
            (crate::acp_cli::flavor_disposition(self.cli, &input.runtime_mode, kind, &options) == crate::acp_cli::Disposition::Allow).then_some("accept")
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
        let offered = match self.cli {
            _ if self.antigravity.is_some() => Some(crate::antigravity::acp::approval_options(params)),
            Some(crate::acp_cli::Kind::Grok) => Some(crate::acp_cli::grok::approval_options(params)),
            Some(crate::acp_cli::Kind::Kiro) => Some(crate::acp_cli::kiro::approval_options(params)),
            _ => None,
        };
        let session_offered = offered.as_ref().is_none_or(|o| o.iter().any(|o| o.decision == domovoi_model::typed::ApprovalDecision::AcceptForSession));
        let rid = turn.open_request_with(&native, &item, request_kind, &prompt, offered);
        state.asked_kinds.insert(rid.clone(), (kind.to_owned(), session_offered));
        state.asked.insert(rid, (rpc_id, options));
    }
}

/// Grok's `session/load`, which can stay unanswered once its replay ends
/// (t3code `sessionLoadReplayIdleReadiness`): loaded when it answers, or a
/// second after the replay's last update; given up after a minute.
async fn replay_settled(load: impl std::future::Future<Output = Result<Value, crate::jsonrpc::RpcError>>, events: &mut mpsc::UnboundedReceiver<Incoming>) -> Result<Value, crate::jsonrpc::RpcError> {
    tokio::pin!(load);
    let give_up = tokio::time::sleep(std::time::Duration::from_secs(60));
    tokio::pin!(give_up);
    let idle = tokio::time::sleep(std::time::Duration::MAX / 4);
    tokio::pin!(idle);
    let mut replayed = false;
    loop {
        tokio::select! {
            answer = &mut load => return answer,
            ev = events.recv() => match ev {
                None | Some(Incoming::Closed) => return Err(crate::jsonrpc::RpcError::Closed),
                Some(_) => {
                    replayed = true;
                    idle.as_mut().reset(tokio::time::Instant::now() + std::time::Duration::from_secs(1));
                }
            },
            () = &mut idle, if replayed => return Ok(json!({})),
            () = &mut give_up => return Err(crate::jsonrpc::RpcError::Closed),
        }
    }
}

/// `text` cut to `max` characters.
fn bounded(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

/// An `http(s)` URL with a host, as the agent wrote it (t3code `normalizeAcpRegistryWebUrl`).
fn web_url(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url.trim()).ok()?;
    (matches!(parsed.scheme(), "http" | "https") && parsed.host_str().is_some_and(|h| !h.is_empty()) && url.len() <= 2048).then(|| parsed.to_string())
}

/// URL sign-ins ACP agents asked for (t3code `AcpRegistryRuntimeCoordinator`
/// `requestUrlAuthentication`): one per instance at a time, shown as its
/// entry's `auth.action` for 10 minutes, a newer one replacing it.
pub mod url_auth {
    use domovoi_model::typed::AcpRegistryUrlAuthAction;
    use std::collections::BTreeMap;

    pub const TTL: std::time::Duration = std::time::Duration::from_secs(10 * 60);

    struct Pending {
        action: AcpRegistryUrlAuthAction,
        consent: tokio::sync::oneshot::Sender<bool>,
        expires: std::time::Instant,
    }

    static PENDING: std::sync::Mutex<BTreeMap<String, Pending>> = std::sync::Mutex::new(BTreeMap::new());

    /// Shows the sign-in; the answer is whether the user opened it.
    pub fn request(instance: &str, elicitation_id: String, url: String, message: String) -> tokio::sync::oneshot::Receiver<bool> {
        let (consent, answer) = tokio::sync::oneshot::channel();
        let now = domovoi_contracts::now_millis();
        let ttl = i64::try_from(TTL.as_millis()).unwrap_or(i64::MAX);
        let action = AcpRegistryUrlAuthAction {
            elicitation_id,
            url,
            message,
            created_at: Some(domovoi_contracts::time_fmt::iso_from_millis(now)),
            expires_at: Some(domovoi_contracts::time_fmt::iso_from_millis(now.saturating_add(ttl))),
        };
        let previous = crate::util::locked(&PENDING).insert(instance.to_owned(), Pending { action, consent, expires: std::time::Instant::now() + TTL });
        if let Some(p) = previous {
            let _ = p.consent.send(false);
        }
        crate::providers::CLAUDE_COMMANDS_CHANGED.notify_one();
        answer
    }

    /// The sign-in shown for `instance`, if one is.
    pub fn action(instance: &str) -> Option<AcpRegistryUrlAuthAction> {
        let mut pending = crate::util::locked(&PENDING);
        if pending.get(instance).is_some_and(|p| p.expires <= std::time::Instant::now() || p.consent.is_closed()) {
            pending.remove(instance);
        }
        pending.get(instance).map(|p| p.action.clone())
    }

    /// The user opened the sign-in: `false` for another or an expired one.
    pub fn accept(instance: &str, elicitation_id: &str) -> bool {
        let taken = {
            let mut pending = crate::util::locked(&PENDING);
            match pending.get(instance) {
                Some(p) if p.action.elicitation_id == elicitation_id => pending.remove(instance),
                _ => None,
            }
        };
        let Some(p) = taken else { return false };
        crate::providers::CLAUDE_COMMANDS_CHANGED.notify_one();
        p.expires > std::time::Instant::now() && p.consent.send(true).is_ok()
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

/// A choice option of the thread's model selection.
fn choice(input: &TurnInput, id: &str) -> Option<String> {
    input.model_selection.options.iter().flatten().find(|o| o.id.as_str() == id).and_then(|o| match &o.value {
        OptionValue::Choice(v) => Some(v.clone()),
        _ => None,
    })
}

/// The values a select config option offers, its groups' too.
fn select_values(option: &Value) -> Vec<String> {
    option["options"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|o| match o["value"].as_str() {
            Some(v) => vec![v.to_owned()],
            None => o["options"].as_array().into_iter().flatten().filter_map(|c| c["value"].as_str().map(str::to_owned)).collect(),
        })
        .collect()
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
    /// When its transcript first said it was done while its message to the
    /// parent was not in the session DB yet (see [`REPLY_GRACE`]).
    done_unreplied: Option<std::time::Instant>,
}

/// How long a child whose transcript is done is waited on for its message to
/// the parent, its result: the two are written separately, and the
/// transcript can come first.
const REPLY_GRACE: std::time::Duration = std::time::Duration::from_secs(3);

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
    /// Grok's questions (`ask_user_question`), the same way.
    xai_questions: HashMap<String, (Value, Value)>,
    /// What each permission asked is for: its tool kind, and whether "for the session" was offered.
    asked_kinds: HashMap<String, (String, bool)>,
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
    /// Answers what the agent asked and no one answered as cancelled.
    async fn cancel_requests(&mut self, rpc: &RpcClient, turn: &mut Turn) {
        for (id, (rpc_id, _)) in std::mem::take(&mut self.asked).into_iter().chain(std::mem::take(&mut self.questions)) {
            let _ = rpc.respond(rpc_id, json!({ "outcome": { "outcome": "cancelled" } })).await;
            turn.resolve_request(&id, "cancel", None);
        }
        for (id, (rpc_id, _)) in std::mem::take(&mut self.xai_questions) {
            let _ = rpc.respond(rpc_id, json!({ "outcome": "cancelled" })).await;
            turn.resolve_request(&id, "cancel", None);
        }
        self.asked_kinds.clear();
    }

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
            if snapshot.status == ChildState::Completed && !snapshot.replied {
                let since = *bg.done_unreplied.get_or_insert_with(std::time::Instant::now);
                if since.elapsed() < REPLY_GRACE {
                    continue;
                }
            }

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
                                    done_unreplied: None,
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
                                done_unreplied: None,
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
    fn url_sign_ins_are_shown_until_opened_or_replaced() {
        assert_eq!(web_url(" https://example.com/login?x=1 ").as_deref(), Some("https://example.com/login?x=1"));
        assert_eq!(web_url("javascript:alert(1)"), None);
        assert_eq!(web_url("file:///etc/passwd"), None);
        let first = url_auth::request("i-url", "e1".into(), "https://a.example/1".into(), "Sign in".into());
        assert_eq!(url_auth::action("i-url").map(|a| a.elicitation_id), Some("e1".into()));
        let mut second = url_auth::request("i-url", "e2".into(), "https://a.example/2".into(), "Sign in".into());
        assert_eq!(first.blocking_recv(), Ok(false), "a newer sign-in declines the older one");
        assert!(!url_auth::accept("i-url", "e1"), "not the one shown");
        assert!(url_auth::accept("i-url", "e2"));
        assert_eq!(second.try_recv(), Ok(true));
        assert!(url_auth::action("i-url").is_none(), "gone once opened");
        assert!(!url_auth::accept("i-url", "e2"), "only once");
    }

    #[test]
    fn steers_wait_for_the_agent_to_read_them() {
        let mut waits = SteerWaits::default();
        let (tx, mut rx) = tokio::sync::oneshot::channel();
        waits.queued("m1".into(), tx);
        assert!(rx.try_recv().is_err(), "queued, not read yet");
        waits.injected("m1");
        assert_eq!(rx.try_recv(), Ok(Ok(())));
        // Read before its send was answered.
        waits.injected("m2");
        let (tx, mut rx) = tokio::sync::oneshot::channel();
        waits.queued("m2".into(), tx);
        assert_eq!(rx.try_recv(), Ok(Ok(())));
        let (tx, mut rx) = tokio::sync::oneshot::channel();
        waits.queued("m3".into(), tx);
        waits.fail_all();
        assert!(matches!(rx.try_recv(), Ok(Err(_))), "the turn ended first");
    }

    #[test]
    fn registry_agents_are_offered_url_sign_ins_and_devin_terminals() {
        let plain = registry_initialize_params(false);
        assert_eq!(plain["clientCapabilities"]["elicitation"], json!({ "url": {} }));
        assert_eq!(plain["clientCapabilities"]["terminal"], false);
        assert_eq!(plain["clientCapabilities"]["fs"], json!({ "readTextFile": false, "writeTextFile": false }));
        assert_eq!(registry_initialize_params(true)["clientCapabilities"]["terminal"], true);
        assert_eq!(select_values(&json!({ "options": [{ "value": "a" }, { "group": "g", "options": [{ "value": "b" }] }] })), ["a", "b"]);
    }

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
