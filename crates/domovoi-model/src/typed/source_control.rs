//! Pull requests as the pull requests page lists and opens them
//! (`pullRequest.ts`), and the host a checkout's remote is on
//! (`sourceControl.ts`).
//!
//! `pullRequests.submitReview`, `pullRequests.comment` and
//! `pullRequests.update` answer with nothing (`Schema.Void`, sent as `null`).

use super::common::{Timestamp, double_option};
use super::ids::ProjectId;
use super::list::ForwardList;
use super::macros::string_enum;
use super::pull_request::{
    PullRequestActor, PullRequestChecksState, PullRequestMergeability, PullRequestReviewDecision, PullRequestState,
};
use serde::{Deserialize, Serialize};
use serde_json::Number;
use std::collections::BTreeMap;

/// The host a checkout's primary remote is on (`SourceControlProviderInfo`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceControlProviderInfo {
    /// A source control provider kind (`github`, `gitlab`, …): an open set of names.
    pub kind: String,
    pub name: String,
    pub base_url: String,
}

string_enum! {
    /// Something a host lets a viewer do to a pull request (`PullRequestAction`).
    PullRequestAction {
        Merge = "merge",
        Ready = "ready",
        Draft = "draft",
        Close = "close",
        Reopen = "reopen",
        DeleteBranch = "delete-branch",
        UpdateBranch = "update-branch",
        EnableAutoMerge = "enable-auto-merge",
        DisableAutoMerge = "disable-auto-merge",
        Revert = "revert",
        ApproveWorkflows = "approve-workflows",
    }
}

string_enum! {
    /// How a pull request's commits land on its base (`PullRequestMergeMethod`).
    PullRequestMergeMethod {
        Merge = "merge",
        Squash = "squash",
        Rebase = "rebase",
        FastForward = "fast-forward",
    }
}

string_enum! {
    /// How one check of a pull request ended, or that it still runs (`PullRequestCheckStatus`).
    PullRequestCheckStatus {
        Pending = "pending",
        ActionRequired = "action-required",
        Success = "success",
        Failure = "failure",
        Skipped = "skipped",
        Neutral = "neutral",
        Cancelled = "cancelled",
    }
}

/// A label on a pull request (`PullRequestLabel`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestLabel {
    pub name: String,
    pub color: Option<String>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub description: Option<Option<String>>,
}

/// Where a pull request sits in a stack of dependent ones (`PullRequestStackMembership`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestStackMembership {
    pub number: u64,
    pub position: u64,
    pub size: u64,
    pub base: String,
}

/// What names one pull request in a call about it (`PullRequestRef`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestRef {
    pub project_id: ProjectId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_account_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_stale: Option<bool>,
    pub repository: String,
    pub number: u64,
}

/// One row of the pull request list (`PullRequestListEntry`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestListEntry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stack: Option<PullRequestStackMembership>,
    /// A source control provider kind (`github`, `gitlab`, …): an open set of names.
    pub provider: String,
    pub host: String,
    pub project_id: ProjectId,
    pub project_title: String,
    pub repository: String,
    pub number: u64,
    pub title: String,
    pub url: String,
    pub author: Option<PullRequestActor>,
    pub head_branch: String,
    pub base_branch: String,
    pub state: PullRequestState,
    pub is_draft: bool,
    pub mergeability: PullRequestMergeability,
    /// Zero until read through `pullRequests.listStats`.
    pub additions: u64,
    pub deletions: u64,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    /// When the server read the row, in milliseconds since the epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<Number>,
    pub viewer_review_requested: bool,
    pub labels: Vec<PullRequestLabel>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_decision: Option<PullRequestReviewDecision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checks_state: Option<PullRequestChecksState>,
}

impl PullRequestListEntry {
    /// What names this row's pull request in a later call about it.
    pub fn reference(&self) -> PullRequestRef {
        PullRequestRef {
            project_id: self.project_id.clone(),
            host: None,
            expected_account_id: None,
            allow_stale: None,
            repository: self.repository.clone(),
            number: self.number,
        }
    }
}

/// One host the listed projects' repositories are on (`PullRequestProviderSummary`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestProviderSummary {
    pub host: String,
    /// A source control provider kind (`github`, `gitlab`, …): an open set of names.
    pub kind: String,
    pub searches_on_host: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actions: Option<ForwardList<PullRequestAction>>,
    pub project_count: u64,
    /// The Domovoi server leaves this out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_ids: Option<Vec<ProjectId>>,
    /// Whether the server can read the host: its tool is installed and signed in.
    pub configured: bool,
    /// Why the host cannot be read, when it cannot.
    pub detail: Option<String>,
}

/// A project whose pull requests could not be listed (`PullRequestListProjectError`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestListProjectError {
    pub project_id: ProjectId,
    pub project_title: String,
    pub message: String,
}

/// The result of `pullRequests.list` (`PullRequestListResult`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestListResult {
    /// The signed-in login per host; a host that could not be read is absent.
    pub viewers: BTreeMap<String, String>,
    pub providers: Vec<PullRequestProviderSummary>,
    pub entries: Vec<PullRequestListEntry>,
    pub errors: Vec<PullRequestListProjectError>,
    pub truncated: bool,
    /// Where each repository's listing carries on from; the Domovoi server never hands any out.
    pub next_cursors: BTreeMap<String, String>,
}

/// The result of `pullRequests.summary` (`PullRequestSummary`): one pull request's state, without its text.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestSummary {
    /// A source control provider kind (`github`, `gitlab`, …): an open set of names.
    pub provider: String,
    pub project_id: ProjectId,
    pub repository: String,
    pub number: u64,
    pub title: String,
    pub url: String,
    pub state: PullRequestState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_draft: Option<bool>,
    pub head_branch: String,
    pub base_branch: String,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub closed_at: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub merged_at: Option<Option<String>>,
    pub updated_at: Timestamp,
    /// When the server read the pull request, in milliseconds since the epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<Number>,
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
    /// `null` when the host says the pull request is in no stack; absent when the read did not ask.
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub stack: Option<Option<PullRequestStackMembership>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_sha: Option<String>,
    /// Absent means the read did not report auto-merge, not that it is off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_merge_enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_merge_method: Option<PullRequestMergeMethod>,
}

/// The result of `pullRequests.runAction`: the pull request as the action left
/// it. A t3code server sends it only when asked to (`replyWithSummary`); the
/// Domovoi server always does.
pub type PullRequestActionResult = Option<PullRequestSummary>;

/// One check that ran, or runs, on a pull request's head (`PullRequestCheck`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestCheck {
    pub name: String,
    pub status: PullRequestCheckStatus,
    pub description: Option<String>,
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
    /// Set on a row that stands for checks the host could not list; the Domovoi server sends none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<bool>,
}

/// A pull request's checks (`PullRequestChecks`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestChecks {
    /// When the server read the checks, in milliseconds since the epoch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observed_at: Option<Number>,
    pub state: PullRequestState,
    pub checks: Vec<PullRequestCheck>,
}

/// The result of `pullRequests.checks`: the schema lets a server answer
/// `null`. The Domovoi server always sends the record.
pub type PullRequestChecksResult = Option<PullRequestChecks>;
