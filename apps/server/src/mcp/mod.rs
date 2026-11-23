//! The agent MCP server (t3code `mcp/McpHttpServer` and its core toolkits):
//! the agents Domovoi runs reach this server back over Streamable HTTP at
//! `/mcp`, to read and message other threads of their project, delegate
//! tasks to child agents, schedule tasks, and look up their project.
//!
//! Each agent session gets a bearer token naming its thread (`session_for`);
//! a call acts as that thread, inside its project. Only connections from
//! this machine are served (never over iroh). `DOMOVOI_NO_AGENT_TOOLS=1` turns
//! the tools off for new sessions.
//!
//! MCP is JSON-RPC: the messages, the tool descriptions and what a tool
//! answers are JSON in MCP's shapes. The threads a tool reads are typed, and
//! so is the task a tool schedules.

#![expect(clippy::disallowed_types, clippy::disallowed_macros, reason = "the MCP wire: JSON-RPC messages, tool schemas, tool arguments and tool results; other tools' MCP config entries")]

use crate::orchestration::commands::{ACTIVE_RUN, Orchestrator};
use crate::state::AppState;
use serde_json::{Value, json};
use std::collections::HashMap;
use domovoi_model::typed::command::MessageDispatch;
use domovoi_model::typed::{Actor, AppThread, ConversationMessage, CreationSource, MessageRole, Run, RunStatus};
use std::sync::{Arc, Mutex, OnceLock};

/// The MCP server's name as agents see it (Claude tools are `mcp__t3__*`).
pub const SERVER_NAME: &str = "t3";

/// `http://127.0.0.1:<port>/mcp`, once the server listens.
pub static ENDPOINT: OnceLock<String> = OnceLock::new();

fn tokens() -> &'static Mutex<HashMap<String, String>> {
    static T: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    T.get_or_init(Default::default)
}

pub fn enabled() -> bool {
    ENDPOINT.get().is_some() && std::env::var_os("DOMOVOI_NO_AGENT_TOOLS").is_none_or(|v| v.is_empty())
}

/// The endpoint and bearer token for an agent of `thread_id` (one token per
/// thread for the server's life), or `None` with the tools off.
pub fn session_for(thread_id: &str) -> Option<(String, String)> {
    if !enabled() {
        return None;
    }
    let mut t = tokens().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    let token = match t.iter().find(|(_, tid)| *tid == thread_id) {
        Some((tok, _)) => tok.clone(),
        None => {
            let tok = format!("domovoi_mcp_{}", crate::util::new_uuid().replace('-', ""));
            t.insert(tok.clone(), thread_id.to_owned());
            tok
        }
    };
    Some((ENDPOINT.get()?.clone(), token))
}

fn thread_for(token: &str) -> Option<String> {
    tokens().lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(token).cloned()
}

/// Claude's `--mcp-config` for the thread's agent.
pub fn claude_config(thread_id: &str) -> Option<String> {
    let (url, token) = session_for(thread_id)?;
    Some(json!({ "mcpServers": { SERVER_NAME: { "type": "http", "url": url, "headers": { "Authorization": format!("Bearer {token}") } } } }).to_string())
}

/// Codex thread config entries (`config` on `thread/start` / `thread/resume`).
pub fn codex_config(thread_id: &str) -> Option<Value> {
    let (url, token) = session_for(thread_id)?;
    Some(json!({ format!("mcp_servers.{SERVER_NAME}"): { "url": url, "http_headers": { "Authorization": format!("Bearer {token}") }, "tool_timeout_sec": 1800 } }))
}

mod artifacts;
pub mod bridge;
mod tasks;
mod threads;
mod worktree;

/// A tool's description for `tools/list`: a strict schema, and whether it only reads.
pub(crate) fn tool(name: &str, description: &str, properties: Value, required: &[&str], read_only: bool) -> Value {
    json!({
        "name": name, "description": description,
        "inputSchema": { "type": "object", "properties": properties, "required": required, "additionalProperties": false },
        "annotations": { "readOnlyHint": read_only, "destructiveHint": !read_only },
    })
}

/// Schema pieces the toolkits share.
pub(crate) fn s(d: &str) -> Value {
    json!({ "type": "string", "description": d })
}
pub(crate) fn n(d: &str) -> Value {
    json!({ "type": "integer", "description": d })
}
pub(crate) fn b(d: &str) -> Value {
    json!({ "type": "boolean", "description": d })
}
pub(crate) fn one_of(values: &[&str], d: &str) -> Value {
    json!({ "type": "string", "enum": values, "description": d })
}
pub(crate) fn object(d: &str) -> Value {
    json!({ "type": "object", "description": d })
}

pub fn tools() -> Vec<Value> {
    let mut all = threads::tools();
    all.extend(tasks::tools());
    all.extend(worktree::tools());
    all.extend(artifacts::tools());
    all
}

/// Tools that only read; Claude runs these without asking.
pub fn read_only_tools() -> Vec<String> {
    tools().into_iter().filter(|t| t["annotations"]["readOnlyHint"] == true).filter_map(|t| t["name"].as_str().map(str::to_owned)).collect()
}

pub(crate) struct Caller {
    pub thread_id: String,
    pub thread: AppThread,
    pub project_id: String,
}

/// A tool call: who calls, with what.
pub(crate) struct Call<'a> {
    pub state: &'a Arc<AppState>,
    pub caller: &'a Caller,
    pub args: &'a Value,
}

impl Call<'_> {
    pub fn str(&self, k: &str) -> Option<String> {
        self.args[k].as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned)
    }
    pub fn required(&self, k: &str) -> Result<String, String> {
        self.str(k).ok_or_else(|| format!("{k} is required."))
    }
    pub fn flag(&self, k: &str) -> bool {
        self.args[k] == true
    }
    pub fn limit(&self, default: u64, max: u64) -> usize {
        self.args["limit"].as_u64().unwrap_or(default).clamp(1, max) as usize
    }
    pub fn store(&self) -> &crate::store::Store {
        &self.state.store
    }
    pub fn orch(&self) -> &Arc<Orchestrator> {
        &self.state.orchestrator
    }
    /// The thread `threadId` names, the caller's own when it names none.
    pub fn thread(&self) -> Result<(String, AppThread), String> {
        match self.str("threadId") {
            None => Ok((self.caller.thread_id.clone(), self.caller.thread.clone())),
            Some(id) if id == self.caller.thread_id => Ok((id, self.caller.thread.clone())),
            Some(id) => self.caller.peer(self.store(), &id).map(|t| (id, t)),
        }
    }
    /// A command id: from `clientRequestId`, so a retried call is one change; else new.
    pub fn command_id(&self, kind: &str) -> String {
        match self.str("clientRequestId") {
            Some(key) => format!("mcp-{kind}:{}:{key}", self.caller.thread_id),
            None => format!("mcp-{kind}:{}", crate::util::new_uuid()),
        }
    }
    /// Dispatches an orchestration command given as its wire JSON.
    pub async fn dispatch(&self, command: Value) -> Result<Value, String> {
        let done = crate::rpc::shell::dispatch_command(self.orch(), command).await.map_err(|e| e.to_string())?;
        Ok(serde_json::to_value(done).unwrap_or(Value::Null))
    }
    pub fn project_folder(&self) -> Option<String> {
        self.store().get_project(&self.caller.project_id).ok().flatten().map(|p| p.workspace_root)
    }
}

pub(crate) fn text_result(v: Value) -> Value {
    json!({ "content": [{ "type": "text", "text": serde_json::to_string_pretty(&v).unwrap_or_default() }], "structuredContent": v })
}

pub(crate) fn tool_error(message: impl Into<String>) -> Value {
    json!({ "content": [{ "type": "text", "text": message.into() }], "isError": true })
}

/// The thread's messages in the order they were written.
pub(crate) fn by_time(messages: &[Arc<ConversationMessage>]) -> Vec<&ConversationMessage> {
    let mut messages: Vec<&ConversationMessage> = messages.iter().map(|message| &**message).collect();
    messages.sort_by(|a, b| a.created_at.as_str().cmp(b.created_at.as_str()));
    messages
}

pub(crate) fn latest_reply(messages: &[Arc<ConversationMessage>]) -> Option<String> {
    by_time(messages).into_iter().rfind(|m| m.role == MessageRole::Assistant && !m.text.trim().is_empty()).map(|m| m.text.clone())
}

/// `working` while a run is active or queued; else the latest run's outcome.
pub(crate) fn work_status(runs: &[Arc<Run>]) -> &'static str {
    if runs.iter().any(|run| run.status == RunStatus::Queued || ACTIVE_RUN.contains(&run.status.as_str())) {
        return "working";
    }
    match runs.iter().max_by_key(|run| run.ordinal).map(|run| &run.status) {
        Some(RunStatus::Failed) => "failed",
        Some(RunStatus::Interrupted | RunStatus::Cancelled) => "stopped",
        Some(_) => "result_available",
        None => "idle",
    }
}

/// The thread's run in progress, if one is.
pub(crate) fn active_run(runs: &[Arc<Run>]) -> Option<&Run> {
    runs.iter().filter(|run| ACTIVE_RUN.contains(&run.status.as_str())).max_by_key(|run| run.ordinal).map(|run| &**run)
}

impl Caller {
    /// A thread the caller may act on: one of its project, not deleted.
    pub(crate) fn peer(&self, store: &crate::store::Store, id: &str) -> Result<AppThread, String> {
        let t = store.app_thread(id).ok().flatten().filter(|t| t.deleted_at.is_none()).ok_or_else(|| format!("No thread {id}."))?;
        if t.project_id.as_str() != self.project_id {
            return Err(format!("Thread {id} is not in this project."));
        }
        Ok(t)
    }
}

async fn call_tool(state: &Arc<AppState>, caller: &Caller, name: &str, args: &Value) -> Result<Value, String> {
    let call = Call { state, caller, args };
    if let Some(answer) = threads::call(&call, name).await {
        return answer;
    }
    if let Some(answer) = tasks::call(&call, name).await {
        return answer;
    }
    if let Some(answer) = worktree::call(&call, name).await {
        return answer;
    }
    if let Some(answer) = artifacts::call(&call, name).await {
        return answer;
    }
    Err(format!("Unknown tool {name}."))
}

/// One JSON-RPC message from an agent; `None` for a notification (no reply).
pub async fn handle(state: &Arc<AppState>, token: &str, msg: &Value) -> Option<Value> {
    let id = msg.get("id").cloned()?;
    let method = msg["method"].as_str().unwrap_or_default();
    let ok = |result: Value| Some(json!({ "jsonrpc": "2.0", "id": id, "result": result }));
    let err = |code: i64, m: &str| Some(json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": m } }));
    match method {
        "initialize" => ok(json!({
            "protocolVersion": msg["params"]["protocolVersion"].as_str().unwrap_or("2025-06-18"),
            "capabilities": { "tools": { "listChanged": false } },
            "serverInfo": { "name": "domovoi-server", "title": "T3 Code (Domovoi)", "version": domovoi_contracts::environment::SERVER_VERSION },
            "instructions": crate::agent_instructions::MCP_SUMMARY,
        })),
        "ping" => ok(json!({})),
        "tools/list" => ok(json!({ "tools": tools() })),
        "tools/call" => {
            let Some(thread_id) = thread_for(token) else { return err(-32001, "Unknown session.") };
            let Some(thread) = state.store.app_thread(&thread_id).ok().flatten().filter(|t| t.deleted_at.is_none()) else {
                return err(-32001, "This thread is gone.");
            };
            let caller = Caller { project_id: thread.project_id.as_str().to_owned(), thread_id, thread };
            let name = msg["params"]["name"].as_str().unwrap_or_default();
            let args = msg["params"].get("arguments").cloned().unwrap_or(json!({}));
            ok(match call_tool(state, &caller, name, &args).await {
                Ok(v) => text_result(v),
                Err(m) => tool_error(m),
            })
        }
        _ => err(-32601, &format!("Method {method} not found.")),
    }
}

/// Whether a bearer token is one this server issued.
pub fn known_token(token: &str) -> bool {
    thread_for(token).is_some()
}

/// The message a delegating thread gets when its task settles.
pub fn completion_message(task_title: &str, task_id: &str, status: &str, reply: Option<&str>) -> String {
    let outcome = match status {
        "result_available" => "finished",
        "failed" => "failed",
        _ => "stopped",
    };
    let mut m = format!("Delegated task \"{task_title}\" ({task_id}) {outcome}.");
    if let Some(r) = reply.map(str::trim).filter(|r| !r.is_empty()) {
        m.push_str("\n\nIts last reply:\n");
        m.push_str(&r.chars().take(20_000).collect::<String>());
    }
    m
}

/// After a run of `thread_id` settles: a delegated task with nothing left
/// to do tells the thread that delegated it, once per run.
pub async fn deliver_completion(orch: &Orchestrator, thread_id: &str, run_id: &str) {
    let store = orch.store();
    let Ok(Some(t)) = store.app_thread(thread_id) else { return };
    let Some(parent) = t.delegated_from.as_ref().and_then(Option::as_ref).map(|from| from.thread_id.as_str().to_owned()) else { return };
    let Ok(runs) = store.records::<Run>(thread_id) else { return };
    let status = work_status(&runs);
    if status == "working" {
        return;
    }
    let Ok(messages) = store.records::<ConversationMessage>(thread_id) else { return };
    let text = completion_message(&t.title, thread_id, status, latest_reply(&messages).as_deref());
    let mid = format!("message:delegated-completion:{}", crate::orchestration::events::enc(run_id));
    let mut report = MessageDispatch::new(format!("delegated-completion:{run_id}"), parent, mid, text, Actor::System, CreationSource::Server);
    report.sender_thread_id = Some(thread_id.into());
    if let Err(err) = orch.dispatch(report.into()).await {
        tracing::info!(%thread_id, "delegated task completion not delivered: {err}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tools_have_strict_schemas_and_completions_read_well() {
        let tools = tools();
        assert!(tools.iter().all(|t| t["inputSchema"]["type"] == "object" && t["inputSchema"]["additionalProperties"] == false));
        let mut names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
        let count = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), count, "each tool once");
        // Every property a tool lists as required is one it has.
        for t in &tools {
            for r in t["inputSchema"]["required"].as_array().unwrap() {
                assert!(t["inputSchema"]["properties"].get(r.as_str().unwrap()).is_some(), "{}: {r}", t["name"]);
            }
        }
        for name in ["t3_thread_list", "t3_thread_read", "t3_thread_wait", "task_status", "t3_project_info", "orchestrator_capabilities", "t3_worktree_status", "list_scheduled_tasks"] {
            assert!(read_only_tools().iter().any(|t| t == name), "{name}");
        }
        let m = completion_message("Fix tests", "t9", "result_available", Some("All green."));
        assert!(m.starts_with("Delegated task \"Fix tests\" (t9) finished.") && m.ends_with("All green."));
        assert!(completion_message("x", "t", "failed", None).ends_with("failed."));
    }
}
