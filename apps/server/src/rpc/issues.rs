//! The `issues.*` RPCs (t3code `IssueService`, `IssueTrackerRegistry` and
//! `issue/linkedThreads`), added in t3code v0.1.11; `issueTrackers.*` is
//! `trackers`.
//!
//! A project's issues live where its repository identity says (its git
//! host's tracker: GitHub, read through `gh` in `github`; GitLab, read
//! through `glab` in `gitlab`, a host whose identity names no kind counting
//! as GitLab when its name says so), and beside them in
//! the standalone tracker scope the project is bound to in Settings (Linear,
//! `linear`). A fork reads its upstream and its own repository. A reference
//! to another repository on the same host is served read-only through a
//! project on that host, and every write passes `require_writable`, as
//! upstream. Failures are t3code's `IssueUnavailableError` (the tracker
//! cannot be used: tool missing, signed out, unsupported) and
//! `IssueOperationError` (one request failed).
//!
//! Limits: only GitHub, GitLab and Linear have a driver; Bitbucket, Azure
//! DevOps and Forgejo hosts are listed as trackers that "cannot be browsed
//! for this host yet", which is what t3code says for a host without one.
//! Reads are not cached and `refresh` changes nothing; a GitHub or GitLab
//! rate limit carries no `retryAt` (neither CLI says when it resets). Each
//! driver's specifics are in its own header.

mod github;
mod gitlab;
mod linear;
pub(crate) mod trackers;

use crate::state::{AppState, ConnCtx};
use domovoi_model::typed::{
    IssueCandidates, IssueCapabilities, IssueCommentInput, IssueCommentsPage, IssueCommentsPageInput, IssueCreateInput, IssueCreateResult, IssueDetail,
    IssueInvolvement, IssueLinkedThread, IssueLinkedThreadsResult, IssueListEntry, IssueListInput, IssueListProjectError, IssueListResult, IssueListSort,
    IssueListState, IssueReactionInput, IssueRef, IssueScopeRef, IssueSetStateInput, IssueState, IssueSummary, IssueTemplateList, IssueTrackerProjectBinding,
    IssueTrackerSummary, IssueUpdateInput, IssueViewerPermissions, RepositoryIdentity, SettledOverride, ThreadIssueKey, IssueComment, IssueEvent,
    IssueLinkedPullRequest, IssueReaction,
};
use domovoi_rpc::Router;
use futures::StreamExt;
use serde::Serialize;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

const DEFAULT_LIST_LIMIT: u64 = 50;
const SCOPE_CONCURRENCY: usize = 4;

// ---------------------------------------------------------------------------
// Failures
// ---------------------------------------------------------------------------

/// Why an issue call failed: t3code's `IssueUnavailableError` or `IssueOperationError`.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "_tag")]
pub enum IssueFailure {
    /// The tracker cannot be read at all for this project.
    #[serde(rename = "IssueUnavailableError")]
    Unavailable {
        /// `tool-missing`, `unauthenticated` or `tracker-unsupported`.
        reason: &'static str,
        #[serde(skip_serializing_if = "Option::is_none")]
        tracker: Option<String>,
    },
    #[serde(rename = "IssueOperationError")]
    Operation {
        operation: String,
        detail: String,
        /// `not-found`, `read-only` or `rate-limited`.
        #[serde(skip_serializing_if = "Option::is_none")]
        reason: Option<&'static str>,
        #[serde(rename = "retryAt", skip_serializing_if = "Option::is_none")]
        retry_at: Option<u64>,
    },
}

impl std::fmt::Display for IssueFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IssueFailure::Unavailable { reason, .. } => f.write_str(match *reason {
                "tool-missing" => "The tool this tracker is read through is not installed or set up.",
                "unauthenticated" => "This tracker has no working credentials.",
                _ => "Issues cannot be browsed for this project yet.",
            }),
            IssueFailure::Operation { operation, detail, .. } => write!(f, "Issue operation {operation} failed: {detail}"),
        }
    }
}

impl std::error::Error for IssueFailure {}

impl IssueFailure {
    pub(crate) fn unsupported() -> Self {
        IssueFailure::Unavailable { reason: "tracker-unsupported", tracker: None }
    }

    pub(crate) fn operation(operation: &str, detail: impl Into<String>) -> Self {
        IssueFailure::Operation { operation: operation.to_owned(), detail: detail.into(), reason: None, retry_at: None }
    }

    fn with_reason(operation: &str, reason: &'static str, detail: impl Into<String>) -> Self {
        IssueFailure::Operation { operation: operation.to_owned(), detail: detail.into(), reason: Some(reason), retry_at: None }
    }

    fn read_only(operation: &str) -> Self {
        Self::with_reason(operation, "read-only", "This issue belongs to a repository outside the selected project and cannot be changed from here.")
    }

    fn not_supported(operation: &str, what: &str) -> Self {
        Self::operation(operation, format!("This tracker does not support {what}."))
    }
}

/// `IssueTrackerError.reason`: what a tracker's failure means.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TrackerReason {
    MissingTool,
    Unauthenticated,
    RateLimited,
    NotFound,
    Failed,
}

/// The one failure shape every tracker reports.
#[derive(Debug, Clone)]
pub(crate) struct TrackerError {
    pub tracker: &'static str,
    /// The tracker's own operation, for the log line a failure leaves.
    pub operation: String,
    pub reason: TrackerReason,
    pub detail: String,
    /// Epoch milliseconds, where the tracker says when to retry.
    pub retry_at: Option<u64>,
}

impl TrackerError {
    pub fn new(tracker: &'static str, operation: &str, reason: TrackerReason, detail: impl Into<String>) -> Self {
        TrackerError { tracker, operation: operation.to_owned(), reason, detail: detail.into(), retry_at: None }
    }

    /// A missing or signed-out tool disables the tracker for the whole workspace.
    fn unusable(&self) -> bool {
        matches!(self.reason, TrackerReason::MissingTool | TrackerReason::Unauthenticated)
    }

    /// `trackerDetail`: the wire errors carry a non-empty one.
    pub fn detail(&self) -> String {
        let detail = self.detail.trim();
        if detail.is_empty() { "The issue tracker request failed.".into() } else { detail.to_owned() }
    }

    /// `toIssueError`.
    fn into_failure(self, operation: &str) -> IssueFailure {
        tracing::debug!(tracker = self.tracker, operation = %self.operation, detail = %self.detail, "issue tracker request failed");
        match self.reason {
            TrackerReason::MissingTool => IssueFailure::Unavailable { reason: "tool-missing", tracker: Some(self.tracker.into()) },
            TrackerReason::Unauthenticated => IssueFailure::Unavailable { reason: "unauthenticated", tracker: Some(self.tracker.into()) },
            reason => IssueFailure::Operation {
                operation: operation.to_owned(),
                detail: self.detail(),
                reason: match reason {
                    TrackerReason::NotFound => Some("not-found"),
                    TrackerReason::RateLimited => Some("rate-limited"),
                    _ => None,
                },
                retry_at: self.retry_at,
            },
        }
    }
}

fn to_issue_error(operation: &str) -> impl Fn(TrackerError) -> IssueFailure + '_ {
    move |e| e.into_failure(operation)
}

// ---------------------------------------------------------------------------
// Trackers and what they hand back
// ---------------------------------------------------------------------------

/// The trackers this server has a driver for (`IssueTrackerRegistry`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    GitHub,
    GitLab,
    Linear,
}

impl Kind {
    fn of(tracker: &str) -> Option<Kind> {
        match tracker {
            github::KIND => Some(Kind::GitHub),
            gitlab::KIND => Some(Kind::GitLab),
            linear::KIND => Some(Kind::Linear),
            _ => None,
        }
    }

    fn capabilities(self) -> IssueCapabilities {
        match self {
            Kind::GitHub => github::CAPABILITIES,
            Kind::GitLab => gitlab::CAPABILITIES,
            Kind::Linear => linear::CAPABILITIES,
        }
    }
}

/// What a listing asks one scope for.
#[derive(Debug, Clone)]
pub(crate) struct ListQuery {
    pub state: IssueListState,
    pub involvement: IssueInvolvement,
    pub limit: u64,
    pub query: Option<String>,
    pub labels: Vec<String>,
    pub assignee: Option<String>,
    pub sort: IssueListSort,
    pub cursor: Option<String>,
}

/// One slice of a scope's issues.
pub(crate) struct IssuePage {
    pub items: Vec<IssueSummary>,
    pub next_cursor: Option<String>,
}

/// An issue as the tracker sees it, before the project is attached (`TrackerIssueDetail`).
pub(crate) struct TrackerIssue {
    pub summary: IssueSummary,
    pub body: String,
    pub viewer: Option<String>,
    pub viewer_permissions: IssueViewerPermissions,
    pub states: Vec<IssueState>,
    pub reactions: Option<Vec<IssueReaction>>,
    pub comments: Vec<IssueComment>,
    pub comments_cursor: Option<String>,
    pub events: Vec<IssueEvent>,
    pub linked_pull_requests: Vec<IssueLinkedPullRequest>,
}

/// A project the registry has a tracker for (`SupportedProject`): which
/// driver, where it is served from, and the container read.
#[derive(Debug, Clone)]
pub(crate) struct Served {
    pub project_id: String,
    pub project_title: String,
    pub workspace_root: String,
    pub tracker: String,
    pub host: String,
    pub scope: String,
    pub kind: Kind,
    /// Where standalone trackers' credentials are kept.
    pub userdata: PathBuf,
}

impl Served {
    async fn viewer(&self) -> Result<String, TrackerError> {
        match self.kind {
            Kind::GitHub => github::viewer(self).await,
            Kind::GitLab => gitlab::viewer(self).await,
            Kind::Linear => linear::viewer(&self.userdata).await,
        }
    }

    async fn list_issues(&self, q: &ListQuery, viewer: Option<&str>) -> Result<IssuePage, TrackerError> {
        match self.kind {
            Kind::GitHub => github::list_issues(self, q, viewer).await,
            Kind::GitLab => gitlab::list_issues(self, q, viewer).await,
            Kind::Linear => linear::list_issues(&self.userdata, &self.scope, q, viewer).await,
        }
    }

    async fn get_issue(&self, key: &str) -> Result<TrackerIssue, TrackerError> {
        match self.kind {
            Kind::GitHub => github::get_issue(self, key).await,
            Kind::GitLab => gitlab::get_issue(self, key).await,
            Kind::Linear => linear::get_issue(&self.userdata, key).await,
        }
    }

    fn into_detail(self, issue: TrackerIssue, writable: bool) -> IssueDetail {
        let mut summary = issue.summary;
        summary.tracker = self.tracker.clone();
        summary.host = self.host.clone();
        summary.scope = self.scope.clone();
        IssueDetail {
            summary,
            project_id: self.project_id.as_str().into(),
            project_title: self.project_title,
            writable,
            capabilities: self.kind.capabilities(),
            viewer_permissions: issue.viewer_permissions,
            states: issue.states,
            viewer: issue.viewer,
            body: issue.body,
            reactions: issue.reactions,
            comments: issue.comments,
            comments_cursor: issue.comments_cursor,
            events: issue.events,
            linked_pull_requests: issue.linked_pull_requests,
        }
    }
}

// ---------------------------------------------------------------------------
// Which tracker a project's issues are in
// ---------------------------------------------------------------------------

/// One project's resolved tracker (`ProjectTrackerBinding`).
#[derive(Debug, Clone, PartialEq)]
struct Binding {
    tracker: String,
    host: String,
    scope: String,
}

/// The repository path of an identity (`repositorySelector`): as the host writes it.
fn repository_path(identity: &RepositoryIdentity) -> Option<String> {
    if let Some(name) = identity.display_name.as_deref().filter(|n| n.contains('/')) {
        return Some(name.trim().to_owned());
    }
    if let (Some(owner), Some(name)) = (identity.owner.as_deref(), identity.name.as_deref()) {
        return Some(format!("{owner}/{name}"));
    }
    identity.canonical_key.split_once('/').map(|(_, path)| path.to_owned()).filter(|p| !p.is_empty())
}

/// The host of a canonical key (`projectChangeRequestHost`), lower case.
fn key_host(canonical_key: &str) -> Option<String> {
    canonical_key.split('/').next().map(str::trim).filter(|h| !h.is_empty()).map(str::to_lowercase)
}

/// Every repository whose issues are the project's own (`issueBindings`): its
/// primary one, then the fork its `origin` names when that is on the same host.
fn git_host_bindings(identity: Option<&RepositoryIdentity>) -> Vec<Binding> {
    let Some(identity) = identity else { return vec![] };
    let Some(host) = key_host(&identity.canonical_key) else { return vec![] };
    // A host whose name says GitLab is GitLab's (t3code's `recognizeRemote`: GitLab Self-Hosted).
    let recognized = crate::glab::recognizes_host(&host).then_some(gitlab::KIND);
    let Some(tracker) = identity.provider.as_deref().filter(|p| *p != "unknown").or(recognized) else { return vec![] };
    let Some(scope) = repository_path(identity) else { return vec![] };
    let primary = Binding { tracker: tracker.to_owned(), host: host.clone(), scope: scope.clone() };
    let Some(origin) = &identity.origin else { return vec![primary] };
    if key_host(&origin.canonical_key).as_deref() != Some(host.as_str()) {
        return vec![primary];
    }
    let origin_scope = origin
        .display_name
        .clone()
        .filter(|n| n.contains('/'))
        .or_else(|| origin.canonical_key.split_once('/').map(|(_, path)| path.to_owned()))
        .filter(|s| !s.is_empty());
    match origin_scope {
        Some(other) if !other.eq_ignore_ascii_case(&scope) => vec![primary, Binding { tracker: tracker.to_owned(), host, scope: other }],
        _ => vec![primary],
    }
}

/// The repository a project is of: as its row says, or as its folder says now.
async fn project_identity(project: &crate::wire::shell::ProjectShell) -> Option<RepositoryIdentity> {
    match &project.repository_identity {
        Some(identity) => Some(identity.clone()),
        None => crate::vcs::repository_identity(&project.workspace_root).await,
    }
}

#[derive(Default)]
struct ProjectFilter<'a> {
    project_id: Option<&'a str>,
    exclude: Option<&'a [domovoi_model::typed::ProjectId]>,
    host: Option<String>,
    /// The link's `host:port`, for a host whose binding keeps its port.
    authority: Option<String>,
}

#[derive(Default)]
struct Workspace {
    supported: Vec<Served>,
    /// host → (tracker, projects), for hosts without a driver.
    unimplemented: Vec<(String, String, HashSet<String>)>,
}

/// `listProjects`: every project's trackers, the bound standalone one first.
async fn list_projects(state: &AppState, filter: ProjectFilter<'_>) -> Result<Workspace, IssueFailure> {
    let projects = state.store.list_projects().map_err(|_| IssueFailure::operation("listProjects", "The project list could not be read."))?;
    let projects: Vec<_> = projects
        .into_iter()
        .filter(|p| filter.project_id.is_none_or(|id| id == p.id))
        .filter(|p| !filter.exclude.is_some_and(|ex| ex.iter().any(|id| id.as_str() == p.id)))
        .collect();
    let mut ws = Workspace::default();
    if projects.is_empty() {
        return Ok(ws);
    }
    // One read per listing, never cached, so Settings changes apply; a failed
    // read is not an unbound project.
    let bound = trackers::bindings(state).map_err(|_| IssueFailure::operation("listProjects", "The project's issue tracker binding could not be read."))?;
    let identities: Vec<Option<RepositoryIdentity>> = futures::stream::iter(projects.iter().map(project_identity)).buffered(SCOPE_CONCURRENCY).collect().await;
    let host = filter.host.as_deref().map(|h| h.trim().to_lowercase());
    for (project, identity) in projects.iter().zip(identities) {
        let mut candidates = vec![];
        if let Some(binding) = bound.get(&project.id).and_then(standalone_binding) {
            candidates.push(binding);
        }
        candidates.extend(git_host_bindings(identity.as_ref()));
        for candidate in candidates {
            if host.as_deref().is_some_and(|h| candidate.host != h && Some(candidate.host.as_str()) != filter.authority.as_deref()) {
                continue;
            }
            let Some(kind) = Kind::of(&candidate.tracker) else {
                match ws.unimplemented.iter_mut().find(|(h, _, _)| *h == candidate.host) {
                    Some(slot) => {
                        slot.2.insert(project.id.clone());
                    }
                    None => ws.unimplemented.push((candidate.host, candidate.tracker, HashSet::from([project.id.clone()]))),
                }
                continue;
            };
            ws.supported.push(Served {
                project_id: project.id.clone(),
                project_title: project.title.clone(),
                workspace_root: project.workspace_root.clone(),
                tracker: candidate.tracker,
                host: candidate.host,
                scope: candidate.scope,
                kind,
                userdata: state.paths.userdata.clone(),
            });
        }
    }
    Ok(ws)
}

/// A binding to a standalone tracker this server ships (`resolveStandaloneBinding`).
fn standalone_binding(binding: &IssueTrackerProjectBinding) -> Option<Binding> {
    trackers::standalone_host(&binding.tracker).map(|host| Binding { tracker: binding.tracker.clone(), host: host.to_owned(), scope: binding.scope.clone() })
}

/// `requireProject`: the project whose checkout and credentials serve a
/// reference, and whether the reference is one of its own scopes.
async fn require_project(state: &AppState, r: &IssueScopeRef) -> Result<(Served, bool), IssueFailure> {
    let ws = list_projects(state, ProjectFilter { project_id: Some(r.project_id.as_str()), ..Default::default() }).await?;
    let host = r.host.as_deref().map(|h| h.trim().to_lowercase());
    let on_host: Vec<&Served> = ws.supported.iter().filter(|c| c.tracker == r.tracker && host.as_deref().is_none_or(|h| c.host == h)).collect();
    let scope = r.scope.trim();
    let own = on_host.iter().find(|c| c.scope.eq_ignore_ascii_case(scope)).copied();
    let served = own.or(on_host.first().copied()).ok_or_else(IssueFailure::unsupported)?;
    let mut served = served.clone();
    served.scope = scope.to_owned();
    Ok((served, own.is_some()))
}

async fn require_writable(state: &AppState, r: &IssueScopeRef, operation: &str) -> Result<Served, IssueFailure> {
    match require_project(state, r).await? {
        (served, true) => Ok(served),
        (_, false) => Err(IssueFailure::read_only(operation)),
    }
}

// ---------------------------------------------------------------------------
// The service
// ---------------------------------------------------------------------------

/// Lower case, so a scope keeps its cursor whichever project's spelling is read.
pub(crate) fn list_cursor_key(host: &str, scope: &str) -> String {
    format!("{host} {}", scope.to_lowercase())
}

/// Insert-ordered map of tracker summaries by host (upstream's `Map`).
#[derive(Default)]
struct Summaries(Vec<IssueTrackerSummary>);

impl Summaries {
    fn get_mut(&mut self, host: &str) -> Option<&mut IssueTrackerSummary> {
        self.0.iter_mut().find(|s| s.host == host)
    }
    fn set(&mut self, summary: IssueTrackerSummary) {
        match self.get_mut(&summary.host.clone()) {
            Some(slot) => *slot = summary,
            None => self.0.push(summary),
        }
    }
}

/// `issues.list`.
pub(crate) async fn list(state: &AppState, input: IssueListInput) -> Result<IssueListResult, IssueFailure> {
    // Trackers name every host so a client narrowed to one can switch; only the narrowed host is read.
    let ws = list_projects(state, ProjectFilter { project_id: input.project_id.as_ref().map(|p| p.as_str()), exclude: input.exclude_project_ids.as_deref(), ..Default::default() }).await?;
    let host = input.host.as_deref().map(|h| h.trim().to_lowercase());
    let limit = input.limit.unwrap_or(DEFAULT_LIST_LIMIT);
    // Worktrees of one repository are separate projects: read each scope once.
    let mut scopes: Vec<(String, Served)> = vec![];
    for project in &ws.supported {
        if host.as_deref().is_some_and(|h| project.host != h) {
            continue;
        }
        let key = list_cursor_key(&project.host, &project.scope);
        if !scopes.iter().any(|(k, _)| *k == key) {
            scopes.push((key, project.clone()));
        }
    }
    let involvement = input.involvement.clone().unwrap_or(IssueInvolvement::All);
    // A continuation names the scopes it wants; one not named has nothing to add.
    let continuing = input.cursors.as_ref();
    let reading: Vec<&(String, Served)> = scopes.iter().filter(|(key, _)| continuing.is_none_or(|c| c.contains_key(key))).collect();
    // Only a narrowed listing compares against the signed-in account, once per host.
    let mut viewer_of: HashMap<String, Result<String, TrackerError>> = HashMap::new();
    if involvement != IssueInvolvement::All {
        for (_, project) in reading.iter().copied() {
            if !viewer_of.contains_key(&project.host) {
                let viewer = project.viewer().await;
                viewer_of.insert(project.host.clone(), viewer);
            }
        }
    }
    let mut viewers = BTreeMap::new();
    for (host, viewer) in &viewer_of {
        if let Ok(login) = viewer {
            viewers.insert(host.clone(), login.clone());
        }
    }

    // Every project counts once, worktrees of a scope read once and forks included.
    let mut trackers = Summaries::default();
    let mut counted = HashSet::new();
    for project in &ws.supported {
        if !counted.insert(format!("{} {}", project.host, project.project_id)) {
            continue;
        }
        match trackers.get_mut(&project.host) {
            Some(existing) => existing.project_count += 1,
            None => trackers.set(IssueTrackerSummary {
                tracker: project.tracker.clone(),
                host: project.host.clone(),
                project_count: 1,
                capabilities: Some(project.kind.capabilities()),
                configured: true,
                detail: None,
            }),
        }
    }

    let base = ListQuery {
        state: input.state.clone(),
        involvement: involvement.clone(),
        limit,
        query: input.query.clone(),
        labels: input.labels.clone().unwrap_or_default(),
        assignee: input.assignee.clone(),
        sort: input.sort.clone().unwrap_or(IssueListSort::Updated),
        cursor: None,
    };
    let mut reads = vec![];
    for (key, project) in reading {
        let cursor = continuing.and_then(|c| c.get(key)).cloned();
        let q = ListQuery { cursor: cursor.clone(), ..base.clone() };
        reads.push(read_scope(key.clone(), cursor, project, q, viewer_of.get(&project.host).cloned()));
    }
    let pages: Vec<_> = futures::stream::iter(reads).buffered(SCOPE_CONCURRENCY).collect().await;

    let mut entries = vec![];
    let mut errors = vec![];
    let mut next_cursors = BTreeMap::new();
    for (key, cursor, project, page) in pages {
        match page {
            Err(failure) => {
                if let Some(cursor) = cursor {
                    next_cursors.insert(key, cursor);
                }
                if failure.unusable() {
                    if let Some(existing) = trackers.get_mut(&project.host) {
                        existing.configured = false;
                        existing.detail = Some(failure.detail());
                    }
                }
                // A fork reads two repositories, so its failure names the one that failed.
                let forked = ws.supported.iter().any(|o| o.project_id == project.project_id && o.scope != project.scope);
                errors.push(IssueListProjectError {
                    project_id: project.project_id.as_str().into(),
                    project_title: if forked { format!("{} ({})", project.project_title, project.scope) } else { project.project_title.clone() },
                    message: failure.detail(),
                });
            }
            Ok(page) => {
                for mut item in page.items {
                    item.tracker = project.tracker.clone();
                    item.host = project.host.clone();
                    item.scope = project.scope.clone();
                    entries.push(IssueListEntry { summary: item, project_id: project.project_id.as_str().into(), project_title: project.project_title.clone() });
                }
                if let Some(next) = page.next_cursor {
                    next_cursors.insert(key, next);
                }
            }
        }
    }
    for (host, tracker, projects) in ws.unimplemented {
        trackers.set(IssueTrackerSummary {
            tracker,
            host,
            project_count: projects.len() as u64,
            capabilities: None,
            configured: false,
            detail: Some("Issues cannot be browsed for this host yet.".into()),
        });
    }
    entries.sort_by(|a, b| b.summary.updated_at.as_str().cmp(a.summary.updated_at.as_str()));
    Ok(IssueListResult {
        viewers,
        trackers: trackers.0,
        entries,
        errors,
        next_cursors,
        scopes: Some(
            scopes
                .into_iter()
                .map(|(_, p)| IssueScopeRef { project_id: p.project_id.as_str().into(), tracker: p.tracker, host: Some(p.host), scope: p.scope })
                .collect(),
        ),
    })
}

/// One slice of one scope, for `list`: the scope's key, the cursor it was read from, and the page.
async fn read_scope(
    key: String,
    cursor: Option<String>,
    project: &Served,
    q: ListQuery,
    viewer: Option<Result<String, TrackerError>>,
) -> (String, Option<String>, &Served, Result<IssuePage, TrackerError>) {
    let page = match viewer {
        Some(Err(e)) => Err(e),
        Some(Ok(login)) => project.list_issues(&q, Some(&login)).await,
        None => project.list_issues(&q, None).await,
    };
    (key, cursor, project, page)
}

/// `issues.detail`.
pub(crate) async fn detail(state: &AppState, r: &IssueRef) -> Result<IssueDetail, IssueFailure> {
    let (served, writable) = require_project(state, &IssueScopeRef::from(r)).await?;
    let issue = served.get_issue(&r.key).await.map_err(to_issue_error("detail"))?;
    Ok(served.into_detail(issue, writable))
}

async fn comments(state: &AppState, input: &IssueCommentsPageInput) -> Result<IssueCommentsPage, IssueFailure> {
    let (served, _) = require_project(state, &IssueScopeRef::from(&input.issue)).await?;
    let key = &input.issue.key;
    match served.kind {
        Kind::GitHub => github::get_comments(&served, key, &input.cursor).await,
        Kind::GitLab => gitlab::get_comments(&served, key, &input.cursor).await,
        Kind::Linear => linear::get_comments(&served.userdata, key, &input.cursor).await,
    }
    .map_err(to_issue_error("comments"))
}

async fn templates(state: &AppState, input: &IssueScopeRef) -> Result<IssueTemplateList, IssueFailure> {
    let (served, _) = require_project(state, input).await?;
    if !served.kind.capabilities().templates {
        return Err(IssueFailure::not_supported("templates", "issue templates"));
    }
    match served.kind {
        Kind::GitHub => github::list_templates(&served).await,
        Kind::GitLab => gitlab::list_templates(&served).await,
        Kind::Linear => linear::list_templates(&served.userdata, &served.scope).await,
    }
    .map_err(to_issue_error("templates"))
}

async fn candidates(state: &AppState, input: &IssueScopeRef) -> Result<IssueCandidates, IssueFailure> {
    let (served, _) = require_project(state, input).await?;
    let caps = served.kind.capabilities();
    if !(caps.labels || caps.assignees) {
        return Err(IssueFailure::not_supported("candidates", "labels or assignees"));
    }
    match served.kind {
        Kind::GitHub => github::list_candidates(&served).await,
        Kind::GitLab => gitlab::list_candidates(&served).await,
        Kind::Linear => linear::list_candidates(&served.userdata, &served.scope).await,
    }
    .map_err(to_issue_error("candidates"))
}

/// `issues.create`.
pub(crate) async fn create(state: &AppState, input: &IssueCreateInput) -> Result<IssueCreateResult, IssueFailure> {
    let served = require_writable(state, &input.scope, "create").await?;
    if !served.kind.capabilities().create {
        return Err(IssueFailure::not_supported("create", "filing issues"));
    }
    match served.kind {
        Kind::GitHub => github::create(&served, &input.title, &input.body, &input.labels, &input.assignees).await,
        Kind::GitLab => gitlab::create(&served, &input.title, &input.body, &input.labels, &input.assignees).await,
        Kind::Linear => linear::create(&served.userdata, &served.scope, &input.title, &input.body, &input.labels, &input.assignees).await,
    }
    .map_err(to_issue_error("create"))
}

/// `issues.comment`.
pub(crate) async fn comment(state: &AppState, input: &IssueCommentInput) -> Result<(), IssueFailure> {
    let served = require_writable(state, &IssueScopeRef::from(&input.issue), "comment").await?;
    if input.body.trim().is_empty() {
        return Err(IssueFailure::operation("comment", "A comment needs some text."));
    }
    if !served.kind.capabilities().comment {
        return Err(IssueFailure::not_supported("comment", "comments"));
    }
    let key = &input.issue.key;
    match served.kind {
        Kind::GitHub => github::comment(&served, key, &input.body).await,
        Kind::GitLab => gitlab::comment(&served, key, &input.body).await,
        Kind::Linear => linear::comment_on(&served.userdata, &served.scope, key, &input.body).await,
    }
    .map_err(to_issue_error("comment"))
}

async fn update(state: &AppState, input: &IssueUpdateInput) -> Result<(), IssueFailure> {
    let served = require_writable(state, &IssueScopeRef::from(&input.issue), "update").await?;
    let caps = served.kind.capabilities();
    let wants_words = input.title.is_some() || input.body.is_some();
    if !wants_words && input.labels.is_none() && input.assignees.is_none() {
        return Err(IssueFailure::operation("update", "Nothing to change."));
    }
    if wants_words && !caps.edit {
        return Err(IssueFailure::not_supported("update", "editing the title or body"));
    }
    if input.labels.is_some() && !caps.labels {
        return Err(IssueFailure::not_supported("update", "labels"));
    }
    if input.assignees.is_some() && !caps.assignees {
        return Err(IssueFailure::not_supported("update", "assignees"));
    }
    let (key, title, body, labels, assignees) = (&input.issue.key, input.title.as_deref(), input.body.as_deref(), input.labels.as_deref(), input.assignees.as_deref());
    match served.kind {
        Kind::GitHub => github::update(&served, key, title, body, labels, assignees).await,
        Kind::GitLab => gitlab::update(&served, key, title, body, labels, assignees).await,
        Kind::Linear => linear::update(&served.userdata, &served.scope, key, title, body, labels, assignees).await,
    }
    .map_err(to_issue_error("update"))
}

/// `issues.setState`.
pub(crate) async fn set_state(state: &AppState, input: &IssueSetStateInput) -> Result<(), IssueFailure> {
    let served = require_writable(state, &IssueScopeRef::from(&input.issue), "setState").await?;
    let key = &input.issue.key;
    match served.kind {
        Kind::GitHub => github::set_state(&served, key, &input.state).await,
        Kind::GitLab => gitlab::set_state(&served, key, &input.state).await,
        Kind::Linear => linear::set_state(&served.userdata, &served.scope, key, &input.state).await,
    }
    .map_err(to_issue_error("setState"))
}

async fn react(state: &AppState, input: &IssueReactionInput) -> Result<(), IssueFailure> {
    let served = require_writable(state, &IssueScopeRef::from(&input.issue), "react").await?;
    if !served.kind.capabilities().reactions {
        return Err(IssueFailure::not_supported("react", "reactions"));
    }
    let (key, subject) = (&input.issue.key, input.subject_id.as_deref());
    match served.kind {
        Kind::GitHub => github::set_reaction(&served, key, subject, &input.content, input.reacted).await,
        Kind::GitLab => gitlab::set_reaction(&served, key, subject, &input.content, input.reacted).await,
        Kind::Linear => linear::set_reaction(&served.userdata, &served.scope, key, subject, &input.content, input.reacted).await,
    }
    .map_err(to_issue_error("react"))
}

/// `projectScope`: the tracker and container a project's own issues live in.
pub(crate) async fn project_scope(state: &AppState, project_id: &str) -> Result<IssueScopeRef, IssueFailure> {
    let ws = list_projects(state, ProjectFilter { project_id: Some(project_id), ..Default::default() }).await?;
    let own = ws.supported.into_iter().next().ok_or_else(IssueFailure::unsupported)?;
    Ok(IssueScopeRef { project_id: project_id.into(), tracker: own.tracker, host: Some(own.host), scope: own.scope })
}

/// An issue URL resolved through a project (`resolveUrl`).
pub(crate) struct Resolved {
    /// With its host always written.
    pub r: IssueRef,
    /// Present when resolving read the issue already (a standalone tracker's).
    pub issue: Option<IssueDetail>,
}

/// `resolveUrl`: an issue URL read through a matching project, preferring
/// `preferred`; another scope on the same host is read-only.
pub(crate) async fn resolve_url(state: &AppState, value: &str, preferred: Option<&str>) -> Result<Resolved, IssueFailure> {
    let not_recognised = || IssueFailure::with_reason("resolveUrl", "not-found", "This is not a recognised issue URL.");
    let url = url::Url::parse(value.trim()).ok().filter(|u| matches!(u.scheme(), "https" | "http")).ok_or_else(not_recognised)?;
    let authority = match (url.host_str(), url.port()) {
        (Some(h), Some(p)) => format!("{}:{p}", h.to_lowercase()),
        (Some(h), None) => h.to_lowercase(),
        _ => return Err(not_recognised()),
    };
    // (tracker, host, scope, key, the Linear workspace)
    let mut parsed: Option<(&str, String, String, String, Option<String>)> =
        github::parse_issue_url(&url, false).map(|(host, scope, key)| (github::KIND, host, scope, key, None));
    if parsed.is_none() {
        parsed = gitlab::parse_issue_url(&url, false).map(|(host, scope, key)| (gitlab::KIND, host, scope, key, None));
    }
    if parsed.is_none() {
        parsed = linear::parse_issue_url(&url).map(|(workspace, scope, key)| (linear::KIND, linear::HOST.to_owned(), scope, key, Some(workspace)));
    }
    if parsed.is_none() {
        // A host whose name does not say it (GitHub Enterprise Server, a self-managed GitLab) is vouched for by a project on it.
        let hostname = url.host_str().map(str::to_lowercase);
        let ws = list_projects(state, ProjectFilter { host: hostname, authority: Some(authority.clone()), ..Default::default() }).await?;
        if ws.supported.iter().any(|p| p.kind == Kind::GitHub) {
            parsed = github::parse_issue_url(&url, true).map(|(host, scope, key)| (github::KIND, host, scope, key, None));
        }
        if parsed.is_none() && ws.supported.iter().any(|p| p.kind == Kind::GitLab) {
            parsed = gitlab::parse_issue_url(&url, true).map(|(host, scope, key)| (gitlab::KIND, host, scope, key, None));
        }
    }
    let (tracker, host, scope, key, workspace) = parsed.ok_or_else(not_recognised)?;
    let ws = list_projects(state, ProjectFilter { host: Some(host.clone()), authority: Some(authority.clone()), ..Default::default() }).await?;
    let on_host: Vec<&Served> = ws.supported.iter().filter(|p| p.tracker == tracker).collect();
    let on_authority: Vec<&Served> = on_host.iter().copied().filter(|p| p.host == authority).collect();
    let on_tracker = if on_authority.is_empty() { on_host } else { on_authority };
    let prefer_first = |projects: Vec<&Served>| -> Option<Served> {
        projects.iter().find(|p| Some(p.project_id.as_str()) == preferred).or(projects.first()).map(|p| (*p).clone())
    };
    let own = prefer_first(on_tracker.iter().copied().filter(|p| p.scope.eq_ignore_ascii_case(&scope)).collect());
    let writable = own.is_some();
    let lender = own.or_else(|| prefer_first(on_tracker.clone())).ok_or_else(IssueFailure::unsupported)?;
    let mut served = lender.clone();
    if !writable {
        served.scope = scope.clone();
    }
    let r = IssueRef { project_id: served.project_id.as_str().into(), tracker: served.tracker.clone(), host: Some(served.host.clone()), scope: served.scope.clone(), key: key.clone() };
    let Some(workspace) = workspace else { return Ok(Resolved { r, issue: None }) };
    // Team keys repeat across workspaces; only the credential's canonical issue URL identifies one.
    let mut reader = lender;
    reader.scope = scope;
    let issue = reader.get_issue(&key).await.map_err(to_issue_error("resolveUrl"))?;
    let canonical = url::Url::parse(&issue.summary.url).ok().and_then(|u| linear::parse_issue_url(&u)).map(|(w, _, _)| w);
    if canonical.as_deref() != Some(workspace.as_str()) {
        return Err(IssueFailure::with_reason("resolveUrl", "not-found", "This issue URL does not belong to the connected workspace."));
    }
    Ok(Resolved { r, issue: Some(served.into_detail(issue, writable)) })
}

/// `issues.linkedThreads` (`listLinkedIssueThreads`): the threads whose links
/// name the issue, most recently active first.
pub(crate) fn linked_threads(state: &AppState, key: &ThreadIssueKey) -> Result<IssueLinkedThreadsResult, IssueFailure> {
    let threads = state.store.app_threads().map_err(|_| IssueFailure::operation("linkedThreads", "Could not load linked threads."))?;
    Ok(linked_threads_of(threads, key))
}

fn linked_threads_of(threads: Vec<domovoi_model::typed::AppThread>, key: &ThreadIssueKey) -> IssueLinkedThreadsResult {
    let mut rows: Vec<_> = threads
        .into_iter()
        .filter(|t| t.deleted_at.is_none() && t.lineage.side_chat != Some(true))
        .filter(|t| t.issues.iter().flatten().any(|link| link.key.same_issue(key)))
        .collect();
    rows.sort_by(|a, b| b.updated_at.as_str().cmp(a.updated_at.as_str()).then_with(|| a.id.as_str().cmp(b.id.as_str())));
    IssueLinkedThreadsResult {
        threads: rows
            .into_iter()
            .map(|t| IssueLinkedThread {
                settled: t.settled_at.as_ref().and_then(Option::as_ref).is_some() || t.settled_override == Some(Some(SettledOverride::Settled)),
                id: t.id,
                project_id: t.project_id,
                title: t.title,
                archived_at: t.archived_at,
            })
            .collect(),
    }
}

/// `IssueListInput` within the bounds its schema checks: a payload outside
/// them is refused as it decodes, as t3code's decode refuses it.
#[derive(serde::Deserialize)]
#[serde(try_from = "IssueListInput")]
struct CheckedListInput(IssueListInput);

impl TryFrom<IssueListInput> for CheckedListInput {
    type Error = String;
    fn try_from(input: IssueListInput) -> Result<Self, String> {
        let blank = |s: &str| s.trim().is_empty();
        if input.limit.is_some_and(|l| !(1..=200).contains(&l)) {
            return Err("limit must be between 1 and 200".into());
        }
        if input.query.as_deref().is_some_and(|q| blank(q) || q.chars().count() > 200) {
            return Err("query must be 1 to 200 characters".into());
        }
        if input.labels.as_ref().is_some_and(|l| l.len() > 20 || l.iter().any(|n| blank(n) || n.chars().count() > 200)) {
            return Err("labels must be at most 20 names of 1 to 200 characters".into());
        }
        if input.assignee.as_deref().is_some_and(|a| blank(a) || a.chars().count() > 320) {
            return Err("assignee must be 1 to 320 characters".into());
        }
        if input.exclude_project_ids.as_ref().is_some_and(|ids| ids.len() > 1_000) {
            return Err("excludeProjectIds must name at most 1000 projects".into());
        }
        Ok(CheckedListInput(input))
    }
}

pub fn register(r: Router<ConnCtx>) -> Router<ConnCtx> {
    let r = r
        .unary("issues.list", |ctx: Arc<ConnCtx>, input: CheckedListInput| async move { list(&ctx.state, input.0).await })
        .unary("issues.detail", |ctx: Arc<ConnCtx>, input: IssueRef| async move { detail(&ctx.state, &input).await })
        .unary("issues.comments", |ctx: Arc<ConnCtx>, input: IssueCommentsPageInput| async move { comments(&ctx.state, &input).await })
        .unary("issues.templates", |ctx: Arc<ConnCtx>, input: IssueScopeRef| async move { templates(&ctx.state, &input).await })
        .unary("issues.candidates", |ctx: Arc<ConnCtx>, input: IssueScopeRef| async move { candidates(&ctx.state, &input).await })
        .unary("issues.create", |ctx: Arc<ConnCtx>, input: IssueCreateInput| async move { create(&ctx.state, &input).await })
        .unary("issues.comment", |ctx: Arc<ConnCtx>, input: IssueCommentInput| async move { comment(&ctx.state, &input).await })
        .unary("issues.update", |ctx: Arc<ConnCtx>, input: IssueUpdateInput| async move { update(&ctx.state, &input).await })
        .unary("issues.setState", |ctx: Arc<ConnCtx>, input: IssueSetStateInput| async move { set_state(&ctx.state, &input).await })
        .unary("issues.react", |ctx: Arc<ConnCtx>, input: IssueReactionInput| async move { react(&ctx.state, &input).await })
        .unary("issues.linkedThreads", |ctx: Arc<ConnCtx>, input: ThreadIssueKey| async move { linked_threads(&ctx.state, &input) });
    trackers::register(r)
}

#[cfg(test)]
mod tests {
    use super::*;
    use domovoi_model::typed::{AppThread, RepositoryIdentityLocator, RepositoryOrigin};
    use serde_json::json;

    fn identity(key: &str, display: &str, provider: &str) -> RepositoryIdentity {
        RepositoryIdentity {
            canonical_key: key.into(),
            locator: RepositoryIdentityLocator { source: "git-remote".into(), remote_name: "origin".into(), remote_url: format!("https://{key}.git") },
            web_url: None,
            root_path: None,
            display_name: Some(display.into()),
            provider: Some(provider.into()),
            owner: None,
            name: None,
            origin: None,
        }
    }

    #[test]
    fn a_project_reads_its_repository_and_the_fork_it_came_from() {
        let own = identity("github.com/me/calc", "Me/Calc", "github");
        assert_eq!(git_host_bindings(Some(&own)), vec![Binding { tracker: "github".into(), host: "github.com".into(), scope: "Me/Calc".into() }]);
        let mut forked = own.clone();
        forked.origin = Some(RepositoryOrigin { canonical_key: "github.com/acme/calc".into(), remote_url: None, display_name: Some("acme/calc".into()) });
        assert_eq!(git_host_bindings(Some(&forked)).iter().map(|b| b.scope.as_str()).collect::<Vec<_>>(), ["Me/Calc", "acme/calc"]);
        // An origin on another host, or the same repository, adds nothing.
        forked.origin = Some(RepositoryOrigin { canonical_key: "gitlab.com/acme/calc".into(), remote_url: None, display_name: None });
        assert_eq!(git_host_bindings(Some(&forked)).len(), 1);
        forked.origin = Some(RepositoryOrigin { canonical_key: "github.com/me/calc".into(), remote_url: None, display_name: Some("me/calc".into()) });
        assert_eq!(git_host_bindings(Some(&forked)).len(), 1);
        assert!(git_host_bindings(Some(&identity("example.com/a/b", "a/b", "unknown"))).is_empty());
        let self_hosted = git_host_bindings(Some(&identity("gitlab.example.com/g/sub/p", "g/sub/p", "unknown")));
        assert_eq!(self_hosted, vec![Binding { tracker: "gitlab".into(), host: "gitlab.example.com".into(), scope: "g/sub/p".into() }]);
        assert!(git_host_bindings(None).is_empty());
        assert_eq!(list_cursor_key("github.com", "Acme/Calc"), "github.com acme/calc");
    }

    #[test]
    fn tracker_failures_become_the_wire_errors() {
        let failure = |reason| serde_json::to_value(TrackerError::new("github", "x", reason, "  ").into_failure("detail")).unwrap();
        assert_eq!(failure(TrackerReason::MissingTool), json!({ "_tag": "IssueUnavailableError", "reason": "tool-missing", "tracker": "github" }));
        assert_eq!(failure(TrackerReason::Unauthenticated)["reason"], "unauthenticated");
        assert_eq!(
            failure(TrackerReason::NotFound),
            json!({ "_tag": "IssueOperationError", "operation": "detail", "detail": "The issue tracker request failed.", "reason": "not-found" })
        );
        let mut limited = TrackerError::new("linear", "x", TrackerReason::RateLimited, "slow");
        limited.retry_at = Some(9);
        assert_eq!(serde_json::to_value(limited.into_failure("list")).unwrap()["retryAt"], 9);
        assert_eq!(serde_json::to_value(IssueFailure::read_only("create")).unwrap()["reason"], "read-only");
    }

    #[test]
    fn linked_threads_are_most_recently_active_first() {
        let link = |key: &str| json!({ "tracker": "github", "host": "github.com", "scope": "acme/calc", "key": key, "projectId": "p",
            "url": "u", "source": "manual", "linkedAt": "2026-01-01T00:00:00Z", "displayKey": "#1", "title": "t" });
        let thread = |id: &str, updated: &str, links: serde_json::Value, extra: serde_json::Value| -> AppThread {
            let mut t = crate::test_records::with(crate::test_records::thread(id, "p"), json!({ "title": id, "updatedAt": updated, "issues": links }));
            if let (Some(t), Some(extra)) = (t.as_object_mut(), extra.as_object()) {
                t.extend(extra.clone());
            }
            serde_json::from_value(t).unwrap()
        };
        let threads = vec![
            thread("a", "2026-01-01T00:00:00Z", json!([link("1")]), json!({})),
            thread("b", "2026-03-01T00:00:00Z", json!([link("1")]), json!({ "settledOverride": "settled" })),
            thread("c", "2026-03-01T00:00:00Z", json!([link("1")]), json!({ "archivedAt": "2026-03-02T00:00:00Z" })),
            thread("d", "2026-04-01T00:00:00Z", json!([link("2")]), json!({})),
            thread("e", "2026-05-01T00:00:00Z", json!([link("1")]), json!({ "deletedAt": "2026-05-02T00:00:00Z" })),
        ];
        let key = ThreadIssueKey { tracker: "github".into(), host: "GitHub.com".into(), scope: "Acme/Calc".into(), key: " 1".into() };
        let got = linked_threads_of(threads, &key);
        assert_eq!(got.threads.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), ["b", "c", "a"]);
        assert!(got.threads[0].settled && !got.threads[1].settled && got.threads[1].archived_at.is_some());
    }
}
