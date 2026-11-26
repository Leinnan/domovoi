//! Thread tools: list, read,
//! search, message, wait for, launch, configure, interrupt, rename, organize,
//! fork and merge back threads of the caller's project, and manage their
//! queues and pending requests.

#![expect(clippy::disallowed_types, clippy::disallowed_macros, reason = "the MCP wire: tool schemas, tool arguments and tool results")]

use super::{Call, active_run, b, by_time, latest_reply, n, object, one_of, s, tool, work_status};
use serde_json::{Value, json};
use std::time::Duration;
use domovoi_model::typed::{AppThread, ConversationMessage, Run, RunStatus, RuntimeRequest, TurnItem};

/// The most a thread's text read returns per item, by default.
const MAX_CHARS_PER_ITEM: usize = 20_000;

pub fn tools() -> Vec<Value> {
    vec![
        tool("thread_list", "List threads of this project (not this one): id, title, status (working, result_available, failed, stopped, idle), last update. Newest first.",
            json!({
                "statuses": { "type": "array", "items": { "type": "string" }, "description": "Only threads with one of these statuses." },
                "titleContains": s("Only threads whose title contains this (any case)."),
                "settled": b("Only settled threads (true) or only unsettled ones (false)."),
                "snoozed": b("Only snoozed threads (true) or only others (false)."),
                "includeSubagents": b("Include subagents' child threads (default false)."),
                "cursor": n("From `nextCursor`."),
                "limit": n("At most this many (default 20, at most 100)."),
                "environmentId": s("Linked environment to list (every project there). Omit for this one. Ids are then that environment's."),
            }), &[], true),
        tool("thread_read", "Read a thread of this project: its settings, status, recent runs and latest messages (view `messages`, default) or latest work items (view `activity`).",
            json!({
                "threadId": s("The thread to read."),
                "view": one_of(&["messages", "activity"], "messages (default) or activity."),
                "limit": n("Latest messages or items to return (default 20, at most 100)."),
                "runLimit": n("Recent runs to list (default 5, at most 50)."),
                "maxCharsPerItem": n("Cut each text at this many characters (default 20000)."),
            }), &["threadId"], true),
        tool("thread_search", "Search this project's threads by title and message text.",
            json!({ "query": s("What to look for."), "limit": n("At most this many threads (default 20, at most 100).") }), &["query"], true),
        tool("thread_send", "Send a message to another thread of this project, as its user would. `mode`: auto (default: starts it, or queues behind work in progress), queue, steer (into the running turn), restart (stop the running turn and start with this). `freshContext` starts a new agent session without the conversation so far.",
            json!({
                "threadId": s("The thread to message."), "message": s("What to send."),
                "mode": one_of(&["auto", "queue", "steer", "restart"], "How it reaches the thread (default auto)."),
                "freshContext": b("Start a new agent session for it."),
                "clientRequestId": s("Repeat the same id to retry without sending twice."),
            }), &["threadId", "message"], false),
        tool("thread_wait", "Wait until a thread of this project (or one of its runs) has finished, then return its status and last reply.",
            json!({
                "threadId": s("The thread to wait for."), "runId": s("Wait for this run only."),
                "timeoutMs": n("Give up after this long (default 300000, at most 1800000)."),
                "timeoutSeconds": n("The same, in seconds."),
            }), &["threadId"], true),
        tool("thread_launch", "Start a new top-level thread in this project, only when the user asks for a new thread (a subagent or delegated task is `delegate_task`). Without `workspaceStrategy` it works in the project's folder.",
            json!({
                "title": s("The thread's title."), "message": s("Its first message; none leaves it waiting."),
                "modelSelection": object("{\"instanceId\":\"claudeAgent\",\"model\":\"...\"}; default: yours."),
                "runtimeMode": one_of(&["approval-required", "auto-accept-edits", "full-access"], "Default: yours."),
                "interactionMode": one_of(&["default", "plan"], "Default: default."),
                "workspaceStrategy": object("{\"type\":\"root\"}, {\"type\":\"worktree\",\"baseRef\":\"main\",\"startFromOrigin\":true} or {\"type\":\"existing_worktree\",\"worktreePath\":\"...\"}."),
                "clientRequestId": s("Repeat the same id to retry without launching twice."),
            }), &["title"], false),
        tool("thread_configure", "Read a thread's model, runtime mode and interaction mode; with `modelSelection`, switch its model for the next turns.",
            json!({ "threadId": s("Default: this thread."), "modelSelection": object("{\"instanceId\":\"codex\",\"model\":\"...\"}.") }), &[], false),
        tool("thread_interrupt", "Interrupt another thread's running turn (`stop` also stops what is queued behind it). Not for delegated tasks: use `task_cancel`.",
            json!({ "threadId": s("The thread."), "runId": s("Only this run."), "reason": s("Why, for its transcript."), "stop": b("Stop the thread: the turn and its queue.") }), &["threadId"], false),
        tool("thread_update", "Rename a thread (`rename` with `title`) or have its title written again from the conversation (`regenerate_title`).",
            json!({ "threadId": s("Default: this thread."), "action": one_of(&["rename", "regenerate_title"], "What to do."), "title": s("The new title, for rename.") }), &["action"], false),
        tool("thread_organize", "Pin, unpin, snooze (until `snoozedUntil`), unsnooze, settle, unsettle, archive, unarchive or mark unread a thread.",
            json!({
                "threadId": s("Default: this thread."),
                "action": one_of(&["pin", "unpin", "snooze", "unsnooze", "settle", "unsettle", "archive", "unarchive", "mark_unread"], "What to do."),
                "snoozedUntil": s("ISO time, for snooze."),
            }), &["action"], false),
        tool("thread_transfer", "Fork a thread (a new thread continuing its conversation), merge a fork's work back into the thread it was forked from, or list a thread's transfers.",
            json!({
                "action": one_of(&["fork", "merge_back", "status"], "What to do."),
                "threadId": s("The thread to fork, or the fork to merge back; default: this thread."),
                "title": s("The fork's title."),
            }), &["action"], false),
        tool("queue", "A thread's queued messages: list, read, edit (`text`), cancel, reorder (before `beforeRunId`, or last) or promote_to_steer (into the running `targetRunId`).",
            json!({
                "action": one_of(&["list", "read", "edit", "cancel", "reorder", "promote_to_steer"], "What to do."),
                "threadId": s("Default: this thread."), "queuedRunId": s("The queued message's run."), "text": s("New text, for edit."),
                "beforeRunId": s("For reorder: put it before this queued run (none: last)."), "targetRunId": s("For promote_to_steer."),
            }), &["action"], false),
        tool("pending_request", "Approvals and questions another thread of this project waits on: list them, read one, or respond (`decision` accept, acceptForSession, decline, cancel; or `answers` by question id).",
            json!({
                "action": one_of(&["list", "read", "respond"], "What to do."),
                "threadId": s("The thread."), "requestId": s("The request."),
                "decision": one_of(&["accept", "acceptForSession", "decline", "cancel"], "For an approval."),
                "answers": object("For questions: {\"<question id>\": [\"<option label or text>\"]}."),
            }), &["action", "threadId"], false),
    ]
}

pub async fn call(c: &Call<'_>, name: &str) -> Option<Result<Value, String>> {
    Some(match name {
        "thread_list" => list(c),
        "thread_read" => read(c),
        "thread_search" => search(c),
        "thread_send" => send(c).await,
        "thread_wait" => wait(c).await,
        "thread_launch" => launch(c).await,
        "thread_configure" => configure(c).await,
        "thread_interrupt" => interrupt(c).await,
        "thread_update" => update(c).await,
        "thread_organize" => organize(c).await,
        "thread_transfer" => transfer(c).await,
        "queue" => queue(c).await,
        "pending_request" => pending(c).await,
        _ => return None,
    })
}

fn runs_of(c: &Call<'_>, id: &str) -> Result<Vec<std::sync::Arc<Run>>, String> {
    c.store().records::<Run>(id).map_err(|e| e.to_string())
}

fn is_subagent_child(t: &AppThread) -> bool {
    serde_json::to_value(&t.lineage).ok().is_some_and(|l| l["relationshipToParent"] == "subagent")
}

fn list(c: &Call<'_>) -> Result<Value, String> {
    let store = c.store();
    let statuses: Option<Vec<String>> = c.args["statuses"].as_array().map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_owned)).collect());
    let title = c.str("titleContains").map(|t| t.to_lowercase());
    let mut threads: Vec<AppThread> = store
        .app_threads()
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|t| t.project_id.as_str() == c.caller.project_id && t.id.as_str() != c.caller.thread_id && t.deleted_at.is_none())
        .filter(|t| c.flag("includeSubagents") || !is_subagent_child(t))
        .filter(|t| title.as_ref().is_none_or(|q| t.title.to_lowercase().contains(q)))
        .filter(|t| c.args["settled"].as_bool().is_none_or(|want| t.settled_at.as_ref().and_then(Option::as_ref).is_some() == want))
        .filter(|t| c.args["snoozed"].as_bool().is_none_or(|want| t.snoozed_until.as_ref().and_then(Option::as_ref).is_some() == want))
        .collect();
    threads.sort_by(|a, b| b.updated_at.as_str().cmp(a.updated_at.as_str()));
    let mut rows = vec![];
    for t in threads {
        let status = store.records::<Run>(t.id.as_str()).map(|runs| work_status(&runs)).unwrap_or("idle");
        if statuses.as_ref().is_some_and(|s| !s.iter().any(|w| w == status)) {
            continue;
        }
        rows.push(json!({ "threadId": t.id, "title": t.title, "status": status, "updatedAt": t.updated_at, "archived": t.archived_at.is_some(),
            "model": t.model_selection.model, "providerInstanceId": t.model_selection.instance_id }));
    }
    let from = c.args["cursor"].as_u64().unwrap_or(0) as usize;
    let page: Vec<Value> = rows.iter().skip(from).take(c.limit(20, 100)).cloned().collect();
    let next = (from + page.len() < rows.len()).then_some(from + page.len());
    Ok(json!({ "threads": page, "nextCursor": next }))
}

fn cut(text: &str, max: usize) -> Value {
    if text.chars().count() <= max {
        return json!(text);
    }
    json!(format!("{}… [cut at {max} characters]", text.chars().take(max).collect::<String>()))
}

fn read(c: &Call<'_>) -> Result<Value, String> {
    let id = c.required("threadId")?;
    let t = if id == c.caller.thread_id { c.caller.thread.clone() } else { c.caller.peer(c.store(), &id)? };
    let runs = runs_of(c, &id)?;
    let max = c.args["maxCharsPerItem"].as_u64().map_or(MAX_CHARS_PER_ITEM, |m| m.clamp(100, MAX_CHARS_PER_ITEM as u64) as usize);
    let n = c.limit(20, 100);
    let mut recent: Vec<&Run> = runs.iter().map(|r| &**r).collect();
    recent.sort_by_key(|r| std::cmp::Reverse(r.ordinal));
    let run_limit = c.args["runLimit"].as_u64().unwrap_or(5).clamp(1, 50) as usize;
    let recent_runs: Vec<Value> = recent.iter().take(run_limit).map(|r| json!({ "runId": r.id, "ordinal": r.ordinal, "status": r.status, "requestedAt": r.requested_at })).collect();
    let detail = json!({ "threadId": id, "title": t.title, "status": work_status(&runs), "providerInstanceId": t.model_selection.instance_id, "model": t.model_selection.model,
        "runtimeMode": t.runtime_mode, "interactionMode": t.interaction_mode, "worktreePath": t.worktree_path, "branch": t.branch,
        "activeRunId": active_run(&runs).map(|r| r.id.clone()), "recentRuns": recent_runs });
    if c.args["view"] == "activity" {
        let mut items: Vec<Value> = c.store().records::<TurnItem>(&id).map_err(|e| e.to_string())?.iter().filter_map(|i| serde_json::to_value(&**i).ok()).collect();
        items.sort_by_key(|i| i["ordinal"].as_u64().unwrap_or(0));
        let skip = items.len().saturating_sub(n);
        let shown: Vec<Value> = items[skip..]
            .iter()
            .map(|i| {
                let text = ["text", "input", "markdown", "title", "prompt"].iter().find_map(|k| i[*k].as_str()).unwrap_or_default();
                json!({ "itemId": i["id"], "type": i["type"], "status": i["status"], "runId": i["runId"], "text": cut(text, max), "output": i["output"].as_str().map(|o| cut(o, max)) })
            })
            .collect();
        return Ok(json!({ "thread": detail, "omittedItems": skip, "items": shown }));
    }
    let stored = c.store().records::<ConversationMessage>(&id).map_err(|e| e.to_string())?;
    let msgs = by_time(&stored);
    let skip = msgs.len().saturating_sub(n);
    let messages: Vec<Value> = msgs[skip..].iter().map(|m| json!({ "role": m.role, "text": cut(&m.text, max), "createdAt": m.created_at, "runId": m.run_id })).collect();
    Ok(json!({ "thread": detail, "threadId": id, "title": t.title, "status": work_status(&runs), "omittedMessages": skip, "messages": messages }))
}

fn search(c: &Call<'_>) -> Result<Value, String> {
    let query = c.required("query")?;
    let found = crate::rpc::orchestration::search_in_project(c.state, &query, &c.caller.project_id, c.limit(20, 100)).map_err(|e| e.to_string())?;
    Ok(json!({ "currentThreadId": c.caller.thread_id, "matches": found }))
}

async fn send(c: &Call<'_>) -> Result<Value, String> {
    let id = c.required("threadId")?;
    let text = c.str("message").or_else(|| c.str("text")).ok_or("message is required.")?;
    if id == c.caller.thread_id {
        return Err("Use your own reply to talk in this thread.".into());
    }
    c.caller.peer(c.store(), &id)?;
    let runs = runs_of(c, &id)?;
    let active = active_run(&runs).map(|r| r.id.as_str().to_owned());
    let mode = c.str("mode").unwrap_or_else(|| "auto".into());
    let dispatch_mode = match (mode.as_str(), &active) {
        ("steer", Some(run)) => json!({ "type": "steer_active", "targetRunId": run }),
        ("steer", None) => return Err("Nothing is running there to steer; send it with mode queue.".into()),
        ("restart", Some(run)) => json!({ "type": "restart_active", "targetRunId": run }),
        _ => json!({ "type": "queue_after_active" }),
    };
    let command_id = c.command_id("send");
    let mid = format!("message:{command_id}");
    let mut command = json!({ "type": "message.dispatch", "commandId": command_id, "threadId": id, "messageId": mid, "text": text, "attachments": [],
        "createdBy": "agent", "creationSource": "mcp", "dispatchMode": dispatch_mode });
    if c.flag("freshContext") {
        command["freshContext"] = json!(true);
    }
    c.dispatch(command).await?;
    let stored = c.store().records::<ConversationMessage>(&id).map_err(|e| e.to_string())?;
    let run_id = stored.iter().find(|m| m.id.as_str() == mid).and_then(|m| m.run_id.clone());
    let runs = runs_of(c, &id)?;
    let run = run_id.as_ref().and_then(|r| runs.iter().find(|x| x.id == *r));
    let delivery = match (mode.as_str(), run.map(|r| &r.status)) {
        ("steer", _) if run.is_some_and(|r| Some(r.id.as_str()) == active.as_deref()) => "steered",
        ("restart", _) if active.is_some() => "restarted",
        (_, Some(RunStatus::Queued)) => "queued",
        _ => "started",
    };
    Ok(json!({ "threadId": id, "messageId": mid, "runId": run_id, "status": run.map(|r| r.status.clone()), "delivery": delivery }))
}

async fn wait(c: &Call<'_>) -> Result<Value, String> {
    let id = c.required("threadId")?;
    c.caller.peer(c.store(), &id)?;
    let timeout = c.args["timeoutMs"].as_u64().map(Duration::from_millis).or_else(|| c.args["timeoutSeconds"].as_u64().map(Duration::from_secs)).unwrap_or(Duration::from_secs(300));
    let deadline = tokio::time::Instant::now() + timeout.clamp(Duration::from_secs(1), Duration::from_secs(1800));
    let run_id = c.str("runId");
    loop {
        let runs = runs_of(c, &id)?;
        let (done, status) = match &run_id {
            Some(r) => {
                let run = runs.iter().find(|x| x.id.as_str() == r).ok_or_else(|| format!("No run {r} in {id}."))?;
                let settled = !(run.status == RunStatus::Queued || crate::orchestration::commands::ACTIVE_RUN.contains(&run.status.as_str()));
                (settled, serde_json::to_value(&run.status).unwrap_or(Value::Null))
            }
            None => {
                let status = work_status(&runs);
                (status != "working", json!(status))
            }
        };
        if done {
            let messages = c.store().records::<ConversationMessage>(&id).map_err(|e| e.to_string())?;
            return Ok(json!({ "threadId": id, "runId": run_id, "status": status, "timedOut": false, "lastReply": latest_reply(&messages) }));
        }
        if tokio::time::Instant::now() >= deadline {
            return Ok(json!({ "threadId": id, "runId": run_id, "status": status, "timedOut": true }));
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

/// Launches a top-level thread of the caller's project; the launch's projection as the tool answers it.
pub(crate) async fn launch_thread(c: &Call<'_>, spec: &Value, command_id: String) -> Result<Value, String> {
    let t = &c.caller.thread;
    let thread_id = format!("thread:project:{}:{}", c.caller.project_id, crate::util::new_uuid());
    let model = spec.get("modelSelection").filter(|m| m.is_object()).cloned().unwrap_or_else(|| json!(t.model_selection));
    let mut input = json!({
        "commandId": command_id, "creationSource": "mcp", "createdBy": "agent", "threadId": thread_id, "projectId": c.caller.project_id,
        "title": spec["title"].as_str().filter(|s| !s.trim().is_empty()).unwrap_or("New thread"), "generateTitle": spec["title"].as_str().is_none(),
        "modelSelection": model, "runtimeMode": spec.get("runtimeMode").cloned().filter(Value::is_string).unwrap_or_else(|| json!(t.runtime_mode)),
        "interactionMode": spec.get("interactionMode").cloned().filter(Value::is_string).unwrap_or_else(|| json!("default")),
        "workspaceStrategy": spec.get("workspaceStrategy").cloned().filter(Value::is_object).unwrap_or_else(|| json!({ "type": "root" })),
    });
    if let Some(text) = spec["message"].as_str().or(spec["prompt"].as_str()).filter(|s| !s.trim().is_empty()) {
        input["initialMessage"] = json!({ "text": text, "attachments": [] });
    }
    let launched = crate::rpc::shell::launch_thread(c.orch(), input).await.map_err(|e| e.to_string())?;
    let v = serde_json::to_value(&launched).unwrap_or(Value::Null);
    let thread = &v["projection"]["thread"];
    let run = v["projection"]["runs"].as_array().and_then(|r| r.last()).cloned().unwrap_or(Value::Null);
    Ok(json!({ "threadId": thread["id"], "projectId": thread["projectId"], "title": thread["title"], "modelSelection": thread["modelSelection"],
        "worktreePath": thread["worktreePath"], "runId": run["id"], "status": run["status"] }))
}

async fn launch(c: &Call<'_>) -> Result<Value, String> {
    c.required("title")?;
    launch_thread(c, c.args, c.command_id("launch")).await
}

async fn configure(c: &Call<'_>) -> Result<Value, String> {
    let (id, t) = c.thread()?;
    if let Some(selection) = c.args.get("modelSelection").filter(|m| m.is_object()) {
        c.dispatch(json!({ "type": "thread.model-selection.set", "commandId": c.command_id("configure"), "threadId": id, "modelSelection": selection })).await?;
        let t = c.store().app_thread(&id).ok().flatten().unwrap_or(t);
        return Ok(json!({ "threadId": id, "modelSelection": t.model_selection, "runtimeMode": t.runtime_mode, "interactionMode": t.interaction_mode }));
    }
    Ok(json!({ "threadId": id, "modelSelection": t.model_selection, "runtimeMode": t.runtime_mode, "interactionMode": t.interaction_mode }))
}

async fn interrupt(c: &Call<'_>) -> Result<Value, String> {
    let id = c.required("threadId")?;
    if id == c.caller.thread_id {
        return Err("To stop this thread, end your turn.".into());
    }
    c.caller.peer(c.store(), &id)?;
    let runs = runs_of(c, &id)?;
    let target = c.str("runId").or_else(|| active_run(&runs).map(|r| r.id.as_str().to_owned()));
    if c.flag("stop") {
        c.dispatch(json!({ "type": "thread.stop", "commandId": c.command_id("stop"), "threadId": id, "reason": c.str("reason") })).await?;
        return Ok(json!({ "threadId": id, "runId": target, "status": "interrupt_requested" }));
    }
    let Some(run) = target else { return Ok(json!({ "threadId": id, "runId": null, "status": "no_active_run" })) };
    c.dispatch(json!({ "type": "run.interrupt", "commandId": c.command_id("interrupt"), "threadId": id, "runId": run, "reason": c.str("reason") })).await?;
    Ok(json!({ "threadId": id, "runId": run, "status": "interrupt_requested" }))
}

async fn update(c: &Call<'_>) -> Result<Value, String> {
    let (id, _) = c.thread()?;
    let mut command = json!({ "type": "thread.metadata.update", "commandId": c.command_id("update"), "threadId": id });
    match c.required("action")?.as_str() {
        "rename" => command["title"] = json!(c.required("title")?),
        "regenerate_title" => command["regenerateTitle"] = json!(true),
        other => return Err(format!("Unknown action {other}.")),
    }
    let done = c.dispatch(command).await?;
    let t = c.store().app_thread(&id).ok().flatten();
    Ok(json!({ "threadId": id, "sequence": done["sequence"], "title": t.as_ref().map(|t| t.title.clone()), "titleRegeneration": t.map(|t| t.title_regeneration) }))
}

async fn organize(c: &Call<'_>) -> Result<Value, String> {
    let (id, _) = c.thread()?;
    let action = c.required("action")?;
    let ty = match action.as_str() {
        "pin" => "thread.pin",
        "unpin" => "thread.unpin",
        "snooze" => "thread.snooze",
        "unsnooze" => "thread.unsnooze",
        "settle" => "thread.settle",
        "unsettle" => "thread.unsettle",
        "archive" => "thread.archive",
        "unarchive" => "thread.unarchive",
        "mark_unread" => "thread.mark-unread",
        other => return Err(format!("Unknown action {other}.")),
    };
    let mut command = json!({ "type": ty, "commandId": c.command_id("organize"), "threadId": id });
    if action == "snooze" {
        command["snoozedUntil"] = json!(c.required("snoozedUntil")?);
    }
    let done = c.dispatch(command).await?;
    Ok(json!({ "threadId": id, "action": action, "sequence": done["sequence"] }))
}

async fn transfer(c: &Call<'_>) -> Result<Value, String> {
    let (id, t) = c.thread()?;
    match c.required("action")?.as_str() {
        "fork" => {
            let target = format!("thread:project:{}:{}", c.caller.project_id, crate::util::new_uuid());
            let mut command = json!({ "type": "thread.fork", "commandId": c.command_id("fork"), "sourceThreadId": id, "targetThreadId": target,
                "sourcePoint": { "type": "latest_stable" }, "createdBy": "agent", "creationSource": "mcp" });
            if let Some(title) = c.str("title") {
                command["title"] = json!(title);
            }
            let done = c.dispatch(command).await?;
            Ok(json!({ "sequence": done["sequence"], "targetThreadId": target }))
        }
        "merge_back" => {
            let parent = serde_json::to_value(&t.lineage).ok().and_then(|l| l["parentThreadId"].as_str().map(str::to_owned)).ok_or("This thread is not a fork.")?;
            let done = c
                .dispatch(json!({ "type": "thread.merge_back", "commandId": c.command_id("merge"), "sourceThreadId": id, "targetThreadId": parent,
                    "sourcePoint": { "type": "latest_stable" }, "createdBy": "agent", "creationSource": "mcp" }))
                .await?;
            Ok(json!({ "sequence": done["sequence"], "targetThreadId": parent }))
        }
        "status" => {
            let outline = c.store().thread_outline(&id).map_err(|e| e.to_string())?;
            let transfers: Vec<Value> = outline.context_transfers.iter().map(|x| json!({ "id": x.id, "sourceThreadId": x.source_thread_id, "targetThreadId": x.target_thread_id, "status": x.status, "kind": x.kind })).collect();
            Ok(json!({ "transfers": transfers }))
        }
        other => Err(format!("Unknown action {other}.")),
    }
}

async fn queue(c: &Call<'_>) -> Result<Value, String> {
    let (id, _) = c.thread()?;
    let runs = runs_of(c, &id)?;
    let mut queued: Vec<&Run> = runs.iter().filter(|r| r.status == RunStatus::Queued).map(|r| &**r).collect();
    queued.sort_by_key(|r| r.queue_position.flatten().unwrap_or(u64::MAX));
    let messages = c.store().records::<ConversationMessage>(&id).map_err(|e| e.to_string())?;
    let text_of = |r: &Run| messages.iter().find(|m| m.id == r.user_message_id).map(|m| m.text.clone()).unwrap_or_default();
    let run_arg = || c.required("queuedRunId");
    match c.required("action")?.as_str() {
        "list" => Ok(json!({ "items": queued.iter().map(|r| { let t = text_of(r); json!({ "queuedRunId": r.id, "text": cut(&t, 500), "truncated": t.chars().count() > 500 }) }).collect::<Vec<_>>() })),
        "read" => {
            let run = run_arg()?;
            let r = queued.iter().find(|r| r.id.as_str() == run).ok_or_else(|| format!("No queued run {run}."))?;
            Ok(json!({ "queuedRunId": r.id, "text": text_of(r) }))
        }
        "edit" => c.dispatch(json!({ "type": "queued-run.edit", "commandId": c.command_id("queue-edit"), "threadId": id, "runId": run_arg()?, "text": c.required("text")? })).await,
        "cancel" => c.dispatch(json!({ "type": "queued-run.cancel", "commandId": c.command_id("queue-cancel"), "threadId": id, "runId": run_arg()? })).await,
        "reorder" => c.dispatch(json!({ "type": "queued-run.reorder", "commandId": c.command_id("queue-reorder"), "threadId": id, "runId": run_arg()?, "beforeRunId": c.str("beforeRunId") })).await,
        "promote_to_steer" => {
            let target = c.str("targetRunId").or_else(|| active_run(&runs).map(|r| r.id.as_str().to_owned())).ok_or("Nothing is running to steer into.")?;
            c.dispatch(json!({ "type": "queued-message.promote-to-steer", "commandId": c.command_id("queue-steer"), "threadId": id, "queuedRunId": run_arg()?, "targetRunId": target })).await
        }
        other => Err(format!("Unknown action {other}.")),
    }
}

async fn pending(c: &Call<'_>) -> Result<Value, String> {
    let id = c.required("threadId")?;
    if id == c.caller.thread_id {
        return Err("Your own requests are answered by the user.".into());
    }
    c.caller.peer(c.store(), &id)?;
    let requests = c.store().records::<RuntimeRequest>(&id).map_err(|e| e.to_string())?;
    let open: Vec<Value> = requests.iter().filter_map(|r| serde_json::to_value(&**r).ok()).filter(|r| r["status"] == "pending").collect();
    match c.required("action")?.as_str() {
        "list" => Ok(json!({ "requests": open.iter().map(|r| json!({ "requestId": r["id"], "kind": r["kind"], "title": r["title"], "prompt": r["prompt"] })).collect::<Vec<_>>() })),
        "read" => {
            let rid = c.required("requestId")?;
            let r = open.iter().find(|r| r["id"] == json!(rid)).ok_or_else(|| format!("No pending request {rid}."))?;
            let items = c.store().records::<TurnItem>(&id).map_err(|e| e.to_string())?;
            let questions = items.iter().filter_map(|i| serde_json::to_value(&**i).ok()).find(|i| i["requestId"] == json!(rid)).map(|i| i["questions"].clone());
            Ok(json!({ "request": r, "questions": questions }))
        }
        "respond" => {
            let rid = c.required("requestId")?;
            let mut command = json!({ "type": "runtime-request.respond", "commandId": c.command_id("respond"), "threadId": id, "requestId": rid });
            match (c.str("decision"), c.args.get("answers").filter(|a| a.is_object())) {
                (Some(d), _) => command["decision"] = json!(d),
                (None, Some(a)) => command["answers"] = a.clone(),
                (None, None) => return Err("Give a decision or answers.".into()),
            }
            c.dispatch(command).await
        }
        other => Err(format!("Unknown action {other}.")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_text_is_cut_and_says_so() {
        assert_eq!(cut("short", 10), json!("short"));
        assert_eq!(cut("abcdef", 3), json!("abc… [cut at 3 characters]"));
    }
}
