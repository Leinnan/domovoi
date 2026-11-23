//! Codex adapter over `codex app-server` (JSON-RPC on stdio). One app-server
//! process serves every thread; notifications are routed by native thread id.

#![expect(clippy::disallowed_types, clippy::disallowed_macros, reason = "Codex app-server JSON-RPC: params, items, server requests and the replies to them")]

use super::turn::{Plan, Turn, TurnEnd, WindowUse, Work, approval_kind, decision_allows};
use super::{Control, TurnInput};
use crate::jsonrpc::{Incoming, RpcClient};
use crate::orchestration::commands::Answers;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use domovoi_model::typed::{
    FileChangeDetail, ModelSelection, PlanStep, PlanStepStatus, ProviderSessionStatus, ProviderThreadStatus, QuestionOption, TurnItemStatus,
    UserInputQuestion,
};
use tokio::sync::{Mutex, mpsc};

pub struct CodexServer {
    rpc: Arc<RpcClient>,
    routes: Arc<Mutex<HashMap<String, mpsc::UnboundedSender<Incoming>>>>,
    loaded: Mutex<HashSet<String>>,
}

/// Approval and sandbox policy per t3code runtime mode (as recorded).
pub fn policies(runtime_mode: &str) -> (Value, Value) {
    match runtime_mode {
        "approval-required" => (json!("untrusted"), json!({ "type": "readOnly" })),
        "auto-accept-edits" | "auto" => (json!("on-request"), json!({ "type": "workspaceWrite" })),
        _ => (json!("never"), json!({ "type": "dangerFullAccess" })),
    }
}

fn effort(model_selection: &ModelSelection) -> Option<String> {
    super::claude::chosen(model_selection, &["reasoningEffort", "effort"]).map(str::to_owned)
}

impl CodexServer {
    pub async fn start(log_dir: std::path::PathBuf) -> Result<Arc<Self>, String> {
        let log_path = log_dir.join("codex-app-server.ndjson");
        let log = crate::logging::protocol_log(log_path);
        let codex = crate::providers::executable(crate::providers::CODEX_TOOL)?;
        let (rpc, mut rx) = RpcClient::spawn(&codex.to_string_lossy(), &["app-server"], None, log).map_err(|e| format!("could not start codex app-server: {e}"))?;
        let routes: Arc<Mutex<HashMap<String, mpsc::UnboundedSender<Incoming>>>> = Arc::default();
        let r = routes.clone();
        let router_rpc = rpc.clone();
        tokio::spawn(async move {
            while let Some(msg) = rx.recv().await {
                // The account's limits: no thread's, so no turn hears them.
                if let Incoming::Notification { method, params } = &msg
                    && method == "account/rateLimits/updated"
                {
                    crate::usage_limits::merge(crate::providers::CODEX, crate::usage_limits::codex_windows(&params["rateLimits"]));
                    continue;
                }
                let thread = match &msg {
                    Incoming::Notification { params, .. } | Incoming::Request { params, .. } => {
                        params["threadId"].as_str().or(params["conversationId"].as_str()).map(str::to_owned)
                    }
                    Incoming::Closed => None,
                };
                let routed = {
                    let routes = r.lock().await;
                    match (&msg, &thread) {
                        (Incoming::Closed, _) => {
                            for tx in routes.values() {
                                let _ = tx.send(Incoming::Closed);
                            }
                            true
                        }
                        (_, Some(t)) => routes.get(t).is_some_and(|tx| tx.send(msg.clone()).is_ok()),
                        _ => false,
                    }
                };
                // A request no turn is listening for would block Codex for ever.
                if let (false, Incoming::Request { id, method, .. }) = (routed, &msg) {
                    let _ = router_rpc.respond_error(id.clone(), -32601, &format!("domovoi-server has no active turn for {method}")).await;
                }
            }
        });
        rpc.request("initialize", json!({
            "capabilities": { "experimentalApi": true },
            "clientInfo": { "name": "T3 Code", "title": "T3 Code", "version": domovoi_contracts::environment::SERVER_VERSION },
        })).await.map_err(|e| format!("codex initialize failed: {e}"))?;
        rpc.notify("initialized", None).await.map_err(|e| e.to_string())?;
        let server = Arc::new(CodexServer { rpc, routes, loaded: Mutex::new(HashSet::new()) });
        // The account's limits now, before a turn reports any.
        let s = server.clone();
        tokio::spawn(async move { s.read_limits().await });
        Ok(server)
    }

    /// Reads the account's limits (`account/rateLimits/read`).
    pub async fn read_limits(&self) {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(10), crate::providers::codex::read_limits(&self.rpc)).await;
    }

    /// `feedback/upload` for a native thread; returns the feedback id.
    pub async fn upload_feedback(&self, native_thread_id: &str, reason: Option<&str>) -> Result<String, String> {
        let mut params = json!({ "classification": "bug", "includeLogs": true, "threadId": native_thread_id });
        if let Some(r) = reason {
            params["reason"] = json!(r);
        }
        let res = self.rpc.request("feedback/upload", params).await.map_err(|e| e.to_string())?;
        Ok(res["threadId"].as_str().unwrap_or(native_thread_id).to_owned())
    }

    pub async fn revert(&self, native_thread_id: &str, before_native_turn: &str) -> Result<(), String> {
        self.rpc.request("thread/revert", json!({ "threadId": native_thread_id, "beforeTurnId": before_native_turn }))
            .await.map(|_| ()).map_err(|e| e.to_string())
    }

    /// Runs one turn to completion, recording it on `turn`.
    pub async fn run_turn(&self, mut turn: Turn, input: TurnInput, mut controls: mpsc::UnboundedReceiver<Control>) -> Turn {
        let model = input.model_selection.model.clone();
        turn.session(ProviderSessionStatus::Ready, &input.cwd, &model, None);
        let native = match turn.native_thread_id() {
            Some(id) => {
                if !self.loaded.lock().await.contains(&id) {
                    if let Err(e) = self.rpc.request("thread/resume", with_mcp(json!({ "threadId": id, "cwd": input.cwd, "model": model, "excludeTurns": true }), &input.thread_id)).await {
                        turn.complete(TurnEnd::Failed, Some(&format!("Could not resume the Codex thread: {e}")));
                        return turn;
                    }
                }
                id
            }
            None if input.fork.is_some() => {
                let (source, last_turn) = input.fork.clone().unwrap_or_default();
                let mut params = json!({ "threadId": source, "excludeTurns": true, "cwd": input.cwd, "model": model });
                if let Some(t) = last_turn {
                    params["lastTurnId"] = json!(t);
                }
                match self.rpc.request("thread/fork", with_mcp(params, &input.thread_id)).await {
                    Ok(r) => r["thread"]["id"].as_str().unwrap_or_default().to_owned(),
                    Err(e) => {
                        turn.complete(TurnEnd::Failed, Some(&format!("Could not fork the Codex thread: {e}")));
                        return turn;
                    }
                }
            }
            None => match self.rpc.request("thread/start", with_mcp(json!({ "cwd": input.cwd, "model": model }), &input.thread_id)).await {
                Ok(r) => r["thread"]["id"].as_str().unwrap_or_default().to_owned(),
                Err(e) => {
                    turn.complete(TurnEnd::Failed, Some(&format!("Could not start a Codex thread: {e}")));
                    return turn;
                }
            },
        };
        self.loaded.lock().await.insert(native.clone());
        #[expect(clippy::disallowed_methods, reason = "filled by the router every thread shares: one slow turn must not hold up the others")]
        let (tx, mut events) = mpsc::unbounded_channel();
        self.routes.lock().await.insert(native.clone(), tx);
        turn.thread_status(ProviderThreadStatus::Active, Some(&native));

        let (approval, sandbox) = policies(&input.runtime_mode);
        let mut items: Vec<Value> = input.images.iter().map(|(_, path)| json!({ "type": "localImage", "path": path })).collect();
        items.push(json!({ "type": "text", "text": input.text }));
        let mut params = json!({
            "threadId": native, "input": items, "cwd": input.cwd, "model": model,
            "approvalPolicy": approval, "approvalsReviewer": "user", "sandboxPolicy": sandbox, "summary": "auto",
        });
        let reasoning = effort(&input.model_selection);
        if let Some(e) = &reasoning {
            params["effort"] = json!(e);
        }
        // Plan first: Codex's plan collaboration mode.
        let mut settings = json!({ "model": model });
        if let Some(e) = &reasoning {
            settings["reasoning_effort"] = json!(e);
        }
        params["collaborationMode"] = json!({ "mode": if input.interaction_mode == "plan" { "plan" } else { "default" }, "settings": settings });
        // What Domovoi's tools are for, while the thread's agent has them.
        if crate::mcp::session_for(&input.thread_id).is_some() {
            let mut context = serde_json::Map::new();
            for (i, section) in crate::agent_instructions::SECTIONS.iter().enumerate() {
                context.insert(format!("t3_code_orchestration_{:02}", i + 1), json!({ "kind": "application", "value": section }));
            }
            params["additionalContext"] = Value::Object(context);
        }
        let native_turn = match self.rpc.request("turn/start", params).await {
            Ok(r) => r["turn"]["id"].as_str().unwrap_or_default().to_owned(),
            Err(e) => {
                self.routes.lock().await.remove(&native);
                turn.complete(TurnEnd::Failed, Some(&format!("Codex rejected the turn: {e}")));
                return turn;
            }
        };
        turn.turn_started(&native_turn);

        let mut texts: HashMap<String, String> = HashMap::new();
        // Running commands: their command line and the output streamed so far.
        let mut outputs: HashMap<String, (Value, String)> = HashMap::new();
        // t3code request id -> the pending server request.
        let mut pending: HashMap<String, Pending> = HashMap::new();
        let mut subs = Collab::default();
        loop {
            tokio::select! {
                Some(c) = controls.recv() => match c {
                    Control::Respond { request_id, decision, answers } => {
                        if let Some(p) = pending.remove(&request_id) {
                            let _ = self.rpc.respond(p.id.clone(), p.reply(&decision, answers.as_ref())).await;
                            turn.resolve_request(&request_id, &decision, answers.as_ref());
                        }
                    }
                    Control::Interrupt => {
                        let _ = self.rpc.request("turn/interrupt", json!({ "threadId": native, "turnId": native_turn })).await;
                    }
                    Control::StopSubagent { reply, .. } => {
                        let _ = reply.send(Err("Codex subagents can't be stopped one by one; stop the turn instead.".into()));
                    }
                    Control::Steer { text, images, reply } => {
                        let mut input: Vec<Value> = images.iter().map(|(_, path)| json!({ "type": "localImage", "path": path })).collect();
                        input.push(json!({ "type": "text", "text": text }));
                        let res = self.rpc.request("turn/steer", json!({ "threadId": native, "expectedTurnId": native_turn, "input": input })).await;
                        let _ = reply.send(res.map(|_| ()).map_err(|e| e.to_string()));
                    }
                },
                Some(msg) = events.recv() => match msg {
                    Incoming::Notification { method, params } => match method.as_str() {
                        "item/started" | "item/completed" => {
                            let item = &params["item"];
                            let id = item["id"].as_str().unwrap_or_default().to_owned();
                            if item["type"] == "commandExecution" {
                                if method == "item/started" {
                                    outputs.insert(id.clone(), (item["command"].clone(), String::new()));
                                } else if let Some((_, streamed)) = outputs.remove(&id)
                                    && item["aggregatedOutput"].as_str().is_none_or(str::is_empty)
                                    && !streamed.is_empty()
                                {
                                    // No aggregate on completion: keep what streamed.
                                    let mut item = item.clone();
                                    item["aggregatedOutput"] = json!(streamed);
                                    record_item(&mut turn, &item, true, &mut texts, &mut subs);
                                    continue;
                                }
                            }
                            record_item(&mut turn, item, method == "item/completed", &mut texts, &mut subs)
                        }
                        "item/commandExecution/outputDelta" => {
                            let id = params["itemId"].as_str().unwrap_or_default().to_owned();
                            let (command, output) = outputs.entry(id.clone()).or_insert_with(|| (Value::Null, String::new()));
                            push_tail(output, params["delta"].as_str().unwrap_or_default());
                            let work = Work::Command { input: command.as_str().unwrap_or_default().to_owned(), output: Some(output.clone()), exit_code: None };
                            turn.item_streaming(&id, work, output.len());
                        }
                        "item/agentMessage/delta" => {
                            let id = params["itemId"].as_str().unwrap_or_default().to_owned();
                            let text = texts.entry(id.clone()).or_default();
                            turn.assistant_delta(&id, text, params["delta"].as_str().unwrap_or_default());
                        }
                        "item/plan/delta" => {
                            let id = params["itemId"].as_str().unwrap_or_default().to_owned();
                            let text = texts.entry(id.clone()).or_default();
                            text.push_str(params["delta"].as_str().unwrap_or_default());
                            if turn.may_flush(&id, text.len()) {
                                turn.plan(&id, Plan::Proposed { markdown: text.clone() }, false);
                            }
                        }
                        "turn/plan/updated" => {
                            let steps: Vec<PlanStep> = params["plan"].as_array().into_iter().flatten().enumerate().map(|(i, step)| {
                                let fallback = format!("Step {}", i + 1);
                                let status = match step["status"].as_str() {
                                    Some("completed") => PlanStepStatus::Completed,
                                    Some("inProgress") => PlanStepStatus::Running,
                                    _ => PlanStepStatus::Pending,
                                };
                                PlanStep { id: format!("step-{}", i + 1), text: text_or(&step["step"], &fallback).to_owned(), status, duration_anchor_at: None, duration_ms: None }
                            }).collect();
                            let explanation = params["explanation"].as_str().map(str::trim).filter(|e| !e.is_empty()).map(str::to_owned);
                            turn.plan(&format!("turn-plan:{native_turn}"), Plan::Todo { steps, explanation }, true);
                        }
                        "thread/tokenUsage/updated" => {
                            if let Some(u) = codex_token_usage(&params["tokenUsage"]) {
                                turn.token_usage(u);
                            }
                        }
                        "error" if params["willRetry"] != true => {
                            if crate::usage_limits::codex_usage_error(&params["error"]["codexErrorInfo"]) {
                                turn.usage_limited(crate::usage_limits::exhausted_reset_at(crate::providers::CODEX));
                            }
                        }
                        "turn/completed" => {
                            if crate::usage_limits::codex_usage_error(&params["turn"]["error"]["codexErrorInfo"]) {
                                turn.usage_limited(crate::usage_limits::exhausted_reset_at(crate::providers::CODEX));
                            }
                            let status = match params["turn"]["status"].as_str() {
                                Some("completed") => "completed",
                                Some("interrupted") => "interrupted",
                                _ => "failed",
                            };
                            let error = params["turn"]["error"]["message"].as_str().map(str::to_owned);
                            subs.agents.end(&turn, status == "completed");
                            turn.complete(TurnEnd::named(status), error.as_deref());
                            break;
                        }
                        _ => {}
                    },
                    Incoming::Request { id, method, params } => {
                        let Some(open) = classify_request(&method, &params) else {
                            let _ = self.rpc.respond_error(id, -32601, &format!("domovoi-server does not handle {method}")).await;
                            continue;
                        };
                        let item_id = params["itemId"].as_str().or(params["callId"].as_str()).unwrap_or_default().to_owned();
                        let native_req = format!("{item_id}:{id}");
                        let rid = match &open {
                            Open::Question(questions) => turn.open_user_input(&native_req, &item_id, questions.clone()),
                            Open::Approval { kind, prompt, .. } => turn.open_request(&native_req, &item_id, approval_kind(kind), prompt),
                        };
                        let reply = match open {
                            Open::Question(_) => Reply::Answers,
                            Open::Approval { reply, .. } => reply,
                        };
                        pending.insert(rid, Pending { id, reply });
                    }
                    Incoming::Closed => {
                        turn.complete(TurnEnd::Failed, Some("The Codex app-server exited."));
                        self.loaded.lock().await.clear();
                        break;
                    }
                },
                else => break,
            }
        }
        self.routes.lock().await.remove(&native);
        turn
    }
}

/// Thread params with the agent MCP server in their `config`.
fn with_mcp(mut params: Value, thread_id: &str) -> Value {
    if let Some(c) = crate::mcp::codex_config(thread_id) {
        params["config"] = c;
    }
    params
}

/// Codex's `tokenUsage` (from `thread/tokenUsage/updated`) as the context
/// use t3code shows: the last model call's tokens against the window.
fn codex_token_usage(u: &Value) -> Option<WindowUse> {
    let last = &u["last"];
    let n = |k: &str| last[k].as_i64().map(|v| v.max(0) as u64);
    Some(WindowUse {
        used_tokens: n("totalTokens")?,
        max_tokens: u["modelContextWindow"].as_u64(),
        input_tokens: n("inputTokens").unwrap_or(0),
        cached_input_tokens: n("cachedInputTokens").unwrap_or(0),
        output_tokens: n("outputTokens").unwrap_or(0),
        reasoning_output_tokens: n("reasoningOutputTokens").unwrap_or(0),
    })
}

/// A server request waiting on the user, and how to answer it.
struct Pending {
    id: Value,
    reply: Reply,
}

/// The answer shape each kind of Codex server request takes.
enum Reply {
    /// `{decision}` (v2 command and file-change approvals).
    Decision,
    /// `{decision: ReviewDecision}` (the legacy exec / patch approvals).
    ReviewDecision,
    /// `{permissions, scope}`: grant what was asked, or nothing.
    Permissions(Value),
    /// `{action, content?}` for an MCP server's form or URL request.
    Elicitation(Value),
    /// `{answers: {questionId: {answers: [..]}}}`.
    Answers,
}

/// What a request opens on the turn.
enum Open {
    Question(Vec<UserInputQuestion>),
    Approval { kind: &'static str, prompt: String, reply: Reply },
}

fn text_or<'a>(v: &'a Value, fallback: &'a str) -> &'a str {
    v.as_str().map(str::trim).filter(|t| !t.is_empty()).unwrap_or(fallback)
}

/// Sorts a Codex server request into a question or an approval; `None` for
/// methods Domovoi can't answer (they get a JSON-RPC error).
fn classify_request(method: &str, params: &Value) -> Option<Open> {
    let approval = |kind, prompt: String, reply| Some(Open::Approval { kind, prompt, reply });
    match method {
        "item/tool/requestUserInput" => Some(Open::Question(user_input_questions(&params["questions"]))),
        "item/commandExecution/requestApproval" => {
            let command = params["command"].as_str().map(str::to_owned).unwrap_or_else(|| text_or(&params["reason"], "Run a command").to_owned());
            approval("command", command, Reply::Decision)
        }
        "item/fileChange/requestApproval" => approval("file-change", text_or(&params["reason"], "Edit files").to_owned(), Reply::Decision),
        "item/permissions/requestApproval" => {
            let p = &params["permissions"];
            let asks = |access: &str| {
                p["fileSystem"][access].as_array().is_some_and(|a| !a.is_empty())
                    || p["fileSystem"]["entries"].as_array().is_some_and(|e| e.iter().any(|x| x["access"] == access))
            };
            let kind = if asks("write") { "file-change" } else if asks("read") { "file-read" } else { "command" };
            approval(kind, text_or(&params["reason"], "Grant additional permissions").to_owned(), Reply::Permissions(p.clone()))
        }
        "mcpServer/elicitation/request" => {
            let server = text_or(&params["serverName"], "An MCP server");
            let prompt = format!("{server}: {}", text_or(&params["message"], "asks for your approval"));
            approval("mcp-elicitation", prompt, Reply::Elicitation(params.clone()))
        }
        "execCommandApproval" => {
            let command = params["command"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(" ")).unwrap_or_default();
            approval("command", if command.is_empty() { "Run a command".into() } else { command }, Reply::ReviewDecision)
        }
        "applyPatchApproval" => approval("file-change", text_or(&params["reason"], "Edit files").to_owned(), Reply::ReviewDecision),
        _ => None,
    }
}

/// Codex's `requestUserInput` questions as t3code user-input questions.
/// Ids stay Codex's own, so answers map straight back.
fn user_input_questions(questions: &Value) -> Vec<UserInputQuestion> {
    questions.as_array().into_iter().flatten().enumerate().map(|(i, q)| {
        let fallback_id = format!("question-{}", i + 1);
        let options = q["options"].as_array().into_iter().flatten().enumerate().map(|(j, o)| {
            let fallback = format!("Option {}", j + 1);
            let label = text_or(&o["label"], &fallback).to_owned();
            QuestionOption { description: text_or(&o["description"], &label).to_owned(), label, value: None, preview: None }
        }).collect();
        UserInputQuestion {
            id: text_or(&q["id"], &fallback_id).to_owned(),
            header: text_or(&q["header"], "Question").to_owned(),
            question: text_or(&q["question"], "Choose an answer.").to_owned(),
            options,
            multi_select: Some(false),
            allow_custom_answer: None,
            initial_answer: None,
            required: None,
        }
    }).collect()
}

/// One answer as the strings Codex takes (several choices stay separate).
fn answer_strings(v: &Value) -> Vec<String> {
    match v {
        Value::Null => vec![],
        Value::String(s) => vec![s.clone()],
        Value::Array(a) => a.iter().map(|x| x.as_str().map(str::to_owned).unwrap_or_else(|| x.to_string())).collect(),
        other => vec![other.to_string()],
    }
}

impl Pending {
    fn reply(&self, decision: &str, answers: Option<&Answers>) -> Value {
        let allows = decision_allows(decision);
        let for_session = matches!(decision, "acceptForSession" | "acceptAlways");
        match &self.reply {
            Reply::Decision => {
                let d = if for_session { "acceptForSession" } else if allows { "accept" } else if decision == "cancel" { "cancel" } else { "decline" };
                json!({ "decision": d })
            }
            Reply::ReviewDecision => {
                let d = if for_session {
                    json!("approved_for_session")
                } else if allows {
                    json!("approved")
                } else if decision == "cancel" {
                    json!("abort")
                } else {
                    json!({ "denied": { "rejection": "User declined the request." } })
                };
                json!({ "decision": d })
            }
            Reply::Permissions(p) if allows => json!({ "permissions": p, "scope": if for_session { "session" } else { "turn" } }),
            Reply::Permissions(_) => json!({ "permissions": {}, "scope": "turn" }),
            Reply::Elicitation(params) => elicitation_reply(params, decision),
            Reply::Answers => {
                let map: serde_json::Map<String, Value> = answers.into_iter().flatten()
                    .map(|(q, a)| (q.clone(), json!({ "answers": answer_strings(a) }))).collect();
                json!({ "answers": map })
            }
        }
    }
}

/// An MCP elicitation answer: accepting fills the form's defaults (and the
/// persistence choice); a form with a required field Domovoi can't fill, or a URL
/// request (Domovoi opens no browser), is declined.
fn elicitation_reply(params: &Value, decision: &str) -> Value {
    if !decision_allows(decision) {
        return json!({ "action": if decision == "cancel" { "cancel" } else { "decline" } });
    }
    if params["mode"] == "url" {
        return json!({ "action": "decline" });
    }
    let persist = match decision { "acceptForSession" => Some("session"), "acceptAlways" => Some("always"), _ => None };
    let schema = &params["requestedSchema"];
    let mut content = serde_json::Map::new();
    for (key, field) in schema["properties"].as_object().into_iter().flatten() {
        let choices: Vec<&str> = field["enum"].as_array().map(|e| e.iter().filter_map(Value::as_str).collect())
            .or_else(|| field["oneOf"].as_array().map(|o| o.iter().filter_map(|x| x["const"].as_str()).collect()))
            .unwrap_or_default();
        let wanted = |c: &&&str| {
            let c = c.to_ascii_lowercase();
            match persist {
                Some("session") => c.contains("session"),
                Some(_) => c.contains("always") || c.contains("persist"),
                None => matches!(c.as_str(), "once" | "accept" | "approve" | "allow" | "allow_once" | "approve_once"),
            }
        };
        if let Some(c) = choices.iter().find(wanted) {
            content.insert(key.clone(), json!(c));
        } else if field["type"] == "boolean" && (key.eq_ignore_ascii_case("persist") || key.to_ascii_lowercase().contains("always")) {
            content.insert(key.clone(), json!(decision == "acceptAlways"));
        } else if !field["default"].is_null() {
            content.insert(key.clone(), field["default"].clone());
        }
    }
    if schema["required"].as_array().into_iter().flatten().filter_map(Value::as_str).any(|k| !content.contains_key(k)) {
        return json!({ "action": "decline" });
    }
    let mut reply = json!({ "action": "accept" });
    if schema.is_object() {
        reply["content"] = Value::Object(content);
    }
    if let Some(p) = persist {
        reply["_meta"] = json!({ "persist": p });
    }
    reply
}

/// The most of a running command's output kept (its end).
const LIVE_OUTPUT_MAX: usize = 64 * 1024;

/// Appends `delta`, keeping only the last `LIVE_OUTPUT_MAX` bytes (cut on a
/// line start where one is near, else a character boundary).
fn push_tail(output: &mut String, delta: &str) {
    output.push_str(delta);
    if output.len() <= LIVE_OUTPUT_MAX {
        return;
    }
    let mut cut = output.len() - LIVE_OUTPUT_MAX;
    while !output.is_char_boundary(cut) {
        cut += 1;
    }
    if let Some(nl) = output[cut..].find('\n').filter(|n| *n < 1024) {
        cut += nl + 1;
    }
    output.drain(..cut);
}

/// Codex collab agents: subagents, and which spawn started each receiving thread.
#[derive(Default)]
struct Collab {
    agents: super::subagents::Subagents,
    spawned: HashMap<String, String>,
}

/// A collab tool call: `spawnAgent` starts a subagent; any call's
/// `agentsStates` ends the subagents that finished.
fn record_collab(turn: &mut Turn, item: &Value, completed: bool, c: &mut Collab) -> bool {
    let id = item["id"].as_str().unwrap_or_default().to_owned();
    if item["tool"] == "spawnAgent" {
        let prompt = item["prompt"].as_str().unwrap_or_default();
        c.agents.start(turn, &id, prompt, None, item["model"].as_str());
        for r in item["receiverThreadIds"].as_array().into_iter().flatten().filter_map(Value::as_str) {
            c.spawned.insert(r.to_owned(), id.clone());
        }
        if completed && item["status"] == "failed" {
            c.agents.finish(turn, &id, "failed", None);
        }
    }
    for (child, state) in item["agentsStates"].as_object().into_iter().flatten() {
        let Some(owner) = c.spawned.get(child).cloned() else { continue };
        let status = match state["status"].as_str() {
            Some("completed") => "completed",
            Some("errored") => "failed",
            Some("shutdown" | "notFound") => "cancelled",
            _ => continue,
        };
        c.agents.finish(turn, &owner, status, state["message"].as_str());
    }
    item["tool"] == "spawnAgent"
}

/// A `fileChange` item's per-file diffs as one unified patch, each with a
/// `diff --git` header (Codex sends hunks without one).
fn change_patch(changes: &[Value]) -> Option<String> {
    let parts: Vec<String> = changes
        .iter()
        .filter_map(|c| {
            let diff = c["diff"].as_str().filter(|d| !d.trim().is_empty())?;
            let path = c["path"].as_str().unwrap_or("file");
            Some(if diff.starts_with("diff --git") || diff.starts_with("--- ") {
                diff.to_owned()
            } else {
                format!("diff --git a/{path} b/{path}\n--- a/{path}\n+++ b/{path}\n{diff}")
            })
        })
        .collect();
    (!parts.is_empty()).then(|| parts.join("\n"))
}

fn record_item(turn: &mut Turn, item: &Value, completed: bool, texts: &mut HashMap<String, String>, collab: &mut Collab) {
    if item["type"] == "collabAgentToolCall" && record_collab(turn, item, completed, collab) {
        return;
    }
    let id = item["id"].as_str().unwrap_or_default().to_owned();
    let status = if completed {
        match item["status"].as_str() {
            Some("failed") | Some("declined") => TurnItemStatus::Failed,
            _ => TurnItemStatus::Completed,
        }
    } else {
        TurnItemStatus::Running
    };
    match item["type"].as_str().unwrap_or_default() {
        "userMessage" => {}
        "agentMessage" => {
            let text = item["text"].as_str().filter(|t| !t.is_empty()).map(str::to_owned)
                .or_else(|| texts.get(&id).cloned()).unwrap_or_default();
            if completed || !text.is_empty() {
                turn.assistant(&id, &text, !completed, item["phase"].as_str());
            }
        }
        "commandExecution" => {
            let work = Work::Command {
                input: item["command"].as_str().unwrap_or_default().to_owned(),
                output: item["aggregatedOutput"].as_str().map(str::to_owned),
                exit_code: item["exitCode"].as_i64(),
            };
            turn.item(&id, work, status);
        }
        "fileChange" => {
            let changes = item["changes"].as_array().cloned().unwrap_or_default();
            let first = changes.first().and_then(|c| c["path"].as_str()).unwrap_or("file").to_owned();
            let file_name = std::path::Path::new(&first).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or(first.clone());
            let ops = changes
                .iter()
                .map(|c| FileChangeDetail {
                    operation: c["kind"]["type"].as_str().or(c["kind"].as_str()).unwrap_or("update").to_owned(),
                    path: c["path"].as_str().unwrap_or_default().to_owned(),
                    old_path: None,
                    file_type: None,
                    mime_type: None,
                })
                .collect();
            // The patch itself, so an approval can show what it would change.
            turn.item(&id, Work::FileChange { file_name, changes: ops, texts: None, patch: change_patch(&changes), error: None }, status);
        }
        "reasoning" => {
            let text: Vec<String> = item["summary"].as_array().map(|s| s.iter().filter_map(|x| x.as_str().map(str::to_owned).or_else(|| x["text"].as_str().map(str::to_owned))).collect()).unwrap_or_default();
            turn.item(&id, Work::Reasoning { text: text.join("\n"), streaming: !completed }, status);
        }
        "plan" => {
            let text = item["text"].as_str().filter(|t| !t.is_empty()).map(str::to_owned)
                .or_else(|| texts.get(&id).cloned()).unwrap_or_default();
            if completed || !text.is_empty() {
                turn.plan(&id, Plan::Proposed { markdown: text }, completed);
            }
        }
        "webSearch" => {
            let a = &item["action"];
            let mut patterns: Vec<String> = a["queries"].as_array().into_iter().flatten().filter_map(|q| q.as_str().map(str::to_owned)).collect();
            for query in [&a["query"], &a["url"], &item["query"]].into_iter().filter_map(Value::as_str) {
                if !query.trim().is_empty() && !patterns.iter().any(|p| p == query) {
                    patterns.push(query.to_owned());
                }
            }
            turn.item(&id, Work::WebSearch { patterns, results: None }, status);
        }
        "mcpToolCall" => {
            let name = format!("mcp__{}__{}", item["server"].as_str().unwrap_or("mcp"), item["tool"].as_str().unwrap_or("tool"));
            let output = [&item["error"], &item["result"]].into_iter().find(|v| !v.is_null()).cloned();
            let status = if completed && !item["error"].is_null() { TurnItemStatus::Failed } else { status };
            turn.item(&id, Work::Tool { name, input: item["arguments"].clone(), output, viewed_image_path: None }, status);
        }
        "imageView" => {
            let viewed_image_path = item["path"].as_str().filter(|p| !p.is_empty()).map(str::to_owned);
            turn.item(&id, Work::Tool { name: "view_image".into(), input: json!({ "path": item["path"] }), output: None, viewed_image_path }, status);
        }
        "imageGeneration" => {
            let viewed_image_path = item["savedPath"].as_str().filter(|path| !path.is_empty()).map(str::to_owned);
            let input = json!({ "savedPath": item["savedPath"], "revisedPrompt": item["revisedPrompt"] });
            turn.item(&id, Work::Tool { name: "imageGeneration".into(), input, output: None, viewed_image_path }, status);
        }
        "sleep" => {
            turn.item(&id, Work::Tool { name: "sleep".into(), input: json!({ "durationMs": item["durationMs"] }), output: None, viewed_image_path: None }, status);
        }
        "contextCompaction" => turn.item(&id, Work::Compaction { before_token_count: None }, status),
        other => {
            let name = item["tool"].as_str().or(item["name"].as_str()).unwrap_or(other).to_owned();
            turn.item(&id, Work::Tool { name, input: without_inline_data(item), output: None, viewed_image_path: None }, status);
        }
    }
}

const INLINE_DATA_MAX: usize = 64 * 1024;

/// Omit large inline base64 data while preserving ordinary tool text.
fn without_inline_data(value: &Value) -> Value {
    match value {
        Value::String(s) if s.len() > INLINE_DATA_MAX
            && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"+/-_=".contains(&b)) => Value::String(format!("[{} bytes left out]", s.len())),
        Value::Array(items) => Value::Array(items.iter().map(without_inline_data).collect()),
        Value::Object(map) => Value::Object(map.iter().map(|(k, v)| (k.clone(), without_inline_data(v))).collect()),
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pending(method: &str, params: Value) -> (Open, Pending) {
        let open = classify_request(method, &params).expect("handled");
        let reply = match &open {
            Open::Question(_) => Reply::Answers,
            Open::Approval { reply, .. } => match reply {
                Reply::Decision => Reply::Decision,
                Reply::ReviewDecision => Reply::ReviewDecision,
                Reply::Permissions(p) => Reply::Permissions(p.clone()),
                Reply::Elicitation(p) => Reply::Elicitation(p.clone()),
                Reply::Answers => Reply::Answers,
            },
        };
        (open, Pending { id: json!(1), reply })
    }

    #[test]
    fn questions_keep_codex_ids_and_answers_go_back_as_string_lists() {
        let (open, p) = pending("item/tool/requestUserInput", json!({ "itemId": "i", "questions": [
            { "id": "db", "header": "Database", "question": "Which one?", "options": [{ "label": "Postgres", "description": "" }] },
            { "id": "", "question": "" },
        ] }));
        let Open::Question(q) = open else { panic!("a question") };
        assert_eq!(q[0].id, "db");
        assert_eq!(q[0].options[0].description, "Postgres");
        assert_eq!(q[1].id, "question-2");
        assert_eq!(q[1].header, "Question");
        assert_eq!(json!(q[1]), json!({ "id": "question-2", "header": "Question", "question": "Choose an answer.", "options": [], "multiSelect": false }));
        let reply = p.reply("accept", json!({ "db": "Postgres", "question-2": ["a", "b"] }).as_object());
        assert_eq!(reply, json!({ "answers": { "db": { "answers": ["Postgres"] }, "question-2": { "answers": ["a", "b"] } } }));
    }

    #[test]
    fn permission_requests_grant_what_was_asked_or_nothing() {
        let perms = json!({ "fileSystem": { "write": ["/tmp/x"] } });
        let (open, p) = pending("item/permissions/requestApproval", json!({ "itemId": "i", "permissions": perms }));
        assert!(matches!(open, Open::Approval { kind: "file-change", .. }));
        assert_eq!(p.reply("acceptForSession", None), json!({ "permissions": perms, "scope": "session" }));
        assert_eq!(p.reply("accept", None)["scope"], "turn");
        assert_eq!(p.reply("decline", None), json!({ "permissions": {}, "scope": "turn" }));
    }

    #[test]
    fn elicitations_fill_defaults_and_decline_what_they_cannot() {
        let form = json!({ "serverName": "github", "mode": "form", "message": "Allow?", "requestedSchema": {
            "type": "object", "required": ["choice"],
            "properties": { "choice": { "type": "string", "enum": ["allow_once", "always", "deny"] }, "note": { "type": "string", "default": "ok" } },
        } });
        let (_, p) = pending("mcpServer/elicitation/request", form.clone());
        assert_eq!(p.reply("accept", None), json!({ "action": "accept", "content": { "choice": "allow_once", "note": "ok" } }));
        assert_eq!(p.reply("acceptAlways", None)["content"]["choice"], "always");
        assert_eq!(p.reply("acceptAlways", None)["_meta"]["persist"], "always");
        assert_eq!(p.reply("decline", None), json!({ "action": "decline" }));
        assert_eq!(p.reply("cancel", None), json!({ "action": "cancel" }));
        // A required field with nothing to fill it, and URL requests, decline.
        let mut unfillable = form.clone();
        unfillable["requestedSchema"]["properties"]["choice"] = json!({ "type": "string" });
        let (_, p) = pending("mcpServer/elicitation/request", unfillable);
        assert_eq!(p.reply("accept", None), json!({ "action": "decline" }));
        let (_, p) = pending("mcpServer/elicitation/request", json!({ "serverName": "s", "mode": "url", "message": "Sign in" }));
        assert_eq!(p.reply("accept", None), json!({ "action": "decline" }));
    }

    #[test]
    fn legacy_approvals_answer_with_review_decisions_and_unknown_methods_are_refused() {
        let (_, p) = pending("execCommandApproval", json!({ "callId": "c", "command": ["ls", "-la"] }));
        assert_eq!(p.reply("accept", None), json!({ "decision": "approved" }));
        assert_eq!(p.reply("acceptForSession", None), json!({ "decision": "approved_for_session" }));
        assert_eq!(p.reply("cancel", None), json!({ "decision": "abort" }));
        assert!(p.reply("decline", None)["decision"]["denied"].is_object());
        let (_, p) = pending("item/commandExecution/requestApproval", json!({ "itemId": "i", "command": "ls" }));
        assert_eq!(p.reply("acceptAlways", None), json!({ "decision": "acceptForSession" }));
        assert!(classify_request("item/tool/call", &json!({})).is_none());
    }

    #[test]
    fn token_usage_is_the_last_call_against_the_window() {
        let u = codex_token_usage(&json!({
            "total": { "totalTokens": 90000 },
            "last": { "totalTokens": 12000, "inputTokens": 11000, "cachedInputTokens": 8000, "outputTokens": 1000, "reasoningOutputTokens": 300 },
            "modelContextWindow": 272000,
        })).unwrap();
        assert_eq!(u.used_tokens, 12000);
        assert_eq!(u.max_tokens, Some(272000));
        assert_eq!(u.reasoning_output_tokens, 300);
        assert!(codex_token_usage(&json!({})).is_none());
    }

    #[test]
    fn live_output_keeps_its_end_from_a_line_start() {
        let mut out = String::new();
        push_tail(&mut out, "first line\n");
        assert_eq!(out, "first line\n");
        let line = format!("{}\n", "x".repeat(99));
        for _ in 0..1000 {
            push_tail(&mut out, &line);
        }
        assert!(out.len() <= LIVE_OUTPUT_MAX);
        assert!(out.starts_with('x') && out.ends_with('\n'), "cut at a line start");
        // A cut inside a multi-byte character moves to its boundary.
        let mut wide = "é".repeat(LIVE_OUTPUT_MAX);
        push_tail(&mut wide, "ż");
        assert!(wide.len() <= LIVE_OUTPUT_MAX && wide.ends_with('ż'));
    }

    #[test]
    fn file_changes_keep_their_patch_with_headers() {
        let patch = change_patch(&[
            json!({ "path": "src/a.rs", "kind": { "type": "update" }, "diff": "@@ -1 +1 @@\n-old\n+new\n" }),
            json!({ "path": "src/b.rs", "kind": { "type": "add" } }),
        ])
        .expect("a patch");
        assert!(patch.starts_with("diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1 +1 @@"), "{patch}");
        assert!(!patch.contains("src/b.rs"), "a change without a diff adds nothing");
        assert!(change_patch(&[json!({ "path": "x" })]).is_none());
    }
}
