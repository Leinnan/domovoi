//! Delegation and scheduling tools: what
//! providers and models a child can use, delegated tasks and their status,
//! several new threads at once, stopping a subagent, scheduled tasks, and
//! the caller's project.

#![expect(clippy::disallowed_types, clippy::disallowed_macros, reason = "the MCP wire: tool schemas, tool arguments and tool results")]

use super::{Call, active_run, b, latest_reply, n, object, one_of, s, tool, work_status};
use crate::scheduled_tasks::{UpsertInput, UpsertSchedule};
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Duration;
use domovoi_model::typed::{
    Actor, ConversationMessage, CreationSource, InitialMessage, InteractionMode, LinkedThread, ModelSelection, Run, RunStatus, RuntimeMode, ScheduledTask, ScheduledTaskSchedule,
    ScheduledTaskThreadMode, ThreadLaunchInput, WorkspaceStrategy,
};

const MAX_BATCH_THREADS: usize = 20;

pub fn tools() -> Vec<Value> {
    let target = object("{\"providerInstanceId\":\"codex\",\"model\":\"...\",\"options\":[{\"id\":\"effort\",\"value\":\"high\"}]}: from `orchestrator_capabilities`; default: yours.");
    let schedule = object("{\"type\":\"interval\",\"everyMs\":3600000} (at least 60000), {\"type\":\"fixed_time\",\"timeOfDay\":\"09:00\",\"weekdays\":[1,2,3,4,5]} (0 is Sunday; no weekdays: every day), {\"type\":\"once\",\"runAt\":\"2026-01-01T09:00:00+01:00\"}, or {\"type\":\"webhook\"} (runs on each request to its URL).");
    vec![
        tool("orchestrator_capabilities", "The providers and models a child thread or delegated task can use here, and your own. Call once before your first `delegate_task` to another provider or model.",
            json!({ "search": s("Only providers or models whose name contains this.") }), &[], true),
        tool("delegate_task", "Start one child agent on `task` in a new thread of this project, for another provider or model, other settings, or a separate checkout (`isolation: \"worktree\"`). It gets no history from you: put everything it needs in `task`. `mode: \"wait\"` waits for it (up to `timeoutMs`); otherwise keep the `taskId`, and you get a message here when it finishes. No secrets in `task`.",
            json!({
                "task": s("The child's whole brief."), "title": s("A short title for its thread."), "target": target,
                "isolation": one_of(&["shared", "worktree"], "shared (default): this thread's folder; worktree: a new worktree."),
                "mode": one_of(&["async", "wait"], "async (default) or wait for its result."),
                "timeoutMs": n("With mode wait: give up waiting after this long (default 600000, at most 1800000)."),
                "runtimeMode": one_of(&["approval-required", "auto-accept-edits", "full-access"], "Default: yours."),
                "interactionMode": one_of(&["default", "plan"], "Default: default."),
                "instanceId": s("Provider for the child (codex, claudeAgent); `target` is preferred."), "model": s("Model for the child; `target` is preferred."),
                "clientRequestId": s("Repeat the same id to retry without starting it twice."),
            }), &["task"], false),
        tool("task_status", "Read a delegated task you started: working, result_available, failed or stopped, whether it waits on a question or approval, and its latest reply.",
            json!({ "taskId": s("From delegate_task.") }), &["taskId"], true),
        tool("task_cancel", "Stop a delegated task: its running turn and anything queued.",
            json!({ "taskId": s("From delegate_task."), "reason": s("Why, for its transcript.") }), &["taskId"], false),
        tool("create_threads", "Start up to 20 new top-level threads in this project at once, only when the user asks for new threads.",
            json!({ "threads": { "type": "array", "description": "Each {\"prompt\",\"title\",\"target\",\"runtimeMode\",\"interactionMode\"}.", "items": { "type": "object" } },
                "clientRequestId": s("Repeat the same id to retry without creating them twice.") }), &["threads"], false),
        tool("workflow_stop", "Stop one of your subagents (by `subagentId`, from its item) without stopping your turn.",
            json!({ "subagentId": s("The subagent."), "runId": s("Its run; default: your running one.") }), &["subagentId"], false),
        tool("schedule_task", "Schedule a prompt to run later, repeatedly or on webhook requests in this project: in a new thread each time, in this one (`bindToCurrentThread`), or in another thread of this project (`threadId`); `freshSession` gives each run in that thread a new agent session. Give `scheduledTaskId` to change a task: what you leave out (prompt, schedule, its thread) stays as it is. Only schedules: it does not run the prompt now.",
            json!({
                "prompt": s("What the agent is asked each time (needed for a new task)."), "schedule": schedule, "title": s("A name for the task."),
                "enabled": b("Default true, or as it was for a changed task."),
                "bindToCurrentThread": b("true: run in this thread rather than a new one each time; false (on a change): back to a new thread each time."),
                "threadId": s("Run in this thread of the project (from thread_list) rather than a new one each time."),
                "freshSession": b("With a thread: each run starts a new agent session."),
                "scheduledTaskId": s("Change this task instead of making one."),
                "name": s("Same as title."), "thisThread": b("Same as bindToCurrentThread."),
                "clientRequestId": s("Repeat the same id to retry without making it twice."),
            }), &[], false),
        tool("list_scheduled_tasks", "This project's scheduled tasks: schedule, whether enabled, the thread runs go to (threadMode, boundThreadId), the next and last run.", json!({}), &[], true),
        tool("delete_scheduled_task", "Delete one of this project's scheduled tasks.", json!({ "scheduledTaskId": s("The task.") }), &["scheduledTaskId"], false),
        tool("run_scheduled_task_now", "Run one of this project's scheduled tasks now (not a webhook task); answers the thread the run went to.", json!({ "taskId": s("The task.") }), &["taskId"], false),
        tool("project_info", "This thread's project: title, root folder, this thread's worktree and branch, and the project's scripts.", json!({}), &[], true),
    ]
}

pub async fn call(c: &Call<'_>, name: &str) -> Option<Result<Value, String>> {
    Some(match name {
        "orchestrator_capabilities" => capabilities(c).await,
        "delegate_task" => delegate(c).await,
        "task_status" | "task_cancel" => task(c, name == "task_cancel").await,
        "create_threads" => create_threads(c).await,
        "workflow_stop" => workflow_stop(c).await,
        "schedule_task" => schedule(c).await,
        "list_scheduled_tasks" => list_tasks(c),
        "delete_scheduled_task" => delete_task(c),
        "run_scheduled_task_now" => run_now(c).await,
        "project_info" => project_info(c).await,
        _ => return None,
    })
}

async fn capabilities(c: &Call<'_>) -> Result<Value, String> {
    let search = c.str("search").map(|s| s.to_lowercase());
    let mut providers = vec![];
    for p in c.state.providers.snapshot().await {
        let p = serde_json::to_value(&p).unwrap_or(Value::Null);
        if p["enabled"] == false || p["status"] != "ready" {
            continue;
        }
        let named = |v: &Value| search.as_ref().is_none_or(|q| v.as_str().is_some_and(|s| s.to_lowercase().contains(q)));
        let provider_matches = named(&p["instanceId"]) || named(&p["displayName"]) || named(&p["driver"]);
        let models: Vec<Value> = p["models"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .filter(|m| provider_matches || named(&m["slug"]) || named(&m["name"]))
            .map(|m| {
                let options: Vec<Value> = m["capabilities"]["optionDescriptors"].as_array().cloned().unwrap_or_default().into_iter()
                    .map(|o| json!({ "id": o["id"], "values": o["options"].as_array().map(|vs| vs.iter().map(|v| v["id"].clone()).collect::<Vec<_>>()) }))
                    .collect();
                json!({ "id": m["slug"], "label": m["name"], "options": options })
            })
            .collect();
        if models.is_empty() && !provider_matches {
            continue;
        }
        providers.push(json!({ "providerInstanceId": p["instanceId"], "driverKind": p["driver"], "displayName": p["displayName"], "models": models,
            "canRunChildTask": true, "canRunCrossProviderChildTask": true }));
    }
    let t = &c.caller.thread;
    Ok(json!({ "parentThreadId": c.caller.thread_id, "inheritedProviderInstanceId": t.model_selection.instance_id, "inheritedModel": t.model_selection.model,
        "inheritedOptions": t.model_selection.options, "runtimeMode": t.runtime_mode, "interactionMode": t.interaction_mode, "providers": providers,
        "features": { "delegateWait": true, "worktreeIsolation": true, "maxBatchThreads": MAX_BATCH_THREADS } }))
}

/// The model a child runs on: `target`, else `instanceId`/`model`, else the caller's.
fn child_model(c: &Call<'_>, spec: &Value) -> Result<ModelSelection, String> {
    let mut model = c.caller.thread.model_selection.clone();
    if let Some(t) = spec.get("target").filter(|t| t.is_object()) {
        if let Some(i) = t["providerInstanceId"].as_str() {
            model = ModelSelection { instance_id: i.into(), model: t["model"].as_str().unwrap_or_default().to_owned(), options: None };
        } else if let Some(m) = t["model"].as_str() {
            model.model = m.to_owned();
        }
        if let Some(options) = t.get("options").filter(|o| o.is_array()) {
            model.options = serde_json::from_value(options.clone()).ok();
        }
    } else if let Some(i) = spec["instanceId"].as_str().filter(|s| !s.is_empty()) {
        model = ModelSelection { instance_id: i.into(), model: spec["model"].as_str().unwrap_or_default().to_owned(), options: None };
    } else if let Some(m) = spec["model"].as_str().filter(|s| !s.is_empty()) {
        model.model = m.to_owned();
    }
    if model.model.is_empty() {
        return Err("Name a model for that provider (see orchestrator_capabilities).".into());
    }
    Ok(model)
}

fn mode<T: serde::de::DeserializeOwned>(v: &Value, fallback: T) -> T {
    serde_json::from_value(v.clone()).unwrap_or(fallback)
}

async fn delegate(c: &Call<'_>) -> Result<Value, String> {
    let task = c.required("task")?;
    let model = child_model(c, c.args)?;
    let strategy = match (c.args["isolation"].as_str(), c.caller.thread.worktree_path.as_deref()) {
        (Some("worktree"), _) => WorkspaceStrategy::Worktree { base_ref: "HEAD".into(), branch: None, start_from_origin: None },
        (_, Some(path)) => WorkspaceStrategy::ExistingWorktree { worktree_path: path.to_owned(), branch: None },
        _ => WorkspaceStrategy::Root { branch: None },
    };
    let child = match c.str("clientRequestId") {
        Some(key) => format!("thread:project:{}:delegate:{}", c.caller.project_id, crate::orchestration::events::enc(&format!("{}:{key}", c.caller.thread_id))),
        None => format!("thread:project:{}:{}", c.caller.project_id, crate::util::new_uuid()),
    };
    let title = c.str("title").unwrap_or_else(|| crate::adapters::subagents::child_title("Delegated task", None, &task, 1));
    let launch = ThreadLaunchInput {
        command_id: format!("mcp-delegate:{child}").into(),
        creation_source: Some(CreationSource::Mcp),
        thread_id: Some(child.as_str().into()),
        reuse_existing_thread: None,
        project_id: c.caller.project_id.as_str().into(),
        title: title.clone(),
        generate_title: None,
        model_selection: model,
        runtime_mode: mode::<RuntimeMode>(&c.args["runtimeMode"], c.caller.thread.runtime_mode.clone()),
        interaction_mode: mode::<InteractionMode>(&c.args["interactionMode"], InteractionMode::Default),
        workspace_strategy: strategy,
        initial_message: Some(InitialMessage { message_id: None, text: task, context: None, attachments: vec![] }),
        created_by: Some(Actor::Agent),
        delegated_from: Some(LinkedThread {
            environment_id: c.state.environment.environment_id.as_str().into(),
            thread_id: c.caller.thread_id.as_str().into(),
            title: c.caller.thread.title.clone(),
        }),
    };
    let launched = c.orch().launch_thread(launch).await.map_err(|e| e.to_string())?;
    let started = json!({ "taskId": child, "threadId": child, "childThreadId": child, "title": title, "workState": "working", "worktreePath": launched.projection.thread.worktree_path });
    if c.args["mode"] != "wait" {
        return Ok(started);
    }
    let timeout = Duration::from_millis(c.args["timeoutMs"].as_u64().unwrap_or(600_000).clamp(1_000, 1_800_000));
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let status = task_state(c, &child)?;
        if status["workState"] != "working" || status["hasPendingQuestion"] == true || status["pendingApproval"] == true || tokio::time::Instant::now() >= deadline {
            let mut out = status;
            out["timedOut"] = json!(out["workState"] == "working" && tokio::time::Instant::now() >= deadline);
            return Ok(out);
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// A delegated task as `task_status` reports it.
fn task_state(c: &Call<'_>, id: &str) -> Result<Value, String> {
    let t = c.caller.peer(c.store(), id)?;
    let runs = c.store().records::<Run>(id).map_err(|e| e.to_string())?;
    let messages = c.store().records::<ConversationMessage>(id).map_err(|e| e.to_string())?;
    let requests = c.store().records::<domovoi_model::typed::RuntimeRequest>(id).map_err(|e| e.to_string())?;
    let open: Vec<Value> = requests.iter().filter_map(|r| serde_json::to_value(&**r).ok()).filter(|r| r["status"] == "pending").collect();
    let status = work_status(&runs);
    Ok(json!({ "taskId": id, "childThreadId": id, "title": t.title, "workState": status, "childRunId": active_run(&runs).map(|r| r.id.clone()),
        "hasPendingQuestion": open.iter().any(|r| r["kind"] == "user_input"), "pendingApproval": open.iter().any(|r| r["kind"] != "user_input"),
        "summary": if status == "working" { Value::Null } else { json!(latest_reply(&messages)) },
        "next": if open.is_empty() { Value::Null } else { json!("It waits on a request: answer it with pending_request, or tell the user.") } }))
}

async fn task(c: &Call<'_>, cancel: bool) -> Result<Value, String> {
    let id = c.required("taskId")?;
    let t = c.caller.peer(c.store(), &id)?;
    if t.delegated_from.as_ref().and_then(Option::as_ref).is_none_or(|from| from.thread_id.as_str() != c.caller.thread_id) {
        return Err(format!("{id} is not a task you delegated."));
    }
    if !cancel {
        return task_state(c, &id);
    }
    let runs = c.store().records::<Run>(&id).map_err(|e| e.to_string())?;
    for run in runs.iter().filter(|run| run.status == RunStatus::Queued) {
        let _ = c.dispatch(json!({ "type": "queued-run.cancel", "commandId": format!("mcp-cancel:{}", run.id), "threadId": id, "runId": run.id })).await;
    }
    let stopped = c.dispatch(json!({ "type": "thread.stop", "commandId": format!("mcp-stop:{}", crate::util::new_uuid()), "threadId": id, "reason": c.str("reason") })).await.is_ok();
    Ok(json!({ "taskId": id, "cancelled": stopped || work_status(&runs) != "working", "status": if work_status(&runs) == "working" { "cancel_requested" } else { "cancelled" } }))
}

async fn create_threads(c: &Call<'_>) -> Result<Value, String> {
    let specs = c.args["threads"].as_array().ok_or("threads is required.")?;
    if specs.is_empty() || specs.len() > MAX_BATCH_THREADS {
        return Err(format!("Give between 1 and {MAX_BATCH_THREADS} threads."));
    }
    let mut out = vec![];
    for (i, spec) in specs.iter().enumerate() {
        let mut spec = spec.clone();
        if let Some(target) = spec.get("target").cloned().filter(Value::is_object) {
            let model = child_model(c, &json!({ "target": target }))?;
            spec["modelSelection"] = json!(model);
        }
        let command_id = match c.str("clientRequestId") {
            Some(key) => format!("mcp-threads:{}:{key}:{i}", c.caller.thread_id),
            None => format!("mcp-threads:{}", crate::util::new_uuid()),
        };
        out.push(super::threads::launch_thread(c, &spec, command_id).await?);
    }
    Ok(json!({ "threads": out }))
}

async fn workflow_stop(c: &Call<'_>) -> Result<Value, String> {
    let subagent = c.required("subagentId")?;
    let runs = c.store().records::<Run>(&c.caller.thread_id).map_err(|e| e.to_string())?;
    let run = c.str("runId").or_else(|| active_run(&runs).map(|r| r.id.as_str().to_owned()));
    c.dispatch(json!({ "type": "subagent.stop", "commandId": c.command_id("subagent-stop"), "threadId": c.caller.thread_id, "runId": run, "subagentId": subagent })).await?;
    Ok(json!({}))
}

/// A task as the tools answer it.
fn task_view(t: &ScheduledTask) -> Value {
    let v = serde_json::to_value(t).unwrap_or(Value::Null);
    json!({ "scheduledTaskId": v["id"], "title": v["title"], "prompt": v["prompt"], "enabled": v["enabled"], "projectId": v["projectId"],
        "boundThreadId": v["threadId"], "threadMode": v["threadMode"], "freshSession": v["threadMode"] == "fresh_session", "schedule": v["schedule"], "nextRunAt": v["nextRunAt"],
        "lastRunStatus": v["lastRunStatus"], "lastRunError": v["lastRunError"], "webhookUrl": v["webhook"]["url"], "webhookPath": v["webhook"]["path"] })
}

/// One of the caller's project's tasks.
fn project_task(c: &Call<'_>, id: &str) -> Result<ScheduledTask, String> {
    let task = crate::scheduled_tasks::find(c.store(), id).map_err(|e| e.message)?.ok_or_else(|| format!("No scheduled task {id}."))?;
    if task.project_id.as_str() != c.caller.project_id {
        return Err(format!("Scheduled task {id} is not in this project."));
    }
    Ok(task)
}

/// `true`, `false`, or not given: a flag whose absence means something of its own.
fn tri(c: &Call<'_>, keys: &[&str]) -> Option<bool> {
    keys.iter().find_map(|k| c.args[*k].as_bool())
}

/// Where an agent's task works: a
/// task in a thread uses that thread's folder; one starting new threads gets
/// a worktree of the default branch each run, so runs do not share the
/// project's checkout (the project folder when it is no repository).
async fn agent_task_workspace(c: &Call<'_>, bound: bool) -> Option<WorkspaceStrategy> {
    if bound {
        return Some(WorkspaceStrategy::Root { branch: None });
    }
    let root = c.store().get_project(&c.caller.project_id).ok().flatten()?.workspace_root;
    let base = crate::vcs::default_branch_for(&root).await;
    Some(match base {
        Some(base_ref) => WorkspaceStrategy::Worktree { start_from_origin: Some(base_ref.contains('/')), base_ref, branch: None },
        None => WorkspaceStrategy::Root { branch: None },
    })
}

async fn schedule(c: &Call<'_>) -> Result<Value, String> {
    let existing = c.str("scheduledTaskId").map(|id| project_task(c, &id)).transpose()?;
    // A change leaves out what stays: the prompt and schedule are needed only by a new task.
    let prompt = match (&existing, c.str("prompt")) {
        (_, Some(prompt)) => prompt,
        (Some(e), None) => e.prompt.clone(),
        (None, None) => return Err("prompt is required.".into()),
    };
    // OpenCode sends objects as JSON text.
    let schedule = match (&c.args["schedule"], &existing) {
        (Value::Null, Some(e)) => serde_json::to_value(&e.schedule).unwrap_or(Value::Null),
        (Value::String(text), _) => serde_json::from_str::<Value>(text).map_err(|_| "schedule is not JSON.")?,
        (other, _) => other.clone(),
    };
    let schedule = UpsertSchedule::deserialize(&schedule).map_err(|_| "Could not save schedule task: that schedule is not one of interval, fixed_time, once or webhook.")?;
    if let UpsertSchedule::Other(ScheduledTaskSchedule::Interval(every)) = &schedule {
        if every.every_ms < 60_000 {
            return Err("A task runs at most once a minute: everyMs must be at least 60000.".into());
        }
    }
    let bind = tri(c, &["bindToCurrentThread", "thisThread"]);
    let fresh = tri(c, &["freshSession"]);
    // Another thread of this project, by id.
    let named = c.str("threadId");
    if named.is_some() && bind == Some(false) {
        return Err("threadId binds the task to that thread; leave out bindToCurrentThread: false.".into());
    }
    let bound_before = existing.as_ref().and_then(|e| e.thread_id.as_ref()).map(|t| t.as_str().to_owned());
    let thread_id = match (&named, bind) {
        (Some(thread), _) => Some(thread.clone()),
        (None, Some(true)) => Some(c.caller.thread_id.clone()),
        (None, Some(false)) => None,
        // Not said: a new task gets a new thread each time, a changed one keeps its thread.
        (None, None) => bound_before.clone(),
    };
    let was_mode = existing.as_ref().map(|e| e.thread_mode.clone().unwrap_or(if e.thread_id.is_some() { ScheduledTaskThreadMode::Continue } else { ScheduledTaskThreadMode::NewThread }));
    if existing.is_none() && fresh == Some(true) && thread_id.is_none() {
        return Err("freshSession needs bindToCurrentThread.".into());
    }
    // The server keeps a bound thread unless the task goes back to new
    // threads, so this pair would leave runs in the old thread.
    if bind == Some(false) && fresh == Some(true) && bound_before.is_some() {
        return Err("To move this task to a new thread with fresh sessions, first unbind it with bindToCurrentThread: false, then set freshSession: true in a second call.".into());
    }
    let mode = match was_mode {
        // A change that says nothing about where it runs keeps it.
        Some(was) if fresh.is_none() && bind.is_none() && named.is_none() => was,
        was if fresh.unwrap_or(bind != Some(false) && was == Some(ScheduledTaskThreadMode::FreshSession)) => ScheduledTaskThreadMode::FreshSession,
        was if thread_id.is_none() && (bind == Some(false) || was.as_ref().is_none_or(|w| *w == ScheduledTaskThreadMode::NewThread)) => ScheduledTaskThreadMode::NewThread,
        _ => ScheduledTaskThreadMode::Continue,
    };
    let title = c.str("title").or_else(|| c.str("name")).or_else(|| existing.as_ref().map(|t| t.title.clone())).unwrap_or_else(|| prompt.chars().take(60).collect());
    // A new task, or one whose binding flips, gets the folder that goes with
    // where it runs; any other change keeps its own.
    let workspace_strategy = match &existing {
        Some(e) if e.thread_id.is_none() == thread_id.is_none() => Some(e.workspace_strategy.clone()),
        _ => agent_task_workspace(c, thread_id.is_some()).await,
    };
    // The task's runtime and interaction mode are those of the thread that
    // schedules it; a changed task keeps its own.
    let t = &c.caller.thread;
    let input = UpsertInput {
        id: existing.as_ref().map(|e| e.id.as_str().to_owned()),
        command_id: Some(c.command_id("schedule")),
        require_existing: existing.as_ref().map(|_| true),
        title: Some(title),
        prompt: Some(prompt),
        enabled: Some(c.args["enabled"].as_bool().unwrap_or_else(|| existing.as_ref().is_none_or(|e| e.enabled))),
        schedule,
        project_id: Some(c.caller.project_id.clone()),
        thread_id,
        thread_mode: Some(mode),
        workspace_strategy,
        model_selection: existing.as_ref().map_or_else(|| t.model_selection.clone(), |e| e.model_selection.clone()),
        runtime_mode: Some(existing.as_ref().map_or_else(|| t.runtime_mode.clone(), |e| e.runtime_mode.clone())),
        interaction_mode: Some(existing.as_ref().map_or_else(|| t.interaction_mode.clone(), |e| e.interaction_mode.clone())),
        created_by: Some(existing.as_ref().map_or(Actor::Agent, |e| e.created_by.clone())),
        creation_source: Some(existing.as_ref().map_or(CreationSource::Mcp, |e| e.creation_source.clone())),
    };
    let task = crate::scheduled_tasks::upsert(c.store(), input, &jiff::Zoned::now()).map_err(|e| e.message)?;
    crate::scheduled_tasks::service(c.state).notify_changed();
    let mut out = task_view(&task);
    out["task"] = serde_json::to_value(&task).unwrap_or(Value::Null);
    Ok(out)
}

fn list_tasks(c: &Call<'_>) -> Result<Value, String> {
    let tasks = crate::scheduled_tasks::list(c.store()).map_err(|e| e.message)?;
    Ok(json!({ "tasks": tasks.iter().filter(|t| t.project_id.as_str() == c.caller.project_id).map(task_view).collect::<Vec<_>>() }))
}

fn delete_task(c: &Call<'_>) -> Result<Value, String> {
    let id = c.required("scheduledTaskId")?;
    project_task(c, &id)?;
    crate::scheduled_tasks::delete(c.store(), &id).map_err(|e| e.message)?;
    crate::scheduled_tasks::service(c.state).notify_changed();
    Ok(json!({ "scheduledTaskId": id, "deleted": true }))
}

async fn run_now(c: &Call<'_>) -> Result<Value, String> {
    let id = c.str("taskId").or_else(|| c.str("scheduledTaskId")).ok_or("taskId is required.")?;
    project_task(c, &id)?;
    let (ran, thread) = crate::scheduled_tasks::service(c.state).run_now_into(&id).await.map_err(|e| e.message)?;
    let v = task_view(&ran);
    // The thread the run went to: for a new-thread task, the one it started.
    let thread = thread.map_or_else(|| v["boundThreadId"].clone(), Value::from);
    Ok(json!({ "taskId": id, "threadId": thread, "lastRunStatus": v["lastRunStatus"], "lastRunError": v["lastRunError"], "runCount": ran.run_count, "nextRunAt": v["nextRunAt"] }))
}

async fn project_info(c: &Call<'_>) -> Result<Value, String> {
    let p = c.store().get_project(&c.caller.project_id).ok().flatten().ok_or("The project is gone.")?;
    let settings = c.state.settings.read().await.clone();
    let scripts = crate::project_scripts::resolve(&settings, &p.id, &p.scripts, &p.workspace_root);
    Ok(json!({ "projectId": p.id, "title": p.title, "workspaceRoot": p.workspace_root, "threadId": c.caller.thread_id,
        "worktreePath": c.caller.thread.worktree_path, "branch": c.caller.thread.branch, "scripts": scripts }))
}
