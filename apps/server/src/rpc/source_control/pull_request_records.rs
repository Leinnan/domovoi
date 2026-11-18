//! What the pull request RPCs answer with, where the client core of Domovoi has no
//! record of its own for it: t3code's `pullRequest.ts` and
//! `pullRequestReviewGuide.ts` schemas, field for field as this server sends
//! them. (The list, a summary and the checks are `domovoi_model::typed` records.)
//!
//! A field the schema makes optional and this server always sends is a plain
//! field here; one it never sends is left out of the record.

use crate::rpc::Never;
use serde::{Deserialize, Serialize};
use domovoi_model::typed::{
    ModelSelection, ProjectId, PullRequestAction, PullRequestActor, PullRequestCheck, PullRequestLabel, PullRequestMergeMethod, PullRequestMergeability,
    PullRequestState, ThreadId, Timestamp,
};

// ---------------------------------------------------------------------------
// Closed sets of strings
// ---------------------------------------------------------------------------

/// What a review says of the change (`PullRequestReviewVerdict`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReviewVerdict {
    Comment,
    Approve,
    RequestChanges,
}

impl ReviewVerdict {
    /// The wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            ReviewVerdict::Comment => "comment",
            ReviewVerdict::Approve => "approve",
            ReviewVerdict::RequestChanges => "request-changes",
        }
    }
}

/// How a branch is brought up to date with its base (`PullRequestUpdateMethod`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum UpdateMethod {
    Merge,
    Rebase,
}

impl UpdateMethod {
    /// The wire spelling.
    pub fn as_str(self) -> &'static str {
        match self {
            UpdateMethod::Merge => "merge",
            UpdateMethod::Rebase => "rebase",
        }
    }
}

/// Where the branch stands against its base (`PullRequestBaseComparison`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum BaseComparison {
    UpToDate,
    Behind,
    Unknown,
}

/// `PullRequestReactionContent`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReactionContent {
    ThumbsUp,
    ThumbsDown,
    Laugh,
    Hooray,
    Confused,
    Heart,
    Rocket,
    Eyes,
}

/// `PullRequestCommentKind`, as far as an activity lists comments (a review
/// comment is part of its thread).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum CommentKind {
    IssueComment,
    Review,
}

/// The side of a diff a line is on (`PullRequestDiffSide`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DiffSide {
    Left,
    Right,
}

/// `PullRequestFileViewedState`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum FileViewedState {
    Unviewed,
    Viewed,
    Dismissed,
}

/// Where the files a reader has seen are kept (`PullRequestViewedFilesStore`): GitHub keeps them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ViewedFilesStore {
    Host,
}

/// What a composer's reference starts with (`PullRequestReferenceSigil`): GitHub links `#`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum ReferenceSigil {
    #[serde(rename = "#")]
    Hash,
}

/// What a reference candidate is (`PullRequestReferenceCandidate.kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReferenceKind {
    Issue,
    ChangeRequest,
}

/// `PullRequestReviewerKind`: only users are offered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReviewerKind {
    User,
}

// ---------------------------------------------------------------------------
// What the host and the viewer can do
// ---------------------------------------------------------------------------

/// `PullRequestReviewCapabilities`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewCapabilities {
    pub inline_comment: bool,
    pub range_comment: bool,
    pub reply: bool,
    pub resolve: bool,
    pub verdicts: [ReviewVerdict; 3],
}

/// `PullRequestReviewerCapabilities`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewerCapabilities {
    pub request: bool,
    pub list_candidates: bool,
    pub search_candidates: bool,
}

/// `PullRequestEditCapabilities`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EditCapabilities {
    pub change_request: bool,
    pub comment: bool,
    pub maintainer_edits: bool,
}

/// What the host can do at all (`PullRequestCapabilities`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Capabilities {
    pub diff: bool,
    pub comment: bool,
    pub actions: Vec<PullRequestAction>,
    pub merge_methods: [PullRequestMergeMethod; 3],
    pub delete_branch: bool,
    pub update_methods: [UpdateMethod; 2],
    pub search: bool,
    pub reactions: bool,
    pub mentions: bool,
    pub reference_sigils: [ReferenceSigil; 1],
    pub viewed_files: ViewedFilesStore,
    pub review: ReviewCapabilities,
    pub reviewers: ReviewerCapabilities,
    pub edit: EditCapabilities,
    pub stacks: bool,
    pub stack_actions: bool,
    pub labels: bool,
    pub checks_always_report: bool,
}

/// What the signed-in account may do to one pull request (`PullRequestViewerPermissions`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewerPermissions {
    pub actions: Vec<PullRequestAction>,
    pub comment: bool,
    pub resolve: bool,
    pub verdicts: Vec<ReviewVerdict>,
    pub request_reviewers: bool,
    pub labels: bool,
    /// Sent only to an account that may.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stack_rebase: Option<bool>,
    /// The ways the branch can be updated now; absent when it cannot be.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub update_methods: Option<[UpdateMethod; 2]>,
}

/// The ways the repository lets a pull request land (`PullRequestMergeCapabilities`).
#[derive(Debug, Clone, Serialize)]
pub struct MergeCapabilities {
    pub merge: bool,
    pub squash: bool,
    pub rebase: bool,
}

// ---------------------------------------------------------------------------
// One pull request
// ---------------------------------------------------------------------------

/// The result of `pullRequests.detail` (`PullRequestDetail`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestDetail {
    /// A source control provider kind: always `github` here.
    pub provider: &'static str,
    pub capabilities: Capabilities,
    pub viewer_permissions: ViewerPermissions,
    pub project_id: ProjectId,
    pub project_title: String,
    pub workspace_root: String,
    pub repository: String,
    pub number: u64,
    pub title: String,
    pub body: String,
    pub url: String,
    pub author: Option<PullRequestActor>,
    pub state: PullRequestState,
    pub is_draft: bool,
    pub mergeability: PullRequestMergeability,
    pub additions: u64,
    pub deletions: u64,
    pub changed_files: u64,
    pub head_branch: String,
    /// `null` when the head's repository is gone.
    pub head_repository_name_with_owner: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub head_sha: Option<String>,
    pub base_branch: String,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    /// When the server read the pull request, in milliseconds since the epoch.
    pub observed_at: u64,
    pub merged_at: Option<Timestamp>,
    pub closed_at: Option<Timestamp>,
    pub reviewers: Vec<PullRequestActor>,
    pub labels: Vec<PullRequestLabel>,
    pub checks: Vec<PullRequestCheck>,
    pub merge_capabilities: MergeCapabilities,
    /// The signed-in login, when it could be read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub viewer: Option<String>,
    pub base_comparison: BaseComparison,
    pub auto_merge_enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub auto_merge_method: Option<PullRequestMergeMethod>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub maintainer_can_modify: Option<bool>,
}

/// The result of `pullRequests.preview` (`PullRequestPreview`): what a link to a pull request shows.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestPreview {
    pub project_id: ProjectId,
    pub repository: String,
    pub number: u64,
    pub title: String,
    pub url: String,
    pub author: Option<PullRequestActor>,
    pub state: PullRequestState,
    pub is_draft: bool,
    pub created_at: Timestamp,
}

/// One kind of reaction to a comment or a pull request (`PullRequestReaction`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Reaction {
    pub content: ReactionContent,
    pub count: u64,
    pub actors: Vec<String>,
    pub viewer_has_reacted: bool,
}

/// A comment on the pull request as a whole, or a review's summary (`PullRequestComment`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Comment {
    pub id: String,
    pub kind: CommentKind,
    pub author: Option<PullRequestActor>,
    pub body: String,
    pub created_at: Timestamp,
    pub edited_at: Option<Timestamp>,
    pub url: Option<String>,
    /// Comments on a file are in their thread: these have no path.
    pub path: Option<Never>,
    /// GitHub's state of a review (`APPROVED`, …); `null` for a comment.
    pub review_state: Option<String>,
    pub reactions: Vec<Reaction>,
    pub can_edit: bool,
}

/// One comment of a review thread (`PullRequestThreadComment`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadComment {
    pub id: String,
    pub author: Option<PullRequestActor>,
    pub body: String,
    pub created_at: Timestamp,
    pub edited_at: Option<Timestamp>,
    pub url: Option<String>,
    /// The review that posted the comment, when GitHub names one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review_url: Option<String>,
    pub reactions: Vec<Reaction>,
    pub can_edit: bool,
}

/// A conversation on a line of the diff (`PullRequestReviewThread`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewThread {
    pub id: String,
    pub path: String,
    pub line: Option<u64>,
    pub side: DiffSide,
    pub is_resolved: bool,
    pub is_outdated: bool,
    pub comments: Vec<ThreadComment>,
    pub comment_count: u64,
    /// Where the thread's comments carry on, when there are more; sent as `null` otherwise.
    pub next_comments_cursor: Option<String>,
    pub can_resolve: bool,
}

/// A commit of the pull request (`PullRequestCommit`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Commit {
    pub oid: String,
    pub message_headline: String,
    pub committed_date: Timestamp,
    /// Sent as `null` when GitHub reports no count.
    pub additions: Option<u64>,
    pub deletions: Option<u64>,
    pub authors: Vec<PullRequestActor>,
}

/// The result of `pullRequests.activity` (`PullRequestActivity`): what was said and committed.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestActivity {
    pub author: Option<PullRequestActor>,
    pub reviewers: Vec<PullRequestActor>,
    pub comments: Vec<Comment>,
    pub comment_count: u64,
    pub comments_truncated: bool,
    pub review_threads_truncated: bool,
    pub review_threads: Vec<ReviewThread>,
    pub commits: Vec<Commit>,
    pub reactions: Vec<Reaction>,
}

/// The result of `pullRequests.threadComments` (`PullRequestThreadCommentsResult`): a later page of one thread.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadCommentsPage {
    pub comments: Vec<ThreadComment>,
    pub next_cursor: Option<String>,
}

/// The result of `pullRequests.diffFileContents` (`PullRequestDiffFileContentsResult`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffFileContents {
    pub old_contents: String,
    pub new_contents: String,
}

/// Whether the viewer has seen a file (`PullRequestFileViewed`).
#[derive(Debug, Clone, Serialize)]
pub struct FileViewed {
    pub path: String,
    pub state: FileViewedState,
}

/// The result of `pullRequests.filesViewed` (`PullRequestFilesViewedResult`).
#[derive(Debug, Clone, Serialize)]
pub struct FilesViewed {
    pub files: Vec<FileViewed>,
    pub truncated: bool,
}

/// Lines added and removed (`PullRequestLineCount`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct LineCount {
    pub additions: u64,
    pub deletions: u64,
}

/// A change's line counts by what its files are (`PullRequestChangeBreakdown`,
/// which is this or `null`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize)]
pub struct ChangeBreakdown {
    pub code: LineCount,
    pub tests: LineCount,
    pub mocks: LineCount,
    pub generated: LineCount,
    pub docs: LineCount,
}

// ---------------------------------------------------------------------------
// Lists and candidates
// ---------------------------------------------------------------------------

/// The line counts of one listed pull request (`PullRequestDiffStat`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiffStat {
    pub project_id: ProjectId,
    pub repository: String,
    pub number: u64,
    pub additions: u64,
    pub deletions: u64,
}

/// The result of `pullRequests.listStats` (`PullRequestListStatsResult`).
#[derive(Debug, Clone, Serialize)]
pub struct ListStats {
    pub stats: Vec<DiffStat>,
}

/// An issue or pull request a composer can reference (`PullRequestReferenceCandidate`).
#[derive(Debug, Clone, Serialize)]
pub struct ReferenceCandidate {
    pub reference: String,
    pub kind: ReferenceKind,
    pub number: u64,
    pub title: String,
    pub state: PullRequestState,
}

/// The result of `pullRequests.filterOptions` (`PullRequestFilterOptions`):
/// the options of the kind asked for, the other lists empty.
#[derive(Debug, Clone, Default, Serialize)]
pub struct FilterOptions {
    pub labels: Vec<PullRequestLabel>,
    pub authors: Vec<PullRequestActor>,
    pub references: Vec<ReferenceCandidate>,
    pub truncated: bool,
}

/// Someone who can be asked for a review (`PullRequestReviewerCandidate`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewerCandidate {
    #[serde(flatten)]
    pub actor: PullRequestActor,
    pub id: String,
    pub kind: ReviewerKind,
    pub is_requested: bool,
}

/// The result of `pullRequests.reviewerCandidates` (`PullRequestReviewerCandidateList`).
#[derive(Debug, Clone, Serialize)]
pub struct ReviewerCandidates {
    pub candidates: Vec<ReviewerCandidate>,
    pub truncated: bool,
}

/// A label of the repository, and whether the pull request has it (`PullRequestLabelCandidate`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LabelCandidate {
    pub name: String,
    pub color: Option<String>,
    pub description: Option<String>,
    pub is_applied: bool,
}

/// The result of `pullRequests.labelCandidates` (`PullRequestLabelCandidateList`).
#[derive(Debug, Clone, Serialize)]
pub struct LabelCandidates {
    pub candidates: Vec<LabelCandidate>,
    pub truncated: bool,
}

// ---------------------------------------------------------------------------
// Routing and linked threads
// ---------------------------------------------------------------------------

/// The result of `pullRequests.routingIdentity` (`PullRequestRoutingIdentityResult`): who is signed in to a host.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RoutingIdentity {
    pub account_id: String,
    pub viewer: String,
    pub host: String,
    pub provider: &'static str,
}

/// The result of `pullRequests.routing` (`PullRequestRoutingResult`): the
/// project and account a pull request is read through.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Routing {
    pub host: String,
    pub provider: &'static str,
    pub account_id: String,
    pub viewer: String,
    pub project_title: String,
    pub workspace_root: String,
}

/// A thread linked to a pull request (an element of `PullRequestLinkedThreadsResult.threads`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkedThread {
    pub id: ThreadId,
    pub project_id: ProjectId,
    pub title: String,
    pub archived_at: Option<Timestamp>,
    pub settled: bool,
}

/// The result of `pullRequests.linkedThreads` (`PullRequestLinkedThreadsResult`).
#[derive(Debug, Clone, Serialize)]
pub struct LinkedThreads {
    pub threads: Vec<LinkedThread>,
}

// ---------------------------------------------------------------------------
// Review guides (`pullRequestReviewGuide.ts`). A guide is also what is kept
// on disk, so it is read back as well as written.
// ---------------------------------------------------------------------------

/// What a maintainer should judge at a stop (`PullRequestReviewGuideAttention.kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AttentionKind {
    Tradeoff,
    Contract,
    Product,
    Risk,
    Accessibility,
}

/// `PullRequestReviewGuideAttention`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GuideAttention {
    pub kind: AttentionKind,
    pub note: String,
}

/// Which side of the diff an excerpt shows (`PullRequestReviewGuideExcerpt.side`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExcerptSide {
    New,
    Old,
}

/// Lines of the diff worth showing (`PullRequestReviewGuideExcerpt`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GuideExcerpt {
    pub path: String,
    pub side: ExcerptSide,
    pub start: u64,
    pub end: u64,
    pub caption: String,
}

/// A changed file a stop owns without showing it (an element of `PullRequestReviewGuideStop.files`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GuideFile {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// An explanation anchored to a line (`PullRequestReviewGuideNote`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GuideNote {
    pub path: String,
    pub line: u64,
    pub title: String,
    pub body: String,
}

/// One stop of the tour (`PullRequestReviewGuideStop`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GuideStop {
    pub title: String,
    pub summary: String,
    pub attention: Option<GuideAttention>,
    pub excerpts: Vec<GuideExcerpt>,
    pub files: Vec<GuideFile>,
    pub notes: Vec<GuideNote>,
    pub questions: Vec<String>,
}

/// A guided tour of a pull request at one head (`PullRequestReviewGuide`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewGuide {
    pub head_sha: String,
    pub generated_at: Timestamp,
    pub model_selection: ModelSelection,
    pub stops: Vec<GuideStop>,
}

/// A guide being written, or the last attempt that failed (`PullRequestReviewGuideRun`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "_tag")]
pub enum GuideRun {
    #[serde(rename_all = "camelCase")]
    Running { head_sha: String, started_at: Timestamp },
    #[serde(rename_all = "camelCase")]
    Failed { head_sha: String, failed_at: Timestamp, detail: String },
}

/// The result of `pullRequests.reviewGuide` and `pullRequests.generateReviewGuide` (`PullRequestReviewGuideResult`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewGuideResult {
    pub guide: Option<ReviewGuide>,
    pub run: Option<GuideRun>,
    pub current_head_sha: Option<String>,
}
