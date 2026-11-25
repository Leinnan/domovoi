//! `pullRequests.*` RPCs, after t3code's `PullRequestService`,
//! `PullRequestReviewGuideService` and `linkedThreads`: which project a pull
//! request is read through, what a viewer may do, and what is refused. What
//! the host says is read by its driver: GitHub's in `github` (through `gh`,
//! `crate::gh`), GitLab's merge requests in `gitlab` (through `glab`,
//! `crate::glab`); the answers are `pull_request_records` and
//! `domovoi_model::typed` records, the failures `pull_request_failure`'s.
//!
//! Projects are routed to a repository the way t3code does (`requireProject`):
//! the project's repository identity names the host, the path and the
//! provider kind, which picks the driver ([`Provider`], t3code's
//! `PullRequestProviderRegistry`). A host whose kind the identity leaves out
//! is GitLab when its name says so (`gitlab.com`, or a `gitlab` label: t3code's
//! GitLab Self-Hosted). Bitbucket and other hosts are listed as "cannot be
//! browsed here yet", which is what t3code says for hosts without a provider.
//!
//! Differences from t3code, all answered in its wire shapes:
//! - no read caches: every read asks `gh` again, so `allowStale` and
//!   `invalidate` only bump the refresh counter readers subscribe to;
//! - listings are not paginated: one page of up to `limit` rows per repository,
//!   `truncated` set when a repository had more, and no continuation cursors;
//! - stacks (`stack`, stack actions) are not read: `stack` answers null;
//! - checks leave out t3code's workflow-run approval placeholders;
//! - review guides (`review_guides`) are written from a scratch worktree at
//!   the head and kept per pull request; the settled review discussion is
//!   not passed to the model;
//! - GitLab's limits are in `gitlab`'s header; its viewed files are not kept
//!   (`filesViewed` answers none, `setFilesViewed` refuses) and its review
//!   threads are not paged (`threadComments` refuses, as t3code does);
//! - the linked-thread change detector starts with the first pull request RPC
//!   rather than at boot, so "unknown since start" counts from then.

#![expect(clippy::disallowed_types, reason = "a request buffered to be read twice: as the pull request reference (whose error text is the failure's detail) and as the method's fields")]

use super::github::{self, FileOfDiff, ListRow, Listing};
use super::gitlab;
use super::pull_request_failure::{Failure, Provider, cli_detail};
use super::pull_request_records::{
    ChangeBreakdown, DiffFileContents, DiffStat, FilesViewed, FilterOptions, LabelCandidate, LabelCandidates, LinkedThread, LinkedThreads, ListStats,
    PullRequestActivity, PullRequestDetail, PullRequestPreview, ReferenceKind, ReviewGuideResult, ReviewerCandidate, ReviewerCandidates, ReviewerKind, Routing,
    RoutingIdentity, ThreadCommentsPage,
};
use super::review_guides::GuideSettings;
use crate::gh::lenient::{Fields, Loose, Rows, Scalar, rows};
use crate::gh::{self, GhError, Var};
use crate::rpc::Never;
use crate::state::{AppState, ConnCtx};
use effect_rpc::Router;
use futures::StreamExt;
use futures::stream::BoxStream;
use serde::de::{DeserializeOwned, IgnoredAny};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::future::Future;
use std::sync::{Arc, OnceLock};
use domovoi_model::typed::{
    AppThread, EventBody, PullRequestAction, PullRequestChecks, PullRequestLinkSource, PullRequestListProjectError, PullRequestListResult,
    PullRequestProviderSummary, PullRequestSummary, RepositoryIdentity, SettledOverride,
};
use tokio::sync::watch;

const DEFAULT_LIST_LIMIT: usize = 99;

// ---------------------------------------------------------------------------
// Payloads
// ---------------------------------------------------------------------------

/// What names one pull request in a call about it (`PullRequestRef`).
#[derive(Deserialize, Clone, Debug)]
#[serde(rename_all = "camelCase")]
pub struct PrRef {
    pub project_id: String,
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub expected_account_id: Option<String>,
    pub repository: String,
    pub number: u64,
}

/// A reference as a payload gave it. One that cannot be read is no refused
/// payload: the call fails with why, as it does for any reference that
/// leads nowhere.
#[derive(Debug)]
struct Reference(Result<PrRef, String>);

impl Reference {
    /// The payload is held as JSON first, because a call's payload is read
    /// twice: as the reference here, and as the method's own fields.
    fn read(payload: Value) -> Self {
        Reference(serde_json::from_value(payload).map_err(|e| e.to_string()))
    }

    fn resolve(self) -> Result<PrRef, Failure> {
        self.0.map_err(|e| Failure::operation("resolveRepository", format!("Invalid pull request reference: {e}")))
    }
}

impl Default for Reference {
    /// What a payload without a reference gives.
    fn default() -> Self {
        Reference::read(Value::Null)
    }
}

impl<'de> Deserialize<'de> for Reference {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Value::deserialize(deserializer).map(Reference::read)
    }
}

/// The payload of a call about one pull request: its reference, and beside
/// it the fields the method takes, each read if it is there and of the
/// expected type (`gh::lenient`).
struct Call<T> {
    reference: Reference,
    fields: T,
}

impl<'de, T: DeserializeOwned + Default> Deserialize<'de> for Call<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let payload = Value::deserialize(deserializer)?;
        let fields = Fields::<T>::deserialize(&payload).map_err(serde::de::Error::custom)?.0;
        Ok(Call { reference: Reference::read(payload), fields })
    }
}

/// A value of a payload that is handed on to GitHub as a variable of a
/// GraphQL request: a string as one, a number or a boolean as it is written,
/// and `null` for anything else or nothing.
fn var(given: &Loose<Scalar>) -> Var<'_> {
    match &given.0 {
        Some(Scalar::Text(text)) => Var::Text(text),
        Some(Scalar::Number(number)) => Var::Raw(number.to_string()),
        Some(Scalar::Flag(flag)) => Var::Raw(flag.to_string()),
        None => Var::null(),
    }
}

/// The fields of a call that takes none beside its reference.
#[derive(Deserialize, Default)]
struct NoFields {}

// ---------------------------------------------------------------------------
// Project routing
// ---------------------------------------------------------------------------

/// The project and the GitHub repository a pull request is read through:
/// the project's own checkout of it, or another's, or (for github.com) none.
#[derive(Clone, Debug)]
pub struct Supported {
    pub project_id: String,
    pub title: String,
    pub workspace_root: String,
    pub repository: String,
    pub host: String,
    /// The driver the host is read through.
    pub provider: Provider,
}

impl Supported {
    pub fn cwd(&self) -> Option<&str> {
        Some(self.workspace_root.as_str())
    }

    /// `(accountId, viewer)` of the account the host is read as.
    async fn identity(&self) -> Result<(String, String), GhError> {
        match self.provider {
            Provider::GitHub => gh::identity(self.cwd(), &self.host).await,
            Provider::GitLab => crate::glab::identity(self.cwd(), &self.host).await,
        }
    }
}

#[derive(Default)]
struct Workspace {
    supported: Vec<Supported>,
    /// host → (kind, projectCount)
    unimplemented: Vec<(String, String, u64)>,
}

/// `(host, owner/name, provider kind)` of a repository identity.
fn identity_route(identity: &RepositoryIdentity) -> Option<(String, String, Option<String>)> {
    let (host, path) = identity.canonical_key.split_once('/')?;
    let repository = match (identity.owner.as_deref(), identity.name.as_deref()) {
        (Some(o), Some(n)) => format!("{o}/{n}"),
        _ => path.to_owned(),
    };
    Some((host.to_lowercase(), repository, identity.provider.clone()))
}

/// The provider kind of a routed identity: as the identity says, or GitLab
/// for a host whose name says it is one (`recognizeRemote`).
fn route_kind(host: &str, kind: Option<String>) -> Option<String> {
    kind.filter(|k| k != "unknown").or_else(|| crate::glab::recognizes_host(host).then(|| "gitlab".to_owned()))
}

/// The repository a project is of: as its row says, or as its folder says now.
async fn project_identity(project: &domovoi_contracts::shell::ProjectShell) -> Option<RepositoryIdentity> {
    match &project.repository_identity {
        Some(identity) => Some(identity.clone()),
        None => crate::vcs::repository_identity(&project.workspace_root).await,
    }
}

async fn workspace_projects(
    state: &AppState,
    project_id: Option<&str>,
    project_ids: Option<&[String]>,
    host: Option<&str>,
) -> Result<Workspace, Failure> {
    let projects = state
        .store
        .list_projects()
        .map_err(|_| Failure::operation("listProjects", "The project list could not be read."))?;
    let mut ws = Workspace::default();
    let mut seen = HashSet::new();
    for project in projects {
        if project_id.is_some_and(|id| id != project.id) || project_ids.is_some_and(|ids| !ids.contains(&project.id)) {
            continue;
        }
        let Some((repo_host, repository, kind)) = project_identity(&project).await.as_ref().and_then(identity_route) else { continue };
        let Some(kind) = route_kind(&repo_host, kind) else { continue };
        if host.is_some_and(|h| h.to_lowercase() != repo_host) {
            continue;
        }
        let key = format!("{repo_host}/{repository}").to_lowercase();
        if !seen.insert(key) {
            continue;
        }
        let Some(provider) = Provider::of(&kind) else {
            match ws.unimplemented.iter_mut().find(|(h, _, _)| *h == repo_host) {
                Some(slot) => slot.2 += 1,
                None => ws.unimplemented.push((repo_host, kind, 1)),
            }
            continue;
        };
        ws.supported.push(Supported {
            project_id: project.id.clone(),
            title: project.title.clone(),
            workspace_root: project.workspace_root.clone(),
            repository,
            host: repo_host,
            provider,
        });
    }
    Ok(ws)
}

/// t3code's `requireProject`.
async fn require_project(state: &AppState, r: &PrRef) -> Result<Supported, Failure> {
    let ws = workspace_projects(state, Some(&r.project_id), None, None).await?;
    let own = ws.supported.first().cloned();
    let repository = r.repository.trim();
    let host = r.host.as_deref().map(|h| h.trim().to_lowercase());
    if let Some(own) = &own {
        if own.repository.eq_ignore_ascii_case(repository) && host.as_deref().is_none_or(|h| h == own.host) {
            return Ok(own.clone());
        }
    }
    let Some(host) = host else {
        return Err(match own {
            None => Failure::unsupported(),
            Some(_) => Failure::operation("resolveRepository", "The change request does not belong to the selected project."),
        });
    };
    let all = workspace_projects(state, None, None, Some(&host)).await?;
    let route = all
        .supported
        .iter()
        .find(|c| c.repository.eq_ignore_ascii_case(repository))
        .or_else(|| all.supported.first())
        .cloned();
    if let Some(mut route) = route {
        if !route.repository.eq_ignore_ascii_case(repository) {
            route.repository = repository.to_owned();
        }
        return Ok(route);
    }
    // GitHub reads public hosts without a checkout (`readsPublicHostWithoutCheckout`).
    if host == "github.com" {
        if let Ok(Some(project)) = state.store.get_project(&r.project_id) {
            return Ok(Supported {
                project_id: project.id,
                title: project.title,
                workspace_root: project.workspace_root,
                repository: repository.to_owned(),
                host,
                provider: Provider::GitHub,
            });
        }
    }
    Err(Failure::unsupported())
}

/// t3code's `withRoutingCredential`: an expected account must be the signed-in one.
async fn verify_account(state: &AppState, r: &PrRef) -> Result<(), Failure> {
    let Some(expected) = &r.expected_account_id else { return Ok(()) };
    let rejected = || Failure::operation("routeIdentity", "The signed-in account could not be verified before starting the operation.");
    let project = require_project(state, r).await.map_err(|_| rejected())?;
    if r.host.as_deref().map(str::to_lowercase).as_deref() != Some(project.host.as_str()) {
        return Err(rejected());
    }
    match project.identity().await {
        Ok((account, _)) if &account == expected => Ok(()),
        _ => Err(rejected()),
    }
}

/// Routes a reference after the account check every wrapped read and write starts with.
async fn routed(state: &AppState, reference: Reference) -> Result<(PrRef, Supported), Failure> {
    let r = reference.resolve()?;
    verify_account(state, &r).await?;
    let project = require_project(state, &r).await?;
    Ok((r, project))
}

/// For the agent's pull request tools (`crate::mcp::pull_requests`): the
/// host and path of the project's own GitHub or GitLab repository.
pub(crate) async fn project_repository(state: &AppState, project_id: &str) -> Option<(String, String)> {
    let ws = workspace_projects(state, Some(project_id), None, None).await.ok()?;
    ws.supported.into_iter().next().map(|p| (p.host, p.repository))
}

/// For the agent's pull request tools: a pull request's summary (and its
/// checks with `with_checks`), read through `project_id` as RPC reads are.
pub(crate) async fn read_for_agent(
    state: &AppState,
    project_id: &str,
    host: &str,
    repository: &str,
    number: u64,
    with_checks: bool,
) -> Result<(PullRequestSummary, Option<PullRequestChecks>), Failure> {
    let r = PrRef { project_id: project_id.to_owned(), host: Some(host.to_owned()), expected_account_id: None, repository: repository.to_owned(), number };
    let p = require_project(state, &r).await?;
    let summary = read_summary(&p, number, false).await.map_err(failed(&p, "summary"))?;
    let checks = if with_checks { Some(read_checks(&p, number).await.map_err(failed(&p, "checks"))?) } else { None };
    Ok((summary, checks))
}

/// The web address of a pull request on `host` (t3code's `changeRequestUrl`):
/// GitLab's `/-/merge_requests/N` on a host whose name says it is GitLab, else GitHub's `/pull/N`.
pub(crate) fn change_request_url(host: &str, repository: &str, number: u64) -> String {
    if crate::glab::recognizes_host(host) {
        format!("https://{host}/{repository}/-/merge_requests/{number}")
    } else {
        format!("https://{host}/{repository}/pull/{number}")
    }
}

async fn read_summary(p: &Supported, number: u64, reply: bool) -> Result<PullRequestSummary, GhError> {
    match p.provider {
        Provider::GitHub => github::summary(p, number, reply).await,
        Provider::GitLab => gitlab::summary(p, number).await,
    }
}

async fn read_checks(p: &Supported, number: u64) -> Result<PullRequestChecks, GhError> {
    match p.provider {
        Provider::GitHub => github::pull_request_checks(p, number).await,
        Provider::GitLab => gitlab::pull_request_checks(p, number).await,
    }
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Refresh counters and the linked-thread change detector
// ---------------------------------------------------------------------------

fn refreshes() -> &'static watch::Sender<u64> {
    static TX: OnceLock<watch::Sender<u64>> = OnceLock::new();
    TX.get_or_init(|| watch::channel(0).0)
}

fn linked_refreshes() -> &'static watch::Sender<u64> {
    static TX: OnceLock<watch::Sender<u64>> = OnceLock::new();
    TX.get_or_init(|| watch::channel(0).0)
}

fn notify_refresh() {
    refreshes().send_modify(|n| *n += 1);
}

fn notify_linked() {
    linked_refreshes().send_modify(|n| *n += 1);
}

/// Emits the counter's current value when positive, then every later change.
fn counter_stream(tx: &'static watch::Sender<u64>) -> BoxStream<'static, Result<u64, Never>> {
    let mut rx = tx.subscribe();
    async_stream::stream! {
        let first = *rx.borrow_and_update();
        if first > 0 {
            yield Ok(first);
        }
        while rx.changed().await.is_ok() {
            let v = *rx.borrow_and_update();
            yield Ok(v);
        }
    }
    .boxed()
}

/// Visible pull request keys of a thread record (t3code's `linkedKeys`).
fn linked_keys(thread: &AppThread) -> Vec<String> {
    if thread.deleted_at.is_some() || thread.lineage.side_chat == Some(true) {
        return vec![];
    }
    let mut keys: Vec<String> = thread
        .pull_requests
        .iter()
        .flatten()
        .filter(|link| link.source != PullRequestLinkSource::StackDismissed)
        .map(|link| format!("{}/{}#{}", link.host, link.repository, link.number).to_lowercase())
        .collect();
    keys.sort();
    keys
}

/// What readers of linked threads see of `thread`, as one string to compare; `None` for a thread with no link shown.
fn linked_view(thread: &AppThread, keys: &[String]) -> Option<String> {
    if keys.is_empty() {
        return None;
    }
    let settled = thread.settled_at.as_ref().and_then(Option::as_ref).is_some() || thread.settled_override == Some(Some(SettledOverride::Settled));
    serde_json::to_string(&(keys, &thread.title, &thread.project_id, thread.archived_at.is_some(), settled)).ok()
}

#[derive(Debug, PartialEq)]
enum LinkedChange {
    Changed,
    Reordered,
}

/// t3code's `makeLinkedThreadsChangeDetector`.
#[derive(Default)]
struct LinkedDetector {
    views: HashMap<String, Option<String>>,
    active_at: HashMap<String, String>,
}

impl LinkedDetector {
    fn active(&mut self, thread_id: &str, at: &str) -> bool {
        match self.active_at.get(thread_id) {
            Some(prev) if at > prev.as_str() => {
                self.active_at.insert(thread_id.to_owned(), at.to_owned());
                true
            }
            _ => false,
        }
    }

    /// What an event of `thread_id` changed for readers of linked threads.
    /// `thread` is the thread's record when the event carries it.
    fn observe(&mut self, thread_id: &str, ty: &str, occurred_at: &str, thread: Option<&AppThread>) -> Option<LinkedChange> {
        let thread_id = thread_id.to_owned();
        if !ty.starts_with("thread.") {
            if !self.views.contains_key(&thread_id) {
                return Some(LinkedChange::Reordered);
            }
            return self.active(&thread_id, occurred_at).then_some(LinkedChange::Reordered);
        }
        let thread = thread?;
        let view = linked_view(thread, &linked_keys(thread));
        let known = self.views.contains_key(&thread_id) || ty == "thread.created";
        let previous = self.views.get(&thread_id).cloned().flatten();
        if ty == "thread.deleted" {
            self.views.remove(&thread_id);
        } else {
            self.views.insert(thread_id.clone(), view.clone());
        }
        if view.is_none() {
            self.active_at.remove(&thread_id);
        }
        let updated = thread.updated_at.as_str().to_owned();
        if !known || view != previous {
            if view.is_some() {
                self.active_at.insert(thread_id, updated);
            }
            return Some(LinkedChange::Changed);
        }
        self.active(&thread_id, &updated).then_some(LinkedChange::Reordered)
    }
}

/// Starts the detector once per process, on the first pull request RPC.
fn ensure_linked_detector(state: &Arc<AppState>) {
    static STARTED: OnceLock<()> = OnceLock::new();
    if STARTED.set(()).is_err() {
        return;
    }
    let mut events = state.store.subscribe();
    let (reorder_tx, mut reorder_rx) = tokio::sync::mpsc::channel::<()>(1);
    tokio::spawn(async move {
        // Reorders refresh readers at most once per 10s.
        while reorder_rx.recv().await.is_some() {
            notify_linked();
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
        }
    });
    tokio::spawn(async move {
        let mut detector = LinkedDetector::default();
        loop {
            match events.recv().await {
                Ok(e) => {
                    let Some(event) = &e.thread_event else { continue };
                    // Only a thread's own record is looked into: the text of a message is never copied for this.
                    let thread = match &event.body {
                        EventBody::Thread { thread, .. } => Some(&**thread),
                        _ => None,
                    };
                    match detector.observe(event.thread_id.as_str(), event.event_type(), event.occurred_at.as_str(), thread) {
                        Some(LinkedChange::Changed) => notify_linked(),
                        Some(LinkedChange::Reordered) => {
                            let _ = reorder_tx.try_send(());
                        }
                        None => {}
                    }
                }
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => notify_linked(),
                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
            }
        }
    });
}

/// t3code's `pullRequestSyncKey` (lowercased `host`, `repository`, `number`).
fn sync_key(state: &AppState, r: &PrRef) -> Option<(String, String, u64)> {
    let identity = if r.host.is_some() && r.repository.contains('/') {
        None
    } else {
        state.store.get_project(&r.project_id).ok().flatten().and_then(|p| p.repository_identity)
    };
    let host = r.host.clone().or_else(|| identity.as_ref().and_then(identity_route).map(|(h, _, _)| h))?;
    Some((host.to_lowercase(), r.repository.trim().to_lowercase(), r.number))
}

fn linked_threads(state: &AppState, reference: Reference) -> Result<LinkedThreads, Failure> {
    let r = reference.resolve()?;
    let Some((host, repository, number)) = sync_key(state, &r) else { return Ok(LinkedThreads { threads: vec![] }) };
    let fail = |_| Failure::operation("linkedThreads", "Could not load linked threads.");
    let mut rows: Vec<AppThread> = state
        .store
        .app_threads()
        .map_err(fail)?
        .into_iter()
        .filter(|t| t.deleted_at.is_none() && t.lineage.side_chat != Some(true))
        .filter(|t| {
            t.pull_requests.iter().flatten().any(|link| {
                link.source != PullRequestLinkSource::StackDismissed
                    && link.number == number
                    && link.host.eq_ignore_ascii_case(&host)
                    && link.repository.eq_ignore_ascii_case(&repository)
            })
        })
        .collect();
    rows.sort_by(|a, b| b.updated_at.as_str().cmp(a.updated_at.as_str()).then_with(|| a.id.as_str().cmp(b.id.as_str())));
    let threads = rows
        .into_iter()
        .map(|t| LinkedThread {
            settled: t.settled_at.as_ref().and_then(Option::as_ref).is_some() || t.settled_override == Some(Some(SettledOverride::Settled)),
            id: t.id,
            project_id: t.project_id,
            title: t.title,
            archived_at: t.archived_at,
        })
        .collect();
    Ok(LinkedThreads { threads })
}

// ---------------------------------------------------------------------------
// list / listStats / filterOptions / routing
// ---------------------------------------------------------------------------

/// `PullRequestListInput`. Unlike the other payloads of the group this one
/// is read strictly: a field of the wrong type refuses the call.
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct ListInput {
    state: String,
    #[serde(default)]
    involvement: Option<String>,
    #[serde(default)]
    filters: Fields<ListFilters>,
    #[serde(default)]
    project_id: Option<String>,
    #[serde(default)]
    project_ids: Option<Vec<String>>,
    #[serde(default)]
    host: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
    /// Where to carry on from: the map's values are never read, as none can be one this server handed out.
    #[serde(default)]
    cursors: Option<BTreeMap<String, IgnoredAny>>,
    #[serde(default)]
    query: Option<String>,
}

/// `PullRequestListFilters`; anything in it that is not what the schema says filters nothing.
#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct ListFilters {
    draft: Loose<String>,
    review: Loose<String>,
    checks: Loose<String>,
    /// Every group must match; any name within a group suffices.
    labels: Loose<Vec<Loose<Vec<Loose<String>>>>>,
    excluded_labels: Loose<Vec<Loose<String>>>,
    author: Loose<String>,
}

/// What of a listing's filters GitLab narrows itself (`listableLabels`,
/// `filterParams`): groups of one label, excluded labels, author, draft and review.
fn narrowing(f: &ListFilters) -> gitlab::Narrowing<'_> {
    let one_label_groups = f.labels.iter().flatten().filter_map(|g| g.as_ref().filter(|g| g.len() == 1)).filter_map(|g| g[0].as_deref());
    gitlab::Narrowing {
        labels: one_label_groups.collect(),
        excluded_labels: f.excluded_labels.iter().flatten().filter_map(|l| l.as_deref()).collect(),
        author: f.author.as_deref(),
        draft: f.draft.as_deref().filter(|d| matches!(*d, "only" | "hide")),
        review: f.review.as_deref(),
    }
}

/// t3code's `matchesRowFilters`.
fn matches_filters(row: &ListRow, f: &ListFilters, viewer: &str) -> bool {
    let e = &row.entry;
    let names: HashSet<String> = e.labels.iter().map(|l| l.name.trim().to_lowercase()).collect();
    let holds = |l: &Loose<String>| l.as_deref().is_some_and(|l| names.contains(&l.trim().to_lowercase()));
    let draft_ok = f.draft.as_deref().is_none_or(|d| e.is_draft == (d == "only"));
    let review_ok = match f.review.as_deref() {
        Some("none") => e.review_decision.is_none(),
        Some(r) => e.review_decision.as_ref().is_some_and(|decision| decision.as_str() == r),
        None => true,
    };
    let checks_ok = f.checks.as_deref().is_none_or(|c| e.checks_state.as_ref().is_some_and(|state| state.as_str() == c));
    let labels_ok = f.labels.as_ref().is_none_or(|groups| groups.iter().all(|g| g.as_ref().is_some_and(|g| g.iter().any(holds))));
    let excluded_ok = f.excluded_labels.as_ref().is_none_or(|ls| !ls.iter().any(holds));
    let author_ok = f.author.as_deref().is_none_or(|a| {
        let wanted = if a == "@me" { viewer } else { a };
        row.author_login.as_deref().is_some_and(|l| l.eq_ignore_ascii_case(wanted))
    });
    draft_ok && review_ok && checks_ok && labels_ok && excluded_ok && author_ok
}

async fn list(state: &AppState, input: ListInput) -> Result<PullRequestListResult, Failure> {
    if input.cursors.as_ref().is_some_and(|c| !c.is_empty()) {
        // Domovoi never hands out continuation cursors, so any cursor is one it cannot carry on from.
        return Err(Failure::operation("list", "The list could not be carried on from where it left off."));
    }
    let ws = workspace_projects(state, input.project_id.as_deref(), input.project_ids.as_deref(), input.host.as_deref()).await?;
    let mut hosts: Vec<String> = vec![];
    for p in &ws.supported {
        if !hosts.contains(&p.host) {
            hosts.push(p.host.clone());
        }
    }
    let mut viewers: BTreeMap<String, String> = BTreeMap::new();
    let mut providers: Vec<PullRequestProviderSummary> = vec![];
    let mut errors: Vec<(String, Provider, GhError)> = vec![];
    for host in &hosts {
        let first = ws.supported.iter().find(|p| &p.host == host).expect("hosts come from the supported providers");
        let count = ws.supported.iter().filter(|p| &p.host == host).count();
        let identity = first.identity().await;
        let detail = match &identity {
            Ok((_, login)) => {
                viewers.insert(host.clone(), login.clone());
                None
            }
            Err(e) => {
                errors.push((host.clone(), first.provider, e.clone()));
                Some(cli_detail(first.provider, e))
            }
        };
        let actions = match first.provider {
            Provider::GitHub => github::all_actions(),
            Provider::GitLab => gitlab::all_actions(),
        };
        providers.push(PullRequestProviderSummary {
            host: host.clone(),
            kind: first.provider.kind().to_owned(),
            searches_on_host: true,
            actions: Some(actions.into()),
            project_count: count as u64,
            project_ids: None,
            configured: identity.is_ok(),
            detail,
        });
    }
    for (host, kind, count) in &ws.unimplemented {
        providers.push(PullRequestProviderSummary {
            host: host.clone(),
            kind: kind.clone(),
            searches_on_host: false,
            actions: Some(vec![].into()),
            project_count: *count,
            project_ids: None,
            configured: false,
            detail: Some("This host cannot be browsed here yet.".to_owned()),
        });
    }
    let project_error = |p: &Supported, message: String| PullRequestListProjectError { project_id: p.project_id.as_str().into(), project_title: p.title.clone(), message };
    let readable: Vec<&Supported> = ws.supported.iter().filter(|p| viewers.contains_key(&p.host)).collect();
    let mut list_errors: Vec<PullRequestListProjectError> =
        ws.supported.iter().filter(|p| !viewers.contains_key(&p.host)).map(|p| project_error(p, format!("{} could not be read.", p.repository))).collect();
    if readable.is_empty() {
        let list_result = PullRequestListResult { viewers, providers, entries: vec![], errors: list_errors, truncated: false, next_cursors: BTreeMap::new() };
        let blocking = errors.iter().find(|(_, _, e)| e.is_unusable()).or(errors.first());
        return match blocking {
            Some((_, provider, e)) => Err(Failure::from_cli("list", *provider, e).with_list(list_result)),
            None => Ok(list_result),
        };
    }
    let limit = input.limit.unwrap_or(DEFAULT_LIST_LIMIT).max(1);
    let observed = now_ms();
    let listing = Listing { state: &input.state, involvement: input.involvement.as_deref(), query: input.query.as_deref(), fetch: limit + 1 };
    let narrowing = narrowing(&input.filters);
    let reads = futures::future::join_all(readable.iter().map(|p| {
        let (listing, narrowing, viewer) = (&listing, &narrowing, viewers.get(&p.host).map(String::as_str).unwrap_or(""));
        async move {
            match p.provider {
                Provider::GitHub => github::list(p, listing, observed).await,
                Provider::GitLab => gitlab::list(p, listing, viewer, narrowing, observed).await,
            }
        }
    }))
    .await;
    let mut entries = vec![];
    let mut truncated = false;
    for (p, read) in readable.iter().zip(reads) {
        let viewer = viewers.get(&p.host).map(String::as_str).unwrap_or("");
        match read {
            Ok(mut rows) => {
                if rows.len() > limit {
                    truncated = true;
                    rows.truncate(limit);
                }
                for row in rows.into_iter().filter(|row| matches_filters(row, &input.filters, viewer)) {
                    let author_is_viewer = row.author_login.as_deref().is_some_and(|l| l.eq_ignore_ascii_case(viewer));
                    let mut entry = row.entry;
                    entry.viewer_review_requested = !author_is_viewer && row.review_request_logins.iter().any(|l| l.eq_ignore_ascii_case(viewer));
                    entries.push(entry);
                }
            }
            Err(e) => list_errors.push(project_error(p, cli_detail(p.provider, &e))),
        }
    }
    entries.sort_by(|a, b| b.updated_at.as_str().cmp(a.updated_at.as_str()));
    Ok(PullRequestListResult { viewers, providers, entries, errors: list_errors, truncated, next_cursors: BTreeMap::new() })
}

/// `PullRequestListStatsInput`: references that cannot all be read are none.
#[derive(Deserialize, Default)]
#[serde(default)]
struct ListStatsInput {
    refs: Loose<Vec<PrRef>>,
}

async fn list_stats(state: &AppState, input: ListStatsInput) -> Result<ListStats, Failure> {
    let refs = input.refs.0.unwrap_or_default();
    if refs.is_empty() {
        return Ok(ListStats { stats: vec![] });
    }
    let ws = workspace_projects(state, None, None, None).await?;
    let mut wanted: Vec<(Supported, u64)> = vec![];
    for r in &refs {
        if let Some(p) = ws.supported.iter().find(|p| p.project_id == r.project_id && p.repository.eq_ignore_ascii_case(r.repository.trim())) {
            if !wanted.iter().any(|(w, n)| w.project_id == p.project_id && *n == r.number) {
                wanted.push((p.clone(), r.number));
            }
        }
    }
    let reads = futures::future::join_all(wanted.iter().map(|(p, n)| async move {
        match p.provider {
            Provider::GitHub => github::line_counts(p, *n).await,
            Provider::GitLab => gitlab::line_counts(p, *n).await,
        }
    }))
    .await;
    let stats = wanted
        .iter()
        .zip(reads)
        .filter_map(|((p, n), read)| {
            let (additions, deletions) = read.ok()?;
            Some(DiffStat { project_id: p.project_id.as_str().into(), repository: p.repository.clone(), number: *n, additions, deletions })
        })
        .collect();
    Ok(ListStats { stats })
}

const FILTER_OPTIONS_LIMIT: usize = 100;

/// t3code's `rankFilterOptions`.
fn rank_filter_options<T>(items: Vec<T>, query: Option<&str>, key_of: fn(&T) -> String, texts_of: fn(&T) -> Vec<String>, host_order: bool, pages_truncated: bool) -> (Vec<T>, bool) {
    let query = query.unwrap_or("").trim().to_lowercase();
    let mut unique: Vec<T> = vec![];
    let mut seen = HashSet::new();
    for item in items {
        if seen.insert(key_of(&item).to_lowercase()) {
            unique.push(item);
        }
    }
    let rank = |item: &T| -> Option<u8> {
        if query.is_empty() {
            return Some(0);
        }
        let texts: Vec<String> = texts_of(item).into_iter().map(|t| t.to_lowercase()).collect();
        if texts.contains(&query) {
            Some(0)
        } else if texts.iter().any(|t| t.starts_with(&query)) {
            Some(1)
        } else if texts.iter().any(|t| t.contains(&query)) {
            Some(2)
        } else {
            None
        }
    };
    let mut ranked: Vec<(u8, String, T)> = unique.into_iter().filter_map(|i| Some((rank(&i)?, key_of(&i), i))).collect();
    ranked.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| if host_order { std::cmp::Ordering::Equal } else { a.1.to_lowercase().cmp(&b.1.to_lowercase()) }));
    let truncated = ranked.len() > FILTER_OPTIONS_LIMIT || pages_truncated;
    (ranked.into_iter().take(FILTER_OPTIONS_LIMIT).map(|(_, _, i)| i).collect(), truncated)
}

/// `PullRequestFilterOptionsInput`.
#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct FilterOptionsInput {
    /// `labels` or `references`; anything else asks for authors.
    kind: Loose<String>,
    project_id: Loose<String>,
    project_ids: Loose<Vec<String>>,
    host: Loose<String>,
    query: Loose<String>,
}

/// What a filter or a composer can offer, from every repository in scope; one that cannot be read offers nothing.
async fn filter_options(state: &AppState, input: FilterOptionsInput) -> Result<FilterOptions, Failure> {
    let ws = workspace_projects(state, input.project_id.as_deref(), input.project_ids.as_deref(), input.host.as_deref()).await?;
    let query = input.query.as_deref();
    let q = query.unwrap_or("").trim();
    let mut truncated = false;
    match input.kind.as_deref() {
        Some("labels") => {
            let mut items = vec![];
            for p in &ws.supported {
                let page = match p.provider {
                    Provider::GitHub => github::repository_labels(p, "100", q).await.map(|page| (page.len() >= 100, page)),
                    Provider::GitLab => gitlab::repository_labels(p, q).await.map(|(page, more)| (more, page)),
                };
                if let Ok((more, page)) = page {
                    truncated |= more;
                    items.extend(page);
                }
            }
            let (labels, truncated) = rank_filter_options(items, query, |l| l.name.clone(), |l| vec![l.name.clone()], false, truncated);
            Ok(FilterOptions { labels, truncated, ..Default::default() })
        }
        Some("references") => {
            let mut items = vec![];
            for p in &ws.supported {
                for (command, kind) in [("issue", ReferenceKind::Issue), ("pr", ReferenceKind::ChangeRequest)] {
                    let found = match p.provider {
                        Provider::GitHub => github::references(p, command, kind, q).await,
                        Provider::GitLab => gitlab::references(p, kind, q).await,
                    };
                    if let Ok(found) = found {
                        items.extend(found);
                    }
                }
            }
            let (references, truncated) = rank_filter_options(items, query, |r| r.reference.clone(), |r| vec![r.number.to_string(), r.title.clone()], true, false);
            Ok(FilterOptions { references, truncated, ..Default::default() })
        }
        _ => {
            let mut items = vec![];
            for p in &ws.supported {
                let users = match p.provider {
                    Provider::GitHub => github::assignable_users(p, q).await,
                    Provider::GitLab => gitlab::assignable_users(p, q).await,
                };
                if let Ok((users, more)) = users {
                    truncated |= more;
                    items.extend(users);
                }
            }
            let (authors, truncated) =
                rank_filter_options(items, query, |a| a.login.clone(), |a| [Some(a.login.clone()), a.name.clone()].into_iter().flatten().collect(), false, truncated);
            Ok(FilterOptions { authors, truncated, ..Default::default() })
        }
    }
}

/// `PullRequestRoutingIdentityInput`.
#[derive(Deserialize, Default)]
#[serde(default)]
struct HostInput {
    host: Loose<String>,
}

async fn routing_identity(state: &AppState, input: HostInput) -> Result<RoutingIdentity, Failure> {
    let host = input.host.as_deref().unwrap_or("").to_lowercase();
    let ws = workspace_projects(state, None, None, Some(&host)).await?;
    let Some(p) = ws.supported.first() else { return Err(Failure::unsupported()) };
    let (account_id, viewer) = p.identity().await.map_err(failed(p, "routeIdentity"))?;
    Ok(RoutingIdentity { account_id, viewer, host, provider: p.provider.kind() })
}

async fn routing(state: &AppState, reference: Reference) -> Result<Routing, Failure> {
    let r = reference.resolve()?;
    let p = require_project(state, &r).await?;
    let (account_id, viewer) = p.identity().await.map_err(failed(&p, "routeIdentity"))?;
    if account_id.trim().is_empty() || viewer.trim().is_empty() {
        return Err(Failure::operation("routeIdentity", "The signed-in account could not be verified."));
    }
    Ok(Routing { host: p.host, provider: p.provider.kind(), account_id, viewer, project_title: p.title, workspace_root: p.workspace_root })
}

// ---------------------------------------------------------------------------
// Reads of one pull request
// ---------------------------------------------------------------------------

/// A failed `gh` or `glab` of `p`'s host under `operation`.
fn failed(p: &Supported, operation: &'static str) -> impl Fn(GhError) -> Failure + use<> {
    let provider = p.provider;
    move |e| Failure::from_cli(operation, provider, &e)
}

async fn summary(p: Supported, r: PrRef, _: NoFields) -> Result<PullRequestSummary, Failure> {
    read_summary(&p, r.number, false).await.map_err(failed(&p, "summary"))
}

/// Stacks are not read: no pull request is said to be in one.
async fn stack(_: Supported, _: PrRef, _: NoFields) -> Result<Option<Never>, Failure> {
    Ok(None)
}

async fn detail(p: Supported, r: PrRef, _: NoFields) -> Result<PullRequestDetail, Failure> {
    match p.provider {
        Provider::GitHub => github::detail(&p, r.number).await,
        Provider::GitLab => gitlab::detail(&p, r.number).await,
    }
    .map_err(failed(&p, "detail"))
}

async fn preview(p: Supported, r: PrRef, _: NoFields) -> Result<PullRequestPreview, Failure> {
    match p.provider {
        Provider::GitHub => github::preview(&p, r.number).await,
        Provider::GitLab => gitlab::preview(&p, r.number).await,
    }
    .map_err(failed(&p, "preview"))
}

async fn checks(p: Supported, r: PrRef, _: NoFields) -> Result<PullRequestChecks, Failure> {
    read_checks(&p, r.number).await.map_err(failed(&p, "checks"))
}

async fn activity(p: Supported, r: PrRef, _: NoFields) -> Result<PullRequestActivity, Failure> {
    let found = match p.provider {
        Provider::GitHub => github::activity(&p, r.number).await,
        Provider::GitLab => gitlab::activity(&p, r.number).await,
    }
    .map_err(failed(&p, "activity"))?;
    found.ok_or_else(|| Failure::not_found("activity", "The pull request was not found."))
}

/// `PullRequestThreadCommentsInput`, beside the reference.
#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct ThreadCommentsInput {
    thread_id: Loose<Scalar>,
    cursor: Loose<Scalar>,
}

async fn thread_comments(p: Supported, _: PrRef, input: ThreadCommentsInput) -> Result<ThreadCommentsPage, Failure> {
    if p.provider == Provider::GitLab {
        return Err(Failure::operation("threadComments", "This host does not page review thread comments."));
    }
    let page = github::thread_comments(&p, var(&input.thread_id), var(&input.cursor)).await.map_err(failed(&p, "threadComments"))?;
    page.ok_or_else(|| Failure::operation("threadComments", "The review conversation was not found."))
}

/// `PullRequestDiffFileContentsInput`, beside the reference.
#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct DiffFileInput {
    commit: Loose<String>,
    change_type: Loose<String>,
    old_path: Loose<String>,
    new_path: Loose<String>,
}

async fn diff_file_contents(p: Supported, r: PrRef, input: DiffFileInput) -> Result<DiffFileContents, Failure> {
    let file = FileOfDiff {
        commit: input.commit.as_deref(),
        change: input.change_type.as_deref().unwrap_or("change"),
        old_path: input.old_path.as_deref().unwrap_or(""),
        new_path: input.new_path.as_deref().unwrap_or(""),
    };
    match p.provider {
        Provider::GitHub => github::diff_file_contents(&p, r.number, &file).await,
        Provider::GitLab => gitlab::diff_file_contents(&p, r.number, &file).await,
    }
    .map_err(failed(&p, "diffFileContents"))
}

async fn files_viewed(p: Supported, r: PrRef, _: NoFields) -> Result<FilesViewed, Failure> {
    match p.provider {
        Provider::GitHub => github::files_viewed(&p, r.number).await.map_err(failed(&p, "filesViewed")),
        // GitLab keeps no viewed marks, and this server keeps none of its own.
        Provider::GitLab => Ok(FilesViewed { files: vec![], truncated: false }),
    }
}

async fn change_breakdown(p: Supported, r: PrRef, _: NoFields) -> Result<Option<ChangeBreakdown>, Failure> {
    match p.provider {
        Provider::GitHub => github::change_breakdown(&p, r.number).await,
        Provider::GitLab => gitlab::change_breakdown(&p, r.number).await,
    }
    .map_err(failed(&p, "changeBreakdown"))
}

async fn head_sha(p: &Supported, number: u64) -> Result<Option<String>, GhError> {
    match p.provider {
        Provider::GitHub => github::head_sha(p, number).await,
        Provider::GitLab => gitlab::head_sha(p, number).await,
    }
}

/// t3code's `reviewGuides.get`: the guide's key first, then the head commit.
async fn review_guide(state: &AppState, r: &PrRef) -> Result<ReviewGuideResult, Failure> {
    verify_account(state, r).await?;
    if state.store.get_project(&r.project_id).ok().flatten().is_none() || sync_key(state, r).is_none() {
        return Err(Failure::operation("reviewGuide", "Could not identify this pull request's host."));
    }
    let p = require_project(state, r).await?;
    let head = head_sha(&p, r.number).await.map_err(failed(&p, "detail"))?;
    let key = super::review_guides::key(&p.host, &p.repository, r.number);
    Ok(super::review_guides::result(&state.paths.userdata, &key, head.as_deref()))
}

/// `generateReviewGuide`: starts writing a guide for the current head (the
/// result says it runs) unless the guide already covers that head.
async fn generate_review_guide(state: &AppState, reference: Reference) -> Result<ReviewGuideResult, Failure> {
    let settings = GuideSettings::of(&*state.settings.read().await);
    if !settings.enabled {
        return Err(Failure::operation("generateReviewGuide", "Review guides are off. Turn them on in Settings → Source Control."));
    }
    let r = reference.resolve()?;
    review_guide(state, &r).await?;
    let p = require_project(state, &r).await?;
    let source = match p.provider {
        Provider::GitHub => github::guide_source(&p, r.number).await,
        Provider::GitLab => gitlab::guide_source(&p, r.number).await,
    }
    .map_err(failed(&p, "detail"))?;
    let head = source.head.ok_or_else(|| Failure::operation("generateReviewGuide", "The pull request has no head commit."))?;
    let key = super::review_guides::key(&p.host, &p.repository, r.number);
    let userdata = state.paths.userdata.clone();
    if super::review_guides::stored(&userdata, &key).is_some_and(|g| g.head_sha == head) {
        return Ok(super::review_guides::result(&userdata, &key, Some(&head)));
    }
    let base_branch = source.base_branch;
    // Fetching the head first lets the merge base be found locally.
    let mut fetch = crate::process::git(&p.workspace_root);
    let head_ref = match p.provider {
        Provider::GitHub => format!("pull/{}/head", r.number),
        Provider::GitLab => format!("merge-requests/{}/head", r.number),
    };
    fetch.args(["fetch", "--quiet", "origin", &head_ref]);
    let _ = crate::process::output(fetch, crate::process::SLOW).await;
    let base_commit = super::review_guides::base_commit(&p.workspace_root, &base_branch, &head).await
        .ok_or_else(|| Failure::operation("generateReviewGuide", format!("Could not find where this change branches from {base_branch}.")))?;
    let change = super::review_guides::Change { number: r.number, title: source.title, description: source.body, base_branch, base_commit };
    super::review_guides::start(userdata.clone(), key.clone(), p.workspace_root.clone(), change, head.clone(), settings);
    Ok(super::review_guides::result(&userdata, &key, Some(&head)))
}

// ---------------------------------------------------------------------------
// Writes
// ---------------------------------------------------------------------------

const ACTION_ACCESS_REFUSALS: [(&str, &str); 11] = [
    ("merge", "You need write access on this repository to merge."),
    ("ready", "You need write access on this repository, or to have opened this change request, to mark it ready for review."),
    ("draft", "You need write access on this repository, or to have opened this change request, to return it to a draft."),
    ("close", "You need write access on this repository, or to have opened this change request, to close it."),
    ("update-branch", "You need write access on this repository, or to have opened this change request, to update its branch."),
    ("reopen", "You need write access on this repository, or to have opened this change request, to reopen it."),
    ("enable-auto-merge", "Auto-merge needs write access on this repository, and the repository has to allow it."),
    ("disable-auto-merge", "You need write access on this repository to stop it being merged for you once it is ready."),
    ("revert", "You need write access on this repository to open a revert pull request."),
    ("delete-branch", "You need write access on the source repository to delete its branch."),
    ("approve-workflows", "You need write access on this repository to approve workflows from a fork pull request."),
];

fn refusal(action: &str) -> &'static str {
    ACTION_ACCESS_REFUSALS.iter().find(|(a, _)| *a == action).map(|(_, r)| *r).unwrap_or("You do not have access to do that.")
}

/// `PullRequestActionInput`, beside the reference.
#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct ActionInput {
    action: Loose<String>,
    merge_method: Loose<String>,
    update_method: Loose<String>,
    /// Given (whatever it is) when the action is for a whole stack.
    stack_number: Option<IgnoredAny>,
    expected_stack_heads: Option<IgnoredAny>,
}

/// Validates `pullRequests.runAction` against the host's capabilities (before
/// any read): the action to run, when it is one the host has.
fn check_action(provider: Provider, input: &ActionInput) -> Result<PullRequestAction, Failure> {
    let named = input.action.as_deref().unwrap_or("");
    if input.stack_number.is_some() {
        let ok = ["merge", "update-branch"].contains(&named)
            && input.expected_stack_heads.is_some()
            && !(named == "update-branch" && input.update_method.as_deref() != Some("rebase"));
        if !ok {
            return Err(Failure::operation("runAction", "This stack action is not supported or has no expected head revision."));
        }
        return Err(Failure::operation("runAction", "domovoi-server cannot run stack actions yet."));
    }
    let action = PullRequestAction::from(named);
    let offered = match provider {
        Provider::GitHub => !matches!(action, PullRequestAction::Other(_)),
        Provider::GitLab => gitlab::all_actions().contains(&action),
    };
    if !offered {
        return Err(Failure::operation("runAction", format!("This host cannot {named} a change request.")));
    }
    if let Some(m) = input.merge_method.as_deref() {
        if !["merge", "squash", "rebase"].contains(&m) {
            return Err(Failure::operation("runAction", format!("This host cannot merge with the {m} strategy.")));
        }
    }
    if let Some(m) = input.update_method.as_deref() {
        let methods: &[&str] = match provider {
            Provider::GitHub => &["merge", "rebase"],
            Provider::GitLab => &["rebase"],
        };
        if !methods.contains(&m) {
            return Err(Failure::operation("runAction", format!("This host cannot update a branch by {m}.")));
        }
    }
    Ok(action)
}

/// What the viewer may do to one pull request, as the host says now.
async fn viewer_permissions(p: &Supported, number: u64) -> Result<super::pull_request_records::ViewerPermissions, GhError> {
    match p.provider {
        Provider::GitHub => github::viewer_permissions(p, number).await,
        Provider::GitLab => gitlab::viewer_permissions(p, number).await,
    }
}

async fn run_gh(p: &Supported, args: &[&str]) -> Result<(), GhError> {
    gh::run(p.cwd(), &p.host, args, None).await.map(|_| ())
}

/// A GraphQL mutation, whose answer says nothing a caller reads.
async fn mutate(p: &Supported, query: &str, vars: &[(&str, Var<'_>)]) -> Result<(), GhError> {
    gh::graphql::<IgnoredAny>(p.cwd(), &p.host, query, vars).await.map(|_| ())
}

async fn run_action(p: Supported, r: PrRef, input: ActionInput) -> Result<PullRequestSummary, Failure> {
    let p = &p;
    let number = r.number;
    let action = check_action(p.provider, &input)?;
    let fail = failed(p, "runAction");
    let viewer = viewer_permissions(p, number).await.map_err(&fail)?;
    if !viewer.actions.contains(&action) {
        return Err(Failure::operation("runAction", refusal(action.as_str())));
    }
    if let Some(m) = input.update_method.as_deref() {
        if !viewer.update_methods.is_some_and(|offered| offered.iter().any(|method| method.as_str() == m)) {
            return Err(Failure::operation("runAction", refusal("update-branch")));
        }
    }
    if p.provider == Provider::GitLab {
        gitlab::run_action(p, number, &action, input.merge_method.as_deref()).await.map_err(&fail)?;
        let summary = gitlab::summary(p, number).await.map_err(&fail)?;
        notify_refresh();
        return Ok(summary);
    }
    let n = number.to_string();
    let repo = p.repository.as_str();
    let method_flag = match input.merge_method.as_deref() {
        Some("squash") => "--squash",
        Some("rebase") => "--rebase",
        _ => "--merge",
    };
    match action {
        PullRequestAction::Merge => run_gh(p, &["pr", "merge", &n, "--repo", repo, method_flag]).await.map_err(&fail)?,
        PullRequestAction::EnableAutoMerge => run_gh(p, &["pr", "merge", &n, "--repo", repo, "--auto", method_flag]).await.map_err(&fail)?,
        PullRequestAction::DisableAutoMerge => run_gh(p, &["pr", "merge", &n, "--repo", repo, "--disable-auto"]).await.map_err(&fail)?,
        PullRequestAction::Ready => run_gh(p, &["pr", "ready", &n, "--repo", repo]).await.map_err(&fail)?,
        PullRequestAction::Draft => run_gh(p, &["pr", "ready", &n, "--repo", repo, "--undo"]).await.map_err(&fail)?,
        PullRequestAction::Close => run_gh(p, &["pr", "close", &n, "--repo", repo]).await.map_err(&fail)?,
        PullRequestAction::Reopen => run_gh(p, &["pr", "reopen", &n, "--repo", repo]).await.map_err(&fail)?,
        PullRequestAction::Revert => run_gh(p, &["pr", "revert", &n, "--repo", repo]).await.map_err(&fail)?,
        PullRequestAction::UpdateBranch => {
            let mut args = vec!["pr", "update-branch", &n, "--repo", repo];
            if input.update_method.as_deref() == Some("rebase") {
                args.push("--rebase");
            }
            run_gh(p, &args).await.map_err(&fail)?
        }
        PullRequestAction::DeleteBranch => {
            let (head_repo, branch) = github::head_branch(p, number).await.map_err(&fail)?;
            let endpoint = format!("repos/{head_repo}/git/refs/heads/{branch}");
            run_gh(p, &["api", "-X", "DELETE", &endpoint]).await.map_err(&fail)?
        }
        PullRequestAction::ApproveWorkflows => {
            let sha = github::head_sha(p, number).await.map_err(&fail)?.unwrap_or_default();
            for id in github::runs_awaiting_approval(p, &sha).await.map_err(&fail)? {
                let approve = format!("repos/{repo}/actions/runs/{id}/approve");
                run_gh(p, &["api", "-X", "POST", &approve]).await.map_err(&fail)?;
            }
        }
        PullRequestAction::Other(_) => unreachable!("checked above"),
    }
    let summary = github::summary(p, number, true).await.map_err(&fail)?;
    notify_refresh();
    Ok(summary)
}

/// `PullRequestUpdateInput`, beside the reference.
#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct UpdateInput {
    title: Loose<String>,
    body: Loose<String>,
    maintainer_can_modify: Loose<bool>,
}

async fn update(p: Supported, r: PrRef, input: UpdateInput) -> Result<(), Failure> {
    let (title, body, maintainer) = (input.title.as_deref(), input.body.as_deref(), input.maintainer_can_modify.0);
    if title.is_none() && body.is_none() && maintainer.is_none() {
        return Err(Failure::operation("update", "Nothing was changed."));
    }
    let n = r.number.to_string();
    let fail = failed(&p, "update");
    if p.provider == Provider::GitLab {
        return gitlab::update(&p, r.number, title, body, maintainer).await.map_err(&fail);
    }
    if title.is_some() || body.is_some() {
        let mut args = vec!["pr", "edit", n.as_str(), "--repo", p.repository.as_str()];
        if let Some(t) = title {
            args.extend(["--title", t]);
        }
        if body.is_some() {
            args.extend(["--body-file", "-"]);
        }
        gh::run(p.cwd(), &p.host, &args, body).await.map_err(&fail)?;
    }
    if let Some(m) = maintainer {
        let endpoint = format!("repos/{}/pulls/{n}", p.repository);
        let field = format!("maintainer_can_modify={m}");
        run_gh(&p, &["api", "-X", "PATCH", &endpoint, "-F", &field]).await.map_err(&fail)?;
    }
    Ok(())
}

/// `PullRequestCommentInput`, beside the reference.
#[derive(Deserialize, Default)]
#[serde(default)]
struct CommentInput {
    body: Loose<String>,
}

async fn comment(p: Supported, r: PrRef, input: CommentInput) -> Result<(), Failure> {
    let body = input.body.as_deref().unwrap_or("");
    if p.provider == Provider::GitLab {
        return gitlab::comment(&p, r.number, body).await.map_err(failed(&p, "comment"));
    }
    gh::run(p.cwd(), &p.host, &["pr", "comment", &r.number.to_string(), "--repo", &p.repository, "--body-file", "-"], Some(body))
        .await
        .map(|_| ())
        .map_err(failed(&p, "comment"))
}

/// `PullRequestCommentUpdateInput`, beside the reference.
#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct CommentUpdateInput {
    comment_id: Loose<Scalar>,
    /// `review-comment` for a comment on a line; anything else is edited as a comment on the pull request.
    kind: Loose<String>,
    body: Loose<String>,
}

/// An id a payload handed on as a string or a number, as text (GitLab's note and discussion ids).
fn id_text(given: &Loose<Scalar>) -> Option<String> {
    match &given.0 {
        Some(Scalar::Text(text)) => Some(text.clone()),
        Some(Scalar::Number(number)) => Some(number.to_string()),
        _ => None,
    }
}

async fn update_comment(p: Supported, r: PrRef, input: CommentUpdateInput) -> Result<(), Failure> {
    if p.provider == Provider::GitLab {
        let id = id_text(&input.comment_id).ok_or_else(|| Failure::operation("updateComment", "The comment was not named."))?;
        return gitlab::update_comment(&p, r.number, &id, input.body.as_deref().unwrap_or("")).await.map_err(failed(&p, "updateComment"));
    }
    let query = if input.kind.as_deref() == Some("review-comment") {
        "mutation($id: ID!, $body: String!) { updatePullRequestReviewComment(input: { pullRequestReviewCommentId: $id, body: $body }) { clientMutationId } }"
    } else {
        "mutation($id: ID!, $body: String!) { updateIssueComment(input: { id: $id, body: $body }) { clientMutationId } }"
    };
    // The body is text: an empty one was refused before.
    mutate(&p, query, &[("id", var(&input.comment_id)), ("body", Var::Text(input.body.as_deref().unwrap_or("")))]).await.map_err(failed(&p, "updateComment"))
}

/// `PullRequestThreadReplyInput`, beside the reference.
#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct ThreadReplyInput {
    thread_id: Loose<Scalar>,
    body: Loose<String>,
}

async fn reply_to_thread(p: Supported, r: PrRef, input: ThreadReplyInput) -> Result<(), Failure> {
    if p.provider == Provider::GitLab {
        let id = id_text(&input.thread_id).ok_or_else(|| Failure::operation("replyToThread", "The review conversation was not named."))?;
        return gitlab::reply_to_thread(&p, r.number, &id, input.body.as_deref().unwrap_or("")).await.map_err(failed(&p, "replyToThread"));
    }
    let q = "mutation($id: ID!, $body: String!) { addPullRequestReviewThreadReply(input: { pullRequestReviewThreadId: $id, body: $body }) { clientMutationId } }";
    // The body is text: an empty one was refused before.
    mutate(&p, q, &[("id", var(&input.thread_id)), ("body", Var::Text(input.body.as_deref().unwrap_or("")))]).await.map_err(failed(&p, "replyToThread"))
}

/// A place in a diff (`PullRequestReviewPosition`): an `added` line by its
/// new number, a `deleted` one by its old, a `context` line by the one of its `side`.
#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct ReviewPosition {
    pub kind: Loose<String>,
    pub new_line: Loose<u64>,
    pub old_line: Loose<u64>,
    pub side: Loose<String>,
}

/// A comment of a review being submitted (`PullRequestReviewCommentDraft`);
/// with a `startPosition`, on the lines from there to `position`.
#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
pub struct ReviewCommentDraft {
    /// Handed on to GitHub as it came.
    pub path: Loose<Scalar>,
    /// The name the file had before the change (GitLab resolves a place against both sides).
    pub old_path: Loose<String>,
    pub body: Loose<Scalar>,
    pub position: Fields<ReviewPosition>,
    pub start_position: Fields<ReviewPosition>,
}

/// `PullRequestSubmitReviewInput`, beside the reference.
#[derive(Deserialize, Default)]
#[serde(default)]
struct SubmitReviewInput {
    verdict: Loose<String>,
    body: Loose<String>,
    comments: Rows<ReviewCommentDraft>,
}

const VERDICT_LABELS: [(&str, &str); 3] = [("comment", "review"), ("approve", "approve"), ("request-changes", "request changes on")];

async fn submit_review(p: Supported, r: PrRef, input: SubmitReviewInput) -> Result<(), Failure> {
    let verdict = input.verdict.as_deref().unwrap_or("comment");
    let label = VERDICT_LABELS.iter().find(|(v, _)| *v == verdict).map(|(_, l)| *l).unwrap_or("review");
    let body = input.body.as_deref().unwrap_or("");
    let comments: Vec<ReviewCommentDraft> = input.comments.0.unwrap_or_default().into_iter().map(|c| c.0).collect();
    // GitLab asks for no summary (`reviewSummaryRequired`).
    if p.provider == Provider::GitHub && verdict != "approve" && body.trim().is_empty() && comments.is_empty() {
        return Err(Failure::operation("submitReview", "A review needs a summary or at least one comment."));
    }
    let fail = failed(&p, "submitReview");
    let viewer = viewer_permissions(&p, r.number).await.map_err(&fail)?;
    if !viewer.verdicts.iter().any(|allowed| allowed.as_str() == verdict) {
        return Err(Failure::operation("submitReview", format!("You need write access on this repository to {label} a change request.")));
    }
    match p.provider {
        Provider::GitHub => github::submit_review(&p, r.number, verdict, body, &comments).await,
        Provider::GitLab => gitlab::submit_review(&p, r.number, verdict, body, &comments).await,
    }
    .map_err(&fail)
}

/// `PullRequestThreadResolutionInput`, beside the reference.
#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct ThreadResolutionInput {
    thread_id: Loose<Scalar>,
    resolved: Loose<bool>,
}

async fn set_thread_resolution(p: Supported, r: PrRef, input: ThreadResolutionInput) -> Result<(), Failure> {
    let fail = failed(&p, "setThreadResolution");
    let viewer = viewer_permissions(&p, r.number).await.map_err(&fail)?;
    if !viewer.resolve {
        return Err(Failure::operation(
            "setThreadResolution",
            "You need write access on this repository, or to have opened this change request, to resolve a review conversation.",
        ));
    }
    if p.provider == Provider::GitLab {
        let id = id_text(&input.thread_id).ok_or_else(|| Failure::operation("setThreadResolution", "The review conversation was not named."))?;
        return gitlab::set_thread_resolution(&p, r.number, &id, input.resolved.0 == Some(true)).await.map_err(&fail);
    }
    let query = if input.resolved.0 == Some(true) {
        "mutation($id: ID!) { resolveReviewThread(input: { threadId: $id }) { clientMutationId } }"
    } else {
        "mutation($id: ID!) { unresolveReviewThread(input: { threadId: $id }) { clientMutationId } }"
    };
    mutate(&p, query, &[("id", var(&input.thread_id))]).await.map_err(&fail)
}

/// `PullRequestReactionInput`, beside the reference.
#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct ReactionInput {
    /// The comment reacted to; without one, the pull request itself.
    subject_id: Loose<String>,
    content: Loose<String>,
    reacted: Loose<bool>,
}

async fn set_reaction(p: Supported, r: PrRef, input: ReactionInput) -> Result<(), Failure> {
    let fail = failed(&p, "setReaction");
    if p.provider == Provider::GitLab {
        let award = gitlab::award_name(input.content.as_deref().unwrap_or("")).ok_or_else(|| Failure::operation("setReaction", "Unknown reaction."))?;
        return gitlab::set_reaction(&p, r.number, input.subject_id.as_deref(), award, input.reacted.0 == Some(true)).await.map_err(&fail);
    }
    let content = github::github_reaction(input.content.as_deref().unwrap_or("")).ok_or_else(|| Failure::operation("setReaction", "Unknown reaction."))?;
    let subject = match input.subject_id.0 {
        Some(s) => s,
        None => github::node_id(&p, r.number).await.map_err(&fail)?,
    };
    let query = if input.reacted.0 == Some(true) {
        "mutation($id: ID!, $content: ReactionContent!) { addReaction(input: { subjectId: $id, content: $content }) { clientMutationId } }"
    } else {
        "mutation($id: ID!, $content: ReactionContent!) { removeReaction(input: { subjectId: $id, content: $content }) { clientMutationId } }"
    };
    mutate(&p, query, &[("id", Var::Text(&subject)), ("content", Var::Text(content))]).await.map_err(&fail)
}

/// `PullRequestReviewerCandidatesInput`, beside the reference.
#[derive(Deserialize, Default)]
#[serde(default)]
struct ReviewerCandidatesInput {
    query: Loose<String>,
}

async fn reviewer_candidates(p: Supported, r: PrRef, input: ReviewerCandidatesInput) -> Result<ReviewerCandidates, Failure> {
    let fail = failed(&p, "reviewerCandidates");
    if p.provider == Provider::GitLab {
        return gitlab::reviewer_candidates(&p, r.number, input.query.as_deref().unwrap_or("")).await.map_err(&fail);
    }
    let (users, requests) = tokio::join!(github::assignable_users(&p, input.query.as_deref().unwrap_or("")), github::review_requests(&p, r.number));
    let (users, truncated) = users.map_err(&fail)?;
    let requests = requests.map_err(&fail)?;
    let candidates = users
        .into_iter()
        .filter(|u| u.login.to_lowercase() != requests.author)
        .map(|actor| ReviewerCandidate { id: actor.login.clone(), kind: ReviewerKind::User, is_requested: requests.requested.contains(&actor.login.to_lowercase()), actor })
        .collect();
    Ok(ReviewerCandidates { candidates, truncated })
}

/// `PullRequestReviewerRequestInput`, beside the reference.
#[derive(Deserialize, Default)]
#[serde(default)]
struct ReviewerRequestInput {
    reviewers: Rows<ReviewerInput>,
    requested: Loose<bool>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ReviewerInput {
    id: Loose<String>,
}

async fn request_reviewers(p: Supported, r: PrRef, input: ReviewerRequestInput) -> Result<(), Failure> {
    let fail = failed(&p, "requestReviewers");
    let viewer = viewer_permissions(&p, r.number).await.map_err(&fail)?;
    if !viewer.request_reviewers {
        return Err(Failure::operation("requestReviewers", "You need write access on this repository to ask for a review."));
    }
    let ids: Vec<&str> = rows(&input.reviewers).filter_map(|reviewer| reviewer.id.as_deref()).collect();
    if ids.is_empty() {
        return Ok(());
    }
    if p.provider == Provider::GitLab {
        return gitlab::request_reviewers(&p, r.number, &ids, input.requested.0 == Some(true)).await.map_err(&fail);
    }
    let joined = ids.join(",");
    let flag = if input.requested.0 == Some(true) { "--add-reviewer" } else { "--remove-reviewer" };
    run_gh(&p, &["pr", "edit", &r.number.to_string(), "--repo", &p.repository, flag, &joined]).await.map_err(&fail)
}

const LABEL_CHANGE_REFUSAL: &str = "You need triage access on this repository to change its labels.";

async fn label_candidates(p: Supported, r: PrRef, _: NoFields) -> Result<LabelCandidates, Failure> {
    let fail = failed(&p, "labelCandidates");
    let viewer = viewer_permissions(&p, r.number).await.map_err(&fail)?;
    if !viewer.labels {
        return Err(Failure::operation("labelCandidates", LABEL_CHANGE_REFUSAL));
    }
    if p.provider == Provider::GitLab {
        return gitlab::label_candidates(&p, r.number).await.map_err(&fail);
    }
    let (all, applied) = tokio::join!(github::repository_labels(&p, "200", ""), github::applied_labels(&p, r.number));
    let all = all.map_err(&fail)?;
    let applied: HashSet<String> = applied.map_err(&fail)?.iter().map(|l| l.name.to_lowercase()).collect();
    let truncated = all.len() >= 200;
    let candidates = all
        .into_iter()
        .map(|l| LabelCandidate { is_applied: applied.contains(&l.name.to_lowercase()), name: l.name, color: l.color, description: l.description.flatten() })
        .collect();
    Ok(LabelCandidates { candidates, truncated })
}

/// `PullRequestLabelChangeInput`, beside the reference.
#[derive(Deserialize, Default)]
#[serde(default)]
struct LabelChangeInput {
    labels: Loose<Vec<Loose<String>>>,
    applied: Loose<bool>,
}

async fn set_labels(p: Supported, r: PrRef, input: LabelChangeInput) -> Result<(), Failure> {
    let fail = failed(&p, "setLabels");
    let viewer = viewer_permissions(&p, r.number).await.map_err(&fail)?;
    if !viewer.labels {
        return Err(Failure::operation("setLabels", LABEL_CHANGE_REFUSAL));
    }
    let labels: Vec<&str> = input.labels.iter().flatten().filter_map(|label| label.as_deref()).collect();
    if labels.is_empty() {
        return Ok(());
    }
    if p.provider == Provider::GitLab {
        return gitlab::set_labels(&p, r.number, &labels, input.applied.0 == Some(true)).await.map_err(&fail);
    }
    let joined = labels.join(",");
    let flag = if input.applied.0 == Some(true) { "--add-label" } else { "--remove-label" };
    run_gh(&p, &["pr", "edit", &r.number.to_string(), "--repo", &p.repository, flag, &joined]).await.map_err(&fail)
}

/// `PullRequestSetFilesViewedInput`, beside the reference.
#[derive(Deserialize, Default)]
#[serde(default)]
struct FilesViewedInput {
    files: Rows<FileViewedInput>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct FileViewedInput {
    path: Loose<Scalar>,
    viewed: Loose<bool>,
}

async fn set_files_viewed(p: Supported, r: PrRef, input: FilesViewedInput) -> Result<(), Failure> {
    let fail = failed(&p, "setFilesViewed");
    if p.provider == Provider::GitLab {
        return Err(Failure::operation("setFilesViewed", "This host keeps no record of the files you have seen."));
    }
    let id = github::node_id(&p, r.number).await.map_err(&fail)?;
    for f in rows(&input.files) {
        let query = if f.viewed.0 == Some(true) {
            "mutation($id: ID!, $path: String!) { markFileAsViewed(input: { pullRequestId: $id, path: $path }) { clientMutationId } }"
        } else {
            "mutation($id: ID!, $path: String!) { unmarkFileAsViewed(input: { pullRequestId: $id, path: $path }) { clientMutationId } }"
        };
        mutate(&p, query, &[("id", Var::Text(&id)), ("path", var(&f.path))]).await.map_err(&fail)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

/// Registers a method that routes its reference, checks the expected account, then runs `f`.
fn routed_method<T, O, F, Fut>(r: Router<ConnCtx>, tag: &'static str, f: F) -> Router<ConnCtx>
where
    T: DeserializeOwned + Default + Send + 'static,
    O: Serialize + Send + 'static,
    F: Fn(Supported, PrRef, T) -> Fut + Send + Sync + Copy + 'static,
    Fut: Future<Output = Result<O, Failure>> + Send + 'static,
{
    r.unary(tag, move |ctx: Arc<ConnCtx>, call: Call<T>| async move {
        ensure_linked_detector(&ctx.state);
        let (pr, project) = routed(&ctx.state, call.reference).await?;
        f(project, pr, call.fields).await
    })
}

/// Registers a method that sends text: an empty `body` is refused (with
/// `empty`) before the project is resolved, as in t3code.
fn bodied_method<T, F, Fut>(r: Router<ConnCtx>, tag: &'static str, empty: &'static str, body_of: fn(&T) -> &Loose<String>, f: F) -> Router<ConnCtx>
where
    T: DeserializeOwned + Default + Send + 'static,
    F: Fn(Supported, PrRef, T) -> Fut + Send + Sync + Copy + 'static,
    Fut: Future<Output = Result<(), Failure>> + Send + 'static,
{
    let operation = tag.trim_start_matches("pullRequests.");
    r.unary(tag, move |ctx: Arc<ConnCtx>, call: Call<T>| async move {
        if body_of(&call.fields).as_deref().unwrap_or("").trim().is_empty() {
            return Err(Failure::operation(operation, empty));
        }
        let (pr, project) = routed(&ctx.state, call.reference).await?;
        f(project, pr, call.fields).await
    })
}

/// `PullRequestReportStateInput`: only the reference is read.
#[derive(Deserialize, Default)]
#[serde(default)]
struct ReportStateInput {
    reference: Reference,
}

/// `PullRequestInvalidateInput`.
#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct InvalidateInput {
    /// Given (whatever it is) when one pull request is to be read again rather than everything.
    reference: Option<IgnoredAny>,
    files_viewed_only: Loose<bool>,
}

pub fn register(r: Router<ConnCtx>) -> Router<ConnCtx> {
    let r = r
        .unary("pullRequests.list", |ctx: Arc<ConnCtx>, input: ListInput| async move {
            ensure_linked_detector(&ctx.state);
            list(&ctx.state, input).await
        })
        .unary("pullRequests.listStats", |ctx: Arc<ConnCtx>, input: Fields<ListStatsInput>| async move { list_stats(&ctx.state, input.0).await })
        .unary("pullRequests.filterOptions", |ctx: Arc<ConnCtx>, input: Fields<FilterOptionsInput>| async move { filter_options(&ctx.state, input.0).await })
        .unary("pullRequests.routingIdentity", |ctx: Arc<ConnCtx>, input: Fields<HostInput>| async move { routing_identity(&ctx.state, input.0).await })
        .unary("pullRequests.routing", |ctx: Arc<ConnCtx>, call: Call<NoFields>| async move { routing(&ctx.state, call.reference).await })
        .unary("pullRequests.linkedThreads", |ctx: Arc<ConnCtx>, call: Call<NoFields>| async move {
            ensure_linked_detector(&ctx.state);
            linked_threads(&ctx.state, call.reference)
        })
        .unary("pullRequests.reportState", |ctx: Arc<ConnCtx>, input: Fields<ReportStateInput>| async move {
            let r = input.0.reference.resolve()?;
            require_project(&ctx.state, &r).await?;
            Ok::<_, Failure>(())
        })
        .unary("pullRequests.invalidate", |_: Arc<ConnCtx>, input: Fields<InvalidateInput>| async move {
            if input.reference.is_none() && input.files_viewed_only.0 != Some(true) {
                gh::forget_identities();
                crate::glab::forget_identities();
            }
            notify_refresh();
            Ok::<_, Never>(())
        })
        .stream("pullRequests.subscribeRefreshes", |ctx: Arc<ConnCtx>, _: IgnoredAny| {
            ensure_linked_detector(&ctx.state);
            counter_stream(refreshes())
        })
        .stream("pullRequests.subscribeLinkedThreadRefreshes", |ctx: Arc<ConnCtx>, _: IgnoredAny| {
            ensure_linked_detector(&ctx.state);
            counter_stream(linked_refreshes())
        })
        .unary("pullRequests.reviewGuide", |ctx: Arc<ConnCtx>, call: Call<NoFields>| async move { review_guide(&ctx.state, &call.reference.resolve()?).await })
        .unary("pullRequests.generateReviewGuide", |ctx: Arc<ConnCtx>, call: Call<NoFields>| async move { generate_review_guide(&ctx.state, call.reference).await });
    let r = routed_method(r, "pullRequests.summary", summary);
    let r = routed_method(r, "pullRequests.stack", stack);
    let r = routed_method(r, "pullRequests.detail", detail);
    let r = routed_method(r, "pullRequests.preview", preview);
    let r = routed_method(r, "pullRequests.checks", checks);
    let r = routed_method(r, "pullRequests.activity", activity);
    let r = routed_method(r, "pullRequests.threadComments", thread_comments);
    let r = routed_method(r, "pullRequests.diffFileContents", diff_file_contents);
    let r = routed_method(r, "pullRequests.filesViewed", files_viewed);
    let r = routed_method(r, "pullRequests.changeBreakdown", change_breakdown);
    let r = routed_method(r, "pullRequests.setFilesViewed", set_files_viewed);
    let r = routed_method(r, "pullRequests.runAction", run_action);
    let r = routed_method(r, "pullRequests.update", update);
    let r = routed_method(r, "pullRequests.submitReview", submit_review);
    let r = routed_method(r, "pullRequests.setThreadResolution", set_thread_resolution);
    let r = routed_method(r, "pullRequests.setReaction", set_reaction);
    let r = routed_method(r, "pullRequests.reviewerCandidates", reviewer_candidates);
    let r = routed_method(r, "pullRequests.requestReviewers", request_reviewers);
    let r = routed_method(r, "pullRequests.labelCandidates", label_candidates);
    let r = routed_method(r, "pullRequests.setLabels", set_labels);
    let r = bodied_method(r, "pullRequests.comment", "A comment cannot be empty.", |input: &CommentInput| &input.body, comment);
    let r = bodied_method(r, "pullRequests.replyToThread", "A reply cannot be empty.", |input: &ThreadReplyInput| &input.body, reply_to_thread);
    bodied_method(r, "pullRequests.updateComment", "A comment cannot be empty.", |input: &CommentUpdateInput| &input.body, update_comment)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn thread_event(ty: &str, links: Value, updated: &str) -> Value {
        let thread = crate::test_records::with(crate::test_records::thread("t1", "p"), json!({ "title": "x", "pullRequests": links, "updatedAt": updated }));
        json!({ "threadId": "t1", "type": ty, "occurredAt": updated, "payload": thread })
    }

    /// Hands the detector an event given as the JSON the log holds.
    fn observe(d: &mut LinkedDetector, event: &Value) -> Option<LinkedChange> {
        let thread: Option<AppThread> = event.get("payload").map(|thread| serde_json::from_value(thread.clone()).unwrap());
        d.observe(event["threadId"].as_str().unwrap(), event["type"].as_str().unwrap(), event["occurredAt"].as_str().unwrap(), thread.as_ref())
    }

    #[test]
    fn linked_thread_detector_follows_t3code() {
        let mut d = LinkedDetector::default();
        // A thread unseen since start counts as changed on its first full event.
        assert_eq!(observe(&mut d, &thread_event("thread.pinned", json!([]), "1")), Some(LinkedChange::Changed));
        assert_eq!(observe(&mut d, &thread_event("thread.pinned", json!([]), "2")), None);
        let linked = |source: &str| json!([{ "host": "github.com", "repository": "o/r", "number": 1, "url": "https://github.com/o/r/pull/1",
            "source": source, "linkedAt": "1", "snapshot": null, "stack": null }]);
        let link = linked("manual");
        assert_eq!(observe(&mut d, &thread_event("thread.metadata-updated", link.clone(), "3")), Some(LinkedChange::Changed));
        // Activity on a linked thread reorders answers.
        assert_eq!(observe(&mut d, &json!({ "threadId": "t1", "type": "message.created", "occurredAt": "4" })), Some(LinkedChange::Reordered));
        assert_eq!(observe(&mut d, &json!({ "threadId": "t1", "type": "message.created", "occurredAt": "4" })), None);
        assert_eq!(observe(&mut d, &json!({ "threadId": "t9", "type": "message.created", "occurredAt": "4" })), Some(LinkedChange::Reordered));
        let dismissed = linked("stack-dismissed");
        assert_eq!(observe(&mut d, &thread_event("thread.metadata-updated", dismissed, "5")), Some(LinkedChange::Changed));
    }

    #[test]
    fn routes_repository_identities() {
        let identity = |fields: Value| -> RepositoryIdentity {
            let mut whole = json!({ "locator": { "source": "git-remote", "remoteName": "origin", "remoteUrl": "u" } });
            whole.as_object_mut().unwrap().extend(fields.as_object().unwrap().clone());
            serde_json::from_value(whole).unwrap()
        };
        let named = identity(json!({ "canonicalKey": "github.com/o/r", "owner": "O", "name": "R", "provider": "github" }));
        assert_eq!(identity_route(&named), Some(("github.com".into(), "O/R".into(), Some("github".into()))));
        assert_eq!(identity_route(&identity(json!({ "canonicalKey": "GitLab.com/group/sub/repo" }))), Some(("gitlab.com".into(), "group/sub/repo".into(), None)));
        assert_eq!(identity_route(&identity(json!({ "canonicalKey": "no-slash" }))), None);
    }

    /// A row of a listing with what the filters look at.
    fn row(labels: &[&str], draft: bool, review: Option<&str>, author: &str) -> ListRow {
        let entry = json!({
            "provider": "github", "host": "github.com", "projectId": "p", "projectTitle": "P", "repository": "o/r", "number": 1, "title": "T",
            "url": "u", "author": null, "headBranch": "h", "baseBranch": "main", "state": "open", "isDraft": draft, "mergeability": "unknown",
            "additions": 0, "deletions": 0, "createdAt": "1", "updatedAt": "1", "viewerReviewRequested": false,
            "labels": labels.iter().map(|name| json!({ "name": name, "color": null })).collect::<Vec<_>>(), "reviewDecision": review,
        });
        ListRow { entry: serde_json::from_value(entry).unwrap(), author_login: Some(author.to_owned()), review_request_logins: vec![] }
    }

    fn filters(given: Value) -> ListFilters {
        serde_json::from_value::<Fields<ListFilters>>(given).unwrap().0
    }

    #[test]
    fn filters_rows_like_t3code() {
        let item = row(&["Bug"], true, Some("approved"), "Me");
        assert!(matches_filters(&item, &filters(json!({ "draft": "only", "author": "@me", "labels": [["bug", "x"]] })), "me"));
        assert!(!matches_filters(&item, &filters(json!({ "draft": "hide" })), "me"));
        assert!(!matches_filters(&item, &filters(json!({ "excludedLabels": ["BUG"] })), "me"));
        assert!(!matches_filters(&item, &filters(json!({ "review": "none" })), "me"));
        assert!(matches_filters(&item, &filters(json!({ "review": "approved" })), "me"));
        assert!(!matches_filters(&item, &filters(json!({ "checks": "passing" })), "me"), "no checks reported");
        assert!(!matches_filters(&item, &filters(json!({ "labels": ["bug"] })), "me"), "a group that is no list matches nothing");
        assert!(matches_filters(&item, &filters(json!("not filters")), "me"));
    }

    #[test]
    fn ranks_filter_options() {
        let items = vec!["bugfix", "bug", "a-bug", "other", "BUG"];
        let (ranked, truncated) = rank_filter_options(items, Some("bug"), |l| l.to_string(), |l| vec![l.to_string()], false, false);
        assert_eq!(ranked, ["bug", "bugfix", "a-bug"]);
        assert!(!truncated);
    }

    fn action(given: Value) -> Result<PullRequestAction, Failure> {
        check_action(Provider::GitHub, &serde_json::from_value::<Fields<ActionInput>>(given).unwrap())
    }

    #[test]
    fn refuses_unsupported_actions_before_reading() {
        assert!(action(json!({ "action": "merge", "mergeMethod": "fast-forward" })).is_err());
        assert!(action(json!({ "action": "merge", "stackNumber": 2 })).is_err());
        assert!(action(json!({ "action": "explode" })).is_err());
        assert_eq!(action(json!({ "action": "close" })).unwrap(), PullRequestAction::Close);
        assert_eq!(action(json!({ "action": "merge", "stackNumber": null })).unwrap(), PullRequestAction::Merge, "a null stack is no stack");
        let on_gitlab = |given: Value| check_action(Provider::GitLab, &serde_json::from_value::<Fields<ActionInput>>(given).unwrap());
        assert!(on_gitlab(json!({ "action": "approve-workflows" })).is_err(), "GitLab has no workflow approvals");
        assert!(on_gitlab(json!({ "action": "update-branch", "updateMethod": "merge" })).is_err(), "GitLab only rebases");
        assert_eq!(on_gitlab(json!({ "action": "update-branch", "updateMethod": "rebase" })).unwrap(), PullRequestAction::UpdateBranch);
    }

    #[test]
    fn hosts_named_gitlab_route_to_gitlab() {
        assert_eq!(route_kind("gitlab.example.com", None).as_deref(), Some("gitlab"));
        assert_eq!(route_kind("git.example.com", Some("unknown".into())), None);
        assert_eq!(route_kind("gitlab.example.com", Some("github".into())).as_deref(), Some("github"), "the identity's own kind wins");
        assert_eq!(change_request_url("gitlab.com", "g/a", 3), "https://gitlab.com/g/a/-/merge_requests/3");
        assert_eq!(change_request_url("github.com", "o/r", 3), "https://github.com/o/r/pull/3");
    }

    #[test]
    fn a_payload_is_read_as_a_reference_and_as_fields() {
        let call: Call<CommentInput> = serde_json::from_value(json!({ "projectId": "p", "repository": "o/r", "number": 3, "body": "hi", "more": 1 })).unwrap();
        let reference = call.reference.resolve().unwrap();
        assert_eq!((reference.project_id.as_str(), reference.repository.as_str(), reference.number), ("p", "o/r", 3));
        assert_eq!(call.fields.body.as_deref(), Some("hi"));

        // A reference that cannot be read is the call's failure, with what was wrong with it; the fields are read all the same.
        let call: Call<CommentInput> = serde_json::from_value(json!({ "projectId": "p", "body": 5 })).unwrap();
        assert_eq!(call.fields.body.0, None);
        assert_eq!(
            json!(call.reference.resolve().unwrap_err()),
            json!({ "_tag": "PullRequestOperationError", "operation": "resolveRepository", "detail": "Invalid pull request reference: missing field `repository`" })
        );
        let call: Call<NoFields> = serde_json::from_value(json!("o/r#3")).unwrap();
        assert_eq!(
            json!(call.reference.resolve().unwrap_err()),
            json!({ "_tag": "PullRequestOperationError", "operation": "resolveRepository",
                    "detail": "Invalid pull request reference: invalid type: string \"o/r#3\", expected struct PrRef" })
        );
        let report: Fields<ReportStateInput> = serde_json::from_value(json!({ "state": "open" })).unwrap();
        assert!(report.0.reference.resolve().is_err(), "no reference is one that cannot be read");
    }
}
