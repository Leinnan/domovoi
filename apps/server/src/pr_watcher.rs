//! Background pull request watcher and automated review trigger worker.
//!
//! 1. Follows the pull requests threads link. A watched one (the agent's
//!    `watch_pull_request`, or the watch a pull request made from a thread
//!    starts) is read every cycle from GitHub (`gh`) or GitLab (`glab`);
//!    what the read says is written back to the link (its `snapshot`, which
//!    is what rows, chips and notifications show) and to the watch (what it
//!    has seen, so nothing is told twice). The thread's agent is woken with a
//!    system message for what the watch's policy names: everything for a
//!    watch an agent started; only failed checks, a bounded number of times
//!    in a row, for one that follows a pull request made from the thread.
//!    Links nobody watches are read at most every ten minutes while their
//!    pull request is open and their thread was active in the last two weeks.
//!    A merge settles the thread (`sidebarAutoSettleOnMerge`) when nothing
//!    else holds it open.
//! 2. Monitors opted-in projects (`autoReviewPrs`) for new PRs or new head pushes
//!    and enqueues incremental AI code reviews automatically.
//! 3. Scans PR comments for configured trigger commands (`reviewCommentTrigger`,
//!    e.g. `/review`), deduplicates them in `pr_comment_activations`, posts a
//!    confirmation comment, and starts a review run.
//!
//! Limits: a watch reports on its first read (no baseline is held back); a
//! merge that wakes the agent (a watch an agent started) does not settle the
//! thread, since the message it sends is activity on it; review comments wake
//! nobody, they are counted on the snapshot.

#![expect(clippy::disallowed_types, clippy::disallowed_macros, reason = "orchestrator commands dispatched as JSON payloads")]

use crate::orchestration::commands::pull_requests::{PullRequestKey, is_visible, links_of, same_snapshot};
use crate::review::{self, StartReviewInput, store as review_store};
use crate::rpc::source_control::github::{self, Listing};
use crate::rpc::source_control::gitlab;
use crate::rpc::source_control::pull_request_failure::Provider;
use crate::rpc::source_control::pull_requests::{PrRef, read_for_agent, read_reviews_for_agent, require_project};
use crate::rpc::source_control::review_guides;
use crate::state::SharedState;
use domovoi_model::typed::command::{ThreadAutoSettle, ThreadPullRequestLinkSync, ThreadPullRequestWatchSync};
use domovoi_model::typed::{
    AppThread, PullRequestChecks, PullRequestChecksState, PullRequestLink, PullRequestSnapshot, PullRequestState, PullRequestSummary, PullRequestWatch,
    PullRequestWatchPolicy, PullRequestWatchUntil, SettledOverride, ShellStatus, Timestamp, shell_status,
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

/// What a read of a watched pull request means for its watch.
#[derive(Clone, Debug, PartialEq)]
pub struct WatchOutcome {
    /// The watch as it is to be kept: what it has now seen.
    pub watch: PullRequestWatch,
    /// The watch is over.
    pub end: bool,
    /// What to tell the thread's agent, waking it.
    pub wake: Option<String>,
}

/// What a watch records of failed checks whose names the host did not give.
const UNNAMED_CHECKS: &str = "(checks)";

/// What the watch `watch` makes of a read of its pull request.
///
/// A watch tells of failed checks once per set of failing names and of
/// passing checks once, per head: a push re-arms both. Without a policy (a
/// watch an agent started) every transition wakes the agent. With one, only
/// what it names does, and failed checks at most `max_fix_attempts` times in
/// a row: the count starts again when the checks pass, and when the user has
/// written since the last such wake (`user_wrote_at`).
pub fn evaluate_watch_transition(
    watch: &PullRequestWatch,
    summary: &PullRequestSummary,
    checks: Option<&PullRequestChecks>,
    user_wrote_at: Option<&str>,
    at: &Timestamp,
) -> WatchOutcome {
    let label = format!("{}#{} ({})", summary.repository, summary.number, summary.url);
    let follows = watch.policy.is_some();
    let policy = watch.policy.clone().unwrap_or(PullRequestWatchPolicy { wake_on_failed_checks: true, wake_on_passed: true, wake_on_merged: true, max_fix_attempts: 0 });
    let mut next = watch.clone();

    // Merged or closed ends every watch.
    let ended = match summary.state {
        PullRequestState::Merged => Some(format!("Pull request {label} has been merged.")),
        PullRequestState::Closed => Some(format!("Pull request {label} was closed without merging.")),
        _ => None,
    };
    if let Some(message) = ended {
        return WatchOutcome { watch: next, end: true, wake: policy.wake_on_merged.then_some(message) };
    }

    // A new head: what was told of the old one says nothing of this one.
    if let Some(head) = &summary.head_sha
        && watch.head_sha.as_ref() != Some(head)
    {
        if watch.head_sha.is_some() {
            next.failed_checks.clear();
            next.passed = false;
        }
        next.head_sha = Some(head.clone());
    }

    let until_merged = matches!(watch.until, Some(PullRequestWatchUntil::Merged));
    let (mut end, mut wake) = (false, None);
    match summary.checks_state.as_ref().and_then(|s| s.as_ref()) {
        Some(PullRequestChecksState::Failing) => {
            let mut names: Vec<String> = checks.into_iter().flat_map(|c| c.checks.iter()).filter(|c| c.status.as_str() == "failure").map(|c| c.name.clone()).collect();
            names.sort();
            names.dedup();
            let seen = if names.is_empty() { vec![UNNAMED_CHECKS.to_owned()] } else { names.clone() };
            if next.failed_checks != seen {
                next.failed_checks = seen;
                next.passed = false;
                end = !until_merged;
                let detail = if names.is_empty() { "one or more checks failed".to_owned() } else { format!("failing checks: {}", names.join(", ")) };
                let user_wrote_since = matches!((user_wrote_at, next.last_fix_wake_at.as_ref()), (Some(wrote), Some(woken)) if wrote > woken.as_str());
                let attempts = if user_wrote_since { 0 } else { next.fix_attempts.unwrap_or(0) };
                if policy.wake_on_failed_checks && (policy.max_fix_attempts == 0 || attempts < policy.max_fix_attempts) {
                    next.wakes += 1;
                    wake = Some(if follows {
                        next.fix_attempts = Some(attempts + 1);
                        next.last_fix_wake_at = Some(at.clone());
                        let of = if policy.max_fix_attempts == 0 { String::new() } else { format!(" of {}", policy.max_fix_attempts) };
                        format!(
                            "Pull request {label} checks failed ({detail}). Read them with `pull_request_read` (sections: checks), fix what broke, then commit and push to the pull request's branch. This is automatic attempt {}{of}; say so instead of pushing if the failure is not this change's.",
                            attempts + 1
                        )
                    } else {
                        format!("Pull request {label} checks failed ({detail}).")
                    });
                } else if follows {
                    next.fix_attempts = Some(attempts);
                }
            }
        }
        Some(PullRequestChecksState::Passing) => {
            if !next.passed {
                next.passed = true;
                next.failed_checks.clear();
                next.fix_attempts = None;
                end = !until_merged;
                if policy.wake_on_passed {
                    next.wakes += 1;
                    wake = Some(format!("Pull request {label} checks are now passing."));
                }
            }
        }
        _ => {}
    }
    WatchOutcome { watch: next, end, wake }
}

/// What a link shows of its pull request, from a read of it.
pub fn snapshot_of(summary: &PullRequestSummary, unresolved_review_threads: Option<u64>, at: &Timestamp) -> PullRequestSnapshot {
    let time = |held: &Option<Option<String>>| held.clone().map(|inner| inner.map(Timestamp::from));
    PullRequestSnapshot {
        state: summary.state.clone(),
        title: summary.title.clone(),
        head_branch: summary.head_branch.clone(),
        base_branch: summary.base_branch.clone(),
        is_draft: summary.is_draft == Some(true),
        updated_at: Some(summary.updated_at.clone()),
        synced_at: at.clone(),
        closed_at: time(&summary.closed_at),
        merged_at: time(&summary.merged_at),
        author: summary.author.clone(),
        additions: summary.additions,
        deletions: summary.deletions,
        changed_files: summary.changed_files,
        review_decision: summary.review_decision.clone(),
        checks_state: summary.checks_state.clone(),
        mergeability: summary.mergeability.clone(),
        rebase_required: None,
        unresolved_review_threads,
    }
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
        // Initial short grace period so startup probes finish first (a test that sets its own pace waits that long instead).
        let paced = std::env::var("DOMOVOI_PR_WATCHER_EVERY_MS").ok().and_then(|v| v.parse::<u64>().ok()).map(Duration::from_millis);
        tokio::select! {
            () = shutdown.cancelled() => return,
            () = tokio::time::sleep(paced.unwrap_or(Duration::from_secs(10))) => {}
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

/// How often a link nobody watches is read while its pull request is open.
const UNWATCHED_EVERY: Duration = Duration::from_secs(600);
/// A thread quiet for longer has its unwatched links left alone.
const UNWATCHED_THREAD_AGE_MS: i64 = 14 * 24 * 60 * 60 * 1000;
/// The most unwatched links read in one cycle.
const UNWATCHED_PER_CYCLE: usize = 20;

/// When each unwatched link was last read (`project host repository number`).
static UNWATCHED_READ: std::sync::LazyLock<std::sync::Mutex<HashMap<String, std::time::Instant>>> = std::sync::LazyLock::new(Default::default);

/// Whether an unwatched link is due a read, marking it read if so.
fn unwatched_due(key: &str) -> bool {
    let mut read = crate::util::locked(&UNWATCHED_READ);
    let now = std::time::Instant::now();
    if read.get(key).is_some_and(|at| now.duration_since(*at) < UNWATCHED_EVERY) {
        return false;
    }
    read.insert(key.to_owned(), now);
    true
}

async fn poll_watched_threads(state: &SharedState) {
    let Ok(threads) = state.store.app_threads() else { return };
    // A read with its checks (a watched link) is not one without them.
    type WatchKey = (String, String, String, u64, bool);
    let mut cache: HashMap<WatchKey, Option<(PullRequestSummary, Option<PullRequestChecks>)>> = HashMap::new();
    let mut unwatched_read = 0;
    let now_ms = crate::time_fmt::now_millis();
    let settle_on_merge = state.settings.read().await.sidebar_auto_settle_on_merge != Some(false);

    for t in threads.into_iter().filter(|t| t.deleted_at.is_none() && t.archived_at.is_none()) {
        let recent = crate::time_fmt::millis_from_iso(t.updated_at.as_str()).is_some_and(|at| now_ms - at < UNWATCHED_THREAD_AGE_MS);
        let links = links_of(&t);
        for link in links.iter().filter(|link| is_visible(link)) {
            let over = link.snapshot.as_ref().is_some_and(|snapshot| snapshot.state != PullRequestState::Open);
            if link.watch.is_none() {
                let key = format!("{} {} {} {}", t.project_id, link.host, link.repository, link.number);
                if over || !recent || unwatched_read >= UNWATCHED_PER_CYCLE || !unwatched_due(&key) {
                    continue;
                }
                unwatched_read += 1;
            }
            let cache_key: WatchKey = (t.project_id.as_str().to_owned(), link.host.clone(), link.repository.clone(), link.number, link.watch.is_some());
            let fetched = if let Some(entry) = cache.get(&cache_key) {
                entry.clone()
            } else {
                let res = read_for_agent(state, t.project_id.as_str(), &link.host, &link.repository, link.number, link.watch.is_some()).await.ok();
                cache.insert(cache_key, res.clone());
                res
            };
            let Some((summary, checks)) = fetched else { continue };
            let tid = t.id.as_str();
            let at = Timestamp::from(crate::time_fmt::iso_now());
            let stamp = crate::util::new_uuid();

            // What the link shows. Review conversations are counted again only when the pull request moved.
            let held = link.snapshot.as_ref();
            let moved = held.and_then(|snapshot| snapshot.updated_at.as_ref()).map(Timestamp::as_str) != Some(summary.updated_at.as_str());
            let unresolved = if moved {
                unresolved_review_threads(state, t.project_id.as_str(), &link.host, &link.repository, link.number).await.or(held.and_then(|snapshot| snapshot.unresolved_review_threads))
            } else {
                held.and_then(|snapshot| snapshot.unresolved_review_threads)
            };
            let snapshot = snapshot_of(&summary, unresolved, &at);
            if !held.is_some_and(|held| same_snapshot(held, &snapshot)) {
                let sync = ThreadPullRequestLinkSync {
                    command_id: format!("pr-link-sync:{stamp}").into(),
                    thread_id: tid.into(),
                    host: link.host.clone(),
                    repository: link.repository.clone(),
                    number: link.number,
                    snapshot,
                    stack: None,
                };
                if let Err(e) = state.orchestrator.dispatch(sync.into()).await {
                    tracing::warn!(thread = %tid, number = link.number, "what the pull request shows was not written: {e}");
                }
            }

            // What its watch has seen, and what the agent is told.
            let mut woke = false;
            if let Some(watch) = &link.watch {
                let shell = state.store.thread_shell(tid).ok().flatten();
                let user_wrote_at = shell.as_ref().and_then(|shell| shell.latest_user_authored_message_at.as_ref()).and_then(Option::as_ref).map(Timestamp::as_str);
                let outcome = evaluate_watch_transition(watch, &summary, checks.as_ref(), user_wrote_at, &at);
                if outcome.end || outcome.watch != *watch {
                    let sync = ThreadPullRequestWatchSync {
                        command_id: format!("pr-watch-sync:{stamp}").into(),
                        thread_id: tid.into(),
                        host: link.host.clone(),
                        repository: link.repository.clone(),
                        number: link.number,
                        started_at: watch.started_at.clone(),
                        watch: (!outcome.end).then(|| outcome.watch.clone()),
                        checks_pending_before: None,
                        ended_watch: outcome.end.then(|| outcome.watch.clone()),
                        others: None,
                        wake: None,
                    };
                    if let Err(e) = state.orchestrator.dispatch(sync.into()).await {
                        // Not written: telling the agent now would be told again next cycle.
                        tracing::warn!(thread = %tid, number = link.number, "what the watch saw was not written: {e}");
                        continue;
                    }
                }
                if let Some(text) = outcome.wake {
                    woke = true;
                    let command_id = format!("pr-watch-wake:{stamp}");
                    let dispatch_msg = json!({
                        "type": "message.dispatch",
                        "commandId": command_id,
                        "threadId": tid,
                        "messageId": format!("message:{command_id}"),
                        "text": text,
                        "attachments": [],
                        "createdBy": "system",
                        "creationSource": "server",
                        "dispatchMode": { "type": "queue_after_active" },
                    });
                    let _ = crate::rpc::shell::dispatch_command(&state.orchestrator, dispatch_msg).await;
                }
            }

            // A merge settles the thread, when it was seen to happen and nothing else holds the thread open.
            let merged_now = summary.state == PullRequestState::Merged && (link.watch.is_some() || held.is_some_and(|snapshot| snapshot.state == PullRequestState::Open));
            if merged_now && settle_on_merge && !woke && may_settle(state, &t, link) {
                let settle = ThreadAutoSettle { command_id: format!("pr-merged-settle:{stamp}").into(), thread_id: tid.into(), snapshot_at: None, settled_at: None };
                if let Err(e) = state.orchestrator.dispatch(settle.into()).await {
                    tracing::warn!(thread = %tid, "the thread was not settled after its pull request merged: {e}");
                }
            }
        }
    }
}

/// Whether a thread whose pull request `merged` just merged is to be
/// settled: it is not settled already, it was not told to stay, no run of
/// it is under way or waiting on the user, and no other pull request it
/// links is still open.
fn may_settle(state: &SharedState, thread: &AppThread, merged: &PullRequestLink) -> bool {
    let settled = thread.settled_override == Some(Some(SettledOverride::Settled)) || thread.settled_at.as_ref().and_then(Option::as_ref).is_some();
    let told_to_stay = thread.auto_settle_disabled_at.as_ref().and_then(Option::as_ref).is_some() || thread.settled_override == Some(Some(SettledOverride::Active));
    if settled || told_to_stay {
        return false;
    }
    let busy = state.store.thread_shell(thread.id.as_str()).ok().flatten().is_none_or(|shell| matches!(shell_status(&shell), ShellStatus::Working | ShellStatus::Approval));
    let other_open = links_of(thread).iter().any(|link| {
        is_visible(link) && PullRequestKey::of(link) != PullRequestKey::of(merged) && link.snapshot.as_ref().is_some_and(|snapshot| snapshot.state == PullRequestState::Open)
    });
    !busy && !other_open
}

/// How many conversations on a pull request's diff are still open, if the host says.
async fn unresolved_review_threads(state: &SharedState, project_id: &str, host: &str, repository: &str, number: u64) -> Option<u64> {
    let threads = read_reviews_for_agent(state, project_id, host, repository, number).await.ok()?;
    Some(threads.len() as u64)
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

    fn checks(failing: &[&str]) -> PullRequestChecks {
        let rows: Vec<Value> = failing.iter().map(|name| json!({ "name": name, "status": "failure", "description": null, "url": null })).collect();
        serde_json::from_value(json!({ "state": "open", "checks": rows })).unwrap()
    }

    fn at(minute: u32) -> Timestamp {
        format!("2026-01-01T00:{minute:02}:00.000Z").into()
    }

    /// A read of the pull request at `head`, with these checks failing (`None`: the state alone).
    fn read(watch: &PullRequestWatch, head: &str, state: &str, failing: Option<&[&str]>, user_wrote_at: Option<&str>, minute: u32) -> WatchOutcome {
        let mut s = summary("open", Some(state));
        s.head_sha = Some(head.to_owned());
        evaluate_watch_transition(watch, &s, failing.map(checks).as_ref(), user_wrote_at, &at(minute))
    }

    #[test]
    fn a_watch_an_agent_started_wakes_it_for_every_transition_once() {
        // Pending: nothing to tell, nothing to keep.
        let quiet = evaluate_watch_transition(&watch(None), &summary("open", Some("pending")), None, None, &at(1));
        assert_eq!((quiet.end, quiet.wake.clone(), quiet.watch == watch(None)), (false, None, true));

        // Passing on `until: checks` wakes and ends the watch.
        let n = evaluate_watch_transition(&watch(None), &summary("open", Some("passing")), None, None, &at(1));
        assert!(n.end && n.wake.as_deref().is_some_and(|m| m.contains("passing")), "{n:?}");

        // Passing on `until: merged` wakes once and keeps watching: the next read of the same state says nothing.
        let n = evaluate_watch_transition(&watch(Some("merged")), &summary("open", Some("passing")), None, None, &at(1));
        assert!(!n.end && n.wake.is_some() && n.watch.passed);
        let again = evaluate_watch_transition(&n.watch, &summary("open", Some("passing")), None, None, &at(2));
        assert_eq!((again.wake, again.watch == n.watch), (None, true), "told once");

        // Failed checks whose names the host does not give are told once too.
        let failed = evaluate_watch_transition(&watch(Some("merged")), &summary("open", Some("failing")), None, None, &at(1));
        assert!(failed.wake.as_deref().is_some_and(|m| m.contains("one or more checks failed")));
        assert_eq!(evaluate_watch_transition(&failed.watch, &summary("open", Some("failing")), None, None, &at(2)).wake, None);
        assert_eq!(failed.watch.fix_attempts, None, "no attempts are counted for a watch without a policy");

        // Merged wakes and ends the watch.
        let n = evaluate_watch_transition(&watch(Some("merged")), &summary("merged", Some("passing")), None, None, &at(1));
        assert!(n.end && n.wake.as_deref().is_some_and(|m| m.contains("merged")));
    }

    #[test]
    fn a_watch_that_follows_a_pull_request_wakes_the_agent_only_to_fix_checks_and_not_forever() {
        let mut following = watch(Some("merged"));
        following.policy = Some(PullRequestWatchPolicy::following(true, 2));

        // Passing checks and a merge change what is shown and wake nobody.
        let passed = read(&following, "a1", "passing", None, None, 1);
        assert_eq!((passed.wake.clone(), passed.end, passed.watch.passed), (None, false, true));
        let merged = evaluate_watch_transition(&following, &summary("merged", Some("passing")), None, None, &at(1));
        assert_eq!((merged.wake, merged.end), (None, true));

        // The first failure wakes the agent, with what to do and which attempt it is.
        let first = read(&following, "a1", "failing", Some(&["lint", "test"]), None, 1);
        let told = first.wake.clone().expect("a wake");
        assert!(told.contains("failing checks: lint, test") && told.contains("automatic attempt 1 of 2") && told.contains("pull_request_read"), "{told}");
        assert_eq!((first.watch.fix_attempts, first.watch.last_fix_wake_at.clone(), first.end), (Some(1), Some(at(1)), false));
        // The same failure read again is not told again.
        assert_eq!(read(&first.watch, "a1", "failing", Some(&["test", "lint"]), None, 2).wake, None);

        // The agent pushes; the new head fails differently: the second attempt.
        let second = read(&first.watch, "b2", "failing", Some(&["test"]), None, 3);
        assert!(second.wake.as_deref().is_some_and(|m| m.contains("automatic attempt 2 of 2")));
        assert_eq!(second.watch.fix_attempts, Some(2));
        // It pushes again and it still fails: the bound holds, and what failed is still recorded.
        let third = read(&second.watch, "c3", "failing", Some(&["test"]), None, 5);
        assert_eq!((third.wake.clone(), third.watch.fix_attempts, third.watch.failed_checks.clone()), (None, Some(2), vec!["test".to_owned()]));
        assert_eq!(read(&third.watch, "c3", "failing", Some(&["test"]), None, 6).watch, third.watch, "and nothing changes while it stays so");

        // The user writes in the thread: the agent has its attempts back.
        let after_user = read(&third.watch, "d4", "failing", Some(&["test"]), Some("2026-01-01T00:07:00.000Z"), 8);
        assert!(after_user.wake.as_deref().is_some_and(|m| m.contains("automatic attempt 1 of 2")), "{after_user:?}");
        // Words written before the last wake do not count.
        assert_eq!(read(&third.watch, "d4", "failing", Some(&["test"]), Some("2026-01-01T00:02:00.000Z"), 8).wake, None);

        // Passing checks start the count again.
        let green = read(&third.watch, "e5", "passing", None, None, 9);
        assert_eq!((green.wake.clone(), green.watch.fix_attempts, green.watch.failed_checks.is_empty()), (None, None, true));
        assert!(read(&green.watch, "f6", "failing", Some(&["test"]), None, 10).wake.as_deref().is_some_and(|m| m.contains("attempt 1 of 2")));

        // A policy that does not wake for failures only records them; no limit is no limit.
        let mut quiet = watch(Some("merged"));
        quiet.policy = Some(PullRequestWatchPolicy::following(false, 2));
        let seen = read(&quiet, "a1", "failing", Some(&["lint"]), None, 1);
        assert_eq!((seen.wake, seen.watch.failed_checks), (None, vec!["lint".to_owned()]));
        let mut unbounded = watch(Some("merged"));
        unbounded.policy = Some(PullRequestWatchPolicy::following(true, 0));
        unbounded.fix_attempts = Some(40);
        assert!(read(&unbounded, "a1", "failing", Some(&["lint"]), None, 1).wake.as_deref().is_some_and(|m| m.contains("automatic attempt 41;")));
    }

    #[test]
    fn a_snapshot_is_what_a_read_says() {
        let mut s = summary("open", Some("failing"));
        s.review_decision = Some(Some(domovoi_model::typed::PullRequestReviewDecision::ChangesRequested));
        s.is_draft = Some(true);
        let made = snapshot_of(&s, Some(2), &at(1));
        assert_eq!((made.state.as_str(), made.title.as_str(), made.is_draft, made.unresolved_review_threads), ("open", "Test", true, Some(2)));
        assert_eq!(made.checks_state, Some(Some(PullRequestChecksState::Failing)));
        assert_eq!(made.updated_at.as_ref().map(Timestamp::as_str), Some("2026-01-01T00:00:00.000Z"));
        // Read again later with nothing changed: the same, whenever it was read.
        assert!(same_snapshot(&made, &snapshot_of(&s, Some(2), &at(9))));
        assert!(!same_snapshot(&made, &snapshot_of(&s, Some(3), &at(9))));
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
