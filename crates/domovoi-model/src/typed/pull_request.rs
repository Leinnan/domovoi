//! Pull requests linked to a thread, and the watch that wakes the agent about them.

use super::common::{Timestamp, double_option};
use super::ids::ProjectId;
use super::macros::string_enum;
use serde::{Deserialize, Serialize};

/// The pull request a thread's branch belongs to, in the older single-link form.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkedPullRequest {
    pub project_id: ProjectId,
    pub repository: String,
    pub number: u64,
    pub url: String,
}

/// A pull request linked to a thread, with its last known state.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestLink {
    pub host: String,
    pub repository: String,
    pub number: u64,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository_web_url: Option<String>,
    pub source: PullRequestLinkSource,
    pub linked_at: Timestamp,
    pub snapshot: Option<PullRequestSnapshot>,
    pub stack: Option<PullRequestStack>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watch: Option<PullRequestWatch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended_watch: Option<PullRequestWatch>,
}

string_enum! {
    /// How a pull request came to be linked to a thread.
    PullRequestLinkSource {
        Manual = "manual",
        Created = "created",
        Agent = "agent",
        Stack = "stack",
        StackDismissed = "stack-dismissed",
    }
}

string_enum! {
    /// Whether a pull request is open, closed or merged.
    PullRequestState {
        Open = "open",
        Closed = "closed",
        Merged = "merged",
    }
}

string_enum! {
    /// Where a pull request's review stands.
    PullRequestReviewDecision {
        Approved = "approved",
        ChangesRequested = "changes-requested",
        ReviewRequired = "review-required",
    }
}

string_enum! {
    /// The combined result of a pull request's checks.
    PullRequestChecksState {
        Passing = "passing",
        Failing = "failing",
        Pending = "pending",
    }
}

string_enum! {
    /// Whether a pull request can be merged into its base.
    PullRequestMergeability {
        Mergeable = "mergeable",
        Conflicting = "conflicting",
        Unknown = "unknown",
    }
}

/// A pull request's state as last read from its host.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestSnapshot {
    pub state: PullRequestState,
    pub title: String,
    pub head_branch: String,
    pub base_branch: String,
    pub is_draft: bool,
    pub updated_at: Option<Timestamp>,
    pub synced_at: Timestamp,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<Option<Timestamp>>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub merged_at: Option<Option<Timestamp>>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub author: Option<Option<PullRequestActor>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additions: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deletions: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub changed_files: Option<u64>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub review_decision: Option<Option<PullRequestReviewDecision>>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub checks_state: Option<Option<PullRequestChecksState>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mergeability: Option<PullRequestMergeability>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rebase_required: Option<bool>,
    /// Conversations on the diff nobody resolved yet; absent when not read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unresolved_review_threads: Option<u64>,
}

/// The account that opened a pull request.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestActor {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_bot: Option<bool>,
    pub login: String,
    pub name: Option<String>,
    pub avatar_url: Option<String>,
}

/// The stack of dependent pull requests one belongs to, bottom layer first.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestStack {
    pub kind: PullRequestStackKind,
    pub id: String,
    pub number: u64,
    pub url: String,
    pub base: String,
    pub layers: Vec<PullRequestStackLayer>,
}

string_enum! {
    /// Who maintains a pull request stack.
    PullRequestStackKind {
        Native = "native",
    }
}

/// One pull request of a stack.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestStackLayer {
    pub number: u64,
    pub head_branch: String,
    pub state: PullRequestState,
}

/// What a watch on a pull request has already seen and told the agent.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestWatch {
    pub started_at: Timestamp,
    pub head_sha: Option<String>,
    pub failed_checks: Vec<String>,
    pub passed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub passed_checks: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owed_checks: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub awaiting_approval: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval_announced: Option<bool>,
    pub remarks_through: Timestamp,
    pub remark_ids: Vec<String>,
    pub conflicting: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rebase_required: Option<bool>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub merge_queue_removal_id: Option<Option<String>>,
    pub wakes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub awaiting_baseline: Option<WatchBaseline>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub checks_awaited_since: Option<Option<Timestamp>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checks_pending: Option<bool>,
    /// How long the watch holds its thread: `merged`, or (absent) until every check finished.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<PullRequestWatchUntil>,
    /// What the watch wakes its thread's agent for. Absent on a watch an
    /// agent started: it is woken for everything the watch sees.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy: Option<PullRequestWatchPolicy>,
    /// How many times in a row the agent was woken to fix failed checks,
    /// since they last passed or the user last wrote.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix_attempts: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_fix_wake_at: Option<Timestamp>,
}

/// What a watch wakes its thread's agent for; everything else it sees only
/// changes what the thread shows (and what the user is notified of).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestWatchPolicy {
    /// Wake the agent to fix checks that failed.
    pub wake_on_failed_checks: bool,
    pub wake_on_passed: bool,
    pub wake_on_merged: bool,
    /// The most wakes in a row for failed checks; `0` is no limit.
    pub max_fix_attempts: u64,
}

impl PullRequestWatchPolicy {
    /// The watch of a pull request a person made from a thread: the agent is
    /// woken only to fix what failed, at most `max_fix_attempts` times.
    pub fn following(wake_on_failed_checks: bool, max_fix_attempts: u64) -> Self {
        PullRequestWatchPolicy { wake_on_failed_checks, wake_on_passed: false, wake_on_merged: false, max_fix_attempts }
    }
}

string_enum! {
    /// How long a pull request watch holds its thread (`merged` also ends
    /// when the pull request closes or the agent stops watching it).
    PullRequestWatchUntil {
        Checks = "checks",
        Merged = "merged",
    }
}

string_enum! {
    /// What a new watch records without reporting on its first read.
    WatchBaseline {
        All = "all",
        Checks = "checks",
    }
}
