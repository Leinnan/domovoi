//! Worktree tools (t3code `worktree` toolkit): the caller's worktree, the
//! project's branches, and handing the caller's thread off to a new
//! worktree.
//!
//! A handoff creates the worktree and moves the thread into it. The turn in
//! progress keeps its folder; the next one starts a new agent session in the
//! worktree, which the conversation so far is handed to (a session never
//! moves folders), so the conversation carries on there.

#![expect(clippy::disallowed_types, clippy::disallowed_macros, reason = "the MCP wire: tool schemas, tool arguments and tool results")]

use super::{Call, b, n, one_of, s, tool};
use serde_json::{Value, json};
use std::collections::HashSet;
use std::sync::{Mutex, OnceLock};

pub fn tools() -> Vec<Value> {
    vec![
        tool("worktree_status", "This thread's worktree: attached or not, its path and branch, and the project's root folder. Check before `worktree_handoff`.",
            json!({}), &[], true),
        tool("worktree_list", "The project's branches (local and remote), with the worktree each is checked out in.",
            json!({ "query": s("Only branches whose name contains this."), "limit": n("At most this many (default 50, at most 500)."),
                "refKind": one_of(&["all", "local", "remote"], "Which branches (default all).") }), &[], true),
        tool("worktree_handoff", "Move this thread into a new git worktree on a new `branch` (from `baseRef`, default the project's current branch), running the project's setup script there. Your current turn keeps its folder: end it after this call. The next turn runs in the worktree with the conversation so far; `continuationPrompt` starts it at once.",
            json!({
                "branch": s("The new branch."), "baseRef": s("What it starts from (default: the project's current branch)."),
                "runSetupScript": b("Run the project's worktree setup script (default true)."),
                "continuationPrompt": s("What the next turn, in the worktree, is asked."),
            }), &["branch"], false),
    ]
}

pub async fn call(c: &Call<'_>, name: &str) -> Option<Result<Value, String>> {
    Some(match name {
        "worktree_status" => status(c),
        "worktree_list" => list(c).await,
        "worktree_handoff" => handoff(c).await,
        _ => return None,
    })
}

fn status(c: &Call<'_>) -> Result<Value, String> {
    let t = &c.caller.thread;
    Ok(json!({ "attached": t.worktree_path.is_some(), "worktreePath": t.worktree_path, "branch": t.branch, "projectWorkspaceRoot": c.project_folder(), "defaultStartFromOrigin": false }))
}

async fn list(c: &Call<'_>) -> Result<Value, String> {
    let root = c.project_folder().ok_or("The project is gone.")?;
    let query = c.str("query");
    let mut refs = serde_json::to_value(crate::vcs::list_refs(&root, query.as_deref(), c.limit(50, 500)).await).unwrap_or(Value::Null);
    if let (Some(kind @ ("local" | "remote")), Some(list)) = (c.args["refKind"].as_str(), refs["refs"].as_array_mut()) {
        list.retain(|r| (r["isRemote"] == true) == (kind == "remote"));
    }
    Ok(refs)
}

/// Threads with a handoff in progress: one at a time each, so none makes two worktrees.
fn in_flight() -> &'static Mutex<HashSet<String>> {
    static H: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    H.get_or_init(Mutex::default)
}

struct Claim(String);

impl Drop for Claim {
    fn drop(&mut self) {
        crate::util::locked(in_flight()).remove(&self.0);
    }
}

async fn handoff(c: &Call<'_>) -> Result<Value, String> {
    let branch = c.required("branch")?;
    if c.caller.thread.worktree_path.is_some() {
        return Err("already_in_worktree: this thread already works in a worktree.".into());
    }
    if !crate::util::locked(in_flight()).insert(c.caller.thread_id.clone()) {
        return Err("handoff_in_progress: a worktree handoff is already under way for this thread.".into());
    }
    let _claim = Claim(c.caller.thread_id.clone());
    let project = c.store().get_project(&c.caller.project_id).ok().flatten().ok_or("The project is gone.")?;
    let root = project.workspace_root.clone();
    let base = match c.str("baseRef") {
        Some(base) => base,
        None => crate::vcs::checked_out_branch(&root).await.ok_or("Could not tell the project's current branch (detached HEAD?). Pass baseRef.")?,
    };
    let created = crate::vcs_ops::create_worktree(&root, &base, Some(&branch), None).await.map_err(|e| e.detail)?;
    let path = created.worktree.path;
    let bound = c
        .dispatch(json!({ "type": "thread.metadata.update", "commandId": c.command_id("handoff"), "threadId": c.caller.thread_id,
            "worktreePath": path, "branch": branch }))
        .await;
    if let Err(e) = bound {
        // The thread did not move: the worktree would be left over.
        let mut remove = crate::process::git(&root);
        remove.args(["worktree", "remove", "--force", &path]);
        let _ = crate::process::output(remove, crate::process::SLOW).await;
        return Err(e);
    }
    let setup = if c.args["runSetupScript"] == false {
        json!({ "status": "skipped" })
    } else {
        let settings = c.state.settings.read().await.clone();
        let scripts = crate::project_scripts::resolve(&settings, &project.id, &project.scripts, &root);
        match crate::project_scripts::setup_script(&scripts) {
            None => json!({ "status": "no-script" }),
            Some(script) => {
                let terminal = format!("setup-{}", script.id());
                match crate::project_scripts::run(&c.caller.thread_id, &terminal, script, &root, Some(&path), &path) {
                    Ok(()) => json!({ "status": "started", "terminalId": terminal }),
                    Err(e) => json!({ "status": "failed", "detail": e }),
                }
            }
        }
    };
    let continuation = match c.str("continuationPrompt") {
        None => json!({ "status": "skipped" }),
        Some(text) => {
            let command_id = c.command_id("handoff-continue");
            let sent = c
                .dispatch(json!({ "type": "message.dispatch", "commandId": command_id, "threadId": c.caller.thread_id, "messageId": format!("message:{command_id}"),
                    "text": text, "attachments": [], "createdBy": "agent", "creationSource": "mcp", "dispatchMode": { "type": "queue_after_active" } }))
                .await;
            match sent {
                Ok(_) => json!({ "status": "scheduled", "delivery": "queued" }),
                Err(e) => json!({ "status": "failed", "detail": e }),
            }
        }
    };
    let note = if continuation["status"] == "scheduled" {
        "Handoff recorded. End this turn now: the queued continuation starts the next turn inside the worktree, with the conversation so far. The worktree is not removed when the thread is deleted."
    } else {
        "Handoff recorded. End this turn now: the thread's next message starts a turn inside the worktree, with the conversation so far. The worktree is not removed when the thread is deleted."
    };
    Ok(json!({ "worktreePath": path, "branch": branch, "baseRef": base, "startedFromOrigin": false, "setupScript": setup, "continuation": continuation, "note": note }))
}
