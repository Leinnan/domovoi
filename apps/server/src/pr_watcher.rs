//! Background pull request watcher and automated review trigger worker.
//!
//! 1. Fulfills `watch_pull_request` on active threads: polls GitHub (`gh`) and
//!    GitLab (`glab`) for checks/review/merge transitions and wakes the watching
//!    thread with a system message.
//! 2. Monitors opted-in projects (`autoReviewPrs`) for new PRs or new head pushes
//!    and enqueues incremental AI code reviews automatically.
//! 3. Scans PR comments for configured trigger commands (`reviewCommentTrigger`,
//!    e.g. `/review`), deduplicates them in `pr_comment_activations`, posts a
//!    confirmation comment, and starts a review run.

#![expect(clippy::disallowed_types, clippy::disallowed_macros, reason = "orchestrator commands dispatched as JSON payloads")]

use crate::orchestration::commands::pull_requests::links_of;
use crate::review::{self, StartReviewInput, store as review_store};
use crate::rpc::source_control::github::{self, Listing};
use crate::rpc::source_control::gitlab;
use crate::rpc::source_control::pull_request_failure::Provider;
use crate::rpc::source_control::pull_requests::{PrRef, read_for_agent, require_project};
use crate::rpc::source_control::review_guides;
use crate::state::SharedState;
use domovoi_model::typed::{
    PullRequestChecks, PullRequestChecksState, PullRequestState, PullRequestSummary, PullRequestWatch, PullRequestWatchUntil,
};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::time::Duration;

const DEFAULT_INTERVAL_SECS: u64 = 60;
const MIN_INTERVAL_SECS: u64 = 15;

static WAKE_NOW: tokio::sync::Notify = tokio::sync::Notify::const_new();

/// Wakes the background PR watcher immediately (e.g. after a GitHub/GitLab webhook delivery).
pub fn wake_now() {
    WAKE_NOW.notify_one();
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WatchNotification {
    pub message: String,
    pub end_watch: bool,
}

/// Evaluates whether a watched PR crossed a terminal or notable transition.
pub fn evaluate_watch_transition(
    watch: &PullRequestWatch,
    summary: &PullRequestSummary,
    checks: Option<&PullRequestChecks>,
) -> Option<WatchNotification> {
    let label = format!("{}#{} ({})", summary.repository, summary.number, summary.url);

    // 1. Merged or closed ends every watch immediately.
    match summary.state {
        PullRequestState::Merged => {
            return Some(WatchNotification {
                message: format!("Pull request {label} has been merged."),
                end_watch: true,
            });
        }
        PullRequestState::Closed => {
            return Some(WatchNotification {
                message: format!("Pull request {label} was closed without merging."),
                end_watch: true,
            });
        }
        _ => {}
    }

    // 2. Checks transition.
    let checks_state = summary.checks_state.as_ref().and_then(|s| s.as_ref());
    let until_merged = matches!(watch.until, Some(PullRequestWatchUntil::Merged));

    match checks_state {
        Some(PullRequestChecksState::Failing) => {
            let failing_names: Vec<String> = checks
                .into_iter()
                .flat_map(|c| c.checks.iter())
                .filter(|c| c.status.as_str() == "failure")
                .map(|c| c.name.clone())
                .collect();
            let already_known = !failing_names.is_empty() && failing_names.iter().all(|n| watch.failed_checks.contains(n));
            if !already_known {
                let detail = if failing_names.is_empty() {
                    "one or more checks failed".to_owned()
                } else {
                    format!("failing checks: {}", failing_names.join(", "))
                };
                return Some(WatchNotification {
                    message: format!("Pull request {label} checks failed ({detail})."),
                    end_watch: !until_merged,
                });
            }
        }
        Some(PullRequestChecksState::Passing) => {
            if !watch.passed {
                return Some(WatchNotification {
                    message: format!("Pull request {label} checks are now passing."),
                    end_watch: !until_merged,
                });
            }
        }
        _ => {}
    }

    None
}

/// Matches a PR comment body against a configured trigger command (e.g. `/review` or `@domovoi review`).
pub fn matches_comment_trigger(body: &str, trigger: &str) -> bool {
    let trig = trigger.trim();
    if trig.is_empty() {
        return false;
    }
    body.lines().any(|line| {
        let l = line.trim();
        l.eq_ignore_ascii_case(trig)
            || (l.len() > trig.len()
                && l[..trig.len()].eq_ignore_ascii_case(trig)
                && l.as_bytes().get(trig.len()).is_some_and(|b| b.is_ascii_whitespace()))
    })
}

pub fn start(state: SharedState) {
    let shutdown = state.shutdown.clone();
    tokio::spawn(async move {
        // Initial short grace period so startup probes finish first.
        tokio::select! {
            () = shutdown.cancelled() => return,
            () = tokio::time::sleep(Duration::from_secs(10)) => {}
        }
        loop {
            let (enabled, interval_secs) = {
                let s = state.settings.read().await;
                let enabled = s.pr_watcher_enabled.unwrap_or(true);
                let secs = s.pr_watcher_interval_secs.unwrap_or(DEFAULT_INTERVAL_SECS).max(MIN_INTERVAL_SECS);
                (enabled, secs)
            };
            if enabled {
                poll_once(&state).await;
            }
            let wait = std::env::var("DOMOVOI_PR_WATCHER_EVERY_MS")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .map_or(Duration::from_secs(interval_secs), Duration::from_millis);
            tokio::select! {
                () = shutdown.cancelled() => break,
                () = WAKE_NOW.notified() => {}
                () = tokio::time::sleep(wait) => {}
            }
        }
    });
}

/// Runs one watcher cycle: watched threads first, then opted-in auto-review / comment-trigger projects.
pub async fn poll_once(state: &SharedState) {
    poll_watched_threads(state).await;
    poll_monitored_projects(state).await;
}

async fn poll_watched_threads(state: &SharedState) {
    let Ok(threads) = state.store.app_threads() else { return };
    type WatchKey = (String, String, String, u64);
    let mut cache: HashMap<WatchKey, Option<(PullRequestSummary, Option<PullRequestChecks>)>> = HashMap::new();

    for t in threads.into_iter().filter(|t| t.deleted_at.is_none() && t.archived_at.is_none()) {
        let links = links_of(&t);
        for link in links {
            let Some(watch) = &link.watch else { continue };
            let cache_key: WatchKey = (t.project_id.as_str().to_owned(), link.host.clone(), link.repository.clone(), link.number);
            let fetched = if let Some(entry) = cache.get(&cache_key) {
                entry.clone()
            } else {
                let res = read_for_agent(state, t.project_id.as_str(), &link.host, &link.repository, link.number, true).await.ok();
                cache.insert(cache_key, res.clone());
                res
            };
            let Some((summary, checks)) = fetched else { continue };
            let Some(notice) = evaluate_watch_transition(watch, &summary, checks.as_ref()) else { continue };

            let tid = t.id.as_str().to_owned();
            let command_id = format!("pr-watch-wake:{}:{}:{}", link.repository, link.number, crate::time_fmt::now_millis());
            let dispatch_msg = json!({
                "type": "message.dispatch",
                "commandId": command_id,
                "threadId": tid,
                "messageId": format!("message:{command_id}"),
                "text": notice.message,
                "attachments": [],
                "createdBy": "system",
                "creationSource": "server",
                "dispatchMode": { "type": "queue_after_active" },
            });
            let _ = crate::rpc::shell::dispatch_command(&state.orchestrator, dispatch_msg).await;

            if notice.end_watch {
                let end_cmd = json!({
                    "type": "thread.pull-request.watch",
                    "commandId": format!("pr-watch-end:{}:{}:{}", link.repository, link.number, crate::time_fmt::now_millis()),
                    "threadId": tid,
                    "host": link.host,
                    "repository": link.repository,
                    "number": link.number,
                    "watching": false,
                });
                let _ = crate::rpc::shell::dispatch_command(&state.orchestrator, end_cmd).await;
            }
        }
    }
}

async fn poll_monitored_projects(state: &SharedState) {
    let overrides = {
        let s = state.settings.read().await;
        s.project_settings_overrides.clone().unwrap_or_default()
    };
    if overrides.is_empty() {
        return;
    }
    let db = state.store.db().clone();
    let mut visited_projects = HashSet::new();

    for (project_id_typed, entry) in overrides {
        let project_id = project_id_typed.as_str().to_owned();
        let auto_review = entry.auto_review_prs == Some(true);
        let trigger = entry.review_comment_trigger.clone().unwrap_or_default();
        if !auto_review && trigger.trim().is_empty() {
            continue;
        }
        if !visited_projects.insert(project_id.clone()) {
            continue;
        }
        let Some((host, repository)) = crate::rpc::source_control::pull_requests::project_repository(state, &project_id).await else {
            continue;
        };
        let pr_ref = PrRef {
            project_id: project_id.clone(),
            host: Some(host.clone()),
            expected_account_id: None,
            repository: repository.clone(),
            number: 1,
        };
        let Ok(project) = require_project(state, &pr_ref).await else { continue };

        let open_rows = match project.provider {
            Provider::GitHub => {
                let listing = Listing { state: "open", involvement: None, query: None, fetch: 25 };
                github::list(&project, &listing, crate::time_fmt::now_millis() as u64).await.ok().unwrap_or_default()
            }
            Provider::GitLab => {
                let listing = Listing { state: "open", involvement: None, query: None, fetch: 25 };
                let narrowing = gitlab::Narrowing { labels: vec![], excluded_labels: vec![], author: None, draft: None, review: None };
                gitlab::list(&project, &listing, "", &narrowing, crate::time_fmt::now_millis() as u64)
                    .await
                    .ok()
                    .unwrap_or_default()
            }
        };

        let cursor_since = load_or_init_cursor(&db, &project_id);
        let now_iso = crate::time_fmt::iso_now();

        for row in open_rows {
            if row.entry.is_draft {
                continue;
            }
            let number = row.entry.number;
            let pr_key = review_guides::key(&project.host, &project.repository, number);
            let source = match project.provider {
                Provider::GitHub => github::guide_source(&project, number).await.ok(),
                Provider::GitLab => gitlab::guide_source(&project, number).await.ok(),
            };
            let Some(source) = source else { continue };
            let Some(head_sha) = source.head.clone() else { continue };

            // Auto-review on new PR / new head push.
            if auto_review {
                let latest = review_store::latest_for_pr(&db, &pr_key, Some(head_sha.clone()));
                let already_covered = latest.run.as_ref().is_some_and(|r| {
                    r.head_sha == head_sha && matches!(r.status.as_str(), "queued" | "running" | "validated" | "published")
                });
                if !already_covered {
                    let is_stacked = !matches!(source.base_branch.as_str(), "main" | "master" | "develop" | "trunk");
                    let _ = review::start_review(
                        state.clone(),
                        StartReviewInput {
                            project: project.clone(),
                            number,
                            title: source.title.clone(),
                            author: row.author_login.clone().unwrap_or_default(),
                            description: source.body.clone(),
                            base_ref: source.base_branch.clone(),
                            head_sha: head_sha.clone(),
                            force_full: false,
                            is_stacked_base: is_stacked,
                        },
                    )
                    .await;
                }
            }

            // PR comment activation (e.g. `/review`).
            if !trigger.trim().is_empty() {
                let activity = match project.provider {
                    Provider::GitHub => github::activity(&project, number).await.ok().flatten(),
                    Provider::GitLab => gitlab::activity(&project, number).await.ok().flatten(),
                };
                if let Some(act) = activity {
                    for comment in act.comments {
                        if comment.created_at.as_str() < cursor_since.as_str() {
                            continue;
                        }
                        if !matches_comment_trigger(&comment.body, &trigger) {
                            continue;
                        }
                        if is_comment_processed(&db, &project_id, &comment.id) {
                            continue;
                        }
                        let is_stacked = !matches!(source.base_branch.as_str(), "main" | "master" | "develop" | "trunk");
                        let started = review::start_review(
                            state.clone(),
                            StartReviewInput {
                                project: project.clone(),
                                number,
                                title: source.title.clone(),
                                author: row.author_login.clone().unwrap_or_default(),
                                description: source.body.clone(),
                                base_ref: source.base_branch.clone(),
                                head_sha: head_sha.clone(),
                                force_full: false,
                                is_stacked_base: is_stacked,
                            },
                        )
                        .await;
                        let run_id = started.as_ref().ok().map(|r| r.id.as_str());
                        record_comment_activation(&db, &project_id, &comment.id, number, run_id);
                        if started.is_ok() {
                            let short = &head_sha[..head_sha.len().min(8)];
                            let ack = format!(
                                "<!-- domovoi-review-queued:v1 repo={} pr={number} head={head_sha} -->\n⏳ Domovoi queued an AI code review for commit `{short}`.",
                                project.repository
                            );
                            let _ = match project.provider {
                                Provider::GitHub => crate::gh::run(
                                    project.cwd(),
                                    &project.host,
                                    &["pr", "comment", &number.to_string(), "--repo", &project.repository, "--body", &ack],
                                    None,
                                )
                                .await
                                .map(|_| ()),
                                Provider::GitLab => gitlab::comment(&project, number, &ack).await,
                            };
                        }
                    }
                }
            }
        }

        update_cursor(&db, &project_id, &now_iso);
    }
}

fn load_or_init_cursor(db: &crate::db::Db, project_id: &str) -> String {
    if let Ok(Some(since)) = db.read(|c| {
        c.query_row("SELECT comment_cursor_since FROM pr_watcher_cursors WHERE project_id = ?1", params![project_id], |r| r.get::<_, String>(0))
            .optional()
    }) {
        return since;
    }
    let now = crate::time_fmt::iso_now();
    let _ = db.with(|c| {
        c.execute(
            "INSERT OR IGNORE INTO pr_watcher_cursors (project_id, last_polled_at, comment_cursor_since) VALUES (?1, ?2, ?2)",
            params![project_id, now],
        )
    });
    now
}

fn update_cursor(db: &crate::db::Db, project_id: &str, now_iso: &str) {
    let _ = db.with(|c| {
        c.execute(
            "INSERT OR REPLACE INTO pr_watcher_cursors (project_id, last_polled_at, comment_cursor_since) VALUES (?1, ?2, ?2)",
            params![project_id, now_iso],
        )
    });
}

fn is_comment_processed(db: &crate::db::Db, project_id: &str, comment_id: &str) -> bool {
    db.read(|c| {
        c.query_row(
            "SELECT 1 FROM pr_comment_activations WHERE project_id = ?1 AND comment_id = ?2",
            params![project_id, comment_id],
            |r| r.get::<_, i64>(0),
        )
        .optional()
    })
    .ok()
    .flatten()
    .is_some()
}

fn record_comment_activation(db: &crate::db::Db, project_id: &str, comment_id: &str, pr_number: u64, run_id: Option<&str>) {
    let now = crate::time_fmt::iso_now();
    let _ = db.with(|c| {
        c.execute(
            "INSERT OR IGNORE INTO pr_comment_activations (project_id, comment_id, pr_number, run_id, processed_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![project_id, comment_id, pr_number as i64, run_id, now],
        )
    });
}

/// Optional push-based webhook helper: extracts a PR number from a GitHub `pull_request` /
/// `issue_comment` or GitLab `Merge Request Hook` payload so webhook deliveries can wake
/// the PR watcher immediately.
pub fn webhook_pr_event_number(body: &Value) -> Option<u64> {
    body.pointer("/pull_request/number")
        .and_then(Value::as_u64)
        .or_else(|| body.pointer("/issue/number").and_then(Value::as_u64).filter(|_| body.pointer("/issue/pull_request").is_some()))
        .or_else(|| body.pointer("/object_attributes/iid").and_then(Value::as_u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn summary(state: &str, checks: Option<&str>) -> PullRequestSummary {
        serde_json::from_value(json!({
            "provider": "github",
            "host": "github.com",
            "projectId": "p1",
            "repository": "acme/app",
            "number": 7,
            "title": "Test",
            "url": "https://github.com/acme/app/pull/7",
            "state": state,
            "headBranch": "feat",
            "baseBranch": "main",
            "mergeability": "mergeable",
            "conflicting": false,
            "checksState": checks,
            "createdAt": "2026-01-01T00:00:00.000Z",
            "updatedAt": "2026-01-01T00:00:00.000Z",
            "viewerReviewRequested": false,
            "labels": [],
        }))
        .unwrap()
    }

    fn watch(until: Option<&str>) -> PullRequestWatch {
        serde_json::from_value(json!({
            "startedAt": "2026-01-01T00:00:00.000Z",
            "headSha": null,
            "failedChecks": [],
            "passed": false,
            "conflicting": false,
            "wakes": 0,
            "remarksThrough": "2026-01-01T00:00:00.000Z",
            "remarkIds": [],
            "until": until,
        }))
        .unwrap()
    }

    #[test]
    fn evaluates_checks_and_merge_watch_transitions() {
        // Pending -> no wake.
        assert_eq!(evaluate_watch_transition(&watch(None), &summary("open", Some("pending")), None), None);

        // Passing on `until: checks` -> wakes and ends watch.
        let n = evaluate_watch_transition(&watch(None), &summary("open", Some("passing")), None).unwrap();
        assert!(n.end_watch);
        assert!(n.message.contains("passing"));

        // Passing on `until: merged` -> wakes but keeps watching!
        let n = evaluate_watch_transition(&watch(Some("merged")), &summary("open", Some("passing")), None).unwrap();
        assert!(!n.end_watch);

        // Merged -> wakes and ends watch.
        let n = evaluate_watch_transition(&watch(Some("merged")), &summary("merged", Some("passing")), None).unwrap();
        assert!(n.end_watch);
        assert!(n.message.contains("merged"));
    }

    #[test]
    fn matches_comment_triggers_and_webhook_payloads() {
        assert!(matches_comment_trigger("/review", "/review"));
        assert!(matches_comment_trigger("Looks ready\n/review full", "/review"));
        assert!(!matches_comment_trigger("/reviewer please check", "/review"));
        assert_eq!(webhook_pr_event_number(&json!({ "pull_request": { "number": 42 } })), Some(42));
        assert_eq!(webhook_pr_event_number(&json!({ "object_attributes": { "iid": 9 } })), Some(9));
    }
}
