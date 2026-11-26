//! GitLab's merge requests as the records a client reads, after t3code's
//! `GitLabPullRequestProvider`, `GitLabPullRequestCli` and
//! `gitLabMergeRequestJson` (`source-control-gitlab`). Every read and write
//! goes through `glab api` (`crate::glab`): GitLab's REST API, and its GraphQL
//! API for what REST does not say (approval and pipeline state of listed rows,
//! line counts, label permission, pipeline stages, awards). The actions upstream
//! runs as `glab mr` subcommands run as those.
//!
//! GitLab's JSON is read leniently (`gh::lenient`); a row of a list that lacks
//! what upstream's schema requires of it (`iid`, `title`, `web_url`, branches,
//! times) is skipped rather than failing its page, as upstream skips it.
//! Optional reads (GraphQL extras, stats, awards, stages) cost only their own
//! fields when GitLab cannot answer them; a missing `glab`, a missing login or
//! a rate limit still fails the call.
//!
//! Limits (beside the module header of `pull_requests`): listings are read
//! through REST only (no GraphQL list, so the `changes-requested` review filter
//! is applied to the rows read); trigger-job (bridge) checks are not read when
//! pipeline stages cannot be; ranged review comments are posted at their last
//! line (no `line_range`); the maintainer-edits permission of a fork's merge
//! request is not read (it reads as not granted); `revert`, `delete-branch`,
//! the head guard of a merge (`expectedHeadSha`) and removing agent credits
//! from merge messages are not done; viewed files are not kept (GitLab keeps
//! none, t3code keeps them in its own store); review threads are not paged
//! (GitLab answers a thread whole).

use super::github::{self, FileOfDiff, ListRow, Listing};
use super::pull_request_failure::Provider;
use super::pull_request_records::{
    BaseComparison, Capabilities, ChangeBreakdown, Comment, CommentKind, Commit, DiffFileContents, DiffSide, EditCapabilities, LabelCandidate, LabelCandidates,
    MergeCapabilities, OverviewThread, PullRequestActivity, PullRequestDetail, PullRequestPreview, Reaction, ReactionContent, ReferenceCandidate, ReferenceKind,
    ReferenceSigil, ReviewCapabilities, ReviewThread, ReviewVerdict, ReviewerCandidate, ReviewerCandidates, ReviewerCapabilities, ThreadComment, UpdateMethod,
    ViewerPermissions,
};
use super::pull_requests::{ReviewCommentDraft, ReviewPosition, Supported};
use crate::gh::lenient::{Fields, Loose, Rows, Scalar, rows, trimmed};
use crate::gh::{GhError, GhReason};
use crate::glab::{self, Api, OfIid};
use domovoi_model::typed::{
    PullRequestAction, PullRequestActor, PullRequestCheck, PullRequestCheckStatus, PullRequestChecks, PullRequestChecksState, PullRequestLabel,
    PullRequestListEntry, PullRequestMergeMethod, PullRequestMergeability, PullRequestReviewDecision, PullRequestState, PullRequestSummary,
};
use serde::de::IgnoredAny;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

/// GitLab's `per_page` limit.
const MAX_PAGE_SIZE: usize = 100;
/// How many pages of notes, discussions and awards an activity reads.
const CONVERSATION_PAGES: usize = 10;
const REFERENCE_PAGE_SIZE: usize = 20;
/// Upstream's `DIFF_FILE_MAX_OUTPUT_BYTES`.
const DIFF_FILE_MAX_BYTES: usize = 1024 * 1024;

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

fn text(value: &Loose<String>) -> String {
    value.as_deref().unwrap_or("").to_owned()
}

fn project(p: &Supported) -> String {
    format!("projects/{}", glab::project_path(&p.repository))
}

fn merge_request(p: &Supported, number: u64) -> String {
    format!("{}/merge_requests/{number}", project(p))
}

async fn get<T: serde::de::DeserializeOwned>(p: &Supported, path: &str) -> Result<T, GhError> {
    glab::api_json(p.cwd(), &p.host, &Api::get(path)).await
}

async fn write(p: &Supported, request: Api<'_>) -> Result<(), GhError> {
    glab::api(p.cwd(), &p.host, &request).await.map(|_| ())
}

async fn graphql<T: serde::de::DeserializeOwned + Default>(p: &Supported, query: &str, number: u64) -> Result<T, GhError> {
    glab::graphql(p.cwd(), &p.host, query, OfIid { full_path: p.repository.trim(), iid: number.to_string() }).await
}

/// A read GitLab could not answer is not worth failing the call it serves;
/// an unusable CLI or account, or a rate limit, still reaches the caller
/// (upstream's `orWhenUnanswered` and `recoverRead`).
fn optional<T>(read: Result<T, GhError>, fallback: T) -> Result<T, GhError> {
    match read {
        Err(e) if !matches!(e.reason, GhReason::MissingTool | GhReason::Unauthenticated | GhReason::RateLimited) => Ok(fallback),
        other => other,
    }
}

// ---------------------------------------------------------------------------
// What GitLab answers
// ---------------------------------------------------------------------------

/// A GitLab user (`RawUserSchema`).
#[derive(Deserialize, Default, Clone)]
#[serde(default)]
struct GlUser {
    id: Loose<u64>,
    username: Loose<String>,
    name: Loose<String>,
    avatar_url: Loose<String>,
}

fn actor(raw: &GlUser) -> Option<PullRequestActor> {
    Some(PullRequestActor { is_bot: None, login: trimmed(&raw.username)?, name: trimmed(&raw.name), avatar_url: trimmed(&raw.avatar_url) })
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct GlLabel {
    name: Loose<String>,
    color: Loose<String>,
    description: Loose<String>,
    archived: Loose<bool>,
}

/// `with_labels_details=true` answers label objects; hosts that ignore it still send names.
#[derive(Deserialize)]
#[serde(untagged)]
enum GlLabelRef {
    Name(String),
    Detail(Fields<GlLabel>),
}

fn labels(raw: &Loose<Vec<GlLabelRef>>) -> Vec<PullRequestLabel> {
    raw.iter()
        .flatten()
        .filter_map(|label| match label {
            GlLabelRef::Name(name) => {
                let name = name.trim();
                (!name.is_empty()).then(|| PullRequestLabel { name: name.to_owned(), color: None, description: None })
            }
            GlLabelRef::Detail(l) => Some(PullRequestLabel { name: trimmed(&l.name)?, color: trimmed(&l.color), description: Some(trimmed(&l.description)) }),
        })
        .collect()
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct GlPipeline {
    status: Loose<String>,
    web_url: Loose<String>,
    source: Loose<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct GlDiffRefs {
    base_sha: Loose<String>,
    head_sha: Loose<String>,
    start_sha: Loose<String>,
}

/// `base_sha`, `head_sha` and `start_sha` of a diff (`GitLabDiffRefs`).
#[derive(Debug, Clone, PartialEq)]
pub struct DiffRefs {
    base: String,
    head: String,
    start: String,
}

impl GlDiffRefs {
    fn refs(&self) -> Option<DiffRefs> {
        Some(DiffRefs { base: self.base_sha.0.clone()?, head: self.head_sha.0.clone()?, start: self.start_sha.0.clone()? })
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct GlMergeRequestUser {
    can_merge: Loose<bool>,
}

/// A merge request as REST answers it (`RawMergeRequestSchema`).
#[derive(Deserialize, Default)]
#[serde(default)]
struct GlMergeRequest {
    iid: Loose<u64>,
    title: Loose<String>,
    web_url: Loose<String>,
    description: Loose<String>,
    author: Fields<GlUser>,
    source_branch: Loose<String>,
    target_branch: Loose<String>,
    sha: Loose<String>,
    state: Loose<String>,
    draft: Loose<bool>,
    work_in_progress: Loose<bool>,
    detailed_merge_status: Loose<String>,
    merge_status: Loose<String>,
    has_conflicts: Loose<bool>,
    created_at: Loose<String>,
    updated_at: Loose<String>,
    merged_at: Loose<String>,
    closed_at: Loose<String>,
    reviewers: Rows<GlUser>,
    labels: Loose<Vec<GlLabelRef>>,
    /// GitLab reports `1000+` past its counting limit.
    changes_count: Loose<String>,
    head_pipeline: Option<Fields<GlPipeline>>,
    user: Fields<GlMergeRequestUser>,
    merge_when_pipeline_succeeds: Loose<bool>,
    auto_merge_enabled: Loose<bool>,
    squash_on_merge: Loose<bool>,
    diverged_commits_count: Loose<u64>,
    source_project_id: Loose<u64>,
    target_project_id: Loose<u64>,
    allow_collaboration: Loose<bool>,
    diff_refs: Fields<GlDiffRefs>,
}

impl GlMergeRequest {
    /// What upstream's schema requires of a merge request.
    fn is_whole(&self) -> bool {
        self.iid.is_some()
            && self.title.is_some()
            && self.web_url.is_some()
            && self.source_branch.is_some()
            && self.target_branch.is_some()
            && self.created_at.is_some()
            && self.updated_at.is_some()
    }

    /// `toState`: merged whenever it has a merge time.
    fn state(&self) -> PullRequestState {
        if trimmed(&self.merged_at).is_some() {
            return PullRequestState::Merged;
        }
        match self.state.as_deref().unwrap_or("").trim().to_lowercase().as_str() {
            "merged" => PullRequestState::Merged,
            "closed" => PullRequestState::Closed,
            _ => PullRequestState::Open,
        }
    }

    /// `toMergeability`.
    fn mergeability(&self) -> PullRequestMergeability {
        if self.has_conflicts.0 == Some(true) {
            return PullRequestMergeability::Conflicting;
        }
        match self.merge_status.as_deref().unwrap_or("").trim().to_lowercase().as_str() {
            "can_be_merged" => PullRequestMergeability::Mergeable,
            "cannot_be_merged" => PullRequestMergeability::Conflicting,
            _ => PullRequestMergeability::Unknown,
        }
    }

    fn is_draft(&self) -> bool {
        self.draft.0.or(self.work_in_progress.0).unwrap_or(false)
    }

    fn rebase_required(&self) -> Option<bool> {
        match self.detailed_merge_status.as_deref().map(|s| s.trim().to_lowercase()).as_deref() {
            Some("need_rebase") => Some(true),
            Some("mergeable") => Some(false),
            _ => None,
        }
    }

    /// `toChangedFiles`.
    fn changed_files(&self) -> u64 {
        let digits: String = self.changes_count.as_deref().unwrap_or("").trim().chars().take_while(char::is_ascii_digit).collect();
        digits.parse().unwrap_or(0)
    }

    fn review_request_logins(&self) -> Vec<String> {
        rows(&self.reviewers).filter_map(|r| trimmed(&r.username)).collect()
    }

    /// `toChecks`: the head pipeline as one check.
    fn checks(&self) -> Vec<PullRequestCheck> {
        let Some(pipeline) = &self.head_pipeline else { return vec![] };
        vec![PullRequestCheck {
            name: "Pipeline".into(),
            status: pipeline_status(pipeline.status.as_deref()),
            description: trimmed(&pipeline.source),
            url: trimmed(&pipeline.web_url),
            required: None,
            placeholder: None,
        }]
    }

    /// `undefined` when GitLab said nothing of auto-merge.
    fn auto_merge(&self) -> Option<bool> {
        if self.merge_when_pipeline_succeeds.is_none() && self.auto_merge_enabled.is_none() {
            return None;
        }
        Some(self.merge_when_pipeline_succeeds.0 == Some(true) || self.auto_merge_enabled.0 == Some(true))
    }

    fn auto_merge_method(&self) -> Option<PullRequestMergeMethod> {
        (self.auto_merge() == Some(true) && self.squash_on_merge.0 == Some(true)).then_some(PullRequestMergeMethod::Squash)
    }

    /// Whether target maintainers may push to the source branch; set only on a fork's merge request.
    fn maintainer_can_modify(&self) -> Option<bool> {
        match (self.source_project_id.0, self.target_project_id.0) {
            (Some(source), Some(target)) if source != target => Some(self.allow_collaboration.0 == Some(true)),
            _ => None,
        }
    }

    fn viewer_can_merge(&self) -> bool {
        self.user.can_merge.0 != Some(false)
    }
}

/// `toPipelineStatus`.
fn pipeline_status(value: Option<&str>) -> PullRequestCheckStatus {
    match value.unwrap_or("").trim().to_lowercase().as_str() {
        "success" => PullRequestCheckStatus::Success,
        "failed" => PullRequestCheckStatus::Failure,
        "canceled" | "canceling" => PullRequestCheckStatus::Cancelled,
        "skipped" => PullRequestCheckStatus::Skipped,
        "manual" => PullRequestCheckStatus::Neutral,
        _ => PullRequestCheckStatus::Pending,
    }
}

/// `toChecksState`.
fn checks_state(pipeline_status_text: Option<&str>) -> Option<PullRequestChecksState> {
    match pipeline_status(Some(pipeline_status_text?)) {
        PullRequestCheckStatus::Success => Some(PullRequestChecksState::Passing),
        PullRequestCheckStatus::Failure | PullRequestCheckStatus::Cancelled => Some(PullRequestChecksState::Failing),
        PullRequestCheckStatus::Pending => Some(PullRequestChecksState::Pending),
        _ => None,
    }
}

async fn read_merge_request(p: &Supported, number: u64) -> Result<GlMergeRequest, GhError> {
    let path = format!("{}?include_diverged_commits_count=true", merge_request(p, number));
    let raw: Fields<GlMergeRequest> = get(p, &path).await?;
    if !raw.0.is_whole() {
        return Err(GhError::failed("GitLab CLI returned an unreadable getMergeRequestDetail response."));
    }
    Ok(raw.0)
}

// ---------------------------------------------------------------------------
// GraphQL reads
// ---------------------------------------------------------------------------

const LIST_STATE_FIELDS: &str = "iid approved approvedBy(first: 1) { nodes { username } } reviewers(first: 20) { nodes { username mergeRequestInteraction { reviewState } } } headPipeline { status }";

fn list_states_query() -> String {
    format!("query($fullPath: ID!, $iids: [String!]) {{ metadata {{ enterprise }} project(fullPath: $fullPath) {{ mergeRequests(iids: $iids, first: 100) {{ nodes {{ {LIST_STATE_FIELDS} }} }} }} }}")
}

const STATS_QUERY: &str =
    "query($fullPath: ID!, $iids: [String!]) { project(fullPath: $fullPath) { mergeRequests(iids: $iids, first: 100) { nodes { iid diffStatsSummary { additions deletions } } } } }";

const LABEL_PERMISSION_QUERY: &str = "query($fullPath: ID!, $iid: String!) { project(fullPath: $fullPath) { mergeRequest(iid: $iid) { userPermissions { adminMergeRequest } labels(first: 100) { nodes { title color description } } } } }";

const PIPELINE_STAGES_QUERY: &str = "query($fullPath: ID!, $iid: String!) { project(fullPath: $fullPath) { mergeRequest(iid: $iid) { headPipeline { path stages(first: 100) { nodes { name status detailedStatus { label detailsPath } jobs(first: 100) { nodes { name status allowFailure retried webPath detailedStatus { label } downstreamPipeline { status path } } pageInfo { hasNextPage } } } pageInfo { hasNextPage } } } } } }";

const AWARD_EMOJI_QUERY: &str = "query($fullPath: ID!, $iid: String!, $cursor: String) { currentUser { username } project(fullPath: $fullPath) { mergeRequest(iid: $iid) { awardEmoji { nodes { name user { username } } } notes(first: 100, after: $cursor) { pageInfo { hasNextPage endCursor } nodes { id author { username bot } lastEditedAt lastEditedBy { username } resolved awardEmoji { nodes { name user { username } } } } } } } }";

const FILE_STATS_QUERY: &str =
    "query($fullPath: ID!, $iid: String!) { project(fullPath: $fullPath) { mergeRequest(iid: $iid) { diffStatsSummary { fileCount } diffStats { path additions deletions } } } }";

const SET_SELF_AS_REVIEWER_MUTATION: &str = "mutation($projectPath: ID!, $iid: String!, $reviewers: [String!]!) { mergeRequestSetReviewers(input: { projectPath: $projectPath, iid: $iid, reviewerUsernames: $reviewers, operationMode: APPEND }) { errors } }";

const REQUEST_CHANGES_MUTATION: &str = "mutation($projectPath: ID!, $iid: String!) { mergeRequestRequestChanges(input: { projectPath: $projectPath, iid: $iid }) { errors } }";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct OfIids<'a> {
    full_path: &'a str,
    iids: Vec<String>,
}

#[derive(Deserialize, Default)]
#[serde(default, bound(deserialize = "T: Deserialize<'de> + Default"))]
struct Nodes<T> {
    nodes: Rows<T>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct PageInfo {
    has_next_page: Loose<bool>,
    end_cursor: Loose<String>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase", bound(deserialize = "T: Deserialize<'de> + Default"))]
struct Paged<T> {
    nodes: Rows<T>,
    page_info: Fields<PageInfo>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Username {
    username: Loose<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Metadata {
    enterprise: Loose<bool>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct Interaction {
    review_state: Loose<String>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct StateReviewer {
    merge_request_interaction: Fields<Interaction>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ApprovedBy {
    nodes: Loose<Vec<IgnoredAny>>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct StatusOnly {
    status: Loose<String>,
}

/// A merge request of `LIST_STATES_GRAPHQL_QUERY`.
#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct StateNode {
    iid: Loose<String>,
    approved: Loose<bool>,
    approved_by: Option<Fields<ApprovedBy>>,
    reviewers: Fields<Nodes<StateReviewer>>,
    head_pipeline: Option<Fields<StatusOnly>>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase", bound(deserialize = "T: Deserialize<'de> + Default"))]
struct MergeRequestsOf<T> {
    merge_requests: Fields<Nodes<T>>,
}

#[derive(Deserialize, Default)]
#[serde(default, bound(deserialize = "T: Deserialize<'de> + Default"))]
struct ProjectOf<T> {
    project: Fields<T>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ListStatesData {
    metadata: Fields<Metadata>,
    project: Fields<MergeRequestsOf<StateNode>>,
}

/// `GitLabMergeRequestListState`: the review decision (`None` when it cannot
/// be judged, `Some(None)` for none) and the checks' state.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ListState {
    review_decision: Option<Option<PullRequestReviewDecision>>,
    checks_state: Option<PullRequestChecksState>,
}

/// `toListState`: only Enterprise Edition holds `approved` to approval rules;
/// a reviewer asking for changes outranks both.
fn to_list_state(node: &StateNode, enterprise: Option<bool>) -> ListState {
    let checks_state = node.head_pipeline.as_ref().and_then(|p| checks_state(p.status.as_deref()));
    let changes_requested =
        rows(&node.reviewers.nodes).any(|r| r.merge_request_interaction.review_state.as_deref() == Some("REQUESTED_CHANGES"));
    if changes_requested {
        return ListState { review_decision: Some(Some(PullRequestReviewDecision::ChangesRequested)), checks_state };
    }
    let approvals = node.approved_by.as_ref().and_then(|a| a.nodes.as_ref().map(Vec::len));
    let (Some(approved), Some(approvals)) = (node.approved.0, approvals) else { return ListState { review_decision: None, checks_state } };
    if !approved && approvals == 0 && enterprise.is_none() {
        return ListState { review_decision: None, checks_state };
    }
    let decision = if !approved && (enterprise == Some(true) || approvals > 0) {
        Some(PullRequestReviewDecision::ReviewRequired)
    } else if approvals > 0 {
        Some(PullRequestReviewDecision::Approved)
    } else {
        None
    };
    ListState { review_decision: Some(decision), checks_state }
}

fn list_states_of(data: &ListStatesData) -> HashMap<u64, ListState> {
    let enterprise = data.metadata.enterprise.0;
    rows(&data.project.merge_requests.nodes)
        .filter_map(|node| Some((node.iid.as_deref()?.parse::<u64>().ok()?, to_list_state(node, enterprise))))
        .collect()
}

/// Approval and head pipeline state of listed merge requests; a failed read leaves them unknown.
async fn list_states(p: &Supported, numbers: &[u64]) -> HashMap<u64, ListState> {
    if numbers.is_empty() {
        return HashMap::new();
    }
    let mut states = HashMap::new();
    for chunk in numbers.chunks(MAX_PAGE_SIZE) {
        let vars = OfIids { full_path: p.repository.trim(), iids: chunk.iter().map(u64::to_string).collect() };
        if let Ok(data) = glab::graphql::<ListStatesData, _>(p.cwd(), &p.host, &list_states_query(), vars).await {
            states.extend(list_states_of(&data));
        }
    }
    states
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct LineCounts {
    additions: Loose<u64>,
    deletions: Loose<u64>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct StatsNode {
    iid: Loose<String>,
    diff_stats_summary: Option<Fields<LineCounts>>,
}

/// Line counts of merge requests by number; one GitLab gave no summary for is left out.
fn stats_of(data: &ProjectOf<MergeRequestsOf<StatsNode>>) -> HashMap<u64, (u64, u64)> {
    rows(&data.project.merge_requests.nodes)
        .filter_map(|node| {
            let iid = node.iid.as_deref()?.parse::<u64>().ok().filter(|n| *n > 0)?;
            let counts = node.diff_stats_summary.as_ref()?;
            Some((iid, (counts.additions.0?, counts.deletions.0?)))
        })
        .collect()
}

async fn stats(p: &Supported, numbers: &[u64]) -> Result<HashMap<u64, (u64, u64)>, GhError> {
    let vars = OfIids { full_path: p.repository.trim(), iids: numbers.iter().map(u64::to_string).collect() };
    let data: ProjectOf<MergeRequestsOf<StatsNode>> = glab::graphql(p.cwd(), &p.host, STATS_QUERY, vars).await?;
    Ok(stats_of(&data))
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct UserPermissions {
    admin_merge_request: Loose<bool>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct GqlLabel {
    title: Loose<String>,
    color: Loose<String>,
    description: Loose<String>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct LabelPermissionNode {
    user_permissions: Fields<UserPermissions>,
    labels: Option<Fields<Nodes<GqlLabel>>>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase", bound(deserialize = "T: Deserialize<'de> + Default"))]
struct MergeRequestOf<T> {
    merge_request: Option<Fields<T>>,
}

/// Whoever may change a merge request's metadata may set its labels; the
/// labels ride along with their colours (REST's detail carries none).
async fn label_permission(p: &Supported, number: u64) -> Result<(bool, Option<Vec<PullRequestLabel>>), GhError> {
    let read: Result<ProjectOf<MergeRequestOf<LabelPermissionNode>>, GhError> = graphql(p, LABEL_PERMISSION_QUERY, number).await;
    let data = optional(read.map(Some), None)?;
    let Some(node) = data.as_ref().and_then(|d| d.project.merge_request.as_ref()) else { return Ok((false, None)) };
    let labels = node.labels.as_ref().map(|l| {
        rows(&l.nodes)
            .filter_map(|label| Some(PullRequestLabel { name: trimmed(&label.title)?, color: trimmed(&label.color), description: Some(trimmed(&label.description)) }))
            .collect()
    });
    Ok((node.user_permissions.admin_merge_request.0 == Some(true), labels))
}

/// `colourLabels`: REST names every label, GraphQL colours the ones on its page.
fn colour_labels(rest: Vec<PullRequestLabel>, graphql: Option<Vec<PullRequestLabel>>) -> Vec<PullRequestLabel> {
    let Some(coloured) = graphql else { return rest };
    rest.into_iter().map(|label| coloured.iter().find(|c| c.name == label.name).cloned().unwrap_or(label)).collect()
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct DetailedStatus {
    label: Loose<String>,
    details_path: Loose<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Downstream {
    status: Loose<String>,
    path: Loose<String>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct StageJob {
    name: Loose<String>,
    status: Loose<String>,
    allow_failure: Loose<bool>,
    retried: Loose<bool>,
    web_path: Loose<String>,
    detailed_status: Fields<DetailedStatus>,
    downstream_pipeline: Option<Fields<Downstream>>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct Stage {
    name: Loose<String>,
    status: Loose<String>,
    detailed_status: Fields<DetailedStatus>,
    jobs: Option<Fields<Paged<StageJob>>>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct HeadPipeline {
    path: Loose<String>,
    stages: Option<Fields<Paged<Stage>>>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct PipelineNode {
    head_pipeline: Option<Fields<HeadPipeline>>,
}

/// `decodePipelineStageChecksJson`: the head pipeline as one check per job,
/// named after its stage; a stage whose jobs were not listed whole is one
/// check. `None` when there are no stages, or only some were answered.
fn stage_checks(data: &ProjectOf<MergeRequestOf<PipelineNode>>, origin: &str) -> Option<Vec<PullRequestCheck>> {
    let pipeline = data.project.merge_request.as_ref()?.head_pipeline.as_ref()?;
    let stages = pipeline.stages.as_ref()?;
    if stages.page_info.has_next_page.0 == Some(true) || rows(&stages.nodes).any(|s| trimmed(&s.name).is_none()) {
        return None;
    }
    let link = |path: Option<String>| path.filter(|p| p.starts_with('/')).map(|p| format!("{origin}{p}"));
    let mut checks = vec![];
    for stage in rows(&stages.nodes) {
        let name = trimmed(&stage.name)?;
        let jobs: Vec<&StageJob> = stage.jobs.as_ref().map(|j| rows(&j.nodes).collect()).unwrap_or_default();
        let listed_whole = !jobs.is_empty()
            && stage.jobs.as_ref().is_some_and(|j| j.page_info.has_next_page.0 != Some(true))
            && jobs.iter().all(|job| trimmed(&job.name).is_some());
        if !listed_whole {
            checks.push(PullRequestCheck {
                name,
                status: pipeline_status(stage.status.as_deref()),
                description: trimmed(&stage.detailed_status.label),
                url: link(trimmed(&stage.detailed_status.details_path).or_else(|| trimmed(&pipeline.path))),
                required: None,
                placeholder: None,
            });
            continue;
        }
        for job in jobs.into_iter().filter(|job| job.retried.0 != Some(true)) {
            let downstream = job.downstream_pipeline.as_ref();
            let status = pipeline_status(downstream.map_or(job.status.as_deref(), |d| d.status.as_deref()));
            checks.push(PullRequestCheck {
                name: format!("{name} / {}", text(&job.name).trim()),
                status: if status == PullRequestCheckStatus::Failure && job.allow_failure.0 == Some(true) { PullRequestCheckStatus::Neutral } else { status },
                description: if downstream.is_some() { Some("Downstream pipeline".into()) } else { trimmed(&job.detailed_status.label) },
                url: link(downstream.map_or_else(|| trimmed(&job.web_path), |d| trimmed(&d.path))),
                required: None,
                placeholder: None,
            });
        }
    }
    (!checks.is_empty()).then_some(checks)
}

/// The detail's checks: one per pipeline job where GitLab lists the stages,
/// else the pipeline as one check.
async fn checks_of(p: &Supported, number: u64, raw: &GlMergeRequest) -> Result<Vec<PullRequestCheck>, GhError> {
    let single = raw.checks();
    if single.is_empty() {
        return Ok(single);
    }
    let Some(origin) = raw.web_url.as_deref().and_then(|u| url::Url::parse(u).ok()).map(|u| u.origin().ascii_serialization()) else { return Ok(single) };
    let read: Result<ProjectOf<MergeRequestOf<PipelineNode>>, GhError> = graphql(p, PIPELINE_STAGES_QUERY, number).await;
    let stages = optional(read.map(|data| stage_checks(&data, &origin)), None)?;
    Ok(stages.unwrap_or(single))
}

// ---------------------------------------------------------------------------
// Capabilities and permissions
// ---------------------------------------------------------------------------

/// The actions this server runs on GitLab (upstream's set without `revert` and `delete-branch`).
const ACTIONS: [PullRequestAction; 8] = [
    PullRequestAction::Merge,
    PullRequestAction::Ready,
    PullRequestAction::Draft,
    PullRequestAction::Close,
    PullRequestAction::Reopen,
    PullRequestAction::UpdateBranch,
    PullRequestAction::EnableAutoMerge,
    PullRequestAction::DisableAutoMerge,
];

/// The actions `user.can_merge` answers for.
const MERGE_ACTIONS: [PullRequestAction; 4] =
    [PullRequestAction::Merge, PullRequestAction::UpdateBranch, PullRequestAction::EnableAutoMerge, PullRequestAction::DisableAutoMerge];

const ALL_VERDICTS: [ReviewVerdict; 3] = [ReviewVerdict::Comment, ReviewVerdict::Approve, ReviewVerdict::RequestChanges];

/// The actions GitLab has here (`PullRequestProviderSummary.actions`).
pub fn all_actions() -> Vec<PullRequestAction> {
    ACTIONS.to_vec()
}

/// GitLab has no merge-target-into-source operation.
pub const UPDATE_METHODS: [UpdateMethod; 1] = [UpdateMethod::Rebase];

/// t3code's GitLab `CAPABILITIES`.
pub fn capabilities() -> Capabilities {
    Capabilities {
        diff: true,
        comment: true,
        actions: all_actions(),
        merge_methods: [PullRequestMergeMethod::Merge, PullRequestMergeMethod::Squash, PullRequestMergeMethod::Rebase],
        delete_branch: false,
        update_methods: UPDATE_METHODS.to_vec(),
        search: true,
        reactions: true,
        mentions: true,
        reference_sigils: vec![ReferenceSigil::Hash, ReferenceSigil::Bang],
        viewed_files: None,
        review: ReviewCapabilities { inline_comment: true, range_comment: true, reply: true, resolve: true, verdicts: ALL_VERDICTS },
        reviewers: ReviewerCapabilities { request: true, list_candidates: true, search_candidates: true },
        edit: EditCapabilities { change_request: true, comment: true, maintainer_edits: true },
        stacks: false,
        stack_actions: false,
        labels: true,
        checks_always_report: false,
    }
}

/// `gitLabViewerPermissions`: authors may close, reopen and change the draft
/// state whatever their role, and REST does not say who the author is, so
/// those are offered; GitLab explains any refusal itself.
fn viewer_permissions_of(can_merge: bool, can_edit_labels: bool, can_edit_maintainer_edits: bool) -> ViewerPermissions {
    ViewerPermissions {
        actions: ACTIONS.iter().filter(|a| can_merge || !MERGE_ACTIONS.contains(a)).cloned().collect(),
        comment: true,
        resolve: true,
        verdicts: ALL_VERDICTS.to_vec(),
        request_reviewers: true,
        labels: can_edit_labels,
        stack_rebase: None,
        update_methods: can_merge.then(|| UPDATE_METHODS.to_vec()),
        maintainer_edits: Some(can_edit_maintainer_edits),
    }
}

/// `getViewerPermissions`: straight from the host, as a write is let through on it.
pub async fn viewer_permissions(p: &Supported, number: u64) -> Result<ViewerPermissions, GhError> {
    let (raw, labels) = tokio::join!(read_merge_request(p, number), label_permission(p, number));
    let (raw, (can_edit_labels, _)) = (raw?, labels?);
    Ok(viewer_permissions_of(raw.viewer_can_merge(), can_edit_labels, false))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct GlProjectSettings {
    merge_method: Loose<String>,
    squash_option: Loose<String>,
}

/// `decodeProjectMergeCapabilitiesJson`.
fn merge_capabilities(raw: &GlProjectSettings) -> MergeCapabilities {
    let method = raw.merge_method.as_deref().unwrap_or("").trim().to_lowercase();
    let squash = raw.squash_option.as_deref().unwrap_or("").trim().to_lowercase();
    MergeCapabilities {
        merge: method == "merge",
        rebase: method == "rebase_merge" || method == "ff",
        squash: matches!(squash.as_str(), "always" | "default_on" | "default_off"),
    }
}

// ---------------------------------------------------------------------------
// Lists
// ---------------------------------------------------------------------------

/// What of a listing's filters GitLab narrows itself; the rest are applied to the rows read.
#[derive(Default)]
pub struct Narrowing<'a> {
    /// Groups of one label (a group of alternatives is left to the rows).
    pub labels: Vec<&'a str>,
    pub excluded_labels: Vec<&'a str>,
    /// `@me` stands for the viewer.
    pub author: Option<&'a str>,
    pub draft: Option<&'a str>,
    pub review: Option<&'a str>,
}

/// `listableLabels`: a comma would split a name in two, and `None` and `Any` mean something else to GitLab.
fn listable<'a>(names: &[&'a str]) -> Vec<&'a str> {
    names.iter().copied().filter(|n| !n.contains(',') && !n.trim().eq_ignore_ascii_case("none") && !n.trim().eq_ignore_ascii_case("any")).collect()
}

/// The REST list's query parameters (`stateParam`, `involvementParams`, `searchParams`, `filterParams`).
fn list_params(listing: &Listing<'_>, viewer: &str, narrowing: &Narrowing<'_>, per_page: usize) -> String {
    let mut params: Vec<(&str, String)> = vec![("state", if listing.state == "open" { "opened".into() } else { listing.state.to_owned() })];
    let involvement = listing.involvement.unwrap_or("all");
    match involvement {
        "authored" => params.push(("author_username", viewer.to_owned())),
        "reviewing" => params.push(("reviewer_username", viewer.to_owned())),
        _ => {}
    }
    if let Some(q) = listing.query.map(str::trim).filter(|q| !q.is_empty()) {
        params.push(("search", q.to_owned()));
    }
    let required = listable(&narrowing.labels);
    if !required.is_empty() {
        params.push(("labels", required.join(",")));
    }
    let excluded = listable(&narrowing.excluded_labels);
    if !excluded.is_empty() {
        params.push(("not[labels]", excluded.join(",")));
    }
    if let Some(author) = narrowing.author.filter(|_| involvement != "authored") {
        params.push(("author_username", if author == "@me" { viewer.to_owned() } else { author.to_owned() }));
    }
    if let Some(draft) = narrowing.draft {
        params.push(("wip", if draft == "only" { "yes".into() } else { "no".into() }));
    }
    match narrowing.review {
        Some("approved") => params.push(("approved_by_usernames[]", "Any".into())),
        Some("none") => params.push(("approved_by_ids[]", "None".into())),
        _ => {}
    }
    params.extend([
        ("order_by", "updated_at".into()),
        ("sort", "desc".into()),
        ("with_labels_details", "true".into()),
        ("per_page", per_page.to_string()),
        ("page", "1".into()),
    ]);
    let pairs: Vec<(&str, &str)> = params.iter().map(|(k, v)| (*k, v.as_str())).collect();
    glab::query(&pairs)
}

fn list_row(p: &Supported, raw: &GlMergeRequest, state: Option<&ListState>, observed: u64) -> ListRow {
    let author = actor(&raw.author);
    ListRow {
        author_login: author.as_ref().map(|a| a.login.clone()),
        review_request_logins: raw.review_request_logins(),
        entry: PullRequestListEntry {
            stack: None,
            provider: Provider::GitLab.kind().to_owned(),
            host: p.host.clone(),
            project_id: p.project_id.as_str().into(),
            project_title: p.title.clone(),
            repository: p.repository.clone(),
            number: raw.iid.unwrap_or(0),
            title: text(&raw.title),
            url: text(&raw.web_url),
            author,
            head_branch: text(&raw.source_branch),
            base_branch: text(&raw.target_branch),
            state: raw.state(),
            is_draft: raw.is_draft(),
            mergeability: raw.mergeability(),
            // REST carries no line counts; `pullRequests.listStats` reads them.
            additions: 0,
            deletions: 0,
            created_at: text(&raw.created_at).into(),
            updated_at: text(&raw.updated_at).into(),
            observed_at: Some(observed.into()),
            viewer_review_requested: false,
            labels: labels(&raw.labels),
            review_decision: state.and_then(|s| s.review_decision.clone()).flatten(),
            checks_state: state.and_then(|s| s.checks_state.clone()),
        },
    }
}

/// One page of a project's merge requests, most recently updated first, read at `observed`.
pub async fn list(p: &Supported, listing: &Listing<'_>, viewer: &str, narrowing: &Narrowing<'_>, observed: u64) -> Result<Vec<ListRow>, GhError> {
    let per_page = listing.fetch.clamp(1, MAX_PAGE_SIZE);
    let path = format!("{}/merge_requests?{}", project(p), list_params(listing, viewer, narrowing, per_page));
    let raw: Rows<GlMergeRequest> = get(p, &path).await?;
    let items: Vec<&GlMergeRequest> = rows(&raw).filter(|mr| mr.is_whole()).collect();
    let numbers: Vec<u64> = items.iter().filter_map(|mr| mr.iid.0).collect();
    let states = list_states(p, &numbers).await;
    Ok(items.into_iter().map(|mr| list_row(p, mr, mr.iid.0.and_then(|n| states.get(&n)), observed)).collect())
}

/// Lines added and removed by one merge request.
pub async fn line_counts(p: &Supported, number: u64) -> Result<(u64, u64), GhError> {
    stats(p, &[number]).await?.remove(&number).ok_or_else(|| GhError::failed("GitLab reported no line counts for the merge request."))
}

/// A project's labels, those matching `search` when there is one.
async fn project_labels(p: &Supported, search: &str) -> Result<(Vec<(PullRequestLabel, bool)>, bool), GhError> {
    let mut params = vec![("per_page", "100"), ("include_ancestor_groups", "true")];
    if !search.trim().is_empty() {
        params.push(("search", search.trim()));
    }
    let raw: Rows<GlLabel> = get(p, &format!("{}/labels?{}", project(p), glab::query(&params))).await?;
    let count = raw.as_ref().map_or(0, Vec::len);
    let labels = rows(&raw)
        .filter_map(|l| Some((PullRequestLabel { name: trimmed(&l.name)?, color: trimmed(&l.color), description: Some(trimmed(&l.description)) }, l.archived.0 == Some(true))))
        .collect();
    Ok((labels, count >= MAX_PAGE_SIZE))
}

/// `listRepositoryLabels`, for the label filter: name and colour.
pub async fn repository_labels(p: &Supported, search: &str) -> Result<(Vec<PullRequestLabel>, bool), GhError> {
    let (labels, truncated) = project_labels(p, search).await?;
    Ok((labels.into_iter().map(|(l, _)| PullRequestLabel { description: None, ..l }).collect(), truncated))
}

/// GitLab's user and issue searches answer nothing under three characters (`hostSearchParams`).
fn host_search(search: &str) -> Option<&str> {
    let search = search.trim();
    (search.chars().count() >= 3).then_some(search)
}

/// `projects/:id/users`, as reviewer candidates with their numeric ids.
async fn project_users(p: &Supported, search: &str) -> Result<(Vec<(u64, PullRequestActor)>, bool), GhError> {
    let mut params = vec![("per_page", "100")];
    if let Some(search) = host_search(search) {
        params.push(("search", search));
    }
    let raw: Rows<GlUser> = get(p, &format!("{}/users?{}", project(p), glab::query(&params))).await?;
    let count = raw.as_ref().map_or(0, Vec::len);
    Ok((rows(&raw).filter_map(|u| Some((u.id.0?, actor(u)?))).collect(), count >= MAX_PAGE_SIZE))
}

/// `searchRepositoryAuthors`.
pub async fn assignable_users(p: &Supported, search: &str) -> Result<(Vec<PullRequestActor>, bool), GhError> {
    let (users, truncated) = project_users(p, search).await?;
    Ok((users.into_iter().map(|(_, a)| a).collect(), truncated))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct GlReference {
    iid: Loose<u64>,
    title: Loose<String>,
    state: Loose<String>,
}

/// `referenceSearches`: recently updated issues and merge requests, by title
/// for text; a number also asks for that iid of each.
fn reference_searches(search: &str) -> Vec<Vec<(&'static str, String)>> {
    let text = search.trim();
    let numeric = !text.is_empty() && text.chars().all(|c| c.is_ascii_digit());
    let mut recent = vec![("order_by", "updated_at".to_owned()), ("sort", "desc".to_owned())];
    if !numeric {
        if let Some(search) = host_search(text) {
            recent.extend([("search", search.to_owned()), ("in", "title".to_owned())]);
        }
    }
    let mut searches = vec![];
    if numeric {
        searches.push(vec![("iids[]", text.to_owned())]);
    }
    searches.push(recent);
    searches
}

/// Issues (`#`) or merge requests (`!`) a composer can reference.
pub async fn references(p: &Supported, kind: ReferenceKind, search: &str) -> Result<Vec<ReferenceCandidate>, GhError> {
    let endpoint = if kind == ReferenceKind::Issue { "issues" } else { "merge_requests" };
    let sigil = if kind == ReferenceKind::Issue { "#" } else { "!" };
    let mut found = vec![];
    for mut params in reference_searches(search) {
        params.push(("per_page", REFERENCE_PAGE_SIZE.to_string()));
        let pairs: Vec<(&str, &str)> = params.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let raw: Rows<GlReference> = get(p, &format!("{}/{endpoint}?{}", project(p), glab::query(&pairs))).await?;
        for r in rows(&raw) {
            let (Some(iid), Some(title)) = (r.iid.0.filter(|n| *n > 0), r.title.0.clone()) else { continue };
            let state = match r.state.as_deref() {
                Some("merged") => PullRequestState::Merged,
                Some("opened") => PullRequestState::Open,
                _ => PullRequestState::Closed,
            };
            found.push(ReferenceCandidate { reference: format!("{sigil}{iid}"), kind, number: iid, title, state });
        }
    }
    Ok(found)
}

// ---------------------------------------------------------------------------
// One merge request
// ---------------------------------------------------------------------------

fn summary_of(p: &Supported, raw: &GlMergeRequest, counts: Option<(u64, u64)>, state: Option<&ListState>) -> PullRequestSummary {
    PullRequestSummary {
        provider: Provider::GitLab.kind().to_owned(),
        project_id: p.project_id.as_str().into(),
        repository: p.repository.clone(),
        number: raw.iid.unwrap_or(0),
        title: text(&raw.title),
        url: text(&raw.web_url),
        state: raw.state(),
        is_draft: Some(raw.is_draft()),
        head_branch: text(&raw.source_branch),
        base_branch: text(&raw.target_branch),
        closed_at: Some(trimmed(&raw.closed_at)),
        merged_at: Some(trimmed(&raw.merged_at)),
        updated_at: text(&raw.updated_at).into(),
        observed_at: Some(now_ms().into()),
        author: Some(actor(&raw.author)),
        additions: counts.map(|c| c.0),
        deletions: counts.map(|c| c.1),
        changed_files: Some(raw.changed_files()),
        review_decision: state.and_then(|s| s.review_decision.clone()),
        checks_state: state.and_then(|s| s.checks_state.clone()).map(Some),
        mergeability: Some(raw.mergeability()),
        stack: None,
        head_sha: trimmed(&raw.sha),
        auto_merge_enabled: raw.auto_merge(),
        auto_merge_method: raw.auto_merge_method(),
    }
}

/// `getChangeRequestSummary`: the merge request with its line counts and list state.
pub async fn summary(p: &Supported, number: u64) -> Result<PullRequestSummary, GhError> {
    let one = [number];
    let (raw, counts, states) = tokio::join!(read_merge_request(p, number), stats(p, &one), list_states(p, &one));
    let raw = raw?;
    let counts = optional(counts.map(|mut c| c.remove(&number)), None)?;
    Ok(summary_of(p, &raw, counts, states.get(&number)))
}

/// `getChangeRequest`: the merge request with its checks, project merge settings and what the viewer may do.
pub async fn detail(p: &Supported, number: u64) -> Result<PullRequestDetail, GhError> {
    let settings_path = format!("{}?license=false", project(p));
    let one = [number];
    let (raw, labels_read, counts, settings, identity) = tokio::join!(
        read_merge_request(p, number),
        label_permission(p, number),
        stats(p, &one),
        get::<Fields<GlProjectSettings>>(p, &settings_path),
        glab::identity(p.cwd(), &p.host)
    );
    let (raw, (can_edit_labels, coloured), settings) = (raw?, labels_read?, settings?);
    let counts = optional(counts.map(|mut c| c.remove(&number)), None)?;
    let checks = checks_of(p, number, &raw).await?;
    let state = raw.state();
    let diverged = raw.diverged_commits_count.0;
    Ok(PullRequestDetail {
        provider: Provider::GitLab.kind(),
        capabilities: capabilities(),
        viewer_permissions: viewer_permissions_of(raw.viewer_can_merge(), can_edit_labels, false),
        project_id: p.project_id.as_str().into(),
        project_title: p.title.clone(),
        workspace_root: p.workspace_root.clone(),
        repository: p.repository.clone(),
        number: raw.iid.unwrap_or(number),
        title: text(&raw.title),
        body: text(&raw.description),
        url: text(&raw.web_url),
        author: actor(&raw.author),
        state,
        is_draft: raw.is_draft(),
        mergeability: raw.mergeability(),
        additions: counts.map_or(0, |c| c.0),
        deletions: counts.map_or(0, |c| c.1),
        changed_files: raw.changed_files(),
        head_branch: text(&raw.source_branch),
        head_repository_name_with_owner: None,
        head_sha: trimmed(&raw.sha),
        base_branch: text(&raw.target_branch),
        created_at: text(&raw.created_at).into(),
        updated_at: text(&raw.updated_at).into(),
        observed_at: now_ms(),
        merged_at: trimmed(&raw.merged_at).map(Into::into),
        closed_at: trimmed(&raw.closed_at).map(Into::into),
        reviewers: rows(&raw.reviewers).filter_map(actor).collect(),
        labels: colour_labels(labels(&raw.labels), coloured),
        checks,
        merge_capabilities: merge_capabilities(&settings),
        rebase_required: raw.rebase_required(),
        behind_by: diverged,
        viewer: identity.ok().map(|(_, login)| login),
        // Missing divergence counts mean unknown, not up to date.
        base_comparison: match diverged {
            None => BaseComparison::Unknown,
            Some(0) => BaseComparison::UpToDate,
            Some(_) => BaseComparison::Behind,
        },
        auto_merge_enabled: raw.auto_merge() == Some(true),
        auto_merge_method: raw.auto_merge_method(),
        maintainer_can_modify: raw.maintainer_can_modify(),
    })
}

/// What a link to a merge request shows.
pub async fn preview(p: &Supported, number: u64) -> Result<PullRequestPreview, GhError> {
    let raw = read_merge_request(p, number).await?;
    Ok(PullRequestPreview {
        project_id: p.project_id.as_str().into(),
        repository: p.repository.clone(),
        number: raw.iid.unwrap_or(number),
        title: text(&raw.title),
        url: text(&raw.web_url),
        author: actor(&raw.author),
        state: raw.state(),
        is_draft: raw.is_draft(),
        created_at: text(&raw.created_at).into(),
    })
}

/// `getChangeRequestChecks`: the head pipeline's checks, with whether the merge request is still open.
pub async fn pull_request_checks(p: &Supported, number: u64) -> Result<PullRequestChecks, GhError> {
    let raw = read_merge_request(p, number).await?;
    let checks = checks_of(p, number, &raw).await?;
    Ok(PullRequestChecks { observed_at: Some(now_ms().into()), state: raw.state(), checks })
}

async fn diff_refs(p: &Supported, number: u64) -> Result<DiffRefs, GhError> {
    let raw = read_merge_request(p, number).await?;
    raw.diff_refs.refs().ok_or_else(|| GhError::failed("The merge request reported no diff revisions."))
}

/// The head commit of a merge request, when GitLab reports one.
pub async fn head_sha(p: &Supported, number: u64) -> Result<Option<String>, GhError> {
    Ok(trimmed(&read_merge_request(p, number).await?.sha))
}

/// A merge request's head, target branch and text, for its review guide.
pub async fn guide_source(p: &Supported, number: u64) -> Result<github::GuideSource, GhError> {
    let raw = read_merge_request(p, number).await?;
    Ok(github::GuideSource {
        head: trimmed(&raw.sha),
        base_branch: trimmed(&raw.target_branch).unwrap_or_else(|| "main".into()),
        title: text(&raw.title),
        body: text(&raw.description),
    })
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct FileStat {
    path: Loose<String>,
    additions: Loose<u64>,
    deletions: Loose<u64>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct FileCount {
    #[serde(rename = "fileCount")]
    file_count: Loose<u64>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct FileStatsNode {
    diff_stats_summary: Option<Fields<FileCount>>,
    diff_stats: Rows<FileStat>,
}

/// `getFileStats` → `toChangeBreakdown`: `None` when GitLab listed fewer files than it counted.
pub async fn change_breakdown(p: &Supported, number: u64) -> Result<Option<ChangeBreakdown>, GhError> {
    let data: ProjectOf<MergeRequestOf<FileStatsNode>> = graphql(p, FILE_STATS_QUERY, number).await?;
    let Some(node) = data.project.merge_request.as_ref() else { return Ok(None) };
    let files: Vec<&FileStat> = rows(&node.diff_stats).collect();
    let counted = node.diff_stats_summary.as_ref().and_then(|s| s.file_count.0);
    if counted.is_none_or(|n| (files.len() as u64) < n) {
        return Ok(None);
    }
    Ok(Some(github::breakdown(files.iter().map(|f| (f.path.as_deref().unwrap_or(""), f.additions.unwrap_or(0), f.deletions.unwrap_or(0))))))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct GlCommitRefs {
    id: Loose<String>,
    parent_ids: Loose<Vec<String>>,
}

/// A file's text at a revision; a binary or oversized one is refused.
async fn file_at(p: &Supported, path: &str, revision: &str) -> Result<String, GhError> {
    let endpoint = format!("{}/repository/files/{}/raw?ref={}", project(p), glab::encode(path), glab::encode(revision));
    let contents = glab::api(p.cwd(), &p.host, &Api::get(&endpoint).slow()).await?;
    if contents.len() > DIFF_FILE_MAX_BYTES {
        return Err(GhError::failed(format!("The diff file '{path}' exceeds the 1 MB expansion limit.")));
    }
    if contents.contains('\0') || contents.contains('\u{FFFD}') {
        return Err(GhError::failed(format!("The diff file '{path}' is binary.")));
    }
    Ok(contents)
}

/// A changed file before and after.
pub async fn diff_file_contents(p: &Supported, number: u64, file: &FileOfDiff<'_>) -> Result<DiffFileContents, GhError> {
    let refs = match file.commit {
        Some(commit) => {
            if commit.len() < 7 || !commit.chars().all(|c| c.is_ascii_hexdigit()) {
                return Err(GhError::failed("The named commit was not a commit sha."));
            }
            let raw: Fields<GlCommitRefs> = get(p, &format!("{}/repository/commits/{commit}", project(p))).await?;
            let parent = raw.parent_ids.iter().flatten().next().map(|s| s.trim().to_owned()).filter(|s| !s.is_empty());
            match (parent, trimmed(&raw.id)) {
                (Some(base), Some(head)) => DiffRefs { base: base.clone(), head, start: base },
                (None, Some(head)) if file.change == "new" => DiffRefs { base: String::new(), head, start: String::new() },
                _ => return Err(GhError::failed(format!("Commit {commit} reported no parent revision."))),
            }
        }
        None => diff_refs(p, number).await?,
    };
    let old_contents = if file.change == "new" { String::new() } else { file_at(p, file.old_path, &refs.base).await? };
    let new_contents = if file.change == "deleted" { String::new() } else { file_at(p, file.new_path, &refs.head).await? };
    Ok(DiffFileContents { old_contents, new_contents })
}

// ---------------------------------------------------------------------------
// Activity
// ---------------------------------------------------------------------------

/// GitLab's award name of each reaction (`GITLAB_AWARD_BY_CONTENT`).
const AWARDS: [(&str, &str, ReactionContent); 8] = [
    ("thumbs-up", "thumbsup", ReactionContent::ThumbsUp),
    ("thumbs-down", "thumbsdown", ReactionContent::ThumbsDown),
    ("laugh", "laughing", ReactionContent::Laugh),
    ("hooray", "tada", ReactionContent::Hooray),
    ("confused", "confused", ReactionContent::Confused),
    ("heart", "heart", ReactionContent::Heart),
    ("rocket", "rocket", ReactionContent::Rocket),
    ("eyes", "eyes", ReactionContent::Eyes),
];

/// GitLab's award name (`thumbsup`) of a reaction a client names in t3code's words (`thumbs-up`).
pub fn award_name(content: &str) -> Option<&'static str> {
    AWARDS.iter().find(|(named, _, _)| *named == content).map(|(_, award, _)| *award)
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Award {
    id: Loose<u64>,
    name: Loose<String>,
    user: Fields<Username>,
}

/// `toReactions`: awards grouped by reaction, in the order first given; the
/// viewer counts but is not named among the actors.
fn to_reactions<'a>(awards: impl Iterator<Item = &'a Award>, viewer: Option<&str>) -> Vec<Reaction> {
    let viewer = viewer.map(str::to_lowercase);
    let mut groups: Vec<Reaction> = vec![];
    for award in awards {
        let name = award.name.as_deref().unwrap_or("").trim().to_lowercase();
        let Some(content) = AWARDS.iter().find(|(_, n, _)| *n == name).map(|(_, _, c)| *c) else { continue };
        let Some(user) = trimmed(&award.user.username) else { continue };
        let index = match groups.iter().position(|g| g.content == content) {
            Some(i) => i,
            None => {
                groups.push(Reaction { content, count: 0, actors: vec![], viewer_has_reacted: false });
                groups.len() - 1
            }
        };
        let group = &mut groups[index];
        group.count += 1;
        if viewer.as_deref() == Some(user.to_lowercase().as_str()) {
            group.viewer_has_reacted = true;
        } else {
            group.actors.push(user);
        }
    }
    groups
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct NoteAuthor {
    username: Loose<String>,
    bot: Loose<bool>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct AwardNote {
    id: Loose<String>,
    author: Fields<NoteAuthor>,
    last_edited_at: Loose<String>,
    last_edited_by: Option<Fields<Username>>,
    resolved: Loose<bool>,
    award_emoji: Fields<Nodes<Award>>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct AwardMergeRequest {
    award_emoji: Fields<Nodes<Award>>,
    notes: Fields<Paged<AwardNote>>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct AwardData {
    current_user: Fields<Username>,
    project: Fields<MergeRequestOf<AwardMergeRequest>>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AwardVariables<'a> {
    full_path: &'a str,
    iid: String,
    cursor: Option<String>,
}

/// The awards of a merge request and its notes (`listReactions`).
#[derive(Default)]
struct Awards {
    viewer: Option<String>,
    reactions: Vec<Reaction>,
    by_note: HashMap<String, Vec<Reaction>>,
    edited_at: HashMap<String, String>,
    bots: HashSet<String>,
}

/// `noteIdOf`: the number at the end of a note's global id.
fn note_id_of(gid: &Loose<String>) -> Option<String> {
    let id = trimmed(gid)?.rsplit('/').next()?.to_owned();
    (!id.is_empty() && id.chars().all(|c| c.is_ascii_digit())).then_some(id)
}

/// `textEditedAt`: resolving moves `lastEditedAt` on every note, so only an edit by someone counts.
fn text_edited_at(note: &AwardNote) -> Option<String> {
    let edited = trimmed(&note.last_edited_at)?;
    let by = note.last_edited_by.as_ref().and_then(|u| trimmed(&u.username));
    (by.is_some() && note.resolved.0 != Some(true)).then_some(edited)
}

/// One page of `AWARD_EMOJI_GRAPHQL_QUERY` into `awards`; the cursor of the next page.
fn read_award_page(data: &AwardData, awards: &mut Awards, first: bool) -> Option<String> {
    let viewer = trimmed(&data.current_user.username);
    let merge_request = data.project.merge_request.as_ref();
    if first {
        awards.reactions = merge_request.map(|mr| to_reactions(rows(&mr.award_emoji.nodes), viewer.as_deref())).unwrap_or_default();
        awards.viewer = viewer.clone();
    }
    let notes = merge_request.map(|mr| &mr.notes);
    for note in notes.into_iter().flat_map(|n| rows(&n.nodes)) {
        if let (Some(author), Some(true)) = (trimmed(&note.author.username), note.author.bot.0) {
            awards.bots.insert(author.to_lowercase());
        }
        let Some(id) = note_id_of(&note.id) else { continue };
        let reactions = to_reactions(rows(&note.award_emoji.nodes), viewer.as_deref());
        if !reactions.is_empty() {
            awards.by_note.insert(id.clone(), reactions);
        }
        if let Some(at) = text_edited_at(note) {
            awards.edited_at.insert(id, at);
        }
    }
    let page = notes.map(|n| &n.page_info)?;
    (page.has_next_page.0 == Some(true)).then(|| trimmed(&page.end_cursor)).flatten()
}

async fn list_awards(p: &Supported, number: u64) -> Result<Awards, GhError> {
    let mut awards = Awards::default();
    let mut cursor: Option<String> = None;
    for page in 0..CONVERSATION_PAGES {
        let vars = AwardVariables { full_path: p.repository.trim(), iid: number.to_string(), cursor: cursor.clone() };
        let data: AwardData = glab::graphql(p.cwd(), &p.host, AWARD_EMOJI_QUERY, vars).await?;
        match read_award_page(&data, &mut awards, page == 0) {
            Some(next) => cursor = Some(next),
            None => break,
        }
    }
    Ok(awards)
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct NotePosition {
    position_type: Loose<String>,
    new_path: Loose<String>,
    old_path: Loose<String>,
    new_line: Loose<u64>,
    old_line: Loose<u64>,
    base_sha: Loose<String>,
    start_sha: Loose<String>,
    head_sha: Loose<String>,
}

/// A note of a merge request or of one of its discussions (`RawNoteSchema`, `RawDiscussionNoteSchema`).
#[derive(Deserialize, Default)]
#[serde(default)]
struct GlNote {
    id: Loose<u64>,
    body: Loose<String>,
    author: Fields<GlUser>,
    created_at: Loose<String>,
    system: Loose<bool>,
    #[serde(rename = "type")]
    kind: Loose<String>,
    resolvable: Loose<bool>,
    resolved: Loose<bool>,
    position: Option<Fields<NotePosition>>,
}

impl GlNote {
    fn is_whole(&self) -> bool {
        self.id.is_some() && self.created_at.is_some()
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct GlDiscussion {
    id: Loose<String>,
    notes: Rows<GlNote>,
}

/// A comment as GitLab notes it, before the activity's awards are joined in.
struct NoteComment {
    id: String,
    kind: CommentKind,
    author: Option<PullRequestActor>,
    body: String,
    created_at: String,
    path: Option<String>,
}

/// `decodeNotesJson`: what people wrote; GitLab's own notes and blank ones are left out.
fn note_comments(raw: &Rows<GlNote>) -> Vec<NoteComment> {
    rows(raw)
        .filter(|n| n.is_whole() && n.system.0 != Some(true))
        .filter(|n| !n.body.as_deref().unwrap_or("").trim().is_empty())
        .map(|n| NoteComment {
            id: n.id.unwrap_or(0).to_string(),
            kind: if n.kind.as_deref().map(str::trim) == Some("DiffNote") { CommentKind::ReviewComment } else { CommentKind::IssueComment },
            author: actor(&n.author),
            body: text(&n.body),
            created_at: text(&n.created_at),
            path: n.position.as_ref().and_then(|pos| trimmed(&pos.new_path).or_else(|| trimmed(&pos.old_path))),
        })
        .collect()
}

/// A thread of `decodeDiscussionsJson`, before the activity's awards are joined in.
struct Discussion {
    id: String,
    /// `None` for a thread on the merge request itself.
    path: Option<String>,
    line: Option<u64>,
    side: DiffSide,
    resolved: bool,
    refs: Option<DiffRefs>,
    comments: Vec<NoteComment>,
}

/// `decodeDiscussionsJson`: threads on lines and files, and resolvable ones on
/// the merge request; the ids of the unresolved ones of those (overview findings).
fn discussions_of(raw: &Rows<GlDiscussion>) -> (Vec<Discussion>, Vec<String>) {
    let mut threads = vec![];
    let mut findings = vec![];
    for discussion in rows(raw) {
        let Some(id) = trimmed(&discussion.id) else { continue };
        let notes: Vec<&GlNote> = rows(&discussion.notes).filter(|n| n.is_whole() && n.system.0 != Some(true)).collect();
        let Some(root) = notes.first() else { continue };
        let comments = notes
            .iter()
            .map(|n| NoteComment {
                id: n.id.unwrap_or(0).to_string(),
                kind: CommentKind::ReviewComment,
                author: actor(&n.author),
                body: text(&n.body),
                created_at: text(&n.created_at),
                path: None,
            })
            .collect();
        let resolved = root.resolved.0 == Some(true);
        let Some(position) = &root.position else {
            // A thread started on the overview: resolvable, unlike a single comment.
            if root.resolvable.0 != Some(true) {
                continue;
            }
            if !resolved {
                findings.push(root.id.unwrap_or(0).to_string());
            }
            threads.push(Discussion { id, path: None, line: None, side: DiffSide::Right, resolved, refs: None, comments });
            continue;
        };
        let (side, path, line) = match position.position_type.as_deref() {
            Some("text") => {
                if position.new_line.is_none() {
                    (DiffSide::Left, trimmed(&position.old_path), position.old_line.0)
                } else {
                    (DiffSide::Right, trimmed(&position.new_path), position.new_line.0)
                }
            }
            Some("file" | "image") => (DiffSide::Right, trimmed(&position.new_path).or_else(|| trimmed(&position.old_path)), None),
            _ => continue,
        };
        let Some(path) = path else { continue };
        let refs = match (trimmed(&position.base_sha), trimmed(&position.start_sha), trimmed(&position.head_sha)) {
            (Some(base), Some(start), Some(head)) => Some(DiffRefs { base, head, start }),
            _ => None,
        };
        threads.push(Discussion { id, path: Some(path), line: line.filter(|l| *l > 0), side, resolved, refs, comments });
    }
    (threads, findings)
}

/// Pages of a list endpoint, up to `CONVERSATION_PAGES`; whether more were left unread.
async fn pages<T: serde::de::DeserializeOwned + Default>(p: &Supported, path: &str, extra: &[(&str, &str)]) -> Result<(Vec<Fields<T>>, bool), GhError> {
    let mut all = vec![];
    for page in 1..=CONVERSATION_PAGES {
        let page_text = page.to_string();
        let mut params = vec![("per_page", "100"), ("page", page_text.as_str())];
        params.extend_from_slice(extra);
        let raw: Loose<Vec<Fields<T>>> = get(p, &format!("{path}?{}", glab::query(&params))).await?;
        let got = raw.0.unwrap_or_default();
        let full = got.len() >= MAX_PAGE_SIZE;
        all.extend(got);
        if !full {
            return Ok((all, false));
        }
    }
    Ok((all, true))
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct GlCommitStats {
    additions: Loose<u64>,
    deletions: Loose<u64>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct GlCommit {
    id: Loose<String>,
    title: Loose<String>,
    committed_date: Loose<String>,
    created_at: Loose<String>,
    author_name: Loose<String>,
    author_email: Loose<String>,
    stats: Option<Fields<GlCommitStats>>,
}

/// `decodeCommitsJson`: oldest first.
fn commits_of(raw: &Rows<GlCommit>) -> Vec<Commit> {
    let mut commits: Vec<Commit> = rows(raw)
        .filter_map(|c| {
            let oid = trimmed(&c.id)?;
            let committed = trimmed(&c.committed_date).or_else(|| trimmed(&c.created_at))?;
            let login = trimmed(&c.author_name).or_else(|| trimmed(&c.author_email));
            Some(Commit {
                oid,
                message_headline: text(&c.title),
                committed_date: committed.into(),
                additions: c.stats.as_ref().map(|s| s.additions.unwrap_or(0)),
                deletions: c.stats.as_ref().map(|s| s.deletions.unwrap_or(0)),
                authors: login.map(|login| PullRequestActor { is_bot: None, login, name: trimmed(&c.author_name), avatar_url: None }).into_iter().collect(),
            })
        })
        .collect();
    commits.reverse();
    commits
}

/// What was said and committed on a merge request (`getChangeRequestActivity`).
pub async fn activity(p: &Supported, number: u64) -> Result<Option<PullRequestActivity>, GhError> {
    let base = merge_request(p, number);
    let notes_path = format!("{base}/notes");
    let discussions_path = format!("{base}/discussions");
    let commits_path = format!("{base}/commits?per_page=100&with_stats=true");
    let (notes, discussions, commits, awards, detail, identity) = tokio::join!(
        pages::<GlNote>(p, &notes_path, &[("order_by", "created_at"), ("sort", "asc")]),
        pages::<GlDiscussion>(p, &discussions_path, &[]),
        get::<Rows<GlCommit>>(p, &commits_path),
        list_awards(p, number),
        read_merge_request(p, number),
        glab::identity(p.cwd(), &p.host)
    );
    let detail = match detail {
        Err(e) if e.reason == GhReason::NotFound => return Ok(None),
        other => optional(other.map(Some), None)?,
    };
    let (notes, notes_truncated) = optional(notes, (vec![], true))?;
    let (discussions, discussions_truncated) = optional(discussions, (vec![], true))?;
    let commits = optional(commits, Loose(None))?;
    let awards = optional(awards, Awards::default())?;
    let viewer = awards.viewer.clone().or_else(|| identity.ok().map(|(_, login)| login)).map(|v| v.to_lowercase());
    let url = detail.as_ref().and_then(|d| trimmed(&d.web_url));
    let current_refs = detail.as_ref().and_then(|d| d.diff_refs.refs());

    let notes = Loose(Some(notes));
    let (threads, findings) = discussions_of(&Loose(Some(discussions)));
    let findings: HashSet<String> = findings.into_iter().collect();
    let with_bot = |author: Option<PullRequestActor>| {
        author.map(|a| if awards.bots.contains(&a.login.to_lowercase()) { PullRequestActor { is_bot: Some(true), ..a } } else { a })
    };
    let note_url = |id: &str| url.as_ref().map(|u| format!("{u}#note_{id}"));
    let mine = |author: &Option<PullRequestActor>| viewer.as_deref().is_some_and(|v| author.as_ref().is_some_and(|a| a.login.to_lowercase() == v));
    let thread_comment = |c: NoteComment| ThreadComment {
        edited_at: awards.edited_at.get(&c.id).cloned().map(Into::into),
        url: note_url(&c.id),
        review_url: None,
        reactions: awards.by_note.get(&c.id).cloned().unwrap_or_default(),
        can_edit: mine(&c.author),
        author: with_bot(c.author),
        body: c.body,
        created_at: c.created_at.into(),
        id: c.id,
    };

    let comments: Vec<Comment> = note_comments(&notes)
        .into_iter()
        .map(|c| Comment {
            kind: if findings.contains(&c.id) { CommentKind::ReviewComment } else { c.kind },
            edited_at: awards.edited_at.get(&c.id).cloned().map(Into::into),
            url: note_url(&c.id),
            review_state: None,
            reactions: awards.by_note.get(&c.id).cloned().unwrap_or_default(),
            can_edit: mine(&c.author),
            author: with_bot(c.author),
            body: c.body,
            created_at: c.created_at.into(),
            path: c.path,
            id: c.id,
        })
        .collect();
    let mut review_threads = vec![];
    let mut overview_threads = vec![];
    for thread in threads {
        let count = thread.comments.len() as u64;
        let thread_comments: Vec<ThreadComment> = thread.comments.into_iter().map(thread_comment).collect();
        match thread.path {
            Some(path) => review_threads.push(ReviewThread {
                id: thread.id,
                path,
                line: thread.line,
                side: thread.side,
                is_resolved: thread.resolved,
                // GitLab keeps a thread on the latest diff while its line survives, so older refs mean the code moved.
                is_outdated: matches!((&thread.refs, &current_refs), (Some(t), Some(c)) if t != c),
                comments: thread_comments,
                comment_count: count,
                next_comments_cursor: None,
                can_resolve: true,
            }),
            None => overview_threads.push(OverviewThread {
                id: thread.id,
                path: None,
                line: None,
                side: thread.side,
                is_resolved: thread.resolved,
                is_outdated: false,
                comments: thread_comments,
                comment_count: count,
                next_comments_cursor: None,
                can_resolve: true,
            }),
        }
    }
    Ok(Some(PullRequestActivity {
        author: detail.as_ref().and_then(|d| actor(&d.author)),
        reviewers: detail.as_ref().map(|d| rows(&d.reviewers).filter_map(actor).collect()).unwrap_or_default(),
        comment_count: comments.len() as u64,
        comments,
        comments_truncated: notes_truncated || discussions_truncated,
        review_threads_truncated: discussions_truncated,
        review_threads,
        overview_threads: Some(overview_threads),
        commits: commits_of(&commits),
        reactions: awards.reactions,
    }))
}

// ---------------------------------------------------------------------------
// A merge request made from a branch (`git_workflow`)
// ---------------------------------------------------------------------------

/// A merge request as making or finding one names it.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct MadeMergeRequest {
    #[serde(rename = "iid")]
    pub number: u64,
    #[serde(rename = "web_url")]
    pub url: String,
    #[serde(default)]
    pub title: String,
    #[serde(default, rename = "target_branch")]
    pub base_branch: Option<String>,
    #[serde(default, rename = "source_branch")]
    pub head_branch: Option<String>,
}

#[derive(Serialize)]
struct NewMergeRequest<'a> {
    source_branch: &'a str,
    target_branch: &'a str,
    title: String,
    description: &'a str,
}

/// What a new merge request is made of.
pub struct MergeRequestDraft<'a> {
    pub source_branch: &'a str,
    pub target_branch: &'a str,
    pub title: &'a str,
    pub description: &'a str,
    /// GitLab marks a draft in the title.
    pub draft: bool,
}

/// The open merge request of `branch` in `repository`, if it has one.
pub async fn find_open_merge_request(cwd: Option<&str>, host: &str, repository: &str, branch: &str) -> Result<Option<MadeMergeRequest>, GhError> {
    let path = format!("projects/{}/merge_requests?{}", glab::project_path(repository), glab::query(&[("state", "opened"), ("source_branch", branch), ("per_page", "1")]));
    let found: Vec<MadeMergeRequest> = glab::api_json(cwd, host, &Api::get(&path)).await?;
    Ok(found.into_iter().next())
}

/// Opens a merge request from a pushed branch.
pub async fn create_merge_request(cwd: Option<&str>, host: &str, repository: &str, new: &MergeRequestDraft<'_>) -> Result<MadeMergeRequest, GhError> {
    let path = format!("projects/{}/merge_requests", glab::project_path(repository));
    let already = new.title.trim_start().to_lowercase().starts_with("draft:");
    let title = if new.draft && !already { format!("Draft: {}", new.title) } else { new.title.to_owned() };
    let body = NewMergeRequest { source_branch: new.source_branch, target_branch: new.target_branch, title, description: new.description };
    glab::api_json(cwd, host, &Api::send("POST", &path, &body)?.slow()).await
}

// ---------------------------------------------------------------------------
// Writes
// ---------------------------------------------------------------------------

async fn mr_command(p: &Supported, subcommand: &str, number: u64, flags: &[&str]) -> Result<(), GhError> {
    let n = number.to_string();
    let mut args = vec!["mr", subcommand, n.as_str(), "--repo", p.repository.as_str()];
    args.extend_from_slice(flags);
    glab::run(p.cwd(), &p.host, &args, None, glab::SLOW_TIMEOUT).await.map(|_| ())
}

/// `runMergeRequestAction`, for the actions of [`ACTIONS`].
pub async fn run_action(p: &Supported, number: u64, action: &PullRequestAction, merge_method: Option<&str>) -> Result<(), GhError> {
    let squash: &[&str] = if merge_method == Some("squash") { &["--squash"] } else { &[] };
    match action {
        PullRequestAction::Merge | PullRequestAction::EnableAutoMerge => {
            if merge_method == Some("rebase") {
                // A rebase changes the source SHA, so it is waited for before the merge guard is read.
                mr_command(p, "rebase", number, &[]).await?;
            }
            let head = diff_refs(p, number).await?.head;
            // glab defaults to auto-merge during a running pipeline.
            let auto = if *action == PullRequestAction::Merge { "--auto-merge=false" } else { "--auto-merge=true" };
            let mut flags = vec![auto, "--yes"];
            flags.extend_from_slice(squash);
            flags.extend(["--sha", head.as_str()]);
            mr_command(p, "merge", number, &flags).await
        }
        // `glab mr merge` arms auto-merge and never disarms it.
        PullRequestAction::DisableAutoMerge => {
            let path = format!("{}/cancel_merge_when_pipeline_succeeds", merge_request(p, number));
            write(p, Api::call("POST", &path)).await
        }
        PullRequestAction::Ready => mr_command(p, "update", number, &["--ready"]).await,
        PullRequestAction::Draft => mr_command(p, "update", number, &["--draft"]).await,
        PullRequestAction::Close => mr_command(p, "close", number, &[]).await,
        PullRequestAction::Reopen => mr_command(p, "reopen", number, &[]).await,
        PullRequestAction::UpdateBranch => mr_command(p, "rebase", number, &[]).await,
        other => Err(GhError::failed(format!("GitLab merge request action {} is unsupported here.", other.as_str()))),
    }
}

#[derive(Serialize, Default)]
struct MergeRequestUpdate<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    allow_collaboration: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    add_labels: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    remove_labels: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reviewer_ids: Option<Vec<u64>>,
}

/// `updateMergeRequest`: omitted fields remain unchanged, empty ones clear them.
pub async fn update(p: &Supported, number: u64, title: Option<&str>, body: Option<&str>, maintainer_can_modify: Option<bool>) -> Result<(), GhError> {
    let path = merge_request(p, number);
    let change = MergeRequestUpdate { title, description: body, allow_collaboration: maintainer_can_modify, ..Default::default() };
    write(p, Api::send("PUT", &path, &change)?).await
}

#[derive(Serialize)]
struct NoteBody<'a> {
    body: &'a str,
}

/// `commentOnMergeRequest`.
pub async fn comment(p: &Supported, number: u64, body: &str) -> Result<(), GhError> {
    let path = format!("{}/notes", merge_request(p, number));
    write(p, Api::send("POST", &path, &NoteBody { body })?).await
}

/// `updateNote`.
pub async fn update_comment(p: &Supported, number: u64, note_id: &str, body: &str) -> Result<(), GhError> {
    let path = format!("{}/notes/{}", merge_request(p, number), glab::encode(note_id));
    write(p, Api::send("PUT", &path, &NoteBody { body })?).await
}

/// `replyToDiscussion`.
pub async fn reply_to_thread(p: &Supported, number: u64, discussion_id: &str, body: &str) -> Result<(), GhError> {
    let path = format!("{}/discussions/{}/notes", merge_request(p, number), glab::encode(discussion_id));
    write(p, Api::send("POST", &path, &NoteBody { body })?).await
}

#[derive(Serialize)]
struct Resolution {
    resolved: bool,
}

/// `setDiscussionResolution`.
pub async fn set_thread_resolution(p: &Supported, number: u64, discussion_id: &str, resolved: bool) -> Result<(), GhError> {
    let path = format!("{}/discussions/{}", merge_request(p, number), glab::encode(discussion_id));
    write(p, Api::send("PUT", &path, &Resolution { resolved })?).await
}

/// `setReaction`: an award on the merge request or one of its notes. GitLab
/// deletes an award by its id, so the reader's own award of that name is found first.
pub async fn set_reaction(p: &Supported, number: u64, note_id: Option<&str>, award: &str, reacted: bool) -> Result<(), GhError> {
    let subject = match note_id {
        Some(note) => format!("{}/notes/{}/award_emoji", merge_request(p, number), glab::encode(note)),
        None => format!("{}/award_emoji", merge_request(p, number)),
    };
    if reacted {
        let path = format!("{subject}?{}", glab::query(&[("name", award)]));
        return write(p, Api::call("POST", &path)).await;
    }
    let (_, viewer) = glab::identity(p.cwd(), &p.host).await?;
    for page in 1.. {
        let page_text = page.to_string();
        let listed: Rows<Award> = get(p, &format!("{subject}?{}", glab::query(&[("per_page", "100"), ("page", &page_text)]))).await?;
        let own = rows(&listed).find(|a| {
            a.name.as_deref().map(|n| n.trim().to_lowercase()).as_deref() == Some(award) && trimmed(&a.user.username).as_deref() == Some(viewer.as_str())
        });
        if let Some(id) = own.and_then(|a| a.id.0) {
            return write(p, Api::call("DELETE", &format!("{subject}/{id}"))).await;
        }
        // An empty page establishes exhaustion even when the host caps page size.
        if listed.as_ref().is_none_or(Vec::is_empty) {
            return Ok(());
        }
    }
    Ok(())
}

/// `listReviewerCandidates`: the requested reviewers lead (from the merge
/// request, which a page of users need not reach), then the project's users;
/// the author is dropped, as GitLab refuses to make them a reviewer.
pub async fn reviewer_candidates(p: &Supported, number: u64, query: &str) -> Result<ReviewerCandidates, GhError> {
    let (mr, users) = tokio::join!(read_merge_request(p, number), project_users(p, query));
    let (mr, (users, truncated)) = (mr?, users?);
    let author = trimmed(&mr.author.username);
    let requested_ids: HashSet<u64> = rows(&mr.reviewers).filter_map(|r| r.id.0).collect();
    let mut candidates: Vec<ReviewerCandidate> = vec![];
    if query.trim().is_empty() {
        for reviewer in rows(&mr.reviewers) {
            if let (Some(id), Some(actor)) = (reviewer.id.0, actor(reviewer)) {
                candidates.push(ReviewerCandidate { actor, id: id.to_string(), kind: super::pull_request_records::ReviewerKind::User, is_requested: true });
            }
        }
    }
    for (id, actor) in users {
        if author.as_deref() == Some(actor.login.as_str()) || candidates.iter().any(|c| c.id == id.to_string()) {
            continue;
        }
        candidates.push(ReviewerCandidate { actor, id: id.to_string(), kind: super::pull_request_records::ReviewerKind::User, is_requested: requested_ids.contains(&id) });
    }
    Ok(ReviewerCandidates { candidates, truncated })
}

/// `setReviewerRequest`: GitLab replaces `reviewer_ids` wholesale.
pub async fn request_reviewers(p: &Supported, number: u64, ids: &[&str], requested: bool) -> Result<(), GhError> {
    let mr = read_merge_request(p, number).await?;
    let mut set: Vec<u64> = rows(&mr.reviewers).filter_map(|r| r.id.0).collect();
    for id in ids.iter().filter_map(|id| id.trim().parse::<u64>().ok()).filter(|id| *id > 0) {
        if requested {
            if !set.contains(&id) {
                set.push(id);
            }
        } else {
            set.retain(|known| *known != id);
        }
    }
    let change = MergeRequestUpdate { reviewer_ids: Some(set), ..Default::default() };
    write(p, Api::send("PUT", &merge_request(p, number), &change)?).await
}

/// `listLabelCandidates`: GitLab's picker hides archived labels; applied ones
/// stay so they can be removed, as do applied labels past the first page.
pub async fn label_candidates(p: &Supported, number: u64) -> Result<LabelCandidates, GhError> {
    let (mr, project) = tokio::join!(read_merge_request(p, number), project_labels(p, ""));
    let (mr, (project, truncated)) = (mr?, project?);
    let applied = labels(&mr.labels);
    let applied_names: HashSet<&str> = applied.iter().map(|l| l.name.as_str()).collect();
    let listed: HashSet<&str> = project.iter().map(|(l, _)| l.name.as_str()).collect();
    let mut candidates: Vec<LabelCandidate> = applied
        .iter()
        .filter(|l| !listed.contains(l.name.as_str()))
        .map(|l| LabelCandidate { name: l.name.clone(), color: l.color.clone(), description: None, is_applied: true })
        .collect();
    for (label, archived) in &project {
        if *archived && !applied_names.contains(label.name.as_str()) {
            continue;
        }
        candidates.push(LabelCandidate {
            name: label.name.clone(),
            color: label.color.clone(),
            description: label.description.clone().flatten(),
            is_applied: applied_names.contains(label.name.as_str()),
        });
    }
    Ok(LabelCandidates { candidates, truncated })
}

/// `setLabels`: GitLab would create a label it is asked to add that the
/// project lacks, so each is looked up first; unknown ones refuse the change.
pub async fn set_labels(p: &Supported, number: u64, names: &[&str], applied: bool) -> Result<(), GhError> {
    if applied {
        let mut unknown = vec![];
        for name in names {
            let path = format!("{}/labels/{}?include_ancestor_groups=true", project(p), glab::encode(name));
            // The lookup also matches a numeric id, so the name answered must be the one asked for.
            let found = match get::<Fields<GlLabel>>(p, &path).await {
                Ok(label) => trimmed(&label.name).as_deref() == Some(*name),
                Err(e) if e.reason == GhReason::NotFound => false,
                Err(e) => return Err(e),
            };
            if !found {
                unknown.push(format!("\"{name}\""));
            }
        }
        if !unknown.is_empty() {
            return Err(GhError::failed(format!("The project has no label named {}.", unknown.join(", "))));
        }
    }
    let joined = names.join(",");
    let change = if applied {
        MergeRequestUpdate { add_labels: Some(joined), ..Default::default() }
    } else {
        MergeRequestUpdate { remove_labels: Some(joined), ..Default::default() }
    };
    write(p, Api::send("PUT", &merge_request(p, number), &change)?).await
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ReviewTarget<'a> {
    project_path: &'a str,
    iid: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    reviewers: Option<Vec<String>>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct MutationPayload {
    errors: Loose<Vec<String>>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct MutationData {
    merge_request_set_reviewers: Option<Fields<MutationPayload>>,
    merge_request_request_changes: Option<Fields<MutationPayload>>,
}

/// A GraphQL mutation whose payload's `errors` refuse it (`graphqlMutation`).
async fn mutation(p: &Supported, query: &str, target: ReviewTarget<'_>) -> Result<(), GhError> {
    let data: MutationData = glab::graphql(p.cwd(), &p.host, query, target).await?;
    let reasons: Vec<String> = [&data.merge_request_set_reviewers, &data.merge_request_request_changes]
        .into_iter()
        .flatten()
        .flat_map(|payload| payload.errors.0.clone().unwrap_or_default())
        .collect();
    if reasons.is_empty() { Ok(()) } else { Err(GhError::failed(format!("GitLab refused it: {}", reasons.join("; ")))) }
}

/// `gitLabReviewPositionLines`: an added line by its new number, a deleted
/// one by its old, a context line by both.
fn position_lines(position: &ReviewPosition) -> Option<(Option<u64>, Option<u64>)> {
    match position.kind.as_deref()? {
        "added" => Some((None, Some(position.new_line.0?))),
        "deleted" => Some((Some(position.old_line.0?), None)),
        "context" => Some((Some(position.old_line.0?), Some(position.new_line.0?))),
        _ => None,
    }
}

#[derive(Serialize)]
struct Position<'a> {
    base_sha: &'a str,
    head_sha: &'a str,
    start_sha: &'a str,
    position_type: &'static str,
    old_path: &'a str,
    new_path: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    old_line: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    new_line: Option<u64>,
}

#[derive(Serialize)]
struct DiscussionBody<'a> {
    body: &'a Scalar,
    position: Position<'a>,
}

/// The discussion a review comment is posted as; `None` for one without a place in the diff.
fn discussion_body<'a>(refs: &'a DiffRefs, comment: &'a ReviewCommentDraft) -> Option<DiscussionBody<'a>> {
    let Some(Scalar::Text(path)) = comment.path.as_ref() else { return None };
    let (old_line, new_line) = position_lines(&comment.position)?;
    Some(DiscussionBody {
        body: comment.body.as_ref()?,
        position: Position {
            base_sha: &refs.base,
            head_sha: &refs.head,
            start_sha: &refs.start,
            position_type: "text",
            // Both paths are sent because GitLab resolves a position against both sides of the diff.
            old_path: comment.old_path.as_deref().unwrap_or(path),
            new_path: path,
            old_line,
            new_line,
        },
    })
}

/// `submitReview`: GitLab has no pending review, so a review is replayed as
/// separate requests. Requesting changes goes first, approval last, so a
/// half-sent review never approves.
pub async fn submit_review(p: &Supported, number: u64, verdict: &str, body: &str, comments: &[ReviewCommentDraft]) -> Result<(), GhError> {
    let base = merge_request(p, number);
    if verdict == "request-changes" {
        // GitLab keeps the verdict on the reviewer, so the viewer joins the reviewers first.
        let (_, viewer) = glab::identity(p.cwd(), &p.host).await?;
        let target = |reviewers| ReviewTarget { project_path: p.repository.trim(), iid: number.to_string(), reviewers };
        mutation(p, SET_SELF_AS_REVIEWER_MUTATION, target(Some(vec![viewer]))).await?;
        mutation(p, REQUEST_CHANGES_MUTATION, target(None)).await?;
    }
    if !comments.is_empty() {
        let refs = diff_refs(p, number).await?;
        let path = format!("{base}/discussions");
        for comment in comments {
            if let Some(discussion) = discussion_body(&refs, comment) {
                write(p, Api::send("POST", &path, &discussion)?).await?;
            }
        }
    }
    if !body.trim().is_empty() {
        write(p, Api::send("POST", &format!("{base}/notes"), &NoteBody { body })?).await?;
    }
    if verdict == "approve" {
        write(p, Api::call("POST", &format!("{base}/approve"))).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn read<T: serde::de::DeserializeOwned + Default>(value: serde_json::Value) -> T {
        serde_json::from_value::<Fields<T>>(value).unwrap().0
    }

    fn supported() -> Supported {
        Supported {
            project_id: "p".into(),
            title: "P".into(),
            workspace_root: "/nowhere".into(),
            repository: "group/sub/app".into(),
            host: "gitlab.com".into(),
            provider: Provider::GitLab,
        }
    }

    fn merge_request_json() -> serde_json::Value {
        json!({
            "iid": 7, "title": "Add subtraction", "web_url": "https://gitlab.com/group/sub/app/-/merge_requests/7",
            "description": "Body", "author": { "id": 1, "username": "octo", "name": "Octo", "avatar_url": "https://a/x.png" },
            "source_branch": "feature", "target_branch": "main", "sha": "abc123", "state": "opened", "draft": false,
            "detailed_merge_status": "need_rebase", "merge_status": "can_be_merged", "has_conflicts": false,
            "created_at": "2026-01-01T00:00:00Z", "updated_at": "2026-01-02T00:00:00Z", "merged_at": null, "closed_at": null,
            "reviewers": [{ "id": 2, "username": "rev", "name": "Rev" }, { "id": 3 }],
            "labels": ["bug", { "name": "ui", "color": "#ff0000", "description": "UI" }, "  "],
            "changes_count": "1000+", "head_pipeline": { "id": 9, "project_id": 4, "status": "failed", "web_url": "https://p/9", "source": "push" },
            "user": { "can_merge": false }, "merge_when_pipeline_succeeds": true, "squash_on_merge": true,
            "diverged_commits_count": 2, "source_project_id": 5, "target_project_id": 4, "allow_collaboration": null,
            "diff_refs": { "base_sha": "b", "head_sha": "h", "start_sha": "s" }
        })
    }

    #[test]
    fn a_merge_request_maps_like_upstream() {
        let mr: GlMergeRequest = read(merge_request_json());
        assert!(mr.is_whole());
        assert_eq!(mr.state(), PullRequestState::Open);
        assert_eq!(mr.mergeability(), PullRequestMergeability::Mergeable);
        assert_eq!(mr.rebase_required(), Some(true));
        assert_eq!(mr.changed_files(), 1000);
        assert_eq!(mr.review_request_logins(), ["rev"]);
        assert_eq!(mr.auto_merge(), Some(true));
        assert_eq!(mr.auto_merge_method(), Some(PullRequestMergeMethod::Squash));
        assert_eq!(mr.maintainer_can_modify(), Some(false), "a fork's merge request without collaboration");
        assert!(!mr.viewer_can_merge());
        assert_eq!(mr.diff_refs.refs(), Some(DiffRefs { base: "b".into(), head: "h".into(), start: "s".into() }));
        assert_eq!(json!(labels(&mr.labels)), json!([{ "name": "bug", "color": null }, { "name": "ui", "color": "#ff0000", "description": "UI" }]));
        assert_eq!(json!(mr.checks()), json!([{ "name": "Pipeline", "status": "failure", "description": "push", "url": "https://p/9" }]));

        let merged: GlMergeRequest = read(json!({ "state": "opened", "merged_at": "2026-01-03T00:00:00Z", "has_conflicts": true }));
        assert_eq!((merged.state(), merged.mergeability()), (PullRequestState::Merged, PullRequestMergeability::Conflicting));
        assert!(!merged.is_whole(), "a row without the required fields is skipped");
        let wip: GlMergeRequest = read(json!({ "work_in_progress": true, "state": "closed" }));
        assert!(wip.is_draft());
        assert_eq!((wip.state(), wip.auto_merge(), wip.checks().len()), (PullRequestState::Closed, None, 0));
    }

    #[test]
    fn a_listed_row_carries_its_state() {
        let mr: GlMergeRequest = read(merge_request_json());
        let state = ListState { review_decision: Some(Some(PullRequestReviewDecision::Approved)), checks_state: Some(PullRequestChecksState::Failing) };
        let row = list_row(&supported(), &mr, Some(&state), 5);
        assert_eq!(row.author_login.as_deref(), Some("octo"));
        let entry = json!(row.entry);
        assert_eq!(entry["provider"], "gitlab");
        assert_eq!(entry["reviewDecision"], "approved");
        assert_eq!(entry["checksState"], "failing");
        assert_eq!(entry["headBranch"], "feature");
        let summary = json!(summary_of(&supported(), &mr, Some((3, 1)), None));
        assert_eq!((summary["additions"].clone(), summary["changedFiles"].clone(), summary["autoMergeMethod"].clone()), (json!(3), json!(1000), json!("squash")));
        assert!(summary.get("reviewDecision").is_none(), "an unread state is left out");
    }

    #[test]
    fn list_states_follow_the_edition() {
        let node = |value: serde_json::Value| -> StateNode { read(value) };
        let changes = node(json!({ "iid": "1", "approved": true, "approvedBy": { "nodes": [{}] },
            "reviewers": { "nodes": [{ "mergeRequestInteraction": { "reviewState": "REQUESTED_CHANGES" } }] }, "headPipeline": { "status": "running" } }));
        assert_eq!(to_list_state(&changes, None), ListState { review_decision: Some(Some(PullRequestReviewDecision::ChangesRequested)), checks_state: Some(PullRequestChecksState::Pending) });
        let approved = node(json!({ "approved": true, "approvedBy": { "nodes": [{}] } }));
        assert_eq!(to_list_state(&approved, Some(false)).review_decision, Some(Some(PullRequestReviewDecision::Approved)));
        let unapproved_ee = node(json!({ "approved": false, "approvedBy": { "nodes": [] } }));
        assert_eq!(to_list_state(&unapproved_ee, Some(true)).review_decision, Some(Some(PullRequestReviewDecision::ReviewRequired)));
        assert_eq!(to_list_state(&unapproved_ee, None).review_decision, None, "the edition unknown: not judged");
        assert_eq!(to_list_state(&unapproved_ee, Some(false)).review_decision, Some(None), "Community Edition requires no review");
        assert_eq!(to_list_state(&node(json!({ "approved": true })), None).review_decision, None, "no approvals read");
        let data: ListStatesData = read(json!({ "metadata": { "enterprise": true }, "project": { "mergeRequests": { "nodes": [{ "iid": "4", "approved": false, "approvedBy": { "nodes": [] }, "headPipeline": { "status": "success" } }, null, { "iid": "x" }] } } }));
        let states = list_states_of(&data);
        assert_eq!(states.len(), 1);
        assert_eq!(states[&4].checks_state, Some(PullRequestChecksState::Passing));
    }

    #[test]
    fn list_parameters_narrow_what_gitlab_can() {
        let listing = Listing { state: "open", involvement: Some("reviewing"), query: Some(" add "), fetch: 300 };
        let narrowing = Narrowing { labels: vec!["bug", "a,b", "None"], excluded_labels: vec!["wontfix"], author: Some("@me"), draft: Some("hide"), review: Some("none") };
        assert_eq!(
            list_params(&listing, "me", &narrowing, 100),
            "state=opened&reviewer_username=me&search=add&labels=bug&not[labels]=wontfix&author_username=me&wip=no&approved_by_ids[]=None&order_by=updated_at&sort=desc&with_labels_details=true&per_page=100&page=1"
        );
        let authored = Listing { state: "merged", involvement: Some("authored"), query: None, fetch: 3 };
        assert_eq!(
            list_params(&authored, "me", &Narrowing { author: Some("other"), review: Some("approved"), ..Default::default() }, 3),
            "state=merged&author_username=me&approved_by_usernames[]=Any&order_by=updated_at&sort=desc&with_labels_details=true&per_page=3&page=1"
        );
    }

    #[test]
    fn pipeline_stages_become_checks() {
        let data: ProjectOf<MergeRequestOf<PipelineNode>> = read(json!({ "project": { "mergeRequest": { "headPipeline": { "path": "/g/a/-/pipelines/9", "stages": {
            "pageInfo": { "hasNextPage": false },
            "nodes": [
                { "name": "build", "status": "success", "jobs": { "pageInfo": { "hasNextPage": false }, "nodes": [
                    { "name": "compile", "status": "success", "webPath": "/g/a/-/jobs/1", "detailedStatus": { "label": "passed" } },
                    { "name": "lint", "status": "failed", "allowFailure": true, "webPath": "/g/a/-/jobs/2" },
                    { "name": "old", "status": "failed", "retried": true },
                    { "name": "deploy", "status": "success", "downstreamPipeline": { "status": "failed", "path": "/g/b/-/pipelines/3" } } ] } },
                { "name": "test", "status": "running", "detailedStatus": { "label": "running", "detailsPath": "/g/a/-/pipelines/9/test" }, "jobs": { "pageInfo": { "hasNextPage": true }, "nodes": [{ "name": "unit" }] } }
            ] } } } } }));
        let checks = stage_checks(&data, "https://gitlab.com").unwrap();
        assert_eq!(
            json!(checks),
            json!([
                { "name": "build / compile", "status": "success", "description": "passed", "url": "https://gitlab.com/g/a/-/jobs/1" },
                { "name": "build / lint", "status": "neutral", "description": null, "url": "https://gitlab.com/g/a/-/jobs/2" },
                { "name": "build / deploy", "status": "failure", "description": "Downstream pipeline", "url": "https://gitlab.com/g/b/-/pipelines/3" },
                { "name": "test", "status": "pending", "description": "running", "url": "https://gitlab.com/g/a/-/pipelines/9/test" }
            ])
        );
        let partial: ProjectOf<MergeRequestOf<PipelineNode>> = read(json!({ "project": { "mergeRequest": { "headPipeline": { "stages": { "pageInfo": { "hasNextPage": true }, "nodes": [{ "name": "x" }] } } } } }));
        assert!(stage_checks(&partial, "https://gitlab.com").is_none(), "a partial list is no list");
    }

    #[test]
    fn discussions_become_threads() {
        let raw: Rows<GlDiscussion> = serde_json::from_value(json!([
            { "id": "d1", "notes": [{ "id": 11, "body": "On a line", "created_at": "1", "resolvable": true, "resolved": false,
                "position": { "position_type": "text", "new_path": "a.rs", "old_path": "a.rs", "new_line": 4, "base_sha": "b", "start_sha": "s", "head_sha": "h" } },
                { "id": 12, "body": "Reply", "created_at": "2" }, { "id": 13, "body": "changed this line", "created_at": "3", "system": true }] },
            { "id": "d2", "notes": [{ "id": 21, "body": "Old side", "created_at": "1", "position": { "position_type": "text", "old_path": "b.rs", "old_line": 2, "new_line": null } }] },
            { "id": "d3", "notes": [{ "id": 31, "body": "Overview finding", "created_at": "1", "resolvable": true, "resolved": false }] },
            { "id": "d4", "notes": [{ "id": 41, "body": "A plain comment", "created_at": "1", "resolvable": false }] },
            { "id": "d5", "notes": [{ "id": 51, "body": "On a file", "created_at": "1", "position": { "position_type": "file", "new_path": "c.png" } }] },
            { "id": "d6", "notes": [{ "id": 61, "created_at": "1", "system": true, "body": "added 1 commit" }] }
        ]))
        .unwrap();
        let (threads, findings) = discussions_of(&raw);
        assert_eq!(threads.iter().map(|t| t.id.as_str()).collect::<Vec<_>>(), ["d1", "d2", "d3", "d5"]);
        assert_eq!((threads[0].path.as_deref(), threads[0].line, threads[0].side, threads[0].comments.len()), (Some("a.rs"), Some(4), DiffSide::Right, 2));
        assert_eq!(threads[0].refs, Some(DiffRefs { base: "b".into(), head: "h".into(), start: "s".into() }));
        assert_eq!((threads[1].path.as_deref(), threads[1].line, threads[1].side), (Some("b.rs"), Some(2), DiffSide::Left));
        assert_eq!((threads[2].path.as_deref(), threads[2].resolved), (None, false));
        assert_eq!((threads[3].path.as_deref(), threads[3].line), (Some("c.png"), None));
        assert_eq!(findings, ["31"]);
    }

    #[test]
    fn notes_commits_and_awards_map_like_upstream() {
        let notes: Rows<GlNote> = serde_json::from_value(json!([
            { "id": 1, "body": "Looks good", "created_at": "1", "author": { "username": "rev" } },
            { "id": 2, "body": "On a file", "created_at": "2", "type": "DiffNote", "position": { "new_path": "a.rs" } },
            { "id": 3, "body": "approved this merge request", "created_at": "3", "system": true },
            { "id": 4, "body": "   ", "created_at": "4" },
            { "body": "no id", "created_at": "5" }
        ]))
        .unwrap();
        let comments = note_comments(&notes);
        assert_eq!(comments.iter().map(|c| (c.id.as_str(), c.kind, c.path.as_deref())).collect::<Vec<_>>(), [("1", CommentKind::IssueComment, None), ("2", CommentKind::ReviewComment, Some("a.rs"))]);

        let commits: Rows<GlCommit> = serde_json::from_value(json!([
            { "id": "c2", "title": "Second", "committed_date": "2", "author_name": "A", "stats": { "additions": 3, "deletions": 1 } },
            { "id": "c1", "title": "First", "created_at": "1", "author_email": "a@x" },
            { "id": " ", "committed_date": "0" }
        ]))
        .unwrap();
        assert_eq!(
            json!(commits_of(&commits)),
            json!([
                { "oid": "c1", "messageHeadline": "First", "committedDate": "1", "additions": null, "deletions": null, "authors": [{ "login": "a@x", "name": null, "avatarUrl": null }] },
                { "oid": "c2", "messageHeadline": "Second", "committedDate": "2", "additions": 3, "deletions": 1, "authors": [{ "login": "A", "name": "A", "avatarUrl": null }] }
            ])
        );

        let data: AwardData = read(json!({ "currentUser": { "username": "Me" }, "project": { "mergeRequest": {
            "awardEmoji": { "nodes": [{ "name": "thumbsup", "user": { "username": "me" } }, { "name": "thumbsup", "user": { "username": "rev" } }, { "name": "tada", "user": { "username": "rev" } }, { "name": "party_parrot", "user": { "username": "rev" } }] },
            "notes": { "pageInfo": { "hasNextPage": true, "endCursor": "next" }, "nodes": [
                { "id": "gid://gitlab/Note/11", "author": { "username": "Bot", "bot": true }, "lastEditedAt": "9", "lastEditedBy": { "username": "Bot" }, "awardEmoji": { "nodes": [{ "name": "heart", "user": { "username": "me" } }] } },
                { "id": "gid://gitlab/DiscussionNote/12", "lastEditedAt": "9", "lastEditedBy": { "username": "x" }, "resolved": true }
            ] } } } }));
        let mut awards = Awards::default();
        assert_eq!(read_award_page(&data, &mut awards, true).as_deref(), Some("next"));
        assert_eq!(
            json!(awards.reactions),
            json!([{ "content": "thumbs-up", "count": 2, "actors": ["rev"], "viewerHasReacted": true }, { "content": "hooray", "count": 1, "actors": ["rev"], "viewerHasReacted": false }])
        );
        assert_eq!(json!(awards.by_note["11"]), json!([{ "content": "heart", "count": 1, "actors": [], "viewerHasReacted": true }]));
        assert_eq!(awards.edited_at.get("11").map(String::as_str), Some("9"));
        assert!(!awards.edited_at.contains_key("12"), "a resolve is no edit");
        assert!(awards.bots.contains("bot"));
        assert_eq!(award_name("hooray"), Some("tada"));
        assert_eq!(award_name("party"), None);
    }

    #[test]
    fn permissions_and_capabilities_follow_upstream() {
        let can = json!(viewer_permissions_of(true, true, false));
        assert_eq!(can["actions"], json!(["merge", "ready", "draft", "close", "reopen", "update-branch", "enable-auto-merge", "disable-auto-merge"]));
        assert_eq!((can["updateMethods"].clone(), can["labels"].clone(), can["maintainerEdits"].clone()), (json!(["rebase"]), json!(true), json!(false)));
        let cannot = json!(viewer_permissions_of(false, false, false));
        assert_eq!(cannot["actions"], json!(["ready", "draft", "close", "reopen"]));
        assert!(cannot.get("updateMethods").is_none());
        let caps = json!(capabilities());
        assert_eq!((caps["referenceSigils"].clone(), caps["updateMethods"].clone()), (json!(["#", "!"]), json!(["rebase"])));
        assert!(caps.get("viewedFiles").is_none());
        let settings: GlProjectSettings = read(json!({ "merge_method": "ff", "squash_option": "default_off" }));
        assert_eq!(json!(merge_capabilities(&settings)), json!({ "merge": false, "squash": true, "rebase": true }));
    }

    #[test]
    fn review_comments_are_placed_on_both_sides() {
        let refs = DiffRefs { base: "b".into(), head: "h".into(), start: "s".into() };
        let draft: ReviewCommentDraft = read(json!({ "path": "new.rs", "oldPath": "old.rs", "body": "Nit", "position": { "kind": "context", "oldLine": 3, "newLine": 4 } }));
        assert_eq!(
            json!(discussion_body(&refs, &draft).unwrap()),
            json!({ "body": "Nit", "position": { "base_sha": "b", "head_sha": "h", "start_sha": "s", "position_type": "text", "old_path": "old.rs", "new_path": "new.rs", "old_line": 3, "new_line": 4 } })
        );
        let added: ReviewCommentDraft = read(json!({ "path": "a.rs", "body": "x", "position": { "kind": "added", "newLine": 9 } }));
        assert_eq!(json!(discussion_body(&refs, &added).unwrap())["position"]["old_path"], "a.rs");
        let nowhere: ReviewCommentDraft = read(json!({ "path": "a.rs", "body": "x", "position": { "kind": "deleted" } }));
        assert!(discussion_body(&refs, &nowhere).is_none());
    }

    #[test]
    fn references_search_by_number_and_title() {
        assert_eq!(reference_searches("12"), vec![vec![("iids[]", "12".to_owned())], vec![("order_by", "updated_at".to_owned()), ("sort", "desc".to_owned())]]);
        assert_eq!(reference_searches("ab").len(), 1, "under three characters: the recent page");
        assert_eq!(reference_searches("abc")[0].len(), 4);
    }
}
