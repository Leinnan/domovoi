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

#![expect(clippy::disallowed_types, clippy::disallowed_macros, reason = "the Agent Client Protocol's JSON-RPC messages, read and written as ACP agents send them")]

use super::turn::{Plan, Turn, TurnEnd, WindowUse, Work};
use super::{Control, TurnInput};
use crate::jsonrpc::{Incoming, RpcClient};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{Mutex, mpsc};
use domovoi_model::typed::{FileChangeDetail, PlanStep, PlanStepStatus, ProviderRequestKind, ProviderSessionStatus, ProviderThreadStatus, TurnItemStatus};

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
    broken: AtomicBool,
    /// The instructions about Domovoi's tools went with a prompt already.
    instructed: AtomicBool,
    /// The mode plan mode replaced, to go back to.
    mode_before_plan: std::sync::Mutex<Option<String>>,
}

impl AcpSession {
    pub async fn start(input: &TurnInput, native_session: Option<String>, log_dir: std::path::PathBuf) -> Result<Arc<Self>, String> {
        let instance = input.model_selection.instance_id.as_str().to_owned();
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
            broken: AtomicBool::new(false),
            instructed: AtomicBool::new(false),
            mode_before_plan: std::sync::Mutex::new(None),
        }))
    }

    pub fn usable(&self) -> bool {
        !self.broken.load(Ordering::SeqCst)
    }

    pub async fn kill(&self) {
        self.rpc.kill().await;
    }

    /// The model and mode the turn wants, set on the session.
    async fn apply_selection(&self, input: &TurnInput) {
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
            let Some(before) = before else { return };
            if self.rpc.request("session/set_mode", json!({ "sessionId": self.session_id, "modeId": before })).await.is_ok() {
                crate::util::locked(&self.session)["modes"]["currentModeId"] = json!(before);
            }
        }
    }

    /// The prompt's content blocks: images the agent takes, then the text
    /// (with the instructions about Domovoi's tools on the session's first).
    fn prompt(&self, input: &TurnInput) -> Vec<Value> {
        use base64::Engine as _;
        let mut blocks = vec![];
        if self.prompt_caps["image"] == true {
            for (mime, path) in &input.images {
                if let Ok(bytes) = std::fs::read(path) {
                    blocks.push(json!({ "type": "image", "mimeType": mime, "data": base64::engine::general_purpose::STANDARD.encode(bytes) }));
                }
            }
        }
        let leading_command = input.text.trim_start().starts_with('/');
        let text = if crate::mcp::session_for(&input.thread_id).is_some() && !leading_command && !self.instructed.swap(true, Ordering::SeqCst) {
            format!("<t3_code_instructions>\n{}\n</t3_code_instructions>\n\n<user_request>\n{}\n</user_request>", crate::agent_instructions::system_prompt(), input.text)
        } else {
            input.text.clone()
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
        self.apply_selection(&input).await;
        turn.turn_started(&format!("pending:{}", crate::util::new_uuid()));
        let prompt = self.prompt(&input);
        let rpc = self.rpc.clone();
        let session_id = self.session_id.clone();
        let (done_tx, mut done) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let _ = done_tx.send(rpc.request("session/prompt", json!({ "sessionId": session_id, "prompt": prompt })).await);
        });
        let mut state = TurnState::default();
        let mut interrupted = false;
        let (end, error) = loop {
            tokio::select! {
                answer = &mut done => {
                    // The reader passed on what came before the answer first: take it in.
                    while let Ok(ev) = events.try_recv() {
                        if let Incoming::Notification { method, params } = ev
                            && method == "session/update"
                        {
                            state.update(&mut turn, &params["update"]);
                        }
                    }
                    break match answer {
                        Ok(Ok(r)) => match r["stopReason"].as_str() {
                            Some("cancelled") => (TurnEnd::Interrupted, None),
                            Some("refusal") => (TurnEnd::Failed, Some("The agent refused to go on.".to_owned())),
                            _ if interrupted => (TurnEnd::Interrupted, None),
                            _ => (TurnEnd::Completed, None),
                        },
                        Ok(Err(_)) if interrupted => (TurnEnd::Interrupted, None),
                        Ok(Err(e)) => (TurnEnd::Failed, Some(format!("The ACP agent failed the turn: {e}"))),
                        Err(_) => (TurnEnd::Failed, Some("The ACP agent stopped answering.".to_owned())),
                    };
                }
                Some(c) = controls.recv() => match c {
                    Control::Respond { request_id, decision, .. } => {
                        if let Some((rpc_id, options)) = state.asked.remove(&request_id) {
                            let _ = self.rpc.respond(rpc_id, permission_outcome(&options, &decision)).await;
                            turn.resolve_request(&request_id, &decision, None);
                        }
                    }
                    Control::Interrupt => {
                        interrupted = true;
                        let _ = self.rpc.notify("session/cancel", Some(json!({ "sessionId": self.session_id }))).await;
                        // A cancelled turn answers what it asked as cancelled (ACP).
                        for (id, (rpc_id, _)) in std::mem::take(&mut state.asked) {
                            let _ = self.rpc.respond(rpc_id, json!({ "outcome": { "outcome": "cancelled" } })).await;
                            turn.resolve_request(&id, "cancel", None);
                        }
                    }
                    Control::Steer { reply, .. } => {
                        let _ = reply.send(Err("ACP agents take more input once the turn ends.".into()));
                    }
                    Control::StopSubagent { reply, .. } => {
                        let _ = reply.send(Err("ACP agents' subagents can't be stopped one by one.".into()));
                    }
                },
                ev = events.recv() => match ev {
                    None | Some(Incoming::Closed) => {
                        self.broken.store(true, Ordering::SeqCst);
                        break if interrupted { (TurnEnd::Interrupted, None) } else { (TurnEnd::Failed, Some("The ACP agent exited.".to_owned())) };
                    }
                    Some(Incoming::Notification { method, params }) if method == "session/update" => {
                        state.update(&mut turn, &params["update"]);
                    }
                    Some(Incoming::Notification { .. }) => {}
                    Some(Incoming::Request { id, method, params }) if method == "session/request_permission" => {
                        self.ask(&mut turn, &mut state, id, &params, &input).await;
                    }
                    Some(Incoming::Request { id, method, .. }) => {
                        let _ = self.rpc.respond_error(id, -32601, &format!("domovoi-server does not offer {method}")).await;
                    }
                },
            }
        };
        drop(events);
        for (id, (rpc_id, _)) in std::mem::take(&mut state.asked) {
            let _ = self.rpc.respond(rpc_id, json!({ "outcome": { "outcome": "cancelled" } })).await;
            turn.resolve_request(&id, "cancel", None);
        }
        state.finish(&mut turn);
        crate::providers::remember_acp_commands(&self.instance, std::mem::take(&mut state.commands));
        turn.complete(end, error.as_deref());
        (turn, self.usable())
    }

    /// A permission request: decided by the runtime mode, or asked.
    async fn ask(&self, turn: &mut Turn, state: &mut TurnState, rpc_id: Value, params: &Value, input: &TurnInput) {
        let call = &params["toolCall"];
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
        let item = call["toolCallId"].as_str().unwrap_or_default().to_owned();
        let prompt = call["title"].as_str().filter(|t| !t.is_empty()).map_or_else(|| "Allow this?".to_owned(), str::to_owned);
        let native = format!("permission:{item}:{}", crate::util::new_uuid());
        let rid = turn.open_request(&native, &item, request_kind, &prompt);
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
    /// Permissions asked, by Domovoi request id: the JSON-RPC id and the options.
    asked: HashMap<String, (Value, Value)>,
    /// The agent's slash commands, when it said.
    commands: Option<Vec<(String, Option<String>)>>,
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
            turn.assistant(&id, &text, false, None);
        }
    }

    fn close_thought(&mut self, turn: &mut Turn) {
        if let Some((id, text)) = self.thought.take() {
            turn.item(&id, Work::Reasoning { text, streaming: false }, TurnItemStatus::Completed);
        }
    }

    fn update(&mut self, turn: &mut Turn, u: &Value) {
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
                if let Some(k) = known["kind"].as_str() {
                    self.kinds.insert(id.clone(), k.to_owned());
                }
                let status = match known["status"].as_str() {
                    Some("completed") => TurnItemStatus::Completed,
                    Some("failed") => TurnItemStatus::Failed,
                    _ => TurnItemStatus::Running,
                };
                turn.item(&id, tool_work(&known), status);
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
            Work::FileChange { file_name, changes, texts, patch: None }
        }
        "search" | "fetch" if call["rawInput"]["query"].is_string() => Work::WebSearch { patterns: vec![call["rawInput"]["query"].as_str().unwrap_or_default().to_owned()] },
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
