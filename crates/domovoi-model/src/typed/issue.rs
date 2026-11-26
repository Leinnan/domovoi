//! Issues and issue trackers (`issue.ts`): a tracker's issues as listings and
//! details carry them, what a call about one names, and the standalone
//! trackers (Linear) a project is bound to; plus the issues linked to a thread
//! (`ThreadIssueLink`, `ThreadIssueKey`).
//!
//! A link records how the tracker named the issue when it was made; nothing
//! refreshes it, and opening the issue reads its live state.

use super::common::Timestamp;
use super::ids::{ProjectId, ThreadId};
use super::macros::string_enum;
use super::pull_request::PullRequestState;
use super::pull_request::PullRequestActor;
use super::source_control::PullRequestLabel;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

string_enum! {
    /// Who linked an issue to a thread (`ThreadIssueLinkSource`).
    ThreadIssueLinkSource {
        Manual = "manual",
        Agent = "agent",
    }
}

/// Identity of an issue as a thread link sees it (`ThreadIssueKey`): tracker
/// level, so the same issue linked through two projects compares equal.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadIssueKey {
    /// `github`, `gitlab`, `linear`, … (`IssueTrackerKind`).
    pub tracker: String,
    pub host: String,
    /// The tracker's own container: `owner/repo`, a team key.
    pub scope: String,
    /// The issue's native id in its scope.
    pub key: String,
}

impl ThreadIssueKey {
    /// Whether two keys name the same issue:
    /// host and scope compare trimmed and case-insensitively, the key trimmed.
    pub fn same_issue(&self, other: &ThreadIssueKey) -> bool {
        self.tracker == other.tracker
            && self.host.trim().eq_ignore_ascii_case(other.host.trim())
            && self.scope.trim().eq_ignore_ascii_case(other.scope.trim())
            && self.key.trim() == other.key.trim()
    }
}

/// An issue linked to a thread (`ThreadIssueLink`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadIssueLink {
    #[serde(flatten)]
    pub key: ThreadIssueKey,
    /// The project the issue is read through.
    pub project_id: ProjectId,
    pub url: String,
    pub source: ThreadIssueLinkSource,
    pub linked_at: Timestamp,
    /// As the tracker wrote the key when it was linked: `#123`, `ENG-123`.
    pub display_key: String,
    pub title: String,
}

// ---------------------------------------------------------------------------
// Closed sets of strings
// ---------------------------------------------------------------------------

string_enum! {
    /// The category every tracker's states fall in (`IssueStateCategory`).
    IssueStateCategory {
        Open = "open",
        Closed = "closed",
    }
}

string_enum! {
    /// What a listing asks for (`IssueListState`).
    IssueListState {
        All = "all",
        Open = "open",
        Closed = "closed",
    }
}

string_enum! {
    /// Whose issues a listing keeps (`IssueInvolvement`).
    IssueInvolvement {
        All = "all",
        Assigned = "assigned",
        Authored = "authored",
        Mentioned = "mentioned",
    }
}

string_enum! {
    /// Newest first by the named date (`IssueListSort`).
    IssueListSort {
        Updated = "updated",
        Created = "created",
    }
}

string_enum! {
    /// Something that happened to an issue (`IssueEventKind`).
    IssueEventKind {
        Closed = "closed",
        Reopened = "reopened",
        Labeled = "labeled",
        Unlabeled = "unlabeled",
        Assigned = "assigned",
        Unassigned = "unassigned",
        Renamed = "renamed",
        Referenced = "referenced",
        StateChanged = "state-changed",
    }
}

string_enum! {
    /// A reaction, in GitHub's names (`IssueReactionContent` = `PullRequestReactionContent`).
    IssueReactionContent {
        ThumbsUp = "thumbs-up",
        ThumbsDown = "thumbs-down",
        Laugh = "laugh",
        Hooray = "hooray",
        Confused = "confused",
        Heart = "heart",
        Rocket = "rocket",
        Eyes = "eyes",
    }
}

impl IssueReactionContent {
    /// Every reaction, in the contract's order.
    pub const ALL: [IssueReactionContent; 8] = [
        IssueReactionContent::ThumbsUp,
        IssueReactionContent::ThumbsDown,
        IssueReactionContent::Laugh,
        IssueReactionContent::Hooray,
        IssueReactionContent::Confused,
        IssueReactionContent::Heart,
        IssueReactionContent::Rocket,
        IssueReactionContent::Eyes,
    ];
}

/// `IssueActor`.
pub type IssueActor = PullRequestActor;
/// `IssueLabel`.
pub type IssueLabel = PullRequestLabel;

// ---------------------------------------------------------------------------
// Issues
// ---------------------------------------------------------------------------

/// Where an issue stands (`IssueState`): the tracker's own name for it and
/// its category.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueState {
    pub category: IssueStateCategory,
    pub name: String,
    /// How a closed state ended, where the tracker says.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub close_reason: Option<String>,
}

/// One kind of reaction to an issue or a comment (`IssueReaction`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueReaction {
    pub content: IssueReactionContent,
    pub count: u64,
    /// Without the viewer, who is in `count` and `viewerHasReacted`.
    pub actors: Vec<String>,
    pub viewer_has_reacted: bool,
}

/// Addresses one issue (`IssueRef`). `host` is absent for the kind's public
/// host; `projectId` names the project whose credentials serve the read.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueRef {
    pub project_id: ProjectId,
    pub tracker: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    pub scope: String,
    pub key: String,
}

/// A scope with no issue in it (`IssueScopeRef`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueScopeRef {
    pub project_id: ProjectId,
    pub tracker: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    pub scope: String,
}

impl From<&IssueRef> for IssueScopeRef {
    fn from(r: &IssueRef) -> Self {
        IssueScopeRef { project_id: r.project_id.clone(), tracker: r.tracker.clone(), host: r.host.clone(), scope: r.scope.clone() }
    }
}

/// One row of a listing (`IssueSummary`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueSummary {
    pub tracker: String,
    pub host: String,
    pub scope: String,
    pub key: String,
    /// `#123` on a git host, `ENG-123` on Linear.
    pub display_key: String,
    pub title: String,
    pub url: String,
    pub author: Option<IssueActor>,
    pub state: IssueState,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub closed_at: Option<Timestamp>,
    pub assignees: Vec<IssueActor>,
    pub labels: Vec<IssueLabel>,
    pub comment_count: u64,
}

/// A row of `issues.list` (`IssueListEntry`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueListEntry {
    #[serde(flatten)]
    pub summary: IssueSummary,
    pub project_id: ProjectId,
    pub project_title: String,
}

/// A comment on an issue (`IssueComment`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueComment {
    pub id: String,
    pub author: Option<IssueActor>,
    pub body: String,
    pub created_at: Timestamp,
    pub url: Option<String>,
    /// Absent from a tracker with no reactions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reactions: Option<Vec<IssueReaction>>,
}

/// Something that happened to an issue that nobody wrote words for (`IssueEvent`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueEvent {
    pub id: String,
    pub kind: IssueEventKind,
    pub actor: Option<IssueActor>,
    pub created_at: Timestamp,
    pub detail: Option<String>,
}

/// A change request the tracker says closes or mentions the issue (`IssueLinkedPullRequest`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueLinkedPullRequest {
    pub repository: String,
    pub number: u64,
    pub title: String,
    pub url: String,
    pub state: PullRequestState,
    pub is_draft: bool,
    /// Merging it closes this issue, rather than only citing it.
    pub closes_issue: bool,
}

/// What a tracker can do at all (`IssueCapabilities`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueCapabilities {
    pub create: bool,
    pub edit: bool,
    pub comment: bool,
    pub labels: bool,
    pub assignees: bool,
    pub reactions: bool,
    pub templates: bool,
    pub linked_pull_requests: bool,
    pub events: bool,
    pub search: bool,
}

impl IssueCapabilities {
    /// A tracker that does everything the contract names.
    pub const ALL: IssueCapabilities = IssueCapabilities {
        create: true,
        edit: true,
        comment: true,
        labels: true,
        assignees: true,
        reactions: true,
        templates: true,
        linked_pull_requests: true,
        events: true,
        search: true,
    };
}

/// What the signed-in account may do with an issue (`IssueViewerPermissions`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueViewerPermissions {
    pub set_state: bool,
    pub comment: bool,
    pub edit: bool,
    pub labels: bool,
    pub assignees: bool,
}

/// One issue with its conversation (`IssueDetail`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueDetail {
    #[serde(flatten)]
    pub summary: IssueSummary,
    pub project_id: ProjectId,
    pub project_title: String,
    /// False for an issue outside every project's scope: read-only here.
    pub writable: bool,
    pub capabilities: IssueCapabilities,
    pub viewer_permissions: IssueViewerPermissions,
    /// Every state the issue can be in, in workflow order.
    pub states: Vec<IssueState>,
    pub viewer: Option<String>,
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reactions: Option<Vec<IssueReaction>>,
    pub comments: Vec<IssueComment>,
    /// Where older comments carry on; null once the conversation is whole.
    pub comments_cursor: Option<String>,
    pub events: Vec<IssueEvent>,
    pub linked_pull_requests: Vec<IssueLinkedPullRequest>,
}

/// `issues.comments`: one page past a cursor (`IssueCommentsPageInput`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueCommentsPageInput {
    #[serde(flatten)]
    pub issue: IssueRef,
    pub cursor: String,
}

/// `IssueCommentsPage`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueCommentsPage {
    pub comments: Vec<IssueComment>,
    pub next_cursor: Option<String>,
}

/// `issues.list` (`IssueListInput`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueListInput {
    pub state: IssueListState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub involvement: Option<IssueInvolvement>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<ProjectId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclude_project_ids: Option<Vec<ProjectId>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
    /// Rows per scope, 1 to 200.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u64>,
    /// Keyed `"<host> <scope>"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursors: Option<BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub labels: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignee: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sort: Option<IssueListSort>,
}

/// One tracker account the workspace has projects on (`IssueTrackerSummary`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueTrackerSummary {
    pub tracker: String,
    pub host: String,
    pub project_count: u64,
    pub capabilities: Option<IssueCapabilities>,
    /// False when the tracker's tool or credentials are missing.
    pub configured: bool,
    pub detail: Option<String>,
}

/// A project whose issues could not be listed (`IssueListProjectError`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueListProjectError {
    pub project_id: ProjectId,
    pub project_title: String,
    pub message: String,
}

/// `IssueListResult`.
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueListResult {
    /// The signed-in account per host that involvement compared against.
    pub viewers: BTreeMap<String, String>,
    pub trackers: Vec<IssueTrackerSummary>,
    /// By update, newest first.
    pub entries: Vec<IssueListEntry>,
    pub errors: Vec<IssueListProjectError>,
    pub next_cursors: BTreeMap<String, String>,
    /// Every scope the listing covers, with one project that reads it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scopes: Option<Vec<IssueScopeRef>>,
}

/// `issues.create` (`IssueCreateInput`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueCreateInput {
    #[serde(flatten)]
    pub scope: IssueScopeRef,
    pub title: String,
    pub body: String,
    pub labels: Vec<String>,
    pub assignees: Vec<String>,
}

/// `IssueCreateResult`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueCreateResult {
    pub key: String,
    pub display_key: String,
    pub url: String,
}

/// `issues.comment` (`IssueCommentInput`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueCommentInput {
    #[serde(flatten)]
    pub issue: IssueRef,
    pub body: String,
}

/// `issues.update` (`IssueUpdateInput`): only the present fields are written.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueUpdateInput {
    #[serde(flatten)]
    pub issue: IssueRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub labels: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub assignees: Option<Vec<String>>,
}

/// `issues.setState` (`IssueSetStateInput`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueSetStateInput {
    #[serde(flatten)]
    pub issue: IssueRef,
    /// The name of one of the issue's `states`.
    pub state: String,
}

/// `issues.react` (`IssueReactionInput`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueReactionInput {
    #[serde(flatten)]
    pub issue: IssueRef,
    /// Absent reacts to the issue itself; present names a comment id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject_id: Option<String>,
    pub content: IssueReactionContent,
    pub reacted: bool,
}

/// A body-shaped starting point for a new issue (`IssueTemplate`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueTemplate {
    pub key: String,
    pub name: String,
    pub about: String,
    pub title: String,
    pub body: String,
    pub labels: Vec<String>,
    pub assignees: Vec<String>,
}

/// A starting point only the tracker's own site renders (`IssueExternalTemplate`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueExternalTemplate {
    pub name: String,
    pub about: String,
    pub url: String,
}

/// `IssueTemplateList`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueTemplateList {
    pub templates: Vec<IssueTemplate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_templates: Option<Vec<IssueExternalTemplate>>,
    pub blank_issues_enabled: bool,
}

/// A label a scope offers (`IssueLabelCandidate`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueLabelCandidate {
    pub name: String,
    pub color: Option<String>,
    pub description: Option<String>,
}

/// A person a scope offers (`IssueAssigneeCandidate`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueAssigneeCandidate {
    #[serde(flatten)]
    pub actor: IssueActor,
    /// How the tracker addresses this person on a write.
    pub id: String,
}

/// `IssueCandidates`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueCandidates {
    pub labels: Vec<IssueLabelCandidate>,
    pub assignees: Vec<IssueAssigneeCandidate>,
    pub truncated: bool,
}

/// A thread an issue is linked to.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueLinkedThread {
    pub id: ThreadId,
    pub project_id: ProjectId,
    pub title: String,
    pub archived_at: Option<Timestamp>,
    pub settled: bool,
}

/// The threads an issue is linked to, most recently active first (`IssueLinkedThreadsResult`).
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueLinkedThreadsResult {
    pub threads: Vec<IssueLinkedThread>,
}

// ---------------------------------------------------------------------------
// Standalone trackers
// ---------------------------------------------------------------------------

/// The credential a standalone tracker is connected with.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueTrackerCredential {
    /// "Personal API key", "API token".
    pub label: String,
    pub help_url: Option<String>,
}

/// A tracker that is not a git host (`StandaloneIssueTrackerDescriptor`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StandaloneIssueTrackerDescriptor {
    pub tracker: String,
    pub display_name: String,
    pub host: String,
    /// "team" on Linear.
    pub scope_noun: String,
    pub credential: IssueTrackerCredential,
}

/// Which standalone tracker scope a project's issues come from (`IssueTrackerProjectBinding`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueTrackerProjectBinding {
    pub tracker: String,
    pub scope: String,
}

/// One container the connected account can read (`IssueTrackerScope`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueTrackerScope {
    pub key: String,
    pub name: String,
}

/// A standalone tracker as the settings page sees it (`IssueTrackerConnection`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueTrackerConnection {
    #[serde(flatten)]
    pub descriptor: StandaloneIssueTrackerDescriptor,
    /// A credential is stored (it may still be refused: `account` null).
    pub connected: bool,
    pub account: Option<String>,
    pub detail: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<String>,
    pub scopes: Vec<IssueTrackerScope>,
}

/// `IssueTrackerConnectionList`.
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueTrackerConnectionList {
    pub trackers: Vec<IssueTrackerConnection>,
    /// Project id → binding; a project absent here reads no standalone tracker.
    pub bindings: BTreeMap<String, IssueTrackerProjectBinding>,
}

/// `issueTrackers.connect` (`IssueTrackerConnectInput`): `null` disconnects.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueTrackerConnectInput {
    pub tracker: String,
    pub token: Option<String>,
}

/// `issueTrackers.bindProject` (`IssueTrackerBindProjectInput`): `null` unbinds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IssueTrackerBindProjectInput {
    pub project_id: ProjectId,
    pub binding: Option<IssueTrackerProjectBinding>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_link_round_trips_and_keys_compare_loosely() {
        let sent = json!({ "tracker": "github", "host": "github.com", "scope": "o/r", "key": "7", "projectId": "p1",
            "url": "https://github.com/o/r/issues/7", "source": "agent", "linkedAt": "2026-11-24T00:00:00.000Z", "displayKey": "#7", "title": "Bug" });
        let link: ThreadIssueLink = serde_json::from_value(sent.clone()).unwrap();
        assert_eq!(serde_json::to_value(&link).unwrap(), sent);
        let other = ThreadIssueKey { tracker: "github".into(), host: " GitHub.com".into(), scope: "O/R".into(), key: "7 ".into() };
        assert!(link.key.same_issue(&other));
        assert!(!link.key.same_issue(&ThreadIssueKey { key: "8".into(), ..other }));
    }

    #[test]
    fn issue_records_round_trip() {
        let summary = json!({ "tracker": "github", "host": "github.com", "scope": "o/r", "key": "7", "displayKey": "#7", "title": "Bug",
            "url": "https://github.com/o/r/issues/7", "author": { "login": "a", "name": null, "avatarUrl": null },
            "state": { "category": "closed", "name": "Completed", "closeReason": "completed" },
            "createdAt": "2026-01-01T00:00:00Z", "updatedAt": "2026-01-02T00:00:00Z", "closedAt": null,
            "assignees": [], "labels": [{ "name": "bug", "color": "d73a4a" }], "commentCount": 2 });
        let mut detail = summary.clone();
        let extra = json!({ "projectId": "p", "projectTitle": "P", "writable": true,
            "capabilities": { "create": true, "edit": true, "comment": true, "labels": true, "assignees": true, "reactions": true,
                "templates": true, "linkedPullRequests": true, "events": true, "search": true },
            "viewerPermissions": { "setState": true, "comment": true, "edit": true, "labels": false, "assignees": false },
            "states": [{ "category": "open", "name": "Open" }], "viewer": "a", "body": "",
            "reactions": [{ "content": "heart", "count": 1, "actors": [], "viewerHasReacted": true }],
            "comments": [{ "id": "c1", "author": null, "body": "x", "createdAt": "2026-01-01T00:00:00Z", "url": null }],
            "commentsCursor": null, "events": [{ "id": "e", "kind": "state-changed", "actor": null, "createdAt": "2026-01-01T00:00:00Z", "detail": "Done" }],
            "linkedPullRequests": [{ "repository": "o/r", "number": 3, "title": "Fix", "url": "u", "state": "merged", "isDraft": false, "closesIssue": true }] });
        for (k, v) in extra.as_object().unwrap() {
            detail[k] = v.clone();
        }
        let read: IssueDetail = serde_json::from_value(detail.clone()).unwrap();
        assert_eq!(serde_json::to_value(&read).unwrap(), detail);
        let connection = json!({ "tracker": "linear", "displayName": "Linear", "host": "linear.app", "scopeNoun": "team",
            "credential": { "label": "Personal API key", "helpUrl": null }, "connected": true, "account": "me", "detail": null,
            "workspace": "acme", "scopes": [{ "key": "ENG", "name": "Engineering" }] });
        let read: IssueTrackerConnection = serde_json::from_value(connection.clone()).unwrap();
        assert_eq!(serde_json::to_value(&read).unwrap(), connection);
        let input = json!({ "projectId": "p", "tracker": "github", "scope": "o/r", "key": "7", "content": "eyes", "reacted": false });
        let read: IssueReactionInput = serde_json::from_value(input.clone()).unwrap();
        assert_eq!(serde_json::to_value(&read).unwrap(), input);
    }
}
