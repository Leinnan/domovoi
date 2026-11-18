//! GitHub's pull requests as the records a client reads. Each function asks
//! `gh` one question (see `crate::gh`), reads GitHub's JSON with a description
//! of it that stays in this file, and hands out a record of t3code's
//! `pullRequest.ts` shapes; the mappings follow t3code's GitHub provider.
//!
//! GitHub's answers are read leniently (`gh::lenient`): a field it leaves
//! out, or sends as something else than expected, is a field without a value,
//! which the mappings give the empty value of its kind.

use super::pull_request_failure::PROVIDER;
use super::pull_request_records::{
    BaseComparison, Capabilities, ChangeBreakdown, Comment, CommentKind, Commit, DiffFileContents, DiffSide, EditCapabilities, FileViewed, FileViewedState,
    FilesViewed, LineCount, MergeCapabilities, PullRequestActivity, PullRequestDetail, PullRequestPreview, Reaction, ReactionContent, ReferenceCandidate,
    ReferenceKind, ReferenceSigil, ReviewCapabilities, ReviewThread, ReviewVerdict, ReviewerCapabilities, ThreadComment, ThreadCommentsPage, UpdateMethod,
    ViewedFilesStore, ViewerPermissions,
};
use super::pull_requests::{ReviewCommentDraft, ReviewPosition, Supported};
use crate::gh::lenient::{Fields, Loose, Rows, Scalar, present, rows, trimmed};
use crate::gh::{self, GhError, GhReason, Var};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use domovoi_model::typed::{
    PullRequestAction, PullRequestActor, PullRequestCheck, PullRequestCheckStatus, PullRequestChecks, PullRequestChecksState, PullRequestLabel,
    PullRequestListEntry, PullRequestMergeMethod, PullRequestMergeability, PullRequestReviewDecision, PullRequestState, PullRequestSummary,
};

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn text(value: &Loose<String>) -> String {
    value.as_deref().unwrap_or("").to_owned()
}

fn owner_name(repository: &str) -> (&str, &str) {
    repository.split_once('/').unwrap_or(("", repository))
}

// ---------------------------------------------------------------------------
// What `gh pr view` and `gh pr list` print
// ---------------------------------------------------------------------------

/// A user, bot or team as GitHub names one: `gh`'s `author` (`is_bot`),
/// GraphQL's `Actor` (`__typename`) and a requested reviewer (a team has a `slug`).
#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct GhActor {
    login: Loose<String>,
    name: Loose<String>,
    avatar_url: Loose<String>,
    #[serde(rename = "__typename")]
    typename: Loose<String>,
    #[serde(rename = "is_bot")]
    is_bot: Loose<bool>,
    slug: Loose<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct GhLabel {
    name: Loose<String>,
    color: Loose<String>,
    description: Loose<String>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct GhReview {
    author: Fields<GhActor>,
    state: Loose<String>,
    is_minimized: Loose<bool>,
}

/// An entry of `statusCheckRollup`: a check run (`name`, `status`,
/// `conclusion`) or a commit status (`context`, `state`).
#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct GhCheck {
    name: Loose<String>,
    context: Loose<String>,
    status: Loose<String>,
    conclusion: Loose<String>,
    state: Loose<String>,
    description: Loose<String>,
    details_url: Loose<String>,
    target_url: Loose<String>,
    is_required: Loose<bool>,
    completed_at: Loose<String>,
    started_at: Loose<String>,
    workflow_name: Loose<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct GhNamed {
    name: Loose<String>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct GhAutoMerge {
    merge_method: Loose<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct GhFile {
    path: Loose<String>,
    additions: Loose<u64>,
    deletions: Loose<u64>,
}

/// A pull request as `gh pr view --json` and `gh pr list --json` print one:
/// only the fields asked for are there. Where a summary tells a field that
/// was not asked for from one without a value, the field is `None` for the
/// first and `Some` for the second.
#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct GhPullRequest {
    id: Loose<String>,
    number: Loose<u64>,
    title: Loose<String>,
    body: Loose<String>,
    url: Loose<String>,
    state: Loose<String>,
    is_draft: Loose<bool>,
    #[serde(deserialize_with = "present")]
    author: Option<Fields<GhActor>>,
    head_ref_name: Loose<String>,
    head_ref_oid: Loose<String>,
    base_ref_name: Loose<String>,
    base_ref_oid: Loose<String>,
    head_repository: Fields<GhNamed>,
    head_repository_owner: Fields<GhActor>,
    is_cross_repository: Loose<bool>,
    created_at: Loose<String>,
    updated_at: Loose<String>,
    #[serde(deserialize_with = "present")]
    closed_at: Option<Loose<String>>,
    #[serde(deserialize_with = "present")]
    merged_at: Option<Loose<String>>,
    additions: Loose<u64>,
    deletions: Loose<u64>,
    changed_files: Loose<u64>,
    #[serde(deserialize_with = "present")]
    mergeable: Option<Loose<String>>,
    merge_state_status: Loose<String>,
    #[serde(deserialize_with = "present")]
    review_decision: Option<Loose<String>>,
    labels: Rows<GhLabel>,
    review_requests: Rows<GhActor>,
    latest_reviews: Rows<GhReview>,
    #[serde(deserialize_with = "present")]
    status_check_rollup: Option<Rows<GhCheck>>,
    maintainer_can_modify: Loose<bool>,
    /// `Some(None)` is GitHub's `null`: auto-merge is off.
    #[serde(deserialize_with = "present")]
    auto_merge_request: Option<Option<Fields<GhAutoMerge>>>,
    files: Rows<GhFile>,
}

impl GhPullRequest {
    fn state(&self) -> PullRequestState {
        gh::state(self.state.as_deref(), self.merged_at.as_ref().and_then(|at| at.as_deref()))
    }

    fn checks(&self) -> &Rows<GhCheck> {
        static NONE: Rows<GhCheck> = Loose(None);
        self.status_check_rollup.as_ref().unwrap_or(&NONE)
    }

    fn review_decision(&self) -> Option<PullRequestReviewDecision> {
        review_decision(self.review_decision.as_ref().and_then(|d| d.as_deref()), &self.latest_reviews)
    }

    fn mergeability(&self) -> PullRequestMergeability {
        mergeability(self.mergeable.as_ref().and_then(|m| m.as_deref()))
    }

    fn author(&self, host: &str) -> Option<PullRequestActor> {
        self.author.as_ref().and_then(|a| actor(a, host))
    }

    fn auto_merge(&self) -> Option<&GhAutoMerge> {
        self.auto_merge_request.as_ref().and_then(Option::as_ref).map(|auto| &auto.0)
    }
}

async fn pr_view(p: &Supported, number: u64, fields: &str) -> Result<GhPullRequest, GhError> {
    let row: Fields<GhPullRequest> = gh::run_json(p.cwd(), &p.host, &["pr", "view", &number.to_string(), "--repo", &p.repository, "--json", fields]).await?;
    Ok(row.0)
}

/// The `data` of a GraphQL answer about a repository.
#[derive(Deserialize)]
#[serde(default, bound(deserialize = "T: Deserialize<'de> + Default"))]
struct RepositoryData<T> {
    repository: Fields<T>,
}

impl<T: Default> Default for RepositoryData<T> {
    fn default() -> Self {
        RepositoryData { repository: Fields(T::default()) }
    }
}

/// The `repository` of a GraphQL request with `$owner`, `$name` and, for a pull request, `$number`.
async fn repository_query<T: DeserializeOwned + Default>(p: &Supported, query: &str, more: &[(&str, Var<'_>)]) -> Result<T, GhError> {
    let (owner, name) = owner_name(&p.repository);
    let mut vars = vec![("owner", Var::Text(owner)), ("name", Var::Text(name))];
    vars.extend_from_slice(more);
    let data: RepositoryData<T> = gh::graphql(p.cwd(), &p.host, query, &vars).await?;
    Ok(data.repository.0)
}

// ---------------------------------------------------------------------------
// Mappings (GitHub's values → t3code's)
// ---------------------------------------------------------------------------

/// t3code's `loginAvatarUrl`: app logins have no profile page.
fn login_avatar_url(login: &str, host: &str) -> Option<String> {
    let ok = !login.is_empty()
        && login.len() <= 39
        && login.chars().next().is_some_and(|c| c.is_ascii_alphanumeric())
        && login.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    ok.then(|| format!("https://{host}/{login}.png?size=80"))
}

/// `PullRequestActor` (none without a login).
fn actor(raw: &GhActor, host: &str) -> Option<PullRequestActor> {
    let login = trimmed(&raw.login)?;
    let avatar_url = trimmed(&raw.avatar_url).or_else(|| login_avatar_url(&login, host));
    let is_bot = (raw.typename.as_deref() == Some("Bot") || raw.is_bot.0 == Some(true)).then_some(true);
    Some(PullRequestActor { is_bot, login, name: trimmed(&raw.name), avatar_url })
}

fn mergeability(raw: Option<&str>) -> PullRequestMergeability {
    match raw.unwrap_or("").trim().to_uppercase().as_str() {
        "MERGEABLE" => PullRequestMergeability::Mergeable,
        "CONFLICTING" => PullRequestMergeability::Conflicting,
        _ => PullRequestMergeability::Unknown,
    }
}

fn merge_method(raw: &Loose<String>) -> Option<PullRequestMergeMethod> {
    match raw.as_deref().unwrap_or("").trim().to_uppercase().as_str() {
        "MERGE" => Some(PullRequestMergeMethod::Merge),
        "SQUASH" => Some(PullRequestMergeMethod::Squash),
        "REBASE" => Some(PullRequestMergeMethod::Rebase),
        _ => None,
    }
}

fn review_decision_raw(raw: Option<&str>) -> Option<PullRequestReviewDecision> {
    match raw.unwrap_or("").trim().to_uppercase().as_str() {
        "APPROVED" => Some(PullRequestReviewDecision::Approved),
        "CHANGES_REQUESTED" => Some(PullRequestReviewDecision::ChangesRequested),
        "REVIEW_REQUIRED" => Some(PullRequestReviewDecision::ReviewRequired),
        _ => None,
    }
}

/// GitHub's `reviewDecision`, falling back to the latest reviews (bot approvals).
fn review_decision(raw: Option<&str>, latest_reviews: &Rows<GhReview>) -> Option<PullRequestReviewDecision> {
    let summarized = review_decision_raw(raw);
    if matches!(summarized, Some(PullRequestReviewDecision::Approved | PullRequestReviewDecision::ChangesRequested)) {
        return summarized;
    }
    let states: Vec<String> =
        rows(latest_reviews).filter(|r| r.is_minimized.0 != Some(true)).map(|r| r.state.as_deref().unwrap_or("").trim().to_uppercase()).collect();
    if states.iter().any(|s| s == "CHANGES_REQUESTED") {
        return Some(PullRequestReviewDecision::ChangesRequested);
    }
    if summarized.is_none() && states.iter().any(|s| s == "APPROVED") {
        return Some(PullRequestReviewDecision::Approved);
    }
    summarized
}

/// The labels that have a name.
fn labels(raw: &Rows<GhLabel>) -> Vec<PullRequestLabel> {
    rows(raw)
        .filter_map(|l| Some(PullRequestLabel { name: trimmed(&l.name)?, color: trimmed(&l.color), description: Some(trimmed(&l.description)) }))
        .collect()
}

/// The logins asked for a review (teams have none).
fn review_request_logins(raw: &Rows<GhActor>) -> Vec<String> {
    rows(raw).filter(|r| r.typename.as_deref() != Some("Team")).filter_map(|r| trimmed(&r.login)).collect()
}

fn check_status(raw: &GhCheck) -> PullRequestCheckStatus {
    let status = raw.status.as_deref().unwrap_or("").trim().to_uppercase();
    if !status.is_empty() && status != "COMPLETED" {
        return PullRequestCheckStatus::Pending;
    }
    let outcome = raw.conclusion.as_deref().filter(|s| !s.is_empty()).or(raw.state.as_deref()).unwrap_or("");
    match outcome.trim().to_uppercase().as_str() {
        "SUCCESS" => PullRequestCheckStatus::Success,
        "ACTION_REQUIRED" => PullRequestCheckStatus::ActionRequired,
        "FAILURE" | "ERROR" | "TIMED_OUT" | "STARTUP_FAILURE" => PullRequestCheckStatus::Failure,
        "CANCELLED" => PullRequestCheckStatus::Cancelled,
        "SKIPPED" => PullRequestCheckStatus::Skipped,
        "PENDING" | "EXPECTED" => PullRequestCheckStatus::Pending,
        _ => PullRequestCheckStatus::Neutral,
    }
}

const UNSET_TIMESTAMP: &str = "0001-01-01T00:00:00Z";
const STILL_RUNNING: &str = "9999-12-31T23:59:59Z";

fn real_timestamp(v: &Loose<String>) -> Option<String> {
    trimmed(v).filter(|s| s != UNSET_TIMESTAMP)
}

/// `statusCheckRollup` → deduplicated `PullRequestCheck`s; reruns keep the newest run.
fn checks(raw: &Rows<GhCheck>) -> Vec<PullRequestCheck> {
    let mut kept: Vec<(String, PullRequestCheck, Option<String>)> = vec![];
    for c in rows(raw) {
        let Some(name) = trimmed(&c.name).or_else(|| trimmed(&c.context)) else { continue };
        let status = check_status(c);
        let at = if status == PullRequestCheckStatus::Pending {
            Some(STILL_RUNNING.to_owned())
        } else {
            real_timestamp(&c.completed_at).or_else(|| real_timestamp(&c.started_at))
        };
        let key = format!("{}\n{}", trimmed(&c.workflow_name).unwrap_or_default(), name);
        let check = PullRequestCheck {
            name,
            status,
            description: trimmed(&c.description),
            url: trimmed(&c.details_url).or_else(|| trimmed(&c.target_url)),
            required: c.is_required.0,
            placeholder: None,
        };
        match kept.iter_mut().find(|(k, _, _)| *k == key) {
            Some(slot) => {
                let newer = match (&at, &slot.2) {
                    (None, kept_at) => kept_at.is_none(),
                    (Some(_), None) => true,
                    (Some(a), Some(b)) => a >= b,
                };
                if newer {
                    slot.1 = check;
                    slot.2 = at;
                }
            }
            None => kept.push((key, check, at)),
        }
    }
    kept.into_iter().map(|(_, c, _)| c).collect()
}

/// `PullRequestChecksState` of a rollup.
fn checks_state(raw: &Rows<GhCheck>) -> Option<PullRequestChecksState> {
    let mut statuses: Vec<PullRequestCheckStatus> = checks(raw).into_iter().map(|c| c.status).collect();
    for c in rows(raw) {
        if trimmed(&c.name).is_none() && trimmed(&c.context).is_none() {
            statuses.push(check_status(c));
        }
    }
    let has = |s: PullRequestCheckStatus| statuses.contains(&s);
    if has(PullRequestCheckStatus::Failure) || has(PullRequestCheckStatus::Cancelled) {
        Some(PullRequestChecksState::Failing)
    } else if has(PullRequestCheckStatus::Pending) || has(PullRequestCheckStatus::ActionRequired) {
        Some(PullRequestChecksState::Pending)
    } else if has(PullRequestCheckStatus::Success) {
        Some(PullRequestChecksState::Passing)
    } else {
        None
    }
}

// ---------------------------------------------------------------------------
// The list
// ---------------------------------------------------------------------------

/// Fields asked of `gh pr list`.
const LIST_FIELDS: &str = "number,title,url,author,headRefName,baseRefName,state,isDraft,mergeable,additions,deletions,createdAt,updatedAt,mergedAt,labels,reviewDecision,reviewRequests,latestReviews,statusCheckRollup";

/// A row of a repository's listing, with what the filters and the viewer's
/// part in it are decided from.
pub struct ListRow {
    /// The row as listed; whether the viewer is asked for a review is the caller's to say.
    pub entry: PullRequestListEntry,
    pub author_login: Option<String>,
    pub review_request_logins: Vec<String>,
}

fn list_row(p: &Supported, raw: &GhPullRequest, observed: u64) -> ListRow {
    let author = raw.author(&p.host);
    ListRow {
        author_login: author.as_ref().map(|a| a.login.clone()),
        review_request_logins: review_request_logins(&raw.review_requests),
        entry: PullRequestListEntry {
            stack: None,
            provider: PROVIDER.to_owned(),
            host: p.host.clone(),
            project_id: p.project_id.as_str().into(),
            project_title: p.title.clone(),
            repository: p.repository.clone(),
            number: raw.number.unwrap_or(0),
            title: text(&raw.title),
            url: text(&raw.url),
            author,
            head_branch: text(&raw.head_ref_name),
            base_branch: text(&raw.base_ref_name),
            state: raw.state(),
            is_draft: raw.is_draft.unwrap_or(false),
            mergeability: raw.mergeability(),
            additions: raw.additions.unwrap_or(0),
            deletions: raw.deletions.unwrap_or(0),
            created_at: text(&raw.created_at).into(),
            updated_at: text(&raw.updated_at).into(),
            observed_at: Some(observed.into()),
            viewer_review_requested: false,
            labels: labels(&raw.labels),
            review_decision: raw.review_decision(),
            checks_state: checks_state(raw.checks()),
        },
    }
}

/// Which pull requests of a repository `gh pr list` is asked for.
pub struct Listing<'a> {
    /// `open`, `closed`, `merged` or `all`, passed on as given.
    pub state: &'a str,
    /// `authored` and `reviewing` narrow the listing to the signed-in account's.
    pub involvement: Option<&'a str>,
    pub query: Option<&'a str>,
    /// How many rows to ask for.
    pub fetch: usize,
}

/// One page of a repository's pull requests, read at `observed`.
pub async fn list(p: &Supported, listing: &Listing<'_>, observed: u64) -> Result<Vec<ListRow>, GhError> {
    let fetch = listing.fetch.to_string();
    let mut args: Vec<&str> = vec!["pr", "list", "--repo", &p.repository, "--state", listing.state, "--limit", &fetch, "--json", LIST_FIELDS];
    let mut search: Vec<&str> = vec![];
    if let Some(q) = listing.query.map(str::trim).filter(|q| !q.is_empty()) {
        search.push(q);
    }
    match listing.involvement {
        Some("authored") => args.extend(["--author", "@me"]),
        Some("reviewing") => search.push("review-requested:@me"),
        _ => {}
    }
    let search = search.join(" ");
    if !search.is_empty() {
        args.extend(["--search", &search]);
    }
    let raw: Rows<GhPullRequest> = gh::run_json(p.cwd(), &p.host, &args).await?;
    Ok(rows(&raw).map(|row| list_row(p, row, observed)).collect())
}

/// Lines added and removed by one pull request.
pub async fn line_counts(p: &Supported, number: u64) -> Result<(u64, u64), GhError> {
    let raw = pr_view(p, number, "additions,deletions").await?;
    Ok((raw.additions.unwrap_or(0), raw.deletions.unwrap_or(0)))
}

/// Up to `limit` of a repository's labels, those matching `search` when there is one.
pub async fn repository_labels(p: &Supported, limit: &str, search: &str) -> Result<Vec<PullRequestLabel>, GhError> {
    let mut args = vec!["label", "list", "--repo", &p.repository, "--limit", limit, "--json", "name,color,description"];
    if !search.is_empty() {
        args.extend(["--search", search]);
    }
    let raw: Rows<GhLabel> = gh::run_json(p.cwd(), &p.host, &args).await?;
    Ok(labels(&raw))
}

/// A row of `gh issue list` or `gh pr list`, as far as a reference needs it.
#[derive(Deserialize, Default)]
#[serde(default)]
struct GhReference {
    number: Loose<u64>,
    title: Loose<String>,
    state: Loose<String>,
}

/// The issues (`issue`) or pull requests (`pr`) of a repository a composer can reference.
pub async fn references(p: &Supported, command: &str, kind: ReferenceKind, search: &str) -> Result<Vec<ReferenceCandidate>, GhError> {
    let mut args = vec![command, "list", "--repo", &p.repository, "--state", "all", "--limit", "20", "--json", "number,title,state"];
    if !search.is_empty() {
        args.extend(["--search", search]);
    }
    let raw: Rows<GhReference> = gh::run_json(p.cwd(), &p.host, &args).await?;
    Ok(rows(&raw)
        .filter_map(|row| {
            let number = row.number.0?;
            Some(ReferenceCandidate { reference: format!("#{number}"), kind, number, title: text(&row.title), state: gh::state(row.state.as_deref(), None) })
        })
        .collect())
}

const ASSIGNABLE_QUERY: &str = "query($owner: String!, $name: String!, $q: String!) { repository(owner: $owner, name: $name) { assignableUsers(first: 100, query: $q) { pageInfo { hasNextPage } nodes { login name avatarUrl } } } }";

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct GhPageInfo {
    has_next_page: Loose<bool>,
    end_cursor: Loose<String>,
}

/// A GraphQL connection: a page of nodes, with how many there are in all where that was asked.
#[derive(Deserialize)]
#[serde(default, rename_all = "camelCase", bound(deserialize = "T: Deserialize<'de> + Default"))]
struct GhConnection<T> {
    total_count: Loose<u64>,
    page_info: Fields<GhPageInfo>,
    nodes: Rows<T>,
}

impl<T> Default for GhConnection<T> {
    fn default() -> Self {
        GhConnection { total_count: Loose(None), page_info: Fields(GhPageInfo::default()), nodes: Loose(None) }
    }
}

impl<T> GhConnection<T> {
    fn has_more(&self) -> bool {
        self.page_info.has_next_page.0 == Some(true)
    }

    /// Where the next page starts, when there is one.
    fn next_cursor(&self) -> Option<String> {
        if self.has_more() { trimmed(&self.page_info.end_cursor) } else { None }
    }
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct GhAssignable {
    assignable_users: Fields<GhConnection<GhActor>>,
}

/// The users of a repository matching `query` (GitHub's first hundred), and whether there are more.
pub async fn assignable_users(p: &Supported, query: &str) -> Result<(Vec<PullRequestActor>, bool), GhError> {
    let repository: GhAssignable = repository_query(p, ASSIGNABLE_QUERY, &[("q", Var::Text(query))]).await?;
    let users = &repository.assignable_users;
    Ok((rows(&users.nodes).filter_map(|u| actor(u, &p.host)).collect(), users.has_more()))
}

// ---------------------------------------------------------------------------
// One pull request
// ---------------------------------------------------------------------------

const SUMMARY_FIELDS: &str = "number,title,url,state,isDraft,author,headRefName,baseRefName,headRefOid,closedAt,mergedAt,updatedAt,additions,deletions,changedFiles,reviewDecision,latestReviews,statusCheckRollup,mergeable,autoMergeRequest";

/// `PullRequestSummary` of what `gh` printed. In the reply to an action
/// (`reply`), the times of closing and merging are sent only if `gh` printed them.
fn to_summary(p: &Supported, raw: &GhPullRequest, reply: bool) -> PullRequestSummary {
    let time = |at: &Option<Loose<String>>| if reply && at.is_none() { None } else { Some(at.as_ref().and_then(trimmed)) };
    PullRequestSummary {
        provider: PROVIDER.to_owned(),
        project_id: p.project_id.as_str().into(),
        repository: p.repository.clone(),
        number: raw.number.unwrap_or(0),
        title: text(&raw.title),
        url: text(&raw.url),
        state: raw.state(),
        is_draft: raw.is_draft.0,
        head_branch: text(&raw.head_ref_name),
        base_branch: text(&raw.base_ref_name),
        closed_at: time(&raw.closed_at),
        merged_at: time(&raw.merged_at),
        updated_at: text(&raw.updated_at).into(),
        observed_at: Some(now_ms().into()),
        author: raw.author.as_ref().map(|a| actor(a, &p.host)),
        additions: raw.additions.0,
        deletions: raw.deletions.0,
        changed_files: raw.changed_files.0,
        review_decision: raw.review_decision.as_ref().map(|_| raw.review_decision()),
        checks_state: raw.status_check_rollup.as_ref().map(checks_state),
        mergeability: raw.mergeable.as_ref().map(|_| raw.mergeability()),
        stack: None,
        head_sha: trimmed(&raw.head_ref_oid),
        auto_merge_enabled: raw.auto_merge_request.as_ref().map(Option::is_some),
        auto_merge_method: raw.auto_merge().and_then(|auto| merge_method(&auto.merge_method)),
    }
}

/// A pull request's state, without its text.
pub async fn summary(p: &Supported, number: u64, reply: bool) -> Result<PullRequestSummary, GhError> {
    Ok(to_summary(p, &pr_view(p, number, SUMMARY_FIELDS).await?, reply))
}

/// Repository access, from GraphQL `repository { REPO_ACCESS_FIELDS }`.
#[derive(Debug, Clone, Default)]
struct RepoAccess {
    can_write: bool,
    can_triage: bool,
    auto_merge_allowed: bool,
    merge_commit: bool,
    squash: bool,
    rebase: bool,
}

const REPO_ACCESS_FIELDS: &str = "viewerPermission autoMergeAllowed mergeCommitAllowed squashMergeAllowed rebaseMergeAllowed";

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct GhRepositoryAccess {
    viewer_permission: Loose<String>,
    auto_merge_allowed: Loose<bool>,
    merge_commit_allowed: Loose<bool>,
    squash_merge_allowed: Loose<bool>,
    rebase_merge_allowed: Loose<bool>,
}

fn to_access(raw: &GhRepositoryAccess) -> RepoAccess {
    let perm = raw.viewer_permission.as_deref().unwrap_or("").to_uppercase();
    let can_write = matches!(perm.as_str(), "ADMIN" | "MAINTAIN" | "WRITE");
    RepoAccess {
        can_write,
        can_triage: can_write || perm == "TRIAGE",
        auto_merge_allowed: raw.auto_merge_allowed.unwrap_or(false),
        merge_commit: raw.merge_commit_allowed.unwrap_or(true),
        squash: raw.squash_merge_allowed.unwrap_or(true),
        rebase: raw.rebase_merge_allowed.unwrap_or(true),
    }
}

async fn repo_access(p: &Supported) -> Result<RepoAccess, GhError> {
    let query = format!("query($owner: String!, $name: String!) {{ repository(owner: $owner, name: $name) {{ {REPO_ACCESS_FIELDS} }} }}");
    Ok(to_access(&repository_query::<GhRepositoryAccess>(p, &query, &[]).await?))
}

/// Everything GitHub lets anyone do to a pull request.
const ALL_ACTIONS: [PullRequestAction; 11] = [
    PullRequestAction::Merge,
    PullRequestAction::Ready,
    PullRequestAction::Draft,
    PullRequestAction::Close,
    PullRequestAction::Reopen,
    PullRequestAction::DeleteBranch,
    PullRequestAction::UpdateBranch,
    PullRequestAction::EnableAutoMerge,
    PullRequestAction::DisableAutoMerge,
    PullRequestAction::Revert,
    PullRequestAction::ApproveWorkflows,
];

/// The actions GitHub has (`PullRequestProviderSummary.actions`).
pub fn all_actions() -> Vec<PullRequestAction> {
    ALL_ACTIONS.to_vec()
}

const ALL_VERDICTS: [ReviewVerdict; 3] = [ReviewVerdict::Comment, ReviewVerdict::Approve, ReviewVerdict::RequestChanges];
const UPDATE_METHODS: [UpdateMethod; 2] = [UpdateMethod::Merge, UpdateMethod::Rebase];

/// t3code's GitHub `CAPABILITIES`.
fn capabilities() -> Capabilities {
    Capabilities {
        diff: true,
        comment: true,
        actions: all_actions(),
        merge_methods: [PullRequestMergeMethod::Merge, PullRequestMergeMethod::Squash, PullRequestMergeMethod::Rebase],
        delete_branch: true,
        update_methods: UPDATE_METHODS,
        search: true,
        reactions: true,
        mentions: true,
        reference_sigils: [ReferenceSigil::Hash],
        viewed_files: ViewedFilesStore::Host,
        review: ReviewCapabilities { inline_comment: true, range_comment: true, reply: true, resolve: true, verdicts: ALL_VERDICTS },
        reviewers: ReviewerCapabilities { request: true, list_candidates: true, search_candidates: true },
        edit: EditCapabilities { change_request: true, comment: true, maintainer_edits: true },
        stacks: true,
        stack_actions: true,
        labels: true,
        checks_always_report: true,
    }
}

/// Per-pull-request facts that decide the viewer's permissions.
struct ViewerFacts {
    did_author: bool,
    state: PullRequestState,
    is_cross_repository: bool,
    merge_state_status: String,
}

/// t3code's `gitHubViewerPermissions`, from what `gh` reports.
fn to_viewer_permissions(access: &RepoAccess, facts: &ViewerFacts) -> ViewerPermissions {
    let can_update = access.can_write || facts.did_author;
    let open = facts.state == PullRequestState::Open;
    let can_delete_branch = access.can_write && !facts.is_cross_repository && !open;
    let can_update_branch = open && can_update && facts.merge_state_status == "BEHIND";
    let can_merge_now = matches!(facts.merge_state_status.as_str(), "CLEAN" | "HAS_HOOKS" | "UNSTABLE");
    let mut actions: Vec<PullRequestAction> = vec![];
    if can_delete_branch {
        actions.push(PullRequestAction::DeleteBranch);
    }
    if access.can_write {
        actions.push(PullRequestAction::Merge);
        if access.auto_merge_allowed && !can_merge_now {
            actions.push(PullRequestAction::EnableAutoMerge);
        }
        actions.extend([PullRequestAction::DisableAutoMerge, PullRequestAction::Revert, PullRequestAction::ApproveWorkflows]);
    }
    if can_update {
        actions.extend([PullRequestAction::Ready, PullRequestAction::Draft, PullRequestAction::Close, PullRequestAction::Reopen]);
    }
    if can_update_branch {
        actions.push(PullRequestAction::UpdateBranch);
    }
    ViewerPermissions {
        actions,
        comment: true,
        resolve: access.can_write || facts.did_author,
        verdicts: if facts.did_author { vec![ReviewVerdict::Comment] } else { ALL_VERDICTS.to_vec() },
        request_reviewers: access.can_write,
        labels: access.can_triage,
        stack_rebase: access.can_write.then_some(true),
        update_methods: can_update_branch.then_some(UPDATE_METHODS),
    }
}

/// t3code's `getViewerPermissions` for one pull request.
pub async fn viewer_permissions(p: &Supported, number: u64) -> Result<ViewerPermissions, GhError> {
    let (raw, access, identity) =
        tokio::join!(pr_view(p, number, "author,state,mergedAt,isCrossRepository,mergeStateStatus"), repo_access(p), gh::identity(p.cwd(), &p.host));
    let (raw, access) = (raw?, access?);
    let viewer = identity.ok().map(|(_, login)| login);
    let author = raw.author.as_ref().and_then(|a| a.login.as_deref()).unwrap_or("");
    let facts = ViewerFacts {
        did_author: viewer.as_deref().is_some_and(|v| v.eq_ignore_ascii_case(author)),
        state: raw.state(),
        is_cross_repository: raw.is_cross_repository.unwrap_or(false),
        merge_state_status: raw.merge_state_status.as_deref().unwrap_or("").to_uppercase(),
    };
    Ok(to_viewer_permissions(&access, &facts))
}

const DETAIL_FIELDS: &str = "number,title,body,url,state,isDraft,author,headRefName,headRefOid,headRepository,headRepositoryOwner,isCrossRepository,baseRefName,createdAt,updatedAt,closedAt,mergedAt,additions,deletions,changedFiles,mergeable,mergeStateStatus,reviewDecision,labels,reviewRequests,latestReviews,statusCheckRollup,maintainerCanModify,autoMergeRequest";

/// `PullRequestDetail` from `gh pr view --json DETAIL_FIELDS`, the repository's access and who is signed in.
fn to_detail(p: &Supported, raw: &GhPullRequest, access: &RepoAccess, viewer: Option<&str>) -> PullRequestDetail {
    let host = p.host.as_str();
    let author = raw.author(host);
    let state = raw.state();
    let merge_state = raw.merge_state_status.as_deref().unwrap_or("").to_uppercase();
    let facts = ViewerFacts {
        did_author: viewer.is_some_and(|v| author.as_ref().is_some_and(|a| a.login.eq_ignore_ascii_case(v))),
        state: state.clone(),
        is_cross_repository: raw.is_cross_repository.unwrap_or(false),
        merge_state_status: merge_state.clone(),
    };
    // Who reviewed, then who is asked to, each once.
    let mut reviewers: Vec<PullRequestActor> = vec![];
    let mut seen = HashSet::new();
    let reviewed = rows(&raw.latest_reviews).map(|r| actor(&r.author, host));
    let requested = rows(&raw.review_requests).map(|r| {
        if r.typename.as_deref() == Some("Team") {
            trimmed(&r.slug).map(|slug| PullRequestActor { is_bot: None, login: slug, name: trimmed(&r.name), avatar_url: None })
        } else {
            actor(r, host)
        }
    });
    for reviewer in reviewed.chain(requested).flatten() {
        if seen.insert(reviewer.login.to_lowercase()) {
            reviewers.push(reviewer);
        }
    }
    let head_repository = match (trimmed(&raw.head_repository_owner.login), trimmed(&raw.head_repository.name)) {
        (Some(owner), Some(name)) => Some(format!("{owner}/{name}")),
        _ => None,
    };
    let base_comparison = match merge_state.as_str() {
        "BEHIND" => BaseComparison::Behind,
        "CLEAN" | "HAS_HOOKS" | "UNSTABLE" => BaseComparison::UpToDate,
        _ => BaseComparison::Unknown,
    };
    PullRequestDetail {
        provider: PROVIDER,
        capabilities: capabilities(),
        viewer_permissions: to_viewer_permissions(access, &facts),
        project_id: p.project_id.as_str().into(),
        project_title: p.title.clone(),
        workspace_root: p.workspace_root.clone(),
        repository: p.repository.clone(),
        number: raw.number.unwrap_or(0),
        title: text(&raw.title),
        body: text(&raw.body),
        url: text(&raw.url),
        author,
        state,
        is_draft: raw.is_draft.unwrap_or(false),
        mergeability: raw.mergeability(),
        additions: raw.additions.unwrap_or(0),
        deletions: raw.deletions.unwrap_or(0),
        changed_files: raw.changed_files.unwrap_or(0),
        head_branch: text(&raw.head_ref_name),
        head_repository_name_with_owner: head_repository,
        head_sha: trimmed(&raw.head_ref_oid),
        base_branch: text(&raw.base_ref_name),
        created_at: text(&raw.created_at).into(),
        updated_at: text(&raw.updated_at).into(),
        observed_at: now_ms(),
        merged_at: raw.merged_at.as_ref().and_then(trimmed).map(Into::into),
        closed_at: raw.closed_at.as_ref().and_then(trimmed).map(Into::into),
        reviewers,
        labels: labels(&raw.labels),
        checks: checks(raw.checks()),
        merge_capabilities: MergeCapabilities { merge: access.merge_commit, squash: access.squash, rebase: access.rebase },
        viewer: viewer.filter(|v| !v.trim().is_empty()).map(str::to_owned),
        base_comparison,
        auto_merge_enabled: raw.auto_merge().is_some(),
        auto_merge_method: raw.auto_merge().and_then(|auto| merge_method(&auto.merge_method)),
        maintainer_can_modify: raw.maintainer_can_modify.0,
    }
}

/// A pull request with its text, checks and what the viewer may do. Who the
/// viewer is may stay unknown: the detail is then that of a stranger.
pub async fn detail(p: &Supported, number: u64) -> Result<PullRequestDetail, GhError> {
    let (raw, access, identity) = tokio::join!(pr_view(p, number, DETAIL_FIELDS), repo_access(p), gh::identity(p.cwd(), &p.host));
    let (raw, access) = (raw?, access?);
    let viewer = identity.ok().map(|(_, login)| login);
    Ok(to_detail(p, &raw, &access, viewer.as_deref()))
}

/// What a link to a pull request shows.
pub async fn preview(p: &Supported, number: u64) -> Result<PullRequestPreview, GhError> {
    let raw = pr_view(p, number, "number,title,url,author,state,isDraft,createdAt,mergedAt").await?;
    Ok(PullRequestPreview {
        project_id: p.project_id.as_str().into(),
        repository: p.repository.clone(),
        number: raw.number.unwrap_or(0),
        title: text(&raw.title),
        url: text(&raw.url),
        author: raw.author(&p.host),
        state: raw.state(),
        is_draft: raw.is_draft.unwrap_or(false),
        created_at: text(&raw.created_at).into(),
    })
}

/// A pull request's checks, with whether it is still open.
pub async fn pull_request_checks(p: &Supported, number: u64) -> Result<PullRequestChecks, GhError> {
    let raw = pr_view(p, number, "state,mergedAt,statusCheckRollup").await?;
    Ok(PullRequestChecks { observed_at: Some(now_ms().into()), state: raw.state(), checks: checks(raw.checks()) })
}

/// The id GraphQL knows a pull request by.
pub async fn node_id(p: &Supported, number: u64) -> Result<String, GhError> {
    pr_view(p, number, "id").await?.id.0.ok_or_else(|| GhError::failed("GitHub did not report the pull request's id."))
}

/// The head commit of a pull request, when GitHub reports one.
pub async fn head_sha(p: &Supported, number: u64) -> Result<Option<String>, GhError> {
    Ok(pr_view(p, number, "headRefOid").await?.head_ref_oid.0.filter(|sha| !sha.is_empty()))
}

/// What a review guide is written from.
pub struct GuideSource {
    pub head: Option<String>,
    pub base_branch: String,
    pub title: String,
    pub body: String,
}

/// A pull request's head, base branch and text, for its review guide.
pub async fn guide_source(p: &Supported, number: u64) -> Result<GuideSource, GhError> {
    let raw = pr_view(p, number, "headRefOid,baseRefName,title,body").await?;
    Ok(GuideSource {
        head: raw.head_ref_oid.0.filter(|sha| !sha.is_empty()),
        base_branch: raw.base_ref_name.0.unwrap_or_else(|| "main".into()),
        title: text(&raw.title),
        body: text(&raw.body),
    })
}

/// The repository and name of a pull request's head branch; the repository
/// is the pull request's own when GitHub does not say.
pub async fn head_branch(p: &Supported, number: u64) -> Result<(String, String), GhError> {
    let raw = pr_view(p, number, "headRefName,headRepository,headRepositoryOwner").await?;
    let repository = match (raw.head_repository_owner.login.as_deref(), raw.head_repository.name.as_deref()) {
        (Some(owner), Some(name)) => format!("{owner}/{name}"),
        _ => p.repository.clone(),
    };
    Ok((repository, text(&raw.head_ref_name)))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct GhWorkflowRuns {
    workflow_runs: Rows<GhWorkflowRun>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct GhWorkflowRun {
    id: Loose<u64>,
}

/// The workflow runs of a head commit that wait for approval.
pub async fn runs_awaiting_approval(p: &Supported, sha: &str) -> Result<Vec<u64>, GhError> {
    let endpoint = format!("repos/{}/actions/runs?status=action_required&head_sha={sha}", p.repository);
    let runs: Fields<GhWorkflowRuns> = gh::run_json(p.cwd(), &p.host, &["api", &endpoint]).await?;
    Ok(rows(&runs.workflow_runs).filter_map(|run| run.id.0).collect())
}

// ---------------------------------------------------------------------------
// Activity
// ---------------------------------------------------------------------------

/// GitHub's name of each reaction, t3code's and the record's.
const REACTIONS: [(&str, &str, ReactionContent); 8] = [
    ("THUMBS_UP", "thumbs-up", ReactionContent::ThumbsUp),
    ("THUMBS_DOWN", "thumbs-down", ReactionContent::ThumbsDown),
    ("LAUGH", "laugh", ReactionContent::Laugh),
    ("HOORAY", "hooray", ReactionContent::Hooray),
    ("CONFUSED", "confused", ReactionContent::Confused),
    ("HEART", "heart", ReactionContent::Heart),
    ("ROCKET", "rocket", ReactionContent::Rocket),
    ("EYES", "eyes", ReactionContent::Eyes),
];

fn reaction_content(github: &str) -> Option<ReactionContent> {
    REACTIONS.iter().find(|(g, _, _)| *g == github).map(|(_, _, c)| *c)
}

/// GitHub's name (`THUMBS_UP`) of a reaction a client names in t3code's words (`thumbs-up`).
pub fn github_reaction(content: &str) -> Option<&'static str> {
    REACTIONS.iter().find(|(_, named, _)| *named == content).map(|(g, _, _)| *g)
}

/// `reactionGroups { content viewerHasReacted reactors(first: 10) { totalCount nodes { ... on Actor { login } } } }`.
#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct GhReactionGroup {
    content: Loose<String>,
    viewer_has_reacted: Loose<bool>,
    reactors: Fields<GhConnection<GhActor>>,
}

fn reactions(raw: &Rows<GhReactionGroup>) -> Vec<Reaction> {
    rows(raw)
        .filter_map(|g| {
            let content = reaction_content(g.content.as_deref()?)?;
            let count = g.reactors.total_count.unwrap_or(0);
            (count > 0).then(|| Reaction {
                content,
                count,
                actors: rows(&g.reactors.nodes).filter_map(|n| trimmed(&n.login)).collect(),
                viewer_has_reacted: g.viewer_has_reacted.0 == Some(true),
            })
        })
        .collect()
}

fn is_review_verdict(state: Option<&str>) -> bool {
    matches!(state.map(str::to_uppercase).as_deref(), Some("APPROVED" | "CHANGES_REQUESTED" | "DISMISSED"))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct GhUrl {
    url: Loose<String>,
}

/// A comment on the pull request, a review's summary (`state`, `submittedAt`)
/// or a comment of a review thread (`pullRequestReview`).
#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct GhComment {
    id: Loose<String>,
    body: Loose<String>,
    state: Loose<String>,
    created_at: Loose<String>,
    submitted_at: Loose<String>,
    last_edited_at: Loose<String>,
    url: Loose<String>,
    is_minimized: Loose<bool>,
    viewer_can_update: Loose<bool>,
    pull_request_review: Fields<GhUrl>,
    author: Fields<GhActor>,
    reaction_groups: Rows<GhReactionGroup>,
}

fn thread_comment(c: &GhComment, host: &str) -> ThreadComment {
    ThreadComment {
        id: text(&c.id),
        author: actor(&c.author, host),
        body: text(&c.body),
        created_at: text(&c.created_at).into(),
        edited_at: trimmed(&c.last_edited_at).map(Into::into),
        url: trimmed(&c.url),
        review_url: trimmed(&c.pull_request_review.url),
        reactions: reactions(&c.reaction_groups),
        can_edit: c.viewer_can_update.0 == Some(true),
    }
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct GhReviewThread {
    id: Loose<String>,
    path: Loose<String>,
    line: Loose<u64>,
    original_line: Loose<u64>,
    diff_side: Loose<String>,
    is_resolved: Loose<bool>,
    is_outdated: Loose<bool>,
    viewer_can_resolve: Loose<bool>,
    comments: Fields<GhConnection<GhComment>>,
}

/// One review thread node → `PullRequestReviewThread`.
fn review_thread(t: &GhReviewThread, host: &str) -> ReviewThread {
    let comments: Vec<ThreadComment> = rows(&t.comments.nodes).map(|c| thread_comment(c, host)).collect();
    ReviewThread {
        id: text(&t.id),
        path: text(&t.path),
        line: t.line.0.or(t.original_line.0),
        side: if t.diff_side.as_deref() == Some("LEFT") { DiffSide::Left } else { DiffSide::Right },
        is_resolved: t.is_resolved.0 == Some(true),
        is_outdated: t.is_outdated.0 == Some(true),
        comment_count: t.comments.total_count.unwrap_or(comments.len() as u64),
        comments,
        next_comments_cursor: t.comments.next_cursor(),
        can_resolve: t.viewer_can_resolve.0 == Some(true),
    }
}

const ACTOR_FIELDS: &str = "login avatarUrl __typename ... on User { name }";
const REACTION_FIELDS: &str = "reactionGroups { content viewerHasReacted reactors(first: 10) { totalCount nodes { ... on Actor { login } } } }";

fn activity_query() -> String {
    format!(
        "query($owner: String!, $name: String!, $number: Int!) {{ repository(owner: $owner, name: $name) {{ pullRequest(number: $number) {{
          author {{ {ACTOR_FIELDS} }} {REACTION_FIELDS}
          latestReviews(first: 50) {{ nodes {{ author {{ {ACTOR_FIELDS} }} }} }}
          comments(first: 100) {{ totalCount pageInfo {{ hasNextPage }} nodes {{ id body createdAt lastEditedAt url isMinimized viewerCanUpdate author {{ {ACTOR_FIELDS} }} {REACTION_FIELDS} }} }}
          reviews(first: 100) {{ totalCount pageInfo {{ hasNextPage }} nodes {{ id body state submittedAt lastEditedAt url isMinimized viewerCanUpdate author {{ {ACTOR_FIELDS} }} {REACTION_FIELDS} }} }}
          reviewThreads(first: 100) {{ pageInfo {{ hasNextPage }} nodes {{ id path line originalLine diffSide isResolved isOutdated viewerCanResolve
            comments(first: 50) {{ totalCount pageInfo {{ hasNextPage endCursor }} nodes {{ id body createdAt lastEditedAt url viewerCanUpdate pullRequestReview {{ url }} author {{ {ACTOR_FIELDS} }} {REACTION_FIELDS} }} }} }} }}
          commits(last: 100) {{ nodes {{ commit {{ oid messageHeadline committedDate additions deletions authors(first: 5) {{ nodes {{ name avatarUrl user {{ login }} }} }} }} }} }}
        }} }} }}"
    )
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct GhCommitAuthor {
    name: Loose<String>,
    avatar_url: Loose<String>,
    user: Fields<GhActor>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct GhCommit {
    oid: Loose<String>,
    message_headline: Loose<String>,
    committed_date: Loose<String>,
    additions: Loose<u64>,
    deletions: Loose<u64>,
    authors: Fields<GhConnection<GhCommitAuthor>>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct GhCommitNode {
    commit: Fields<GhCommit>,
}

/// `repository.pullRequest` of `activity_query`.
#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct GhActivity {
    author: Fields<GhActor>,
    reaction_groups: Rows<GhReactionGroup>,
    latest_reviews: Fields<GhConnection<GhReview>>,
    comments: Fields<GhConnection<GhComment>>,
    reviews: Fields<GhConnection<GhComment>>,
    review_threads: Fields<GhConnection<GhReviewThread>>,
    commits: Fields<GhConnection<GhCommitNode>>,
}

/// A repository's answer about one pull request: `None` when it has none of that number.
#[derive(Deserialize)]
#[serde(default, rename_all = "camelCase", bound(deserialize = "T: Deserialize<'de> + Default"))]
struct GhPullRequestOf<T> {
    pull_request: Option<Fields<T>>,
}

impl<T> Default for GhPullRequestOf<T> {
    fn default() -> Self {
        GhPullRequestOf { pull_request: None }
    }
}

/// `repository.pullRequest` of `activity_query` → `PullRequestActivity`.
fn to_activity(pr: &GhActivity, host: &str) -> PullRequestActivity {
    let mut comments: Vec<Comment> = vec![];
    for c in rows(&pr.comments.nodes).filter(|c| c.is_minimized.0 != Some(true)) {
        comments.push(Comment {
            id: text(&c.id),
            kind: CommentKind::IssueComment,
            author: actor(&c.author, host),
            body: text(&c.body),
            created_at: text(&c.created_at).into(),
            edited_at: trimmed(&c.last_edited_at).map(Into::into),
            url: trimmed(&c.url),
            path: None,
            review_state: None,
            reactions: reactions(&c.reaction_groups),
            can_edit: c.viewer_can_update.0 == Some(true),
        });
    }
    for r in rows(&pr.reviews.nodes) {
        let review_state = trimmed(&r.state);
        let body = r.body.as_deref().unwrap_or("");
        // A review that says nothing and decides nothing is only the carrier of its line comments.
        let Some(submitted) = trimmed(&r.submitted_at) else { continue };
        if r.is_minimized.0 == Some(true) || (body.trim().is_empty() && !is_review_verdict(review_state.as_deref())) {
            continue;
        }
        comments.push(Comment {
            id: text(&r.id),
            kind: CommentKind::Review,
            author: actor(&r.author, host),
            body: body.to_owned(),
            created_at: submitted.into(),
            edited_at: trimmed(&r.last_edited_at).map(Into::into),
            url: trimmed(&r.url),
            path: None,
            review_state,
            reactions: reactions(&r.reaction_groups),
            can_edit: r.viewer_can_update.0 == Some(true),
        });
    }
    comments.sort_by(|a, b| a.created_at.as_str().cmp(b.created_at.as_str()));
    let commits: Vec<Commit> = rows(&pr.commits.nodes)
        .map(|node| {
            let c = &node.commit;
            let authors = rows(&c.authors.nodes)
                .filter_map(|a| {
                    let login = trimmed(&a.user.login).or_else(|| trimmed(&a.name))?;
                    Some(PullRequestActor { is_bot: None, login, name: trimmed(&a.name), avatar_url: trimmed(&a.avatar_url) })
                })
                .collect();
            Commit {
                oid: text(&c.oid),
                message_headline: text(&c.message_headline),
                committed_date: text(&c.committed_date).into(),
                additions: c.additions.0,
                deletions: c.deletions.0,
                authors,
            }
        })
        .collect();
    let mut reviewers: Vec<PullRequestActor> = vec![];
    for reviewer in rows(&pr.latest_reviews.nodes).filter_map(|r| actor(&r.author, host)) {
        if !reviewers.iter().any(|known| known.login == reviewer.login) {
            reviewers.push(reviewer);
        }
    }
    PullRequestActivity {
        author: actor(&pr.author, host),
        reviewers,
        comments,
        comment_count: pr.comments.total_count.unwrap_or(0) + pr.reviews.total_count.unwrap_or(0),
        comments_truncated: pr.comments.has_more() || pr.reviews.has_more(),
        review_threads_truncated: pr.review_threads.has_more(),
        review_threads: rows(&pr.review_threads.nodes).map(|t| review_thread(t, host)).collect(),
        commits,
        reactions: reactions(&pr.reaction_groups),
    }
}

/// What was said and committed on a pull request; `None` when the repository has none of that number.
pub async fn activity(p: &Supported, number: u64) -> Result<Option<PullRequestActivity>, GhError> {
    let repository: GhPullRequestOf<GhActivity> = repository_query(p, &activity_query(), &[("number", Var::number(number))]).await?;
    Ok(repository.pull_request.map(|pr| to_activity(&pr, &p.host)))
}

fn thread_comments_query() -> String {
    format!(
        "query($id: ID!, $cursor: String!) {{ node(id: $id) {{ ... on PullRequestReviewThread {{
          comments(first: 50, after: $cursor) {{ pageInfo {{ hasNextPage endCursor }} nodes {{ id body createdAt lastEditedAt url viewerCanUpdate pullRequestReview {{ url }} author {{ {ACTOR_FIELDS} }} {REACTION_FIELDS} }} }}
        }} }} }}"
    )
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct GhThreadComments {
    comments: Fields<GhConnection<GhComment>>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct GhNode {
    node: Option<Fields<GhThreadComments>>,
}

/// A later page of one review thread's comments (`node(id:)` on a
/// `PullRequestReviewThread`); `None` when GitHub knows no such node.
pub async fn thread_comments(p: &Supported, thread_id: Var<'_>, cursor: Var<'_>) -> Result<Option<ThreadCommentsPage>, GhError> {
    let data: GhNode = gh::graphql(p.cwd(), &p.host, &thread_comments_query(), &[("id", thread_id), ("cursor", cursor)]).await?;
    Ok(data.node.map(|thread| ThreadCommentsPage {
        comments: rows(&thread.comments.nodes).map(|c| thread_comment(c, &p.host)).collect(),
        next_cursor: thread.comments.next_cursor(),
    }))
}

// ---------------------------------------------------------------------------
// Files
// ---------------------------------------------------------------------------

fn percent_encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// A file's text at a commit; empty for a file the repository has not there.
async fn file_at(p: &Supported, path: &str, git_ref: &str) -> Result<String, GhError> {
    let encoded: String = path.split('/').map(percent_encode).collect::<Vec<_>>().join("/");
    let endpoint = format!("repos/{}/contents/{encoded}?ref={}", p.repository, percent_encode(git_ref));
    match gh::run(p.cwd(), &p.host, &["api", "-H", "Accept: application/vnd.github.raw+json", &endpoint], None).await {
        Ok(s) => Ok(s),
        Err(e) if e.reason == GhReason::NotFound => Ok(String::new()),
        Err(e) => Err(e),
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct GhSha {
    sha: Loose<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct GhCommitParents {
    parents: Rows<GhSha>,
}

/// Which file of a diff to read, and how it changed.
pub struct FileOfDiff<'a> {
    /// One commit of the pull request against its parent, rather than the whole change.
    pub commit: Option<&'a str>,
    /// `new` and `deleted` have only one side.
    pub change: &'a str,
    pub old_path: &'a str,
    pub new_path: &'a str,
}

/// A changed file before and after.
pub async fn diff_file_contents(p: &Supported, number: u64, file: &FileOfDiff<'_>) -> Result<DiffFileContents, GhError> {
    let (old_ref, new_ref) = match file.commit {
        Some(commit) => {
            let c: Fields<GhCommitParents> = gh::run_json(p.cwd(), &p.host, &["api", &format!("repos/{}/commits/{commit}", p.repository)]).await?;
            (rows(&c.parents).next().map(|parent| text(&parent.sha)).unwrap_or_default(), commit.to_owned())
        }
        None => {
            let raw = pr_view(p, number, "baseRefOid,headRefOid").await?;
            (text(&raw.base_ref_oid), text(&raw.head_ref_oid))
        }
    };
    let old_contents = if file.change == "new" || old_ref.is_empty() { String::new() } else { file_at(p, file.old_path, &old_ref).await? };
    let new_contents = if file.change == "deleted" { String::new() } else { file_at(p, file.new_path, &new_ref).await? };
    Ok(DiffFileContents { old_contents, new_contents })
}

const FILES_VIEWED_QUERY: &str = "query($owner: String!, $name: String!, $number: Int!) { repository(owner: $owner, name: $name) { pullRequest(number: $number) { files(first: 100) { pageInfo { hasNextPage } nodes { path viewerViewedState } } } } }";

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct GhViewedFile {
    path: Loose<String>,
    viewer_viewed_state: Loose<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct GhViewedFiles {
    files: Fields<GhConnection<GhViewedFile>>,
}

/// Which files of a pull request the viewer has seen (GitHub's first hundred).
pub async fn files_viewed(p: &Supported, number: u64) -> Result<FilesViewed, GhError> {
    let repository: GhPullRequestOf<GhViewedFiles> = repository_query(p, FILES_VIEWED_QUERY, &[("number", Var::number(number))]).await?;
    let pull_request = repository.pull_request.map(|pr| pr.0).unwrap_or_default();
    let files = rows(&pull_request.files.nodes)
        .map(|f| {
            let state = match f.viewer_viewed_state.as_deref() {
                Some("VIEWED") => FileViewedState::Viewed,
                Some("DISMISSED") => FileViewedState::Dismissed,
                _ => FileViewedState::Unviewed,
            };
            FileViewed { path: text(&f.path), state }
        })
        .collect();
    Ok(FilesViewed { files, truncated: pull_request.files.has_more() })
}

/// What a changed file is, judged from its path (t3code's `classifyChangedFile`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileKind {
    Code,
    Tests,
    Mocks,
    Generated,
    Docs,
}

/// t3code's `classifyChangedFile` (path conventions only).
fn classify_changed_file(path: &str) -> FileKind {
    let normalized = path.replace('\\', "/");
    let mut segments: Vec<&str> = normalized.split('/').collect();
    let name = segments.pop().unwrap_or("");
    let dirs: Vec<String> = segments.iter().map(|s| s.to_lowercase()).collect();
    let in_dir = |set: &[&str]| dirs.iter().any(|d| set.contains(&d.as_str()));
    let lower = name.to_lowercase();
    const GENERATED_FILES: [&str; 20] = [
        "bun.lock", "bun.lockb", "cargo.lock", "composer.lock", "deno.lock", "flake.lock", "gemfile.lock", "go.sum",
        "gradle.lockfile", "mix.lock", "npm-shrinkwrap.json", "package-lock.json", "packages.lock.json", "pipfile.lock",
        "pnpm-lock.yaml", "podfile.lock", "poetry.lock", "pubspec.lock", "uv.lock", "yarn.lock",
    ];
    let generated_name = name.contains(".generated.")
        || name.contains(".gen.")
        || name.ends_with(".g.dart")
        || name.ends_with(".pb.go")
        || name.ends_with("_pb2.py")
        || name.ends_with("_pb2_grpc.py")
        || name.ends_with(".pb.cc")
        || name.ends_with(".pb.h")
        || name.contains(".min.js")
        || name.contains(".min.css")
        || name.ends_with(".js.map")
        || name.ends_with(".css.map");
    if GENERATED_FILES.contains(&lower.as_str()) || generated_name || in_dir(&["__generated__", "_generated", "generated"]) {
        return FileKind::Generated;
    }
    let stem_words = ["mock", "mocks", "fixture", "fixtures", "stub", "stubs", "fake", "fakes"];
    let mock_name = name.ends_with(".snap")
        || {
            // `.(mocks?|fixtures?|stubs?|fakes?).<ext>`
            let parts: Vec<&str> = name.split('.').collect();
            parts.len() >= 3 && stem_words.contains(&parts[parts.len() - 2])
        }
        || stem_words.iter().any(|w| {
            name.strip_prefix(w).is_some_and(|rest| {
                rest.is_empty() || rest.starts_with(['-', '_', '.']) || rest.chars().next().is_some_and(|c| c.is_ascii_uppercase())
            })
        });
    if mock_name
        || in_dir(&["__fixtures__", "__mocks__", "__snapshots__", "fakes", "fixture", "fixtures", "mock", "mocks", "stubs", "testdata"])
    {
        return FileKind::Mocks;
    }
    let test_name = {
        let parts: Vec<&str> = lower.split('.').collect();
        let dotted = parts.len() >= 3 && ["test", "spec", "e2e", "eval"].contains(&parts[parts.len() - 2]);
        let stem = lower.rsplit_once('.').map(|(s, _)| s).unwrap_or("");
        let underscored = lower.contains('.') && (stem.ends_with("_test") || stem.ends_with("_spec"));
        let py = (lower.starts_with("test_") && lower.ends_with(".py") && lower.len() > 8) || lower == "conftest.py";
        let setup = (lower.starts_with("vitest.setup.") || lower.starts_with("jest.setup.")) && parts.len() >= 3;
        dotted || underscored || py || setup
    };
    let class_test = name.rsplit_once('.').is_some_and(|(stem, ext)| {
        ["java", "kt", "kts", "scala", "groovy", "cs", "php", "swift", "m"].contains(&ext)
            && (stem.ends_with("Test") || stem.ends_with("Tests") || stem.ends_with("Spec"))
    });
    if test_name || class_test || in_dir(&["__tests__", "__test__", "cypress", "e2e", "evals", "spec", "specs", "test", "tests"]) {
        return FileKind::Tests;
    }
    let docs = ["md", "mdx", "markdown", "rst", "adoc", "txt"].iter().any(|e| lower.ends_with(&format!(".{e}")));
    let not_docs = lower.ends_with(".txt")
        && (lower.starts_with("requirements")
            || lower.starts_with("constraints")
            || ["cmakelists.txt", "robots.txt", "llms.txt"].contains(&lower.as_str()));
    if docs && !not_docs {
        return FileKind::Docs;
    }
    FileKind::Code
}

/// The lines of a change by what its files are.
fn to_breakdown(files: &Rows<GhFile>) -> ChangeBreakdown {
    let mut breakdown = ChangeBreakdown::default();
    for f in rows(files) {
        let bucket: &mut LineCount = match classify_changed_file(f.path.as_deref().unwrap_or("")) {
            FileKind::Code => &mut breakdown.code,
            FileKind::Tests => &mut breakdown.tests,
            FileKind::Mocks => &mut breakdown.mocks,
            FileKind::Generated => &mut breakdown.generated,
            FileKind::Docs => &mut breakdown.docs,
        };
        bucket.additions += f.additions.unwrap_or(0);
        bucket.deletions += f.deletions.unwrap_or(0);
    }
    breakdown
}

/// A change's line counts by what its files are; `None` when `gh` listed
/// fewer files than the change has.
pub async fn change_breakdown(p: &Supported, number: u64) -> Result<Option<ChangeBreakdown>, GhError> {
    let raw = pr_view(p, number, "files,changedFiles").await?;
    let listed = raw.files.as_ref().map_or(0, Vec::len);
    let truncated = raw.changed_files.is_some_and(|n| n as usize > listed);
    Ok((!truncated).then(|| to_breakdown(&raw.files)))
}

// ---------------------------------------------------------------------------
// Reviews
// ---------------------------------------------------------------------------

const REVIEW_REQUESTS_QUERY: &str = "query($owner: String!, $name: String!, $number: Int!) { repository(owner: $owner, name: $name) { pullRequest(number: $number) { author { login } reviewRequests(first: 50) { nodes { requestedReviewer { ... on User { login } ... on Team { combinedSlug } } } } } } }";

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct GhRequestedReviewer {
    login: Loose<String>,
    combined_slug: Loose<String>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct GhReviewRequest {
    requested_reviewer: Fields<GhRequestedReviewer>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct GhReviewRequests {
    author: Fields<GhActor>,
    review_requests: Fields<GhConnection<GhReviewRequest>>,
}

/// Who opened a pull request and who is asked to review it, all in lower case.
pub struct ReviewRequests {
    pub author: String,
    pub requested: HashSet<String>,
}

pub async fn review_requests(p: &Supported, number: u64) -> Result<ReviewRequests, GhError> {
    let repository: GhPullRequestOf<GhReviewRequests> = repository_query(p, REVIEW_REQUESTS_QUERY, &[("number", Var::number(number))]).await?;
    let pr = repository.pull_request.map(|pr| pr.0).unwrap_or_default();
    Ok(ReviewRequests {
        author: pr.author.login.as_deref().unwrap_or("").to_lowercase(),
        requested: rows(&pr.review_requests.nodes)
            .filter_map(|n| n.requested_reviewer.login.as_deref().or(n.requested_reviewer.combined_slug.as_deref()))
            .map(str::to_lowercase)
            .collect(),
    })
}

/// The labels a pull request has.
pub async fn applied_labels(p: &Supported, number: u64) -> Result<Vec<PullRequestLabel>, GhError> {
    Ok(labels(&pr_view(p, number, "labels").await?.labels))
}

/// A review comment position → GitHub REST `(line, side)`.
fn review_position(p: &ReviewPosition) -> Option<(u64, &'static str)> {
    match p.kind.as_deref()? {
        "added" => Some((p.new_line.0?, "RIGHT")),
        "deleted" => Some((p.old_line.0?, "LEFT")),
        "context" => {
            if p.side.as_deref() == Some("left") {
                Some((p.old_line.0?, "LEFT"))
            } else {
                Some((p.new_line.0?, "RIGHT"))
            }
        }
        _ => None,
    }
}

/// A comment of the body of `POST /repos/{repo}/pulls/{n}/reviews`.
#[derive(Serialize)]
struct GhReviewComment<'a> {
    path: Option<&'a Scalar>,
    line: u64,
    side: &'static str,
    body: Option<&'a Scalar>,
    #[serde(skip_serializing_if = "Option::is_none")]
    start_line: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    start_side: Option<&'static str>,
}

/// The body of `POST /repos/{repo}/pulls/{n}/reviews`.
#[derive(Serialize)]
struct GhReviewSubmission<'a> {
    event: &'static str,
    comments: Vec<GhReviewComment<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    body: Option<&'a str>,
}

/// What GitHub is sent for a review: comments without a place in the diff are left out.
fn review_submission<'a>(verdict: &str, body: &'a str, comments: &'a [ReviewCommentDraft]) -> GhReviewSubmission<'a> {
    let event = match verdict {
        "approve" => "APPROVE",
        "request-changes" => "REQUEST_CHANGES",
        _ => "COMMENT",
    };
    let comments = comments
        .iter()
        .filter_map(|c| {
            let (line, side) = review_position(&c.position)?;
            // A range is sent only when it starts elsewhere than it ends.
            let start = review_position(&c.start_position).filter(|start| *start != (line, side));
            Some(GhReviewComment {
                path: c.path.as_ref(),
                line,
                side,
                body: c.body.as_ref(),
                start_line: start.map(|(line, _)| line),
                start_side: start.map(|(_, side)| side),
            })
        })
        .collect();
    GhReviewSubmission { event, comments, body: (!body.trim().is_empty()).then_some(body) }
}

/// Submits a review with its verdict, summary and line comments.
pub async fn submit_review(p: &Supported, number: u64, verdict: &str, body: &str, comments: &[ReviewCommentDraft]) -> Result<(), GhError> {
    let payload = serde_json::to_string(&review_submission(verdict, body, comments)).map_err(|e| GhError::failed(e.to_string()))?;
    let endpoint = format!("repos/{}/pulls/{number}/reviews", p.repository);
    gh::run(p.cwd(), &p.host, &["api", "-X", "POST", &endpoint, "--input", "-"], Some(&payload)).await.map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn project() -> Supported {
        Supported { project_id: "p1".into(), title: "P".into(), workspace_root: "/w".into(), repository: "o/r".into(), host: "github.com".into() }
    }

    fn pull_request(raw: Value) -> GhPullRequest {
        serde_json::from_value::<Fields<GhPullRequest>>(raw).unwrap().0
    }

    #[test]
    fn maps_list_rows() {
        let raw = pull_request(json!({
            "number": 7, "title": "Add calc", "url": "https://github.com/o/r/pull/7",
            "author": { "login": "dev", "name": "Dev", "is_bot": false },
            "headRefName": "feat", "baseRefName": "main", "state": "OPEN", "isDraft": false, "mergeable": "MERGEABLE",
            "additions": 3, "deletions": 1, "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-02T00:00:00Z",
            "labels": [{ "name": "bug", "color": "d73a4a", "description": "" }],
            "reviewDecision": "", "latestReviews": [{ "state": "APPROVED" }],
            "reviewRequests": [{ "__typename": "User", "login": "rev" }, { "__typename": "Team", "slug": "core" }],
            "statusCheckRollup": [
                { "__typename": "CheckRun", "name": "ci", "workflowName": "CI", "status": "COMPLETED", "conclusion": "FAILURE", "completedAt": "2026-01-01T00:00:00Z" },
                { "__typename": "CheckRun", "name": "ci", "workflowName": "CI", "status": "COMPLETED", "conclusion": "SUCCESS", "completedAt": "2026-01-02T00:00:00Z" },
                { "__typename": "StatusContext", "context": "lint", "state": "SUCCESS", "targetUrl": "https://x" }
            ]
        }));
        let row = list_row(&project(), &raw, 5);
        let entry = json!(row.entry);
        assert_eq!(entry["state"], "open");
        assert_eq!(entry["mergeability"], "mergeable");
        assert_eq!(entry["checksState"], "passing");
        assert_eq!(entry["reviewDecision"], "approved");
        assert_eq!(entry["author"]["avatarUrl"], "https://github.com/dev.png?size=80");
        assert_eq!(entry["labels"][0], json!({ "name": "bug", "color": "d73a4a", "description": null }));
        assert_eq!((&entry["provider"], &entry["host"], &entry["projectId"], &entry["observedAt"]), (&json!("github"), &json!("github.com"), &json!("p1"), &json!(5)));
        assert_eq!(row.review_request_logins, vec!["rev"]);
        assert_eq!(row.author_login.as_deref(), Some("dev"));
        let checks = json!(checks(raw.checks()));
        assert_eq!(checks.as_array().unwrap().len(), 2);
        assert_eq!(checks[0], json!({ "name": "ci", "status": "success", "description": null, "url": null }));
        assert_eq!(checks[1]["url"], "https://x");
    }

    #[test]
    fn pending_checks_outrank_finished_ones() {
        let rollup = |raw: Value| serde_json::from_value::<Rows<GhCheck>>(raw).unwrap();
        let running = rollup(json!([
            { "name": "build", "status": "IN_PROGRESS" },
            { "name": "build", "status": "COMPLETED", "conclusion": "SUCCESS", "completedAt": "2026-03-01T00:00:00Z" },
        ]));
        assert_eq!(checks(&running)[0].status, PullRequestCheckStatus::Pending);
        assert_eq!(checks_state(&running), Some(PullRequestChecksState::Pending));
        assert_eq!(checks_state(&rollup(json!([]))), None);
        assert_eq!(checks_state(&rollup(json!(null))), None);
    }

    #[test]
    fn maps_detail_and_permissions() {
        let raw = pull_request(json!({
            "number": 3, "title": "T", "body": "B", "url": "u", "state": "MERGED", "mergedAt": "2026-01-03T00:00:00Z",
            "author": { "login": "me" }, "headRefName": "h", "headRefOid": "abc123", "baseRefName": "main",
            "headRepository": { "name": "r" }, "headRepositoryOwner": { "login": "o" }, "isCrossRepository": false,
            "mergeStateStatus": "UNKNOWN", "autoMergeRequest": null, "maintainerCanModify": false,
            "additions": 1, "deletions": 2, "changedFiles": 3, "latestReviews": [{ "author": { "login": "rev" }, "state": "APPROVED" }],
            "reviewRequests": [], "labels": [], "statusCheckRollup": []
        }));
        let access = to_access(&serde_json::from_value(json!({ "viewerPermission": "WRITE", "autoMergeAllowed": true })).unwrap());
        let d = json!(to_detail(&project(), &raw, &access, Some("me")));
        assert_eq!(d["state"], "merged");
        assert_eq!(d["headRepositoryNameWithOwner"], "o/r");
        assert_eq!(d["headSha"], "abc123");
        assert_eq!(d["viewerPermissions"]["verdicts"], json!(["comment"]));
        assert!(d["viewerPermissions"]["actions"].as_array().unwrap().contains(&json!("delete-branch")));
        assert_eq!(d["viewerPermissions"]["stackRebase"], true);
        assert!(d["viewerPermissions"].get("updateMethods").is_none(), "a merged branch is not updated");
        assert_eq!(d["reviewers"][0]["login"], "rev");
        assert_eq!((&d["autoMergeEnabled"], &d["maintainerCanModify"], &d["viewer"]), (&json!(false), &json!(false), &json!("me")));
        assert_eq!(d["mergeCapabilities"], json!({ "merge": true, "squash": true, "rebase": true }));
        let summary = json!(to_summary(&project(), &raw, false));
        assert_eq!(summary["autoMergeEnabled"], false);
        assert_eq!(summary["mergedAt"], "2026-01-03T00:00:00Z");
        assert_eq!(summary["closedAt"], Value::Null, "not asked for, yet sent: only the reply to an action leaves it out");
        assert!(json!(to_summary(&project(), &raw, true)).get("closedAt").is_none());
        assert!(summary.get("isDraft").is_none() && summary.get("checksState").is_some());
    }

    #[test]
    fn the_capabilities_are_those_of_t3code() {
        assert_eq!(
            json!(capabilities()),
            json!({
                "diff": true, "comment": true,
                "actions": ["merge", "ready", "draft", "close", "reopen", "delete-branch", "update-branch", "enable-auto-merge", "disable-auto-merge", "revert", "approve-workflows"],
                "mergeMethods": ["merge", "squash", "rebase"],
                "deleteBranch": true, "updateMethods": ["merge", "rebase"], "search": true, "reactions": true,
                "mentions": true, "referenceSigils": ["#"], "viewedFiles": "host",
                "review": { "inlineComment": true, "rangeComment": true, "reply": true, "resolve": true, "verdicts": ["comment", "approve", "request-changes"] },
                "reviewers": { "request": true, "listCandidates": true, "searchCandidates": true },
                "edit": { "changeRequest": true, "comment": true, "maintainerEdits": true },
                "stacks": true, "stackActions": true, "labels": true, "checksAlwaysReport": true,
            })
        );
    }

    #[test]
    fn maps_activity() {
        let pr: GhActivity = serde_json::from_value(json!({
            "author": { "login": "a", "__typename": "User" },
            "comments": { "totalCount": 1, "pageInfo": { "hasNextPage": false }, "nodes": [
                { "id": "c1", "body": "hi", "createdAt": "2026-01-02T00:00:00Z", "author": { "login": "b", "__typename": "Bot" },
                  "reactionGroups": [{ "content": "HEART", "viewerHasReacted": true, "reactors": { "totalCount": 1, "nodes": [{ "login": "me" }] } },
                                     { "content": "EYES", "reactors": { "totalCount": 0, "nodes": [] } }] },
                { "id": "c2", "body": "spam", "createdAt": "2026-01-01T00:00:00Z", "isMinimized": true } ] },
            "reviews": { "totalCount": 2, "pageInfo": { "hasNextPage": false }, "nodes": [
                { "id": "r1", "body": "", "state": "APPROVED", "submittedAt": "2026-01-01T00:00:00Z", "author": { "login": "c" } },
                { "id": "r2", "body": "", "state": "COMMENTED", "submittedAt": "2026-01-01T00:00:00Z" } ] },
            "reviewThreads": { "pageInfo": { "hasNextPage": false }, "nodes": [
                { "id": "t1", "path": "a.rs", "line": 4, "diffSide": "LEFT", "isResolved": true, "comments": { "totalCount": 1, "nodes": [] } } ] },
            "commits": { "nodes": [{ "commit": { "oid": "x", "messageHeadline": "m", "committedDate": "d", "authors": { "nodes": [{ "name": "N", "user": null }] } } }] }
        }))
        .unwrap();
        let a = json!(to_activity(&pr, "github.com"));
        let comments = a["comments"].as_array().unwrap();
        assert_eq!(comments.len(), 2);
        assert_eq!(comments[0]["id"], "r1");
        assert_eq!((&comments[0]["kind"], &comments[0]["reviewState"], &comments[0]["path"]), (&json!("review"), &json!("APPROVED"), &Value::Null));
        assert_eq!(comments[1]["author"]["isBot"], true);
        assert_eq!(comments[1]["reactions"], json!([{ "content": "heart", "count": 1, "actors": ["me"], "viewerHasReacted": true }]));
        assert_eq!(a["reviewThreads"][0]["side"], "left");
        assert_eq!(a["reviewThreads"][0]["nextCommentsCursor"], Value::Null);
        assert_eq!(a["commits"][0]["authors"][0]["login"], "N");
        assert_eq!(a["commits"][0]["additions"], Value::Null);
        assert_eq!(a["commentCount"], 3);
    }

    #[test]
    fn classifies_changed_files_like_t3code() {
        for (path, kind) in [
            ("Cargo.lock", FileKind::Generated),
            ("src/__generated__/x.ts", FileKind::Generated),
            ("web/app.min.js", FileKind::Generated),
            ("src/__snapshots__/a.snap", FileKind::Mocks),
            ("src/user.fixture.json", FileKind::Mocks),
            ("mocks.ts", FileKind::Mocks),
            ("src/calc.test.ts", FileKind::Tests),
            ("pkg/calc_test.go", FileKind::Tests),
            ("tests/x.rs", FileKind::Tests),
            ("FooTest.java", FileKind::Tests),
            ("Latest.java", FileKind::Code),
            ("test_calc.py", FileKind::Tests),
            ("README.md", FileKind::Docs),
            ("requirements.txt", FileKind::Code),
            ("src/main.rs", FileKind::Code),
        ] {
            assert_eq!(classify_changed_file(path), kind, "{path}");
        }
        let files = serde_json::from_value(json!([{ "path": "a.rs", "additions": 2, "deletions": 1 }, { "path": "a_test.go", "additions": 5, "deletions": 0 }])).unwrap();
        let b = json!(to_breakdown(&files));
        assert_eq!(b["code"], json!({ "additions": 2, "deletions": 1 }));
        assert_eq!(b["tests"]["additions"], 5);
        assert_eq!(b["docs"], json!({ "additions": 0, "deletions": 0 }));
    }

    #[test]
    fn builds_review_submissions() {
        let comments: Vec<ReviewCommentDraft> = serde_json::from_value(json!([{
            "path": "a.rs", "body": "x",
            "position": { "kind": "context", "oldLine": 3, "newLine": 4, "side": "right" },
            "startPosition": { "kind": "added", "newLine": 2 },
        }]))
        .unwrap();
        let s = json!(review_submission("request-changes", "", &comments));
        assert_eq!(s["event"], "REQUEST_CHANGES");
        assert!(s.get("body").is_none());
        assert_eq!(s["comments"][0], json!({ "path": "a.rs", "line": 4, "side": "RIGHT", "body": "x", "start_line": 2, "start_side": "RIGHT" }));
        assert_eq!(github_reaction("thumbs-up"), Some("THUMBS_UP"));
        assert_eq!(github_reaction("shrug"), None);
        for (_, named, content) in REACTIONS {
            assert_eq!(json!(content), json!(named), "a reaction is sent as a client names it");
        }
    }
}
