//! Issue tools (t3code `mcp/toolkits/issues`: `tools.ts`, `handlers.ts`):
//! list, read, file, comment on and move issues of a project's tracker, and
//! link them to threads (`thread.issue.link` with source `agent`), through the
//! same service the `issues.*` RPCs use (`rpc::issues`).
//!
//! An issue is named by its URL, or by its key in a project's tracker (the
//! project named, else the caller's). A URL resolves through whichever
//! project here is a checkout of its repository, read-only when none is.
//!
//! Limits: a failure is the tool's text (its t3code error tag and message),
//! not a structured error; a thread other than the caller's must be of the
//! caller's project, as for every thread tool here.

#![expect(clippy::disallowed_types, clippy::disallowed_macros, reason = "the MCP wire: tool schemas, tool arguments and tool results")]

use super::{Call, n, object, one_of, s, tool};
use crate::rpc::issues::{self, IssueFailure};
use domovoi_model::typed::{
    IssueCommentInput, IssueCreateInput, IssueDetail, IssueListInput, IssueListState, IssueRef, IssueScopeRef, IssueSetStateInput, ThreadIssueKey,
};
use serde_json::{Value, json};
use std::collections::BTreeMap;

/// A tool's description with t3code's hints: `destructive` apart from `read_only`.
fn issue_tool(name: &str, description: &str, properties: Value, required: &[&str], read_only: bool, destructive: bool) -> Value {
    let mut t = tool(name, description, properties, required, read_only);
    t["annotations"]["destructiveHint"] = json!(destructive);
    t
}

fn target_properties() -> serde_json::Map<String, Value> {
    let Value::Object(fields) = json!({
        "url": s("The issue's web URL. Preferred when you have it."),
        "key": s("The issue's key in the project's tracker, such as its number."),
        "projectId": s("Project whose tracker holds key. Defaults to this thread's project."),
    }) else {
        return serde_json::Map::new();
    };
    fields
}

fn with_target(extra: Value) -> Value {
    let mut fields = target_properties();
    if let Value::Object(extra) = extra {
        fields.extend(extra);
    }
    Value::Object(fields)
}

pub fn tools() -> Vec<Value> {
    let thread = json!({ "threadId": s("Thread to act on. Omit for this thread.") });
    vec![
        issue_tool("list_issues", "List issues in a project's tracker, newest updated first. Pass the returned nextCursors as cursors for the next page.",
            json!({
                "projectId": s("Defaults to this thread's project."),
                "state": one_of(&["all", "open", "closed"], "Defaults to open."),
                "query": s("Free text the tracker matches itself."),
                "limit": n("At most this many per scope (1 to 100)."),
                "cursors": object("nextCursors from an earlier call."),
            }), &[], true, false),
        issue_tool("read_issue", "Read an issue: body, state, labels, assignees, recent comments and linked pull requests. Pass its url, or its key.",
            with_target(json!({})), &[], true, false),
        issue_tool("create_issue", "File an issue in a project's tracker.",
            json!({
                "projectId": s("Defaults to this thread's project."),
                "title": s("The issue's title."),
                "body": s("Its description (markdown)."),
                "labels": { "type": "array", "items": { "type": "string" }, "description": "Label names." },
                "assignees": { "type": "array", "items": { "type": "string" }, "description": "Logins to assign." },
            }), &["title"], false, false),
        issue_tool("comment_on_issue", "Post a comment on an issue.", with_target(json!({ "body": s("The comment (markdown).") })), &["body"], false, false),
        issue_tool("set_issue_state", "Move an issue to one of its states. read_issue lists them under states.",
            with_target(json!({ "state": s("A state name from read_issue's states.") })), &["state"], false, false),
        issue_tool("link_issue", "Link an issue to a thread so T3 Code shows it beside the thread. Link every issue you work on or file.",
            with_target(thread.clone()), &[], false, false),
        issue_tool("unlink_issue", "Remove an issue link from a thread. The issue itself is not changed.", with_target(thread.clone()), &[], false, true),
        issue_tool("list_thread_issues", "List the issues linked to a thread.", thread, &[], true, false),
    ]
}

pub async fn call(c: &Call<'_>, name: &str) -> Option<Result<Value, String>> {
    Some(match name {
        "list_issues" => list_issues(c).await,
        "read_issue" => read_issue(c).await,
        "create_issue" => create_issue(c).await,
        "comment_on_issue" => comment_on_issue(c).await,
        "set_issue_state" => set_issue_state(c).await,
        "link_issue" => link_issue(c).await,
        "unlink_issue" => unlink_issue(c).await,
        "list_thread_issues" => list_thread_issues(c),
        _ => return None,
    })
}

/// A failure as the tool reports it: t3code's tag, then its message.
fn failed(e: IssueFailure) -> String {
    let tag = match &e {
        IssueFailure::Unavailable { .. } => "IssueUnavailableError",
        IssueFailure::Operation { .. } => "IssueOperationError",
    };
    format!("{tag}: {e}")
}

/// A resolved reference: with the host the link stores and whether it may be written.
struct Target {
    r: IssueRef,
    url: Option<String>,
    /// Present when resolving the URL read the issue already.
    issue: Option<IssueDetail>,
}

/// Where a key-only reference is read: the named project's tracker, or the caller's.
async fn project_scope(c: &Call<'_>, requested: Option<String>) -> Result<IssueScopeRef, String> {
    let project_id = requested.unwrap_or_else(|| c.caller.project_id.clone());
    issues::project_scope(c.state, &project_id).await.map_err(|_| format!("IssueProjectNotFoundError: Project {project_id} was not found or has no issue tracker."))
}

/// Whichever shape the agent passed, as one reference (`resolveTarget`).
async fn resolve_target(c: &Call<'_>, project_id: Option<String>) -> Result<Target, String> {
    if let Some(url) = c.str("url") {
        let preferred = project_id.unwrap_or_else(|| c.caller.project_id.clone());
        let resolved = issues::resolve_url(c.state, &url, Some(&preferred)).await.map_err(failed)?;
        return Ok(Target { r: resolved.r, url: Some(url), issue: resolved.issue });
    }
    let Some(key) = c.str("key") else {
        return Err("IssueTargetIncompleteError: Pass either url, or key (with projectId when not running inside a T3 thread).".into());
    };
    let scope = project_scope(c, project_id).await?;
    Ok(Target { r: IssueRef { project_id: scope.project_id, tracker: scope.tracker, host: scope.host, scope: scope.scope, key }, url: None, issue: None })
}

/// How a tool names an issue back: enough to pass to any other issue tool.
fn identity(target: &Target) -> Value {
    let mut out = json!({ "projectId": target.r.project_id, "tracker": target.r.tracker, "host": target.r.host, "scope": target.r.scope, "key": target.r.key });
    if let Some(url) = &target.url {
        out["url"] = json!(url);
    }
    out
}

async fn read(c: &Call<'_>, target: &Target) -> Result<IssueDetail, String> {
    match &target.issue {
        Some(issue) => Ok(issue.clone()),
        None => issues::detail(c.state, &target.r).await.map_err(failed),
    }
}

async fn list_issues(c: &Call<'_>) -> Result<Value, String> {
    let scope = project_scope(c, c.str("projectId")).await?;
    let state = match c.str("state").as_deref() {
        None => IssueListState::Open,
        Some(state @ ("all" | "open" | "closed")) => IssueListState::from(state),
        Some(other) => return Err(format!("state must be all, open or closed, not {other}.")),
    };
    let cursors: Option<BTreeMap<String, String>> = c.args["cursors"].as_object().map(|m| m.iter().filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_owned()))).collect());
    let limit = match c.args.get("limit") {
        None | Some(Value::Null) => None,
        Some(v) => Some(v.as_u64().filter(|n| (1..=100).contains(n)).ok_or("limit must be a whole number from 1 to 100.")?),
    };
    let input = IssueListInput {
        state,
        involvement: None,
        project_id: Some(scope.project_id),
        exclude_project_ids: None,
        host: None,
        limit,
        cursors,
        refresh: None,
        query: c.str("query"),
        labels: None,
        assignee: None,
        sort: None,
    };
    let result = issues::list(c.state, input).await.map_err(failed)?;
    let entries: Vec<Value> = result
        .entries
        .iter()
        .map(|e| {
            let s = &e.summary;
            json!({
                "projectId": e.project_id, "tracker": s.tracker, "host": s.host, "scope": s.scope, "key": s.key, "url": s.url,
                "displayKey": s.display_key, "title": s.title, "state": s.state, "author": s.author.as_ref().map(|a| &a.login),
                "assignees": s.assignees.iter().map(|a| &a.login).collect::<Vec<_>>(), "labels": s.labels.iter().map(|l| &l.name).collect::<Vec<_>>(),
                "updatedAt": s.updated_at, "commentCount": s.comment_count,
            })
        })
        .collect();
    let errors: Vec<Value> = result.errors.iter().map(|e| json!({ "projectId": e.project_id, "message": e.message })).collect();
    Ok(json!({ "issues": entries, "nextCursors": result.next_cursors, "errors": errors }))
}

async fn read_issue(c: &Call<'_>) -> Result<Value, String> {
    let target = resolve_target(c, c.str("projectId")).await?;
    let d = read(c, &target).await?;
    let s = &d.summary;
    let mut out = identity(&target);
    let fields = json!({
        "url": target.url.clone().unwrap_or_else(|| s.url.clone()),
        "displayKey": s.display_key, "title": s.title, "state": s.state, "writable": d.writable, "capabilities": d.capabilities, "states": d.states,
        "author": s.author.as_ref().map(|a| &a.login), "assignees": s.assignees.iter().map(|a| &a.login).collect::<Vec<_>>(),
        "labels": s.labels.iter().map(|l| &l.name).collect::<Vec<_>>(), "createdAt": s.created_at, "updatedAt": s.updated_at, "closedAt": s.closed_at,
        "body": d.body,
        "comments": d.comments.iter().map(|m| json!({ "id": m.id, "author": m.author.as_ref().map(|a| &a.login), "body": m.body, "createdAt": m.created_at })).collect::<Vec<_>>(),
        "commentsTruncated": d.comments_cursor.is_some(),
        "linkedPullRequests": d.linked_pull_requests.iter().map(|p| json!({ "url": p.url, "title": p.title, "state": p.state, "closesIssue": p.closes_issue })).collect::<Vec<_>>(),
    });
    if let (Some(out), Value::Object(fields)) = (out.as_object_mut(), fields) {
        out.extend(fields);
    }
    Ok(out)
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array().into_iter().flatten().filter_map(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned).collect()
}

async fn create_issue(c: &Call<'_>) -> Result<Value, String> {
    let title = c.required("title")?;
    let scope = project_scope(c, c.str("projectId")).await?;
    let input = IssueCreateInput {
        scope: scope.clone(),
        title,
        body: c.args["body"].as_str().unwrap_or_default().to_owned(),
        labels: strings(&c.args["labels"]),
        assignees: strings(&c.args["assignees"]),
    };
    let created = issues::create(c.state, &input).await.map_err(failed)?;
    Ok(json!({ "projectId": scope.project_id, "tracker": scope.tracker, "host": scope.host, "scope": scope.scope, "key": created.key, "url": created.url, "displayKey": created.display_key }))
}

async fn comment_on_issue(c: &Call<'_>) -> Result<Value, String> {
    let body = c.args["body"].as_str().filter(|b| !b.is_empty()).ok_or("body is required.")?.to_owned();
    let target = resolve_target(c, c.str("projectId")).await?;
    issues::comment(c.state, &IssueCommentInput { issue: target.r.clone(), body }).await.map_err(failed)?;
    Ok(identity(&target))
}

async fn set_issue_state(c: &Call<'_>) -> Result<Value, String> {
    let state = c.required("state")?;
    let target = resolve_target(c, c.str("projectId")).await?;
    issues::set_state(c.state, &IssueSetStateInput { issue: target.r.clone(), state }).await.map_err(failed)?;
    Ok(identity(&target))
}

/// The thread a link tool changes: `threadId`, or the caller's own.
fn require_thread(c: &Call<'_>) -> Result<(String, domovoi_model::typed::AppThread), String> {
    c.thread().map_err(|_| format!("IssueThreadNotFoundError: Thread {} was not found.", c.str("threadId").unwrap_or_default()))
}

fn key_of(r: &IssueRef) -> ThreadIssueKey {
    ThreadIssueKey { tracker: r.tracker.clone(), host: r.host.clone().unwrap_or_default(), scope: r.scope.clone(), key: r.key.clone() }
}

async fn link_issue(c: &Call<'_>) -> Result<Value, String> {
    let (thread_id, thread) = require_thread(c)?;
    let target = resolve_target(c, Some(c.str("projectId").unwrap_or_else(|| thread.project_id.as_str().to_owned()))).await?;
    // Reading the issue proves it exists and names it for the thread's row.
    let detail = read(c, &target).await?;
    let url = target.url.clone().unwrap_or_else(|| detail.summary.url.clone());
    let key = key_of(&target.r);
    let already = thread.issues.iter().flatten().any(|link| link.key.same_issue(&key));
    if !already {
        let command = json!({
            "type": "thread.issue.link", "commandId": c.command_id("issue-link"), "threadId": thread_id,
            "tracker": key.tracker, "host": key.host, "scope": key.scope, "key": key.key, "projectId": target.r.project_id,
            "url": url, "displayKey": detail.summary.display_key, "title": detail.summary.title, "source": "agent",
        });
        c.dispatch(command).await.map_err(|_| "IssueLinkFailedError: Could not change the issue link.".to_owned())?;
    }
    let mut out = identity(&target);
    out["url"] = json!(url);
    out["alreadyLinked"] = json!(already);
    Ok(out)
}

async fn unlink_issue(c: &Call<'_>) -> Result<Value, String> {
    let (thread_id, thread) = require_thread(c)?;
    let target = resolve_target(c, Some(c.str("projectId").unwrap_or_else(|| thread.project_id.as_str().to_owned()))).await?;
    let key = key_of(&target.r);
    let Some(existing) = thread.issues.iter().flatten().find(|link| link.key.same_issue(&key)) else {
        let mut out = identity(&target);
        out["wasLinked"] = json!(false);
        return Ok(out);
    };
    let command = json!({
        "type": "thread.issue.unlink", "commandId": c.command_id("issue-unlink"), "threadId": thread_id,
        "tracker": existing.key.tracker, "host": existing.key.host, "scope": existing.key.scope, "key": existing.key.key,
    });
    c.dispatch(command).await.map_err(|_| "IssueLinkFailedError: Could not change the issue link.".to_owned())?;
    let mut out = identity(&target);
    out["url"] = json!(existing.url);
    out["wasLinked"] = json!(true);
    Ok(out)
}

fn list_thread_issues(c: &Call<'_>) -> Result<Value, String> {
    let (_, thread) = require_thread(c)?;
    let issues: Vec<Value> = thread
        .issues
        .iter()
        .flatten()
        .map(|link| {
            json!({ "projectId": link.project_id, "tracker": link.key.tracker, "host": link.key.host, "scope": link.key.scope, "key": link.key.key,
                "url": link.url, "source": link.source, "displayKey": link.display_key, "title": link.title })
        })
        .collect();
    Ok(json!({ "issues": issues }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_toolkit_has_t3codes_names_and_hints() {
        let tools = tools();
        let names: Vec<&str> = tools.iter().filter_map(|t| t["name"].as_str()).collect();
        assert_eq!(names, ["list_issues", "read_issue", "create_issue", "comment_on_issue", "set_issue_state", "link_issue", "unlink_issue", "list_thread_issues"]);
        let hint = |name: &str, hint: &str| tools.iter().find(|t| t["name"] == name).map(|t| t["annotations"][hint] == true).unwrap_or_default();
        assert!(hint("read_issue", "readOnlyHint") && !hint("read_issue", "destructiveHint"));
        assert!(!hint("create_issue", "readOnlyHint") && !hint("create_issue", "destructiveHint"));
        assert!(hint("unlink_issue", "destructiveHint"));
        let read = tools.iter().find(|t| t["name"] == "read_issue").map(|t| t["inputSchema"]["properties"].clone()).unwrap_or_default();
        assert!(read.get("url").is_some() && read.get("key").is_some() && read.get("projectId").is_some());
    }

    #[test]
    fn failures_name_their_tag() {
        assert_eq!(failed(IssueFailure::unsupported()), "IssueUnavailableError: Issues cannot be browsed for this project yet.");
        assert_eq!(failed(IssueFailure::operation("comment", "A comment needs some text.")), "IssueOperationError: Issue operation comment failed: A comment needs some text.");
    }
}
