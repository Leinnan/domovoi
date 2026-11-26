//! Pull request tools: link pull requests
//! to a thread of this project (`thread.pull-request.link`), unlink them,
//! list them, watch them, and read one's state and checks from its host.
//!
//! A pull request is named by its URL, or by `repository` and `number` on
//! `host` (default: the project's repository host). Links and watches are
//! the orchestrator's (`orchestration::commands::pull_requests`); reads go
//! through `rpc::source_control::pull_requests`: GitHub through `gh`,
//! GitLab through `glab`.
//!
//! Limits: no review guides, no creating, updating, reviewing, merging or
//! CI logs (`write_pull_request_guides`, `pull_request_create`, …); reads
//! give `summary` and `checks` only; stacks are not derived (`stack` is
//! null, `chains` empty). A watch is recorded, but nothing polls the host
//! yet, so no check or review wakes the thread.

#![expect(clippy::disallowed_types, clippy::disallowed_macros, reason = "the MCP wire: tool schemas, tool arguments and tool results")]

use super::{Call, n, one_of, s, tool};
use crate::orchestration::commands::pull_requests::{PullRequestKey, is_visible, links_of};
use domovoi_model::typed::command::{PullRequestToLink, ThreadPullRequestLink, ThreadPullRequestUnlink, ThreadPullRequestWatch};
use domovoi_model::typed::{
    AppThread, Command, ParentRelationship, PullRequestChecksState, PullRequestLink, PullRequestLinkSource, PullRequestState, PullRequestWatchUntil,
};
use serde_json::{Value, json};

/// A delegated child's report is how its parent learns the pull request.
const CHILD_REPORT_NEXT: &str = "End your reply with: PR <full URL>, head <SHA>, checks and reviews <state>.";
const PAGE: usize = 50;
const STATES: &[&str] = &["open", "closed", "merged"];

fn target_props(extra: &[(&str, Value)]) -> Value {
    let mut props = json!({
        "threadId": s("Thread. Default: this thread."),
        "url": s("The pull request's URL."),
        "repository": s("owner/name, with `number`, when no `url`."),
        "number": n("With `repository`, when no `url`."),
        "host": s("Default: this project's host."),
    });
    for (k, v) in extra {
        props[*k] = v.clone();
    }
    props
}

fn states(d: &str) -> Value {
    json!({ "type": "array", "items": { "type": "string", "enum": STATES }, "description": d })
}

pub fn tools() -> Vec<Value> {
    vec![
        tool("link_pull_request", "Link a PR to this thread. Domovoi shows its status. For several PRs, use `link_pull_requests`.", target_props(&[]), &[], false),
        tool("link_pull_requests", "Link several PRs to a thread. Use it instead of one `link_pull_request` per PR. URLs that are not PRs come back in `invalidUrls`.",
            json!({ "threadId": s("Thread. Default: this thread."), "urls": { "type": "array", "items": { "type": "string" }, "minItems": 1, "description": "Pull request URLs." } }), &["urls"], false),
        tool("unlink_pull_request", "Remove a PR link from this thread. Succeeds with `wasLinked=false` when it was not linked.", target_props(&[]), &[], false),
        tool("unlink_pull_requests", "Unlink a thread's PRs whose last known state is in `states`. Use it instead of one `unlink_pull_request` per PR. PRs with no known state stay linked.",
            json!({ "threadId": s("Thread. Default: this thread."), "states": states("Unlink PRs in these states.") }), &["states"], false),
        tool("list_thread_pull_requests", "List a thread's PRs, with last known state. Not an end-of-work check.",
            json!({ "threadId": s("Thread. Default: this thread."), "states": states("Only PRs in these states (no known state counts as open)."), "cursor": s("From `nextCursor`.") }), &[], true),
        tool("watch_pull_request", "Watch an open PR for this thread, linking it first if needed. The watch is recorded on the thread; this server does not poll the host yet, so check the PR yourself with `pull_request_read` before you rely on it.",
            target_props(&[("until", one_of(&["checks", "merged"], "How long the watch holds: until checks finish (default for a new watch) or until it merges or closes."))]), &[], false),
        tool("unwatch_pull_request", "Stop watching a PR for this thread. It stays linked.", target_props(&[]), &[], false),
        tool("pull_request_read", "Read a PR's state and checks from its host. Use instead of gh.",
            json!({
                "number": n("Default: the branch's PR, else this thread's latest linked one."),
                "repository": s("owner/name. Default: this project's repository."),
                "sections": { "type": "array", "items": { "type": "string", "enum": ["summary", "checks"] }, "description": "Default: summary, checks." },
            }), &[], true),
    ]
}

pub async fn call(c: &Call<'_>, name: &str) -> Option<Result<Value, String>> {
    Some(match name {
        "link_pull_request" => link(c).await,
        "link_pull_requests" => link_many(c).await,
        "unlink_pull_request" => unlink(c).await,
        "unlink_pull_requests" => unlink_by_state(c).await,
        "list_thread_pull_requests" => list(c),
        "watch_pull_request" => watch(c, true).await,
        "unwatch_pull_request" => watch(c, false).await,
        "pull_request_read" => read(c).await,
        _ => return None,
    })
}

/// A pull request named by a tool call.
#[derive(Clone, Debug, PartialEq)]
struct Target {
    host: String,
    repository: String,
    number: u64,
    url: String,
}

impl Target {
    fn key(&self) -> PullRequestKey {
        PullRequestKey::new(&self.host, &self.repository, self.number)
    }
    fn identity(&self) -> Value {
        json!({ "host": self.host, "repository": self.repository, "number": self.number, "url": self.url })
    }
}

/// A pull request (GitHub `/pull/N`, GitLab `/-/merge_requests/N`, Bitbucket `/pull-requests/N`) from its URL.
fn parse_url(url: &str) -> Option<Target> {
    let rest = url.trim().strip_prefix("https://").or_else(|| url.trim().strip_prefix("http://"))?;
    let rest = rest.split(['?', '#']).next()?;
    let (host, path) = rest.split_once('/')?;
    let host = host.rsplit('@').next()?.to_lowercase();
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    let (repository, number) = ["pull", "pulls", "merge_requests", "pull-requests"].iter().find_map(|word| {
        let at = parts.iter().position(|p| p == word)?;
        let number: u64 = parts.get(at + 1)?.parse().ok().filter(|n| *n > 0)?;
        let repo_parts: Vec<&str> = parts[..at].iter().copied().filter(|p| *p != "-").collect();
        (repo_parts.len() >= 2).then(|| (repo_parts.join("/"), number))
    })?;
    (!host.is_empty()).then(|| Target { url: format!("https://{host}/{}", path.trim_end_matches('/')), host, repository, number })
}

/// `resolveTarget`: the URL, or repository and number on the given (or the project's) host.
async fn resolve(c: &Call<'_>, args: &Value) -> Result<Target, String> {
    if let Some(url) = args["url"].as_str().map(str::trim).filter(|u| !u.is_empty()) {
        return parse_url(url).ok_or_else(|| format!("PullRequestUrlInvalidError: {url} is not a pull request URL."));
    }
    let (Some(repository), Some(number)) = (args["repository"].as_str().map(str::trim).filter(|r| r.contains('/')), args["number"].as_u64().filter(|n| *n > 0)) else {
        return Err("PullRequestTargetIncompleteError: pass `url`, or `repository` (owner/name) and `number`.".into());
    };
    let host = match args["host"].as_str().map(str::trim).filter(|h| !h.is_empty()) {
        Some(host) => host.to_lowercase(),
        None => crate::rpc::source_control::pull_requests::project_repository(c.state, &c.caller.project_id)
            .await
            .map(|(host, _)| host)
            .ok_or("PullRequestHostRequiredError: this project has no repository host; pass `host`.")?,
    };
    let url = crate::rpc::source_control::pull_requests::change_request_url(&host, repository, number);
    Ok(Target { url, host, repository: repository.to_owned(), number })
}

fn is_child(thread: &AppThread) -> bool {
    thread.lineage.relationship_to_parent == Some(ParentRelationship::Subagent)
}

fn linked<'a>(links: &'a [PullRequestLink], key: &PullRequestKey) -> Option<&'a PullRequestLink> {
    links.iter().find(|link| is_visible(link) && key.names(link))
}

async fn send(c: &Call<'_>, command: Command) -> Result<(), String> {
    c.orch().dispatch(command).await.map(|_| ()).map_err(|e| e.to_string())
}

fn link_command(c: &Call<'_>, thread_id: &str, t: &Target) -> Command {
    Command::ThreadPullRequestLink(Box::new(ThreadPullRequestLink {
        command_id: c.command_id("pr-link").into(),
        thread_id: thread_id.into(),
        host: t.host.clone(),
        repository: t.repository.clone(),
        number: t.number,
        url: t.url.clone(),
        source: PullRequestLinkSource::Agent,
        repository_web_url: None,
    }))
}

async fn link(c: &Call<'_>) -> Result<Value, String> {
    let (id, thread) = c.thread()?;
    let target = resolve(c, c.args).await?;
    let mut answer = target.identity();
    let already = linked(&links_of(&thread), &target.key()).is_some();
    if !already {
        send(c, link_command(c, &id, &target)).await.map_err(|e| format!("PullRequestLinkFailedError: Could not link the pull request: {e}"))?;
    }
    answer["alreadyLinked"] = json!(already);
    if is_child(&thread) {
        answer["next"] = json!(CHILD_REPORT_NEXT);
    }
    Ok(answer)
}

/// Pull requests grouped by host and repository, numbers in the order given.
fn by_repository(targets: &[Target]) -> Vec<Value> {
    let mut groups: Vec<(String, String, Vec<u64>)> = vec![];
    for t in targets {
        match groups.iter_mut().find(|(h, r, _)| *h == t.host && *r == t.repository) {
            Some(group) => group.2.push(t.number),
            None => groups.push((t.host.clone(), t.repository.clone(), vec![t.number])),
        }
    }
    groups.into_iter().map(|(host, repository, numbers)| json!({ "host": host, "repository": repository, "numbers": numbers })).collect()
}

async fn link_many(c: &Call<'_>) -> Result<Value, String> {
    let (id, thread) = c.thread()?;
    let urls: Vec<&str> = c.args["urls"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
    if urls.is_empty() {
        return Err("urls is required.".into());
    }
    let mut targets: Vec<Target> = vec![];
    let mut invalid = vec![];
    for url in urls {
        match parse_url(url) {
            Some(t) if !targets.iter().any(|seen| seen.key() == t.key()) => targets.push(t),
            Some(_) => {}
            None => invalid.push(url.to_owned()),
        }
    }
    let links = links_of(&thread);
    let (already, fresh): (Vec<Target>, Vec<Target>) = targets.iter().cloned().partition(|t| linked(&links, &t.key()).is_some());
    for t in &fresh {
        send(c, link_command(c, &id, t)).await.map_err(|e| format!("PullRequestLinkFailedError: Could not link the pull request: {e}"))?;
    }
    let mut answer = json!({ "linked": by_repository(&fresh), "alreadyLinked": by_repository(&already), "linkedCount": targets.len() });
    if !invalid.is_empty() {
        answer["invalidUrls"] = json!(invalid);
    }
    if is_child(&thread) {
        answer["next"] = json!(CHILD_REPORT_NEXT);
    }
    Ok(answer)
}

fn unlink_command(c: &Call<'_>, thread_id: &str, key: &PullRequestKey) -> Command {
    Command::ThreadPullRequestUnlink(Box::new(ThreadPullRequestUnlink {
        command_id: c.command_id(&format!("pr-unlink-{}-{}-{}", key.host, key.repository, key.number)).into(),
        thread_id: thread_id.into(),
        host: key.host.clone(),
        repository: key.repository.clone(),
        number: key.number,
    }))
}

async fn unlink(c: &Call<'_>) -> Result<Value, String> {
    let (id, thread) = c.thread()?;
    let target = resolve(c, c.args).await?;
    let was = linked(&links_of(&thread), &target.key()).is_some();
    if was {
        send(c, unlink_command(c, &id, &target.key())).await.map_err(|e| format!("PullRequestUnlinkFailedError: Could not unlink the pull request: {e}"))?;
    }
    Ok(json!({ "host": target.host, "repository": target.repository, "number": target.number, "wasLinked": was }))
}

async fn unlink_by_state(c: &Call<'_>) -> Result<Value, String> {
    let (id, thread) = c.thread()?;
    let wanted: Vec<&str> = c.args["states"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
    if wanted.is_empty() {
        return Err("states is required.".into());
    }
    let visible: Vec<PullRequestLink> = links_of(&thread).into_iter().filter(is_visible).collect();
    let matching: Vec<(PullRequestKey, PullRequestState)> = visible
        .iter()
        .filter_map(|link| link.snapshot.as_ref().map(|s| (PullRequestKey::of(link), s.state.clone())))
        .filter(|(_, state)| wanted.contains(&state.as_str()))
        .collect();
    for (key, _) in &matching {
        send(c, unlink_command(c, &id, key)).await.map_err(|e| format!("PullRequestUnlinkFailedError: Could not unlink the pull request: {e}"))?;
    }
    let mut groups: Vec<(String, String, String, Vec<u64>)> = vec![];
    for (key, state) in &matching {
        match groups.iter_mut().find(|(h, r, s, _)| *h == key.host && *r == key.repository && s == state.as_str()) {
            Some(group) => group.3.push(key.number),
            None => groups.push((key.host.clone(), key.repository.clone(), state.as_str().to_owned(), vec![key.number])),
        }
    }
    let unlinked: Vec<Value> = groups.into_iter().map(|(host, repository, state, numbers)| json!({ "host": host, "repository": repository, "state": state, "numbers": numbers })).collect();
    Ok(json!({ "unlinked": unlinked, "unlinkedCount": matching.len(), "remaining": visible.len() - matching.len() }))
}

/// A total order that survives unlinking: the cursor names a position, not an index.
fn position(link: &PullRequestLink) -> String {
    format!("{} {} {} {:010}", link.linked_at.as_str(), link.host, link.repository, link.number)
}

fn list(c: &Call<'_>) -> Result<Value, String> {
    let (_, thread) = c.thread()?;
    let wanted: Option<Vec<&str>> = c.args["states"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect());
    let cursor = c.str("cursor");
    let mut after: Vec<(String, PullRequestLink)> = links_of(&thread)
        .into_iter()
        .filter(is_visible)
        .filter(|link| wanted.as_ref().is_none_or(|w| w.contains(&link.snapshot.as_ref().map_or("open", |s| s.state.as_str()))))
        .map(|link| (position(&link), link))
        .filter(|(p, _)| cursor.as_ref().is_none_or(|c| p > c))
        .collect();
    after.sort_by(|a, b| a.0.cmp(&b.0));
    let next = (after.len() > PAGE).then(|| after[PAGE - 1].0.clone());
    let entries: Vec<Value> = after
        .iter()
        .take(PAGE)
        .map(|(_, link)| {
            let snap = link.snapshot.as_ref();
            json!({
                "host": link.host.to_lowercase(), "repository": link.repository, "number": link.number, "url": link.url, "source": link.source,
                "watching": link.watch.is_some(), "state": snap.map(|s| s.state.clone()), "title": snap.map(|s| s.title.clone()),
                "headBranch": snap.map(|s| s.head_branch.clone()), "baseBranch": snap.map(|s| s.base_branch.clone()), "isDraft": snap.map(|s| s.is_draft),
                "stack": null,
            })
        })
        .collect();
    Ok(json!({ "pullRequests": entries, "chains": [], "nextCursor": next }))
}

async fn watch(c: &Call<'_>, watching: bool) -> Result<Value, String> {
    let (id, thread) = c.thread()?;
    let target = resolve(c, c.args).await?;
    if watching && is_child(&thread) {
        return Err("PullRequestWatchFromSubagentError: a delegated child reports its PR to its parent, which watches it.".into());
    }
    let key = target.key();
    let before = links_of(&thread);
    let held = linked(&before, &key);
    if watching && held.and_then(|l| l.snapshot.as_ref()).is_some_and(|s| s.state == PullRequestState::Merged) {
        return Err("PullRequestNotOpenError: The pull request is merged.".into());
    }
    let was_watching = held.is_some_and(|l| l.watch.is_some());
    // A pending rollup has no failure yet, so any failure the first read finds is news.
    let mut checks_pending = None;
    if watching && !was_watching {
        if let Ok((summary, _)) = crate::rpc::source_control::pull_requests::read_for_agent(c.state, &c.caller.project_id, &target.host, &target.repository, target.number, false).await {
            if summary.state == PullRequestState::Merged {
                return Err("PullRequestNotOpenError: The pull request is merged.".into());
            }
            checks_pending = Some(summary.checks_state == Some(Some(PullRequestChecksState::Pending)));
        }
    }
    let until = match c.str("until").as_deref() {
        Some("merged") => Some(PullRequestWatchUntil::Merged),
        Some("checks") => Some(PullRequestWatchUntil::Checks),
        _ => None,
    };
    let command = Command::ThreadPullRequestWatch(Box::new(ThreadPullRequestWatch {
        command_id: c.command_id("pr-watch").into(),
        thread_id: id.as_str().into(),
        host: key.host.clone(),
        repository: key.repository.clone(),
        number: key.number,
        watching,
        until: until.filter(|_| watching),
        checks_pending: checks_pending.filter(|_| watching),
        link: watching.then(|| PullRequestToLink { url: target.url.clone(), source: PullRequestLinkSource::Agent, repository_web_url: None }),
    }));
    send(c, command).await.map_err(|e| format!("PullRequestWatchFailedError: {e}"))?;
    let after = c.store().app_thread(&id).ok().flatten().map(|t| links_of(&t)).unwrap_or_default();
    let mut answer = target.identity();
    answer["watching"] = json!(linked(&after, &key).is_some_and(|l| l.watch.is_some()));
    answer["wasWatching"] = json!(was_watching);
    Ok(answer)
}

async fn read(c: &Call<'_>) -> Result<Value, String> {
    let thread = &c.caller.thread;
    let project = crate::rpc::source_control::pull_requests::project_repository(c.state, &c.caller.project_id).await;
    let links = links_of(thread);
    let latest = links.iter().filter(|l| is_visible(l)).max_by(|a, b| a.linked_at.as_str().cmp(b.linked_at.as_str()));
    let branch = thread.branch_pull_request.as_ref().and_then(Option::as_ref);
    let (host, repository, number) = match (c.args["number"].as_u64(), c.str("repository")) {
        (Some(number), Some(repository)) => (project.as_ref().map(|p| p.0.clone()).unwrap_or_else(|| "github.com".into()), repository, number),
        (Some(number), None) => {
            let (host, repository) = project.clone().ok_or("PullRequestForgeError: this project has no GitHub or GitLab repository; pass `repository`.")?;
            (host, repository, number)
        }
        (None, _) => match (branch, latest) {
            (Some(b), _) => {
                let key = PullRequestKey::of_legacy(b);
                (key.host, b.repository.clone(), b.number)
            }
            (None, Some(l)) => (l.host.clone(), l.repository.clone(), l.number),
            (None, None) => return Err("PullRequestBranchHasNoneError: This thread's branch has no known PR. Pass number.".into()),
        },
    };
    let sections: Vec<String> = c.args["sections"].as_array().map(|a| a.iter().filter_map(|s| s.as_str().map(str::to_owned)).collect()).unwrap_or_else(|| vec!["summary".into(), "checks".into()]);
    if let Some(other) = sections.iter().find(|s| !matches!(s.as_str(), "summary" | "checks")) {
        return Err(format!("invalid_request: section {other} is not read by this server (summary, checks)."));
    }
    let (summary, checks) = crate::rpc::source_control::pull_requests::read_for_agent(c.state, &c.caller.project_id, &host, &repository, number, sections.iter().any(|s| s == "checks"))
        .await
        .map_err(|e| format!("PullRequestForgeError: {e}"))?;
    let mut answer = json!({ "host": host, "repository": repository, "number": number, "url": summary.url });
    if sections.iter().any(|s| s == "summary") {
        answer["summary"] = serde_json::to_value(&summary).unwrap_or(Value::Null);
    }
    if let Some(checks) = checks {
        answer["checks"] = serde_json::to_value(&checks).unwrap_or(Value::Null);
    }
    Ok(answer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pull_request_urls_name_host_repository_and_number() {
        let t = parse_url("https://GitHub.com/Acme/App/pull/42/files?x=1").unwrap();
        assert_eq!((t.host.as_str(), t.repository.as_str(), t.number), ("github.com", "Acme/App", 42));
        let t = parse_url("https://gitlab.example.com/group/sub/repo/-/merge_requests/7").unwrap();
        assert_eq!((t.repository.as_str(), t.number), ("group/sub/repo", 7));
        assert!(parse_url("https://github.com/acme/app/issues/3").is_none());
        assert!(parse_url("https://github.com/acme/pull/0").is_none());
        assert!(parse_url("not a url").is_none());
        let grouped = by_repository(&[t.clone(), Target { number: 8, ..t }]);
        assert_eq!(grouped, vec![json!({ "host": "gitlab.example.com", "repository": "group/sub/repo", "numbers": [7, 8] })]);
    }
}
