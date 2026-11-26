//! A checkout's version control state as the git panel shows it, and the git
//! actions run from it (`git.ts`).
//!
//! `vcs.refreshBranch`, `vcs.removeWorktree` and `vcs.init` answer with
//! nothing (`Schema.Void`, sent as `null`).

use super::common::double_option;
use super::macros::{string_enum, tagged_union_deserialize};
use super::source_control::SourceControlProviderInfo;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

// ---- status ----

/// One changed file of a working tree, with its line counts.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VcsWorkingTreeFile {
    /// Relative to the working tree's `rootPath`.
    pub path: String,
    pub insertions: u64,
    pub deletions: u64,
}

/// The uncommitted changes of a checkout (`VcsStatusLocalResult.workingTree`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VcsWorkingTree {
    /// The absolute root every file path is relative to; absent when the server could not resolve it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_path: Option<String>,
    pub files: Vec<VcsWorkingTreeFile>,
    pub insertions: u64,
    pub deletions: u64,
}

/// How far a branch has moved from the one it is compared against (`VcsStatusLocalResult.branchChanges`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VcsBranchChanges {
    pub base_ref: Option<String>,
    pub insertions: u64,
    pub deletions: u64,
}

/// What a checkout's status says without asking its remote (`VcsStatusLocalResult`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VcsStatusLocal {
    pub is_repo: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_control_provider: Option<SourceControlProviderInfo>,
    pub has_primary_remote: bool,
    pub is_default_ref: bool,
    /// The branch checked out; `null` on a detached head or outside a repository.
    pub ref_name: Option<String>,
    pub has_working_tree_changes: bool,
    pub working_tree: VcsWorkingTree,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_changes: Option<VcsBranchChanges>,
}

string_enum! {
    /// Whether a branch's change request is open, closed or merged (`VcsStatusChangeRequestState`).
    VcsChangeRequestState {
        Open = "open",
        Closed = "closed",
        Merged = "merged",
    }
}

/// The change request opened for a checkout's branch (`VcsStatusChangeRequest`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VcsStatusChangeRequest {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub base_ref: String,
    pub head_ref: String,
    pub state: VcsChangeRequestState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_draft: Option<bool>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<Option<String>>,
}

/// What a checkout's status says about its upstream (`VcsStatusRemoteResult`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VcsStatusRemote {
    pub has_upstream: bool,
    pub ahead_count: u64,
    pub behind_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ahead_of_default_count: Option<u64>,
    /// The branch's change request; the Domovoi server always sends `null`.
    pub pr: Option<VcsStatusChangeRequest>,
}

/// The first item of `subscribeVcsStatus`: both halves of the status.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VcsStatusSnapshot {
    pub local: VcsStatusLocal,
    /// `null` until the remote half is known, and outside a repository.
    pub remote: Option<VcsStatusRemote>,
}

/// A `subscribeVcsStatus` item that replaces the local half of the status.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VcsStatusLocalUpdated {
    pub local: VcsStatusLocal,
}

/// A `subscribeVcsStatus` item that replaces the remote half of the status.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VcsStatusRemoteUpdated {
    pub remote: Option<VcsStatusRemote>,
}

/// One item of `subscribeVcsStatus` (`VcsStatusStreamEvent`).
///
/// `Other` is an item whose `_tag` this build does not know, kept as it was sent.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "_tag", rename_all = "camelCase")]
pub enum VcsStatusStreamEvent {
    Snapshot(VcsStatusSnapshot),
    LocalUpdated(VcsStatusLocalUpdated),
    RemoteUpdated(VcsStatusRemoteUpdated),
    #[serde(untagged)]
    Other(Map<String, Value>),
}

tagged_union_deserialize! {
    VcsStatusStreamEvent by "_tag" {
        "snapshot" => Snapshot,
        "localUpdated" => LocalUpdated,
        "remoteUpdated" => RemoteUpdated,
        _ => Other,
    }
}

/// The result of `vcs.refreshStatus` (`VcsStatusResult`): both halves of a
/// checkout's status in one record.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VcsStatus {
    pub is_repo: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_control_provider: Option<SourceControlProviderInfo>,
    pub has_primary_remote: bool,
    pub is_default_ref: bool,
    pub ref_name: Option<String>,
    pub has_working_tree_changes: bool,
    pub working_tree: VcsWorkingTree,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_changes: Option<VcsBranchChanges>,
    pub has_upstream: bool,
    pub ahead_count: u64,
    pub behind_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ahead_of_default_count: Option<u64>,
    pub pr: Option<VcsStatusChangeRequest>,
}

/// One item of `subscribeVcsBranch` (`VcsBranchStatus`): the branch a checkout is on.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VcsBranchStatus {
    pub ref_name: Option<String>,
    /// Absent from older servers, where a removed checkout looks like a detached head.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_repo: Option<bool>,
}

// ---- refs ----

/// A branch of a repository, local or on a remote (`VcsRef`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VcsRef {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_remote: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_name: Option<String>,
    /// The full ref, such as `refs/remotes/origin/feature`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_ref: Option<String>,
    pub current: bool,
    pub is_default: bool,
    /// The worktree the branch is checked out in, if any.
    pub worktree_path: Option<String>,
}

/// The result of `vcs.listRefs` (`VcsListRefsResult`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VcsListRefsResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_at_repository_root: Option<bool>,
    pub refs: Vec<VcsRef>,
    pub is_repo: bool,
    pub has_primary_remote: bool,
    /// Where the next page starts, when there is one.
    pub next_cursor: Option<u64>,
    pub total_count: u64,
}

string_enum! {
    /// Whether a pull brought anything in (`VcsPullResult.status`).
    VcsPullStatus {
        Pulled = "pulled",
        SkippedUpToDate = "skipped_up_to_date",
    }
}

/// The result of `vcs.pull` (`VcsPullResult`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VcsPullResult {
    pub status: VcsPullStatus,
    pub ref_name: String,
    pub upstream_ref: Option<String>,
}

/// The result of `vcs.switchRef` (`VcsSwitchRefResult`): the branch now checked out.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VcsSwitchRefResult {
    pub ref_name: Option<String>,
}

/// The result of `vcs.createRef` (`VcsCreateRefResult`): the branch created.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VcsCreateRefResult {
    pub ref_name: String,
}

// ---- git actions ----

string_enum! {
    /// What a git action does, from a commit alone to commit, push and pull request (`GitStackedAction`).
    GitStackedAction {
        Commit = "commit",
        Push = "push",
        CreatePr = "create_pr",
        CommitPush = "commit_push",
        CommitPushPr = "commit_push_pr",
    }
}

string_enum! {
    /// One step of a git action (`GitActionProgressPhase`).
    GitActionProgressPhase {
        Branch = "branch",
        Commit = "commit",
        Push = "push",
        Pr = "pr",
    }
}

string_enum! {
    /// Which output of a git hook a line came from (`GitActionProgressStream`).
    GitActionProgressStream {
        Stdout = "stdout",
        Stderr = "stderr",
    }
}

string_enum! {
    /// How the feature branch step of a git action ended (`GitBranchStepStatus`).
    GitBranchStepStatus {
        Created = "created",
        SkippedNotRequested = "skipped_not_requested",
    }
}

string_enum! {
    /// How the commit step of a git action ended (`GitCommitStepStatus`).
    GitCommitStepStatus {
        Created = "created",
        SkippedNoChanges = "skipped_no_changes",
        SkippedNotRequested = "skipped_not_requested",
    }
}

string_enum! {
    /// How the push step of a git action ended (`GitPushStepStatus`).
    GitPushStepStatus {
        Pushed = "pushed",
        SkippedNotRequested = "skipped_not_requested",
        SkippedUpToDate = "skipped_up_to_date",
    }
}

string_enum! {
    /// How the pull request step of a git action ended (`GitPrStepStatus`).
    GitPrStepStatus {
        Created = "created",
        OpenedExisting = "opened_existing",
        SkippedNotRequested = "skipped_not_requested",
    }
}

/// The feature branch step of a finished git action (`GitRunStackedActionResult.branch`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitBranchStep {
    pub status: GitBranchStepStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// The commit step of a finished git action (`GitRunStackedActionResult.commit`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitCommitStep {
    pub status: GitCommitStepStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
}

/// The push step of a finished git action (`GitRunStackedActionResult.push`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitPushStep {
    pub status: GitPushStepStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub set_upstream: Option<bool>,
}

/// The pull request step of a finished git action (`GitRunStackedActionResult.pr`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitPrStep {
    pub status: GitPrStepStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub number: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
}

// ---- a pull request made from a thread ----

/// `git.preparePullRequest`: what a pull request from the checkout at `cwd`
/// would be, for the sheet a person reads before it is made.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestPrepareInput {
    /// The thread the work is of: its base branch, its issue and its links.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<super::ids::ThreadId>,
    pub cwd: String,
    /// The base the person picked, when not the one first answered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_branch: Option<String>,
    /// Write a title and a body from the changes (a text generation call).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generate: Option<bool>,
}

/// A changed file a pull request's commit would take.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestDirtyFile {
    /// Relative to the working tree's root.
    pub path: String,
    pub insertions: u64,
    pub deletions: u64,
}

/// The open pull request the branch has already.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestExisting {
    pub number: u64,
    pub url: String,
    pub title: String,
    /// The thread links it already.
    pub linked: bool,
}

/// An issue the pull request closes when it merges.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestClosingRef {
    pub display_key: String,
    /// The line that closes it, as the body carries it: `Closes #12`.
    pub reference: String,
    pub url: String,
}

/// Why no pull request can be made from a checkout.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestUnavailable {
    /// `not-a-repository`, `detached`, `no-remote`, `unsupported`,
    /// `no-changes`, `tool-missing` or `unauthenticated`.
    pub reason: String,
    pub detail: String,
}

/// What `git.preparePullRequest` answers.
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestPrepareResult {
    /// `github`, `gitlab`, or the host's kind when it has no driver.
    pub provider: String,
    pub repository: Option<String>,
    pub branch: Option<String>,
    /// The branch is the repository's default one: the work goes onto a
    /// branch of its own (`newBranch`) before a pull request is made.
    pub on_default_branch: bool,
    /// The branch the commit would be made on, when not the one checked out.
    pub new_branch: Option<String>,
    pub has_upstream: bool,
    /// Commits the remote's branch does not have.
    pub unpushed: u64,
    pub commits_ahead_of_base: u64,
    pub dirty_files: Vec<PullRequestDirtyFile>,
    /// More files changed than `dirtyFiles` lists.
    pub dirty_truncated: bool,
    pub existing: Option<PullRequestExisting>,
    pub base_branch: String,
    /// The thread's own base first, then the default branch, then others.
    pub base_branches: Vec<String>,
    pub supports_draft: bool,
    /// Present once asked for with `generate`.
    pub title: Option<String>,
    pub body: Option<String>,
    /// Why the title and body are not the text generation model's.
    pub generation_error: Option<String>,
    pub closing: Vec<PullRequestClosingRef>,
    pub unavailable: Option<PullRequestUnavailable>,
}

/// The pull request a git action is to make, as the sheet's reader left it
/// (`git.runStackedAction` `pullRequest`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StackedPullRequest {
    pub title: String,
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_branch: Option<String>,
}

/// The git action a toast's button runs next (`GitRunStackedActionToastRunAction`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitActionToastRunAction {
    pub kind: GitStackedAction,
}

/// A toast button that opens the pull request the action made or found.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitActionToastOpenPr {
    pub label: String,
    pub url: String,
}

/// A toast button that runs the next git action.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitActionToastRun {
    pub label: String,
    pub action: GitActionToastRunAction,
}

/// A toast with no button.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct GitActionToastNoCta {}

/// The button of a finished git action's toast (`GitRunStackedActionToastCta`).
///
/// `Other` is a button whose `kind` this build does not know, kept as it was sent.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GitActionToastCta {
    None(GitActionToastNoCta),
    OpenPr(GitActionToastOpenPr),
    RunAction(GitActionToastRun),
    #[serde(untagged)]
    Other(Map<String, Value>),
}

tagged_union_deserialize! {
    GitActionToastCta by "kind" {
        "none" => None,
        "open_pr" => OpenPr,
        "run_action" => RunAction,
        _ => Other,
    }
}

/// What to tell the user when a git action finished (`GitRunStackedActionToast`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitActionToast {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub cta: GitActionToastCta,
}

/// How each step of a git action ended (`GitRunStackedActionResult`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitRunStackedActionResult {
    pub action: GitStackedAction,
    pub branch: GitBranchStep,
    pub commit: GitCommitStep,
    pub push: GitPushStep,
    pub pr: GitPrStep,
    pub toast: GitActionToast,
}

/// The first event of a git action: the steps it will run (`GitActionStartedEvent`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitActionStarted {
    pub action_id: String,
    pub cwd: String,
    pub action: GitStackedAction,
    pub phases: Vec<GitActionProgressPhase>,
}

/// A step of a git action began (`GitActionPhaseStartedEvent`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitActionPhaseStarted {
    pub action_id: String,
    pub cwd: String,
    pub action: GitStackedAction,
    pub phase: GitActionProgressPhase,
    /// What to show while the step runs, such as `Pushing...`.
    pub label: String,
}

/// A git hook began to run (`GitActionHookStartedEvent`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitActionHookStarted {
    pub action_id: String,
    pub cwd: String,
    pub action: GitStackedAction,
    pub hook_name: String,
}

/// A line a git hook printed (`GitActionHookOutputEvent`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitActionHookOutput {
    pub action_id: String,
    pub cwd: String,
    pub action: GitStackedAction,
    pub hook_name: Option<String>,
    pub stream: GitActionProgressStream,
    pub text: String,
}

/// A git hook ended (`GitActionHookFinishedEvent`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitActionHookFinished {
    pub action_id: String,
    pub cwd: String,
    pub action: GitStackedAction,
    pub hook_name: String,
    pub exit_code: Option<i64>,
    pub duration_ms: Option<u64>,
}

/// A git action ran to its end (`GitActionFinishedEvent`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitActionFinished {
    pub action_id: String,
    pub cwd: String,
    pub action: GitStackedAction,
    pub result: GitRunStackedActionResult,
}

/// A git action stopped at a failure (`GitActionFailedEvent`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitActionFailed {
    pub action_id: String,
    pub cwd: String,
    pub action: GitStackedAction,
    /// The step that failed; `null` when the action failed before its first step.
    pub phase: Option<GitActionProgressPhase>,
    pub message: String,
}

/// One item of `git.runStackedAction` (`GitActionProgressEvent`).
///
/// `Other` is an event whose `kind` this build does not know, kept as it was
/// sent. The Domovoi server runs hooks without reporting them: it sends no
/// `hook_*` event.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GitActionProgressEvent {
    ActionStarted(GitActionStarted),
    PhaseStarted(GitActionPhaseStarted),
    HookStarted(GitActionHookStarted),
    HookOutput(GitActionHookOutput),
    HookFinished(GitActionHookFinished),
    ActionFinished(Box<GitActionFinished>),
    ActionFailed(GitActionFailed),
    #[serde(untagged)]
    Other(Map<String, Value>),
}

tagged_union_deserialize! {
    GitActionProgressEvent by "kind" {
        "action_started" => ActionStarted,
        "phase_started" => PhaseStarted,
        "hook_started" => HookStarted,
        "hook_output" => HookOutput,
        "hook_finished" => HookFinished,
        "action_finished" => ActionFinished,
        "action_failed" => ActionFailed,
        _ => Other,
    }
}

// ---- pull requests as git sees them ----

string_enum! {
    /// Whether a pull request named by reference is open, closed or merged (`GitPullRequestState`).
    GitPullRequestState {
        Open = "open",
        Closed = "closed",
        Merged = "merged",
    }
}

/// The pull request a reference (a number or a URL) names (`GitResolvedPullRequest`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitResolvedPullRequest {
    pub number: u64,
    pub title: String,
    pub url: String,
    pub base_branch: String,
    pub head_branch: String,
    pub state: GitPullRequestState,
}

/// The result of `git.resolvePullRequest` (`GitResolvePullRequestResult`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitResolvePullRequestResult {
    pub pull_request: GitResolvedPullRequest,
}

/// The result of `git.preparePullRequestThread` (`GitPreparePullRequestThreadResult`):
/// where a pull request was checked out for a new thread.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitPreparePullRequestThreadResult {
    pub pull_request: GitResolvedPullRequest,
    pub branch: String,
    /// The worktree made or reused for it; `null` when it was checked out in the project's own folder.
    pub worktree_path: Option<String>,
    /// False when local commits or edits kept a reused worktree from reaching
    /// the pull request's head. The schema's default, when absent, is `true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_on_pull_request_head: Option<bool>,
}
