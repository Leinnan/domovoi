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
use crate::scheduled_tasks::{UpsertInput, UpsertSchedule};
use crate::state::AppState;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashMap;
use domovoi_model::typed::command::{MessageDispatch, QueuedRunCancel, ThreadStop};
use domovoi_model::typed::{
    Actor, AppThread, ConversationMessage, CreationSource, InitialMessage, InteractionMode, LinkedThread, MessageRole, ModelSelection, Run, RunStatus,
    ScheduledTaskThreadMode, ThreadLaunchInput, WorkspaceStrategy,
};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

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

/// Tools that only read; Claude runs these without asking.
pub const READ_ONLY_TOOLS: [&str; 5] = ["t3_thread_list", "t3_thread_read", "t3_thread_wait", "task_status", "t3_project_info"];

/// Codex thread config entries (`config` on `thread/start` / `thread/resume`).
pub fn codex_config(thread_id: &str) -> Option<Value> {
    let (url, token) = session_for(thread_id)?;
    Some(json!({ format!("mcp_servers.{SERVER_NAME}"): { "url": url, "http_headers": { "Authorization": format!("Bearer {token}") }, "tool_timeout_sec": 1800 } }))
}

fn tool(name: &str, description: &str, properties: Value, required: &[&str], read_only: bool) -> Value {
    json!({
        "name": name, "description": description,
        "inputSchema": { "type": "object", "properties": properties, "required": required, "additionalProperties": false },
        "annotations": { "readOnlyHint": read_only, "destructiveHint": !read_only },
    })
}

pub fn tools() -> Vec<Value> {
    let s = |d: &str| json!({ "type": "string", "description": d });
    let n = |d: &str| json!({ "type": "integer", "description": d });
    vec![
        tool("t3_thread_list", "List the other threads of this project: id, title, status, last update. Newest first.",
            json!({ "limit": n("At most this many (default 20, at most 100).") }), &[], true),
        tool("t3_thread_read", "Read a thread of this project: its status and latest messages.",
            json!({ "threadId": s("The thread to read."), "limit": n("Latest messages to return (default 20, at most 100).") }), &["threadId"], true),
        tool("t3_thread_send", "Send a message to another thread of this project, as if its user wrote it. It queues behind work in progress.",
            json!({ "threadId": s("The thread to message."), "message": s("What to send.") }), &["threadId", "message"], false),
        tool("t3_thread_wait", "Wait until a thread of this project has no work running or queued, then return its status and last reply.",
            json!({ "threadId": s("The thread to wait for."), "timeoutSeconds": n("Give up after this long (default 300, at most 1800).") }), &["threadId"], true),
        tool("delegate_task", "Start one child agent on `task` in a new thread of this project. It gets no history from you; put everything it needs in `task`. `isolation: \"worktree\"` gives it its own git worktree. Keep the returned `taskId` for `task_status` and `task_cancel`; you get a message here when it finishes. No secrets in `task`.",
            json!({
                "task": s("The child's whole brief."), "title": s("A short title for its thread."),
                "isolation": { "type": "string", "enum": ["shared", "worktree"], "description": "shared (default): this thread's folder; worktree: a new worktree." },
                "instanceId": s("Provider for the child (codex, claudeAgent); default: yours."), "model": s("Model for the child; default: yours."),
            }), &["task"], false),
        tool("task_status", "Read a delegated task you started: working, result_available or failed, and its latest reply.",
            json!({ "taskId": s("From delegate_task.") }), &["taskId"], true),
        tool("task_cancel", "Stop a delegated task: its running turn and anything queued.",
            json!({ "taskId": s("From delegate_task.") }), &["taskId"], false),
        tool("schedule_task", "Schedule a prompt to run later or repeatedly in this project, in a new thread each time or in this one.",
            json!({
                "name": s("A name for the task."), "prompt": s("What the agent is asked each time."),
                "schedule": { "type": "object", "description": "{\"type\":\"interval\",\"everyMs\":3600000} (at least 60000), {\"type\":\"fixed_time\",\"timeOfDay\":\"09:00\",\"weekdays\":[1,2,3,4,5]} (0 is Sunday; no weekdays: every day), or {\"type\":\"once\",\"runAt\":\"2026-01-01T09:00:00+01:00\"}." },
                "thisThread": { "type": "boolean", "description": "Run in this thread rather than a new one each time." },
            }), &["name", "prompt", "schedule"], false),
        tool("t3_project_info", "This thread's project: title, root folder, this thread's worktree and branch, and the project's scripts.",
            json!({}), &[], true),
    ]
}

struct Caller {
    thread_id: String,
    thread: AppThread,
    project_id: String,
}

fn text_result(v: Value) -> Value {
    json!({ "content": [{ "type": "text", "text": serde_json::to_string_pretty(&v).unwrap_or_default() }], "structuredContent": v })
}

fn tool_error(message: impl Into<String>) -> Value {
    json!({ "content": [{ "type": "text", "text": message.into() }], "isError": true })
}

/// The thread's messages in the order they were written.
fn by_time(messages: &[Arc<ConversationMessage>]) -> Vec<&ConversationMessage> {
    let mut messages: Vec<&ConversationMessage> = messages.iter().map(|message| &**message).collect();
    messages.sort_by(|a, b| a.created_at.as_str().cmp(b.created_at.as_str()));
    messages
}

fn latest_reply(messages: &[Arc<ConversationMessage>]) -> Option<String> {
    by_time(messages).into_iter().rfind(|m| m.role == MessageRole::Assistant && !m.text.trim().is_empty()).map(|m| m.text.clone())
}

/// `working` while a run is active or queued; else the latest run's outcome.
fn work_status(runs: &[Arc<Run>]) -> &'static str {
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

impl Caller {
    /// A thread the caller may act on: one of its project, not deleted.
    fn peer(&self, store: &crate::store::Store, id: &str) -> Result<AppThread, String> {
        let t = store.app_thread(id).ok().flatten().filter(|t| t.deleted_at.is_none()).ok_or_else(|| format!("No thread {id}."))?;
        if t.project_id.as_str() != self.project_id {
            return Err(format!("Thread {id} is not in this project."));
        }
        Ok(t)
    }
}

async fn call_tool(state: &Arc<AppState>, caller: &Caller, name: &str, args: &Value) -> Result<Value, String> {
    let store = &state.store;
    let orch: &Arc<Orchestrator> = &state.orchestrator;
    let str_arg = |k: &str| args[k].as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned);
    let limit = |default: u64| args["limit"].as_u64().unwrap_or(default).clamp(1, 100) as usize;
    match name {
        "t3_thread_list" => {
            let mut threads: Vec<AppThread> = store.app_threads().map_err(|e| e.to_string())?.into_iter()
                .filter(|t| t.project_id.as_str() == caller.project_id && t.id.as_str() != caller.thread_id && t.deleted_at.is_none()).collect();
            threads.sort_by(|a, b| b.updated_at.as_str().cmp(a.updated_at.as_str()));
            let out: Vec<Value> = threads.into_iter().take(limit(20)).map(|t| {
                let status = store.records::<Run>(t.id.as_str()).map(|runs| work_status(&runs)).unwrap_or("idle");
                json!({ "threadId": t.id, "title": t.title, "status": status, "updatedAt": t.updated_at, "archived": t.archived_at.is_some() })
            }).collect();
            Ok(json!({ "threads": out }))
        }
        "t3_thread_read" => {
            let id = str_arg("threadId").ok_or("threadId is required.")?;
            let t = caller.peer(store, &id)?;
            let runs = store.records::<Run>(&id).map_err(|e| e.to_string())?;
            let stored = store.records::<ConversationMessage>(&id).map_err(|e| e.to_string())?;
            let msgs = by_time(&stored);
            let n = limit(20);
            let skip = msgs.len().saturating_sub(n);
            let messages: Vec<Value> = msgs[skip..].iter().map(|m| json!({ "role": m.role, "text": m.text, "createdAt": m.created_at })).collect();
            Ok(json!({ "threadId": id, "title": t.title, "status": work_status(&runs), "omittedMessages": skip, "messages": messages }))
        }
        "t3_thread_send" => {
            let id = str_arg("threadId").ok_or("threadId is required.")?;
            let text = str_arg("message").ok_or("message is required.")?;
            if id == caller.thread_id {
                return Err("Use your own reply to talk in this thread.".into());
            }
            caller.peer(store, &id)?;
            let mid = format!("message:mcp:{}", crate::util::new_uuid());
            let send = MessageDispatch::new(format!("mcp-send:{mid}"), id.as_str(), mid.as_str(), text, Actor::Agent, CreationSource::Mcp);
            orch.dispatch(send.into()).await.map_err(|e| e.to_string())?;
            Ok(json!({ "threadId": id, "messageId": mid, "sent": true }))
        }
        "t3_thread_wait" => {
            let id = str_arg("threadId").ok_or("threadId is required.")?;
            caller.peer(store, &id)?;
            let deadline = tokio::time::Instant::now() + Duration::from_secs(args["timeoutSeconds"].as_u64().unwrap_or(300).clamp(1, 1800));
            loop {
                let status = work_status(&store.records::<Run>(&id).map_err(|e| e.to_string())?);
                if status != "working" {
                    let messages = store.records::<ConversationMessage>(&id).map_err(|e| e.to_string())?;
                    return Ok(json!({ "threadId": id, "status": status, "lastReply": latest_reply(&messages) }));
                }
                if tokio::time::Instant::now() >= deadline {
                    return Ok(json!({ "threadId": id, "status": "working", "timedOut": true }));
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
        "delegate_task" => {
            let task = str_arg("task").ok_or("task is required.")?;
            let mut model = caller.thread.model_selection.clone();
            if let Some(i) = str_arg("instanceId") {
                model = ModelSelection { instance_id: i.into(), model: str_arg("model").unwrap_or_default(), options: None };
            } else if let Some(m) = str_arg("model") {
                model.model = m;
            }
            if model.model.is_empty() {
                return Err("Name a model for that provider.".into());
            }
            let strategy = match (args["isolation"].as_str(), caller.thread.worktree_path.as_deref()) {
                (Some("worktree"), _) => WorkspaceStrategy::Worktree { base_ref: "HEAD".into(), branch: None, start_from_origin: None },
                (_, Some(path)) => WorkspaceStrategy::ExistingWorktree { worktree_path: path.to_owned(), branch: None },
                _ => WorkspaceStrategy::Root { branch: None },
            };
            let child = format!("thread:project:{}:{}", caller.project_id, crate::util::new_uuid());
            let title = str_arg("title").unwrap_or_else(|| crate::adapters::subagents::child_title("Delegated task", None, &task, 1));
            let launch = ThreadLaunchInput {
                command_id: format!("mcp-delegate:{child}").into(),
                creation_source: Some(CreationSource::Mcp),
                thread_id: Some(child.as_str().into()),
                reuse_existing_thread: None,
                project_id: caller.project_id.as_str().into(),
                title: title.clone(),
                generate_title: None,
                model_selection: model,
                runtime_mode: caller.thread.runtime_mode.clone(),
                interaction_mode: InteractionMode::Default,
                workspace_strategy: strategy,
                initial_message: Some(InitialMessage { message_id: None, text: task, context: None, attachments: vec![] }),
                created_by: Some(Actor::Agent),
                delegated_from: Some(LinkedThread {
                    environment_id: state.environment.environment_id.as_str().into(),
                    thread_id: caller.thread_id.as_str().into(),
                    title: caller.thread.title.clone(),
                }),
            };
            let launched = orch.launch_thread(launch).await.map_err(|e| e.to_string())?;
            Ok(json!({ "taskId": child, "threadId": child, "title": title, "workState": "working", "worktreePath": launched.projection.thread.worktree_path }))
        }
        "task_status" | "task_cancel" => {
            let id = str_arg("taskId").ok_or("taskId is required.")?;
            let t = caller.peer(store, &id)?;
            if t.delegated_from.as_ref().and_then(Option::as_ref).is_none_or(|from| from.thread_id.as_str() != caller.thread_id) {
                return Err(format!("{id} is not a task you delegated."));
            }
            let runs = store.records::<Run>(&id).map_err(|e| e.to_string())?;
            let messages = store.records::<ConversationMessage>(&id).map_err(|e| e.to_string())?;
            if name == "task_cancel" {
                for run in runs.iter().filter(|run| run.status == RunStatus::Queued) {
                    let cancel = QueuedRunCancel { command_id: format!("mcp-cancel:{}", run.id).into(), thread_id: id.as_str().into(), run_id: run.id.clone() };
                    let _ = orch.dispatch(cancel.into()).await;
                }
                let stop = ThreadStop {
                    command_id: format!("mcp-stop:{}", crate::util::new_uuid()).into(),
                    thread_id: id.as_str().into(),
                    reason: None,
                    stop_delegated_tasks: None,
                };
                let stopped = orch.dispatch(stop.into()).await.is_ok();
                return Ok(json!({ "taskId": id, "cancelled": stopped || work_status(&runs) != "working" }));
            }
            let status = work_status(&runs);
            Ok(json!({ "taskId": id, "title": t.title, "workState": status, "summary": if status == "working" { Value::Null } else { json!(latest_reply(&messages)) } }))
        }
        "schedule_task" => {
            let name_arg = str_arg("name").ok_or("name is required.")?;
            let prompt = str_arg("prompt").ok_or("prompt is required.")?;
            // The tool's `schedule` is a schedule as `scheduledTasks.upsert` takes one. Anything else (none at
            // all, too) is refused with the message the save gave it when it still got that far.
            let schedule = UpsertSchedule::deserialize(&args["schedule"]).map_err(|_| "Could not save schedule task.")?;
            let this_thread = args["thisThread"] == true;
            // The tool names no workspace, which a task can do without, and no
            // runtime or interaction mode, which a task must have: it takes
            // those of the thread that schedules it.
            let input = UpsertInput {
                id: None,
                command_id: Some(format!("mcp-schedule:{}", crate::util::new_uuid())),
                require_existing: None,
                title: Some(name_arg),
                prompt: Some(prompt),
                enabled: Some(true),
                schedule,
                project_id: Some(caller.project_id.clone()),
                thread_id: this_thread.then(|| caller.thread_id.clone()),
                thread_mode: Some(if this_thread { ScheduledTaskThreadMode::Continue } else { ScheduledTaskThreadMode::NewThread }),
                workspace_strategy: None,
                model_selection: caller.thread.model_selection.clone(),
                runtime_mode: Some(caller.thread.runtime_mode.clone()),
                interaction_mode: Some(caller.thread.interaction_mode.clone()),
                created_by: None,
                creation_source: None,
            };
            let now = jiff::Zoned::now();
            let task = crate::scheduled_tasks::upsert(store, input, &now).map_err(|e| e.message)?;
            crate::scheduled_tasks::service(state).notify_changed();
            Ok(json!({ "task": task }))
        }
        "t3_project_info" => {
            let p = store.get_project(&caller.project_id).ok().flatten().ok_or("The project is gone.")?;
            let settings = state.settings.read().await.clone();
            let scripts = crate::project_scripts::resolve(&settings, &p.id, &p.scripts, &p.workspace_root);
            Ok(json!({ "projectId": p.id, "title": p.title, "workspaceRoot": p.workspace_root, "threadId": caller.thread_id,
                "worktreePath": caller.thread.worktree_path, "branch": caller.thread.branch, "scripts": scripts }))
        }
        other => Err(format!("Unknown tool {other}.")),
    }
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
            "instructions": "Tools of the T3 Code server you run in: read and message other threads of this project, delegate tasks to child agents, schedule tasks.",
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
        for name in READ_ONLY_TOOLS {
            assert!(tools.iter().any(|t| t["name"] == name && t["annotations"]["readOnlyHint"] == true), "{name}");
        }
        let m = completion_message("Fix tests", "t9", "result_available", Some("All green."));
        assert!(m.starts_with("Delegated task \"Fix tests\" (t9) finished.") && m.ends_with("All green."));
        assert!(completion_message("x", "t", "failed", None).ends_with("failed."));
    }
}
