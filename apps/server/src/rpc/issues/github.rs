//! GitHub as an issue tracker (t3code `GitHubIssueTracker`, `gitHubIssueJson`,
//! `gitHubIssueTemplates` and the `issues.graphql` operations), through the
//! GitHub CLI (`crate::gh`): reads are GitHub's GraphQL API, writes its REST
//! API, as upstream has them. A scope is a repository (`owner/name`), a key an
//! issue number.
//!
//! Limits: `gh` answers no rate-limit reset time, so a rate-limited failure
//! carries no `retryAt`; an issue's labels and assignees past the first 100,
//! and the pull requests closing it past the first 20, are not paged in;
//! GitHub's YAML issue forms are not flattened into templates (there is no
//! YAML reader here): a scope lists its markdown templates only, and
//! `config.yml`'s `blank_issues_enabled` is read by its line alone.

use super::{IssuePage, ListQuery, Served, TrackerError, TrackerIssue, TrackerReason};
use crate::gh::lenient::{Fields, Loose, Rows, rows, trimmed};
use crate::gh::{self, GhError, GhReason, Var};
use domovoi_model::typed::{
    IssueActor, IssueAssigneeCandidate, IssueCandidates, IssueCapabilities, IssueComment, IssueCommentsPage, IssueCreateResult, IssueEvent, IssueEventKind,
    IssueInvolvement, IssueLabel, IssueLabelCandidate, IssueLinkedPullRequest, IssueListState, IssueReaction, IssueReactionContent, IssueState,
    IssueStateCategory, IssueSummary, IssueTemplate, IssueTemplateList, IssueViewerPermissions, PullRequestState,
};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

pub const KIND: &str = "github";

pub const CAPABILITIES: IssueCapabilities = IssueCapabilities::ALL;

// ---------------------------------------------------------------------------
// The operations (`issues.graphql`)
// ---------------------------------------------------------------------------

const ISSUE_ROW: &str = "fragment IssueRow on Issue { number title url state stateReason createdAt updatedAt closedAt author { login avatarUrl ... on User { name } } }";

const REACTIONS: &str = "fragment Reactions on Reactable { reactionGroups { content viewerHasReacted reactors(first: 10) { totalCount nodes { ... on User { login } ... on Bot { login } ... on Organization { login } ... on Mannequin { login } } } } }";

const COMMENT_NODE: &str = "id author { login avatarUrl } body createdAt url ...Reactions";

fn search_query() -> String {
    format!(
        "query IssueSearch($q: String!, $cursor: String, $rows: Int!) {{ search(query: $q, type: ISSUE, first: $rows, after: $cursor) {{ pageInfo {{ hasNextPage endCursor }} nodes {{ __typename ... on Issue {{ ...IssueRow assignees(first: 20) {{ nodes {{ login name avatarUrl }} }} labels(first: 20) {{ nodes {{ name color }} }} comments {{ totalCount }} }} }} }} }} {ISSUE_ROW}"
    )
}

fn detail_query() -> String {
    let actor = "actor { login avatarUrl }";
    let assignee = "assignee { ... on User { login name } ... on Bot { login } }";
    let pr = "number title url state isDraft repository { nameWithOwner }";
    format!(
        "query IssueDetail($owner: String!, $name: String!, $number: Int!) {{ viewer {{ login }} repository(owner: $owner, name: $name) {{ viewerPermission issue(number: $number) {{ ...IssueRow \
         assignees(first: 100) {{ pageInfo {{ hasNextPage endCursor }} nodes {{ login name avatarUrl }} }} labels(first: 100) {{ pageInfo {{ hasNextPage endCursor }} nodes {{ name color }} }} \
         body viewerCanUpdate viewerCanClose viewerCanReopen ...Reactions \
         comments(last: 100) {{ totalCount pageInfo {{ hasPreviousPage startCursor }} nodes {{ {COMMENT_NODE} }} }} \
         closedByPullRequestsReferences(first: 20, includeClosedPrs: true) {{ pageInfo {{ hasNextPage endCursor }} nodes {{ {pr} }} }} \
         timelineItems(last: 100, itemTypes: [CLOSED_EVENT, REOPENED_EVENT, LABELED_EVENT, UNLABELED_EVENT, ASSIGNED_EVENT, UNASSIGNED_EVENT, RENAMED_TITLE_EVENT, CROSS_REFERENCED_EVENT]) {{ nodes {{ __typename \
         ... on ClosedEvent {{ id createdAt {actor} }} ... on ReopenedEvent {{ id createdAt {actor} }} \
         ... on LabeledEvent {{ id createdAt {actor} label {{ name }} }} ... on UnlabeledEvent {{ id createdAt {actor} label {{ name }} }} \
         ... on AssignedEvent {{ id createdAt {actor} {assignee} }} ... on UnassignedEvent {{ id createdAt {actor} {assignee} }} \
         ... on RenamedTitleEvent {{ id createdAt {actor} currentTitle }} \
         ... on CrossReferencedEvent {{ id createdAt {actor} source {{ __typename ... on PullRequest {{ {pr} }} ... on Issue {{ number repository {{ nameWithOwner }} }} }} }} \
         }} }} }} }} }} {ISSUE_ROW} {REACTIONS}"
    )
}

fn comments_query() -> String {
    format!(
        "query IssueComments($owner: String!, $name: String!, $number: Int!, $cursor: String!) {{ viewer {{ login }} repository(owner: $owner, name: $name) {{ issue(number: $number) {{ comments(last: 100, before: $cursor) {{ pageInfo {{ hasPreviousPage startCursor }} nodes {{ {COMMENT_NODE} }} }} }} }} }} {REACTIONS}"
    )
}

const NODE_ID_QUERY: &str = "query IssueNodeId($owner: String!, $name: String!, $number: Int!) { repository(owner: $owner, name: $name) { issue(number: $number) { id state stateReason } } }";

const COMMENT_SCOPE_QUERY: &str = "query IssueCommentScope($owner: String!, $name: String!, $number: Int!, $commentId: ID!) { repository(owner: $owner, name: $name) { issue(number: $number) { id } } node(id: $commentId) { __typename ... on IssueComment { issue { id } } } }";

const CANDIDATES_QUERY: &str = "query IssueCandidates($owner: String!, $name: String!) { repository(owner: $owner, name: $name) { labels(first: 100) { pageInfo { hasNextPage } nodes { name color description } } assignableUsers(first: 100) { pageInfo { hasNextPage } nodes { login name avatarUrl } } } }";

const TEMPLATES_QUERY: &str = "query IssueTemplates($owner: String!, $name: String!) { repository(owner: $owner, name: $name) { templateDirectory: object(expression: \"HEAD:.github/ISSUE_TEMPLATE\") { __typename ... on Tree { entries { name object { __typename ... on Blob { text isTruncated } } } } } issueTemplates { filename name about title body assignees(first: 25) { nodes { login } } labels(first: 50) { nodes { name } } } } }";

const ADD_REACTION: &str = "mutation AddReaction($subjectId: ID!, $content: ReactionContent!) { addReaction(input: { subjectId: $subjectId, content: $content }) { reaction { content } } }";
const REMOVE_REACTION: &str = "mutation RemoveReaction($subjectId: ID!, $content: ReactionContent!) { removeReaction(input: { subjectId: $subjectId, content: $content }) { reaction { content } } }";

// ---------------------------------------------------------------------------
// What GitHub answers, as far as it is read
// ---------------------------------------------------------------------------

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct PageInfo {
    has_next_page: Loose<bool>,
    end_cursor: Loose<String>,
    has_previous_page: Loose<bool>,
    start_cursor: Loose<String>,
}

#[derive(Deserialize)]
#[serde(default, rename_all = "camelCase", bound(deserialize = "T: Deserialize<'de> + Default"))]
struct Conn<T> {
    total_count: Loose<u64>,
    page_info: Fields<PageInfo>,
    nodes: Rows<T>,
}

impl<T> Default for Conn<T> {
    fn default() -> Self {
        Conn { total_count: Loose(None), page_info: Fields(PageInfo::default()), nodes: Loose(None) }
    }
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct GhActor {
    login: Loose<String>,
    name: Loose<String>,
    avatar_url: Loose<String>,
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
struct GhReactionGroup {
    content: Loose<String>,
    viewer_has_reacted: Loose<bool>,
    reactors: Fields<Conn<GhActor>>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct GhComment {
    id: Loose<String>,
    author: Fields<GhActor>,
    body: Loose<String>,
    created_at: Loose<String>,
    url: Loose<String>,
    reaction_groups: Rows<GhReactionGroup>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct GhRepositoryName {
    name_with_owner: Loose<String>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct GhPullRequest {
    #[serde(rename = "__typename")]
    typename: Loose<String>,
    number: Loose<u64>,
    title: Loose<String>,
    url: Loose<String>,
    state: Loose<String>,
    is_draft: Loose<bool>,
    repository: Fields<GhRepositoryName>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct GhTimelineItem {
    #[serde(rename = "__typename")]
    typename: Loose<String>,
    id: Loose<String>,
    created_at: Loose<String>,
    actor: Fields<GhActor>,
    label: Fields<GhLabel>,
    assignee: Fields<GhActor>,
    current_title: Loose<String>,
    source: Fields<GhPullRequest>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct GhIssue {
    #[serde(rename = "__typename")]
    typename: Loose<String>,
    id: Loose<String>,
    number: Loose<u64>,
    title: Loose<String>,
    url: Loose<String>,
    state: Loose<String>,
    state_reason: Loose<String>,
    created_at: Loose<String>,
    updated_at: Loose<String>,
    closed_at: Loose<String>,
    author: Fields<GhActor>,
    assignees: Fields<Conn<GhActor>>,
    labels: Fields<Conn<GhLabel>>,
    comments: Fields<Conn<GhComment>>,
    body: Loose<String>,
    viewer_can_update: Loose<bool>,
    viewer_can_close: Loose<bool>,
    viewer_can_reopen: Loose<bool>,
    reaction_groups: Rows<GhReactionGroup>,
    closed_by_pull_requests_references: Fields<Conn<GhPullRequest>>,
    timeline_items: Fields<Conn<GhTimelineItem>>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct GhSearch {
    search: Fields<Conn<GhIssue>>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct GhViewer {
    login: Loose<String>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct GhIssueRepository {
    viewer_permission: Loose<String>,
    issue: Fields<GhIssue>,
    labels: Fields<Conn<GhLabel>>,
    assignable_users: Fields<Conn<GhActor>>,
    template_directory: Fields<GhTree>,
    issue_templates: Rows<GhTemplate>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct GhNode {
    #[serde(rename = "__typename")]
    typename: Loose<String>,
    issue: Fields<GhIssue>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct GhIssueAnswer {
    viewer: Fields<GhViewer>,
    repository: Fields<GhIssueRepository>,
    node: Fields<GhNode>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct GhTree {
    #[serde(rename = "__typename")]
    typename: Loose<String>,
    entries: Rows<GhTreeEntry>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct GhTreeEntry {
    name: Loose<String>,
    object: Fields<GhBlob>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct GhBlob {
    #[serde(rename = "__typename")]
    typename: Loose<String>,
    text: Loose<String>,
    is_truncated: Loose<bool>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct GhLogin {
    login: Loose<String>,
    name: Loose<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct GhTemplate {
    filename: Loose<String>,
    name: Loose<String>,
    about: Loose<String>,
    title: Loose<String>,
    body: Loose<String>,
    assignees: Fields<Conn<GhLogin>>,
    labels: Fields<Conn<GhLogin>>,
}

/// A REST issue answer: what `create` and `update` read of it.
#[derive(Deserialize, Default)]
#[serde(default)]
struct GhRestIssue {
    number: Loose<u64>,
    html_url: Loose<String>,
    labels: Loose<Vec<RestLabel>>,
    assignees: Rows<GhLogin>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RestLabel {
    Name(String),
    Label { name: String },
}

impl RestLabel {
    fn name(&self) -> &str {
        match self {
            RestLabel::Name(name) | RestLabel::Label { name } => name,
        }
    }
}

// ---------------------------------------------------------------------------
// Reading GitHub's answer as the contract's records (`gitHubIssueJson`)
// ---------------------------------------------------------------------------

fn text(value: &Loose<String>) -> String {
    value.as_deref().unwrap_or_default().to_owned()
}

fn actor(raw: &GhActor) -> Option<IssueActor> {
    Some(IssueActor { is_bot: None, login: trimmed(&raw.login)?, name: trimmed(&raw.name), avatar_url: trimmed(&raw.avatar_url) })
}

fn actors(raw: &Rows<GhActor>) -> Vec<IssueActor> {
    rows(raw).filter_map(actor).collect()
}

fn labels(raw: &Rows<GhLabel>) -> Vec<IssueLabel> {
    rows(raw).filter_map(|l| Some(IssueLabel { name: trimmed(&l.name)?, color: trimmed(&l.color), description: None })).collect()
}

fn state_named(category: IssueStateCategory, name: &str, reason: Option<&str>) -> IssueState {
    IssueState { category, name: name.to_owned(), close_reason: reason.map(str::to_owned) }
}

/// What an issue can be moved to: open, or closed for a reason GitHub records (`ISSUE_STATES`).
pub fn issue_states() -> Vec<IssueState> {
    vec![
        state_named(IssueStateCategory::Open, "Open", None),
        state_named(IssueStateCategory::Closed, "Completed", Some("completed")),
        state_named(IssueStateCategory::Closed, "Not planned", Some("not-planned")),
        state_named(IssueStateCategory::Closed, "Duplicate", Some("duplicate")),
    ]
}

/// GitHub's close reasons are upper case; an open issue has none or `REOPENED`.
fn issue_state(state: Option<&str>, reason: Option<&str>) -> IssueState {
    if !state.is_some_and(|s| s.trim().eq_ignore_ascii_case("CLOSED")) {
        return state_named(IssueStateCategory::Open, "Open", None);
    }
    match reason.map(|r| r.trim().to_uppercase()).as_deref() {
        Some("COMPLETED") => state_named(IssueStateCategory::Closed, "Completed", Some("completed")),
        Some("NOT_PLANNED") => state_named(IssueStateCategory::Closed, "Not planned", Some("not-planned")),
        Some("DUPLICATE") => state_named(IssueStateCategory::Closed, "Duplicate", Some("duplicate")),
        _ => state_named(IssueStateCategory::Closed, "Closed", None),
    }
}

fn summary(raw: &GhIssue, target: &Served) -> Option<IssueSummary> {
    let number = raw.number.0?;
    Some(IssueSummary {
        tracker: KIND.into(),
        host: target.host.clone(),
        scope: target.scope.clone(),
        key: number.to_string(),
        display_key: format!("#{number}"),
        title: text(&raw.title),
        url: text(&raw.url),
        author: actor(&raw.author),
        state: issue_state(raw.state.as_deref(), raw.state_reason.as_deref()),
        created_at: text(&raw.created_at).as_str().into(),
        updated_at: text(&raw.updated_at).as_str().into(),
        closed_at: trimmed(&raw.closed_at).map(|at| at.as_str().into()),
        assignees: actors(&raw.assignees.nodes),
        labels: labels(&raw.labels.nodes),
        comment_count: raw.comments.total_count.unwrap_or(0),
    })
}

/// GitHub's name of each reaction (`THUMBS_UP`) beside the contract's.
const REACTION_NAMES: [(&str, IssueReactionContent); 8] = [
    ("THUMBS_UP", IssueReactionContent::ThumbsUp),
    ("THUMBS_DOWN", IssueReactionContent::ThumbsDown),
    ("LAUGH", IssueReactionContent::Laugh),
    ("HOORAY", IssueReactionContent::Hooray),
    ("CONFUSED", IssueReactionContent::Confused),
    ("HEART", IssueReactionContent::Heart),
    ("ROCKET", IssueReactionContent::Rocket),
    ("EYES", IssueReactionContent::Eyes),
];

fn github_reaction(content: &IssueReactionContent) -> Option<&'static str> {
    REACTION_NAMES.iter().find(|(_, c)| c == content).map(|(g, _)| *g)
}

/// `toReactions`: the viewer is in the count, not among the actors.
fn reactions(groups: &Rows<GhReactionGroup>, viewer: Option<&str>) -> Vec<IssueReaction> {
    rows(groups)
        .filter_map(|group| {
            let name = trimmed(&group.content)?.to_uppercase();
            let content = REACTION_NAMES.iter().find(|(g, _)| *g == name)?.1.clone();
            let logins: Vec<String> = rows(&group.reactors.nodes).filter_map(|n| trimmed(&n.login)).collect();
            let count = group.reactors.total_count.unwrap_or(0).max(logins.len() as u64);
            if count == 0 {
                return None;
            }
            let actors = logins.into_iter().filter(|login| viewer.is_none_or(|v| !login.eq_ignore_ascii_case(v))).collect();
            Some(IssueReaction { content, count, actors, viewer_has_reacted: group.viewer_has_reacted.0 == Some(true) })
        })
        .collect()
}

fn comments(raw: &Conn<GhComment>, viewer: Option<&str>) -> Vec<IssueComment> {
    rows(&raw.nodes)
        .filter_map(|c| {
            Some(IssueComment {
                id: trimmed(&c.id)?,
                author: actor(&c.author),
                body: text(&c.body),
                created_at: text(&c.created_at).as_str().into(),
                url: trimmed(&c.url),
                reactions: Some(reactions(&c.reaction_groups, viewer)),
            })
        })
        .collect()
}

fn previous_cursor(page: &PageInfo) -> Option<String> {
    if page.has_previous_page.0 == Some(true) { trimmed(&page.start_cursor) } else { None }
}

fn pull_request_state(state: Option<&str>) -> PullRequestState {
    match state.map(|s| s.trim().to_uppercase()).as_deref() {
        Some("MERGED") => PullRequestState::Merged,
        Some("CLOSED") => PullRequestState::Closed,
        _ => PullRequestState::Open,
    }
}

fn linked_pull_request(raw: &GhPullRequest, closes_issue: bool) -> Option<IssueLinkedPullRequest> {
    Some(IssueLinkedPullRequest {
        repository: trimmed(&raw.repository.name_with_owner)?,
        number: raw.number.0.filter(|n| *n > 0)?,
        title: trimmed(&raw.title)?,
        url: trimmed(&raw.url)?,
        state: pull_request_state(raw.state.as_deref()),
        is_draft: raw.is_draft.0 == Some(true),
        closes_issue,
    })
}

/// The change GitHub recorded, or `None` for a kind the contract has no word for.
fn event_fields(item: &GhTimelineItem) -> Option<(IssueEventKind, Option<String>)> {
    Some(match item.typename.as_deref()? {
        "ClosedEvent" => (IssueEventKind::Closed, None),
        "ReopenedEvent" => (IssueEventKind::Reopened, None),
        "LabeledEvent" => (IssueEventKind::Labeled, trimmed(&item.label.name)),
        "UnlabeledEvent" => (IssueEventKind::Unlabeled, trimmed(&item.label.name)),
        "AssignedEvent" => (IssueEventKind::Assigned, trimmed(&item.assignee.login)),
        "UnassignedEvent" => (IssueEventKind::Unassigned, trimmed(&item.assignee.login)),
        "RenamedTitleEvent" => (IssueEventKind::Renamed, trimmed(&item.current_title)),
        "CrossReferencedEvent" => {
            let repository = trimmed(&item.source.repository.name_with_owner);
            let number = item.source.number.0.unwrap_or(0);
            (IssueEventKind::Referenced, repository.filter(|_| number > 0).map(|r| format!("{r}#{number}")))
        }
        _ => return None,
    })
}

/// `toViewerPermissions`: triage may close an issue it may not edit.
fn viewer_permissions(permission: Option<&str>, issue: &GhIssue) -> IssueViewerPermissions {
    let can_triage = matches!(permission.map(|p| p.trim().to_uppercase()).as_deref(), Some("ADMIN" | "MAINTAIN" | "WRITE" | "TRIAGE"));
    let closed = issue.state.as_deref().is_some_and(|s| s.eq_ignore_ascii_case("CLOSED"));
    IssueViewerPermissions {
        set_state: if closed { issue.viewer_can_reopen.0 == Some(true) } else { issue.viewer_can_close.0 == Some(true) },
        comment: true,
        edit: issue.viewer_can_update.0 != Some(false),
        labels: can_triage,
        assignees: can_triage,
    }
}

fn detail(answer: &GhIssueAnswer, target: &Served) -> Option<TrackerIssue> {
    let issue = &answer.repository.issue;
    let summary = summary(issue, target)?;
    let viewer = trimmed(&answer.viewer.login);
    let mut links: Vec<(String, IssueLinkedPullRequest)> = vec![];
    for node in rows(&issue.closed_by_pull_requests_references.nodes) {
        if let Some(link) = linked_pull_request(node, true) {
            links.push((format!("{}#{}", link.repository.to_lowercase(), link.number), link));
        }
    }
    let mut events = vec![];
    for item in rows(&issue.timeline_items.nodes) {
        if item.typename.as_deref() == Some("CrossReferencedEvent") && item.source.typename.as_deref() == Some("PullRequest") {
            if let Some(link) = linked_pull_request(&item.source, false) {
                let key = format!("{}#{}", link.repository.to_lowercase(), link.number);
                if !links.iter().any(|(k, _)| *k == key) {
                    links.push((key, link));
                }
            }
        }
        let (Some((kind, detail)), Some(id), Some(created_at)) = (event_fields(item), trimmed(&item.id), trimmed(&item.created_at)) else { continue };
        events.push(IssueEvent { id, kind, actor: actor(&item.actor), created_at: created_at.as_str().into(), detail });
    }
    Some(TrackerIssue {
        summary,
        body: text(&issue.body),
        viewer_permissions: viewer_permissions(answer.repository.viewer_permission.as_deref(), issue),
        states: issue_states(),
        reactions: Some(reactions(&issue.reaction_groups, viewer.as_deref())),
        comments: comments(&issue.comments, viewer.as_deref()),
        comments_cursor: previous_cursor(&issue.comments.page_info),
        events,
        linked_pull_requests: links.into_iter().map(|(_, link)| link).collect(),
        viewer,
    })
}

// ---------------------------------------------------------------------------
// Failures
// ---------------------------------------------------------------------------

fn reason_of(error: &GhError) -> TrackerReason {
    match error.reason {
        GhReason::MissingTool => TrackerReason::MissingTool,
        GhReason::Unauthenticated => TrackerReason::Unauthenticated,
        GhReason::RateLimited => TrackerReason::RateLimited,
        GhReason::NotFound => TrackerReason::NotFound,
        GhReason::Forbidden | GhReason::Failed => TrackerReason::Failed,
    }
}

/// t3code's `FAILURE_DETAIL`.
fn failure_detail(reason: TrackerReason) -> &'static str {
    match reason {
        TrackerReason::MissingTool => "GitHub CLI is not installed.",
        TrackerReason::Unauthenticated => "GitHub authentication is unavailable. Sign in and retry.",
        TrackerReason::RateLimited => "GitHub API rate limit exceeded.",
        TrackerReason::NotFound => "GitHub could not find the requested issue.",
        TrackerReason::Failed => "The GitHub issue request failed.",
    }
}

fn fail(operation: &str) -> impl Fn(GhError) -> TrackerError + '_ {
    move |error| {
        let reason = reason_of(&error);
        TrackerError::new(KIND, operation, reason, failure_detail(reason))
    }
}

fn invalid(operation: &str, detail: impl Into<String>) -> TrackerError {
    TrackerError::new(KIND, operation, TrackerReason::Failed, detail)
}

fn not_found(operation: &str) -> TrackerError {
    TrackerError::new(KIND, operation, TrackerReason::NotFound, failure_detail(TrackerReason::NotFound))
}

// ---------------------------------------------------------------------------
// Addressing
// ---------------------------------------------------------------------------

/// `owner/name` of a scope GitHub can address.
fn repository<'a>(scope: &'a str, operation: &str) -> Result<(&'a str, &'a str), TrackerError> {
    let scope = scope.trim();
    let refuse = || invalid(operation, "A repository was named that GitHub cannot address.");
    let (owner, name) = scope.split_once('/').ok_or_else(refuse)?;
    let owner_ok = owner.chars().next().is_some_and(|c| c.is_ascii_alphanumeric()) && owner.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
    let name_ok = !name.is_empty() && name != "." && name != ".." && name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'));
    if owner_ok && name_ok { Ok((owner, name)) } else { Err(refuse()) }
}

/// The issue number a key names: a positive 32-bit integer written plainly.
pub fn issue_number(key: &str) -> Option<u64> {
    let key = key.trim();
    if key.is_empty() || key.starts_with('0') || !key.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    key.parse::<u64>().ok().filter(|n| *n <= 2_147_483_647)
}

struct IssueAt<'a> {
    owner: &'a str,
    name: &'a str,
    number: u64,
}

impl IssueAt<'_> {
    fn path(&self) -> String {
        format!("repos/{}/{}/issues/{}", self.owner, self.name, self.number)
    }
}

fn issue_at<'a>(scope: &'a str, key: &str, operation: &str) -> Result<IssueAt<'a>, TrackerError> {
    let (owner, name) = repository(scope, operation)?;
    let number = issue_number(key).ok_or_else(|| not_found(operation))?;
    Ok(IssueAt { owner, name, number })
}

async fn graphql<T: serde::de::DeserializeOwned + Default>(target: &Served, operation: &str, query: &str, vars: &[(&str, Var<'_>)]) -> Result<T, TrackerError> {
    gh::graphql(Some(&target.workspace_root), &target.host, query, vars).await.map_err(|e| {
        // `gh` read nothing it could hand over: GitHub's answer was not one.
        if e.reason == GhReason::Failed && e.detail == UNREADABLE {
            invalid(operation, format!("GitHub returned an unreadable {operation} response."))
        } else {
            fail(operation)(e)
        }
    })
}

/// What `gh::run_json` says of an answer that is not JSON.
const UNREADABLE: &str = "The GitHub CLI answered with unreadable JSON.";

async fn rest<B: Serialize>(target: &Served, operation: &str, method: &str, path: &str, body: &B) -> Result<String, TrackerError> {
    let payload = serde_json::to_string(body).map_err(|_| invalid(operation, "The request could not be written."))?;
    gh::run(Some(&target.workspace_root), &target.host, &["api", "-X", method, path, "--input", "-"], Some(&payload)).await.map_err(fail(operation))
}

/// REST issue endpoints take pull requests as well: resolve an Issue before writing.
async fn verified_issue<'a>(target: &'a Served, key: &str, operation: &str) -> Result<(IssueAt<'a>, GhIssue), TrackerError> {
    let at = issue_at(&target.scope, key, operation)?;
    let answer: GhIssueAnswer =
        graphql(target, operation, NODE_ID_QUERY, &[("owner", Var::Text(at.owner)), ("name", Var::Text(at.name)), ("number", Var::number(at.number))]).await?;
    let found = answer.repository.0.issue.0;
    if trimmed(&found.id).is_none() {
        return Err(not_found(operation));
    }
    Ok((at, found))
}

// ---------------------------------------------------------------------------
// The tracker
// ---------------------------------------------------------------------------

/// The signed-in account, which involvement filtering compares against.
pub async fn viewer(target: &Served) -> Result<String, TrackerError> {
    gh::identity(Some(&target.workspace_root), &target.host).await.map(|(_, login)| login).map_err(fail("getViewer"))
}

/// Quoted, so no character in it can start a qualifier of its own.
fn quoted(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

/// The search GitHub is asked for (`searchQuery`).
pub fn search_terms(scope: &str, q: &ListQuery, viewer: Option<&str>) -> String {
    let mut terms = vec!["is:issue".to_owned(), format!("repo:{}", scope.trim())];
    match q.state {
        IssueListState::Open => terms.push("is:open".into()),
        IssueListState::Closed => terms.push("is:closed".into()),
        _ => {}
    }
    let qualifier = match q.involvement {
        IssueInvolvement::Assigned => Some("assignee"),
        IssueInvolvement::Authored => Some("author"),
        IssueInvolvement::Mentioned => Some("mentions"),
        _ => None,
    };
    if let (Some(qualifier), Some(viewer)) = (qualifier, viewer) {
        terms.push(format!("{qualifier}:{viewer}"));
    }
    terms.extend(q.labels.iter().map(|label| format!("label:{}", quoted(label))));
    if let Some(assignee) = &q.assignee {
        terms.push(format!("assignee:{}", quoted(assignee)));
    }
    terms.push(format!("sort:{}-desc", q.sort.as_str()));
    terms.extend(q.query.as_deref().unwrap_or_default().split_whitespace().map(quoted));
    terms.join(" ")
}

pub async fn list_issues(target: &Served, q: &ListQuery, viewer: Option<&str>) -> Result<IssuePage, TrackerError> {
    repository(&target.scope, "listIssues")?;
    let search = search_terms(&target.scope, q, viewer);
    let rows_wanted = q.limit.clamp(1, 100);
    let cursor = match &q.cursor {
        Some(c) => Var::Text(c),
        None => Var::null(),
    };
    let answer: GhSearch = graphql(target, "listIssues", &search_query(), &[("q", Var::Text(&search)), ("cursor", cursor), ("rows", Var::number(rows_wanted))]).await?;
    let page = &answer.search;
    let items = rows(&page.nodes).filter(|n| n.typename.as_deref().is_none_or(|t| t == "Issue")).filter_map(|n| summary(n, target)).collect();
    let next_cursor = if page.page_info.has_next_page.0 == Some(true) { trimmed(&page.page_info.end_cursor) } else { None };
    Ok(IssuePage { items, next_cursor })
}

pub async fn get_issue(target: &Served, key: &str) -> Result<TrackerIssue, TrackerError> {
    let at = issue_at(&target.scope, key, "getIssue")?;
    let answer: GhIssueAnswer =
        graphql(target, "getIssue", &detail_query(), &[("owner", Var::Text(at.owner)), ("name", Var::Text(at.name)), ("number", Var::number(at.number))]).await?;
    detail(&answer, target).ok_or_else(|| not_found("getIssue"))
}

pub async fn get_comments(target: &Served, key: &str, cursor: &str) -> Result<IssueCommentsPage, TrackerError> {
    let at = issue_at(&target.scope, key, "getComments")?;
    let answer: GhIssueAnswer = graphql(
        target,
        "getComments",
        &comments_query(),
        &[("owner", Var::Text(at.owner)), ("name", Var::Text(at.name)), ("number", Var::number(at.number)), ("cursor", Var::Text(cursor))],
    )
    .await?;
    let issue = &answer.repository.issue;
    // The query reads no field of the issue but its comments: a missing one has none of them.
    if issue.comments.page_info.has_previous_page.0.is_none() && issue.comments.nodes.is_none() {
        return Err(not_found("getComments"));
    }
    let viewer = trimmed(&answer.viewer.login);
    Ok(IssueCommentsPage { comments: comments(&issue.comments, viewer.as_deref()), next_cursor: previous_cursor(&issue.comments.page_info) })
}

#[derive(Serialize)]
struct CreateBody<'a> {
    title: &'a str,
    body: &'a str,
    labels: &'a [String],
    assignees: &'a [String],
}

pub async fn create(target: &Served, title: &str, body: &str, labels: &[String], assignees: &[String]) -> Result<IssueCreateResult, TrackerError> {
    let (owner, name) = repository(&target.scope, "create")?;
    let out = rest(target, "create", "POST", &format!("repos/{owner}/{name}/issues"), &CreateBody { title, body, labels, assignees }).await?;
    let created: Fields<GhRestIssue> = serde_json::from_str(&out).unwrap_or_default();
    match (created.number.0, trimmed(&created.html_url)) {
        (Some(number), Some(url)) => Ok(IssueCreateResult { key: number.to_string(), display_key: format!("#{number}"), url }),
        _ => Err(invalid("create", "GitHub returned an unreadable create response.")),
    }
}

#[derive(Serialize)]
struct CommentBody<'a> {
    body: &'a str,
}

pub async fn comment(target: &Served, key: &str, body: &str) -> Result<(), TrackerError> {
    let (at, _) = verified_issue(target, key, "comment").await?;
    rest(target, "comment", "POST", &format!("{}/comments", at.path()), &CommentBody { body }).await.map(|_| ())
}

#[derive(Serialize)]
struct UpdateBody<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    body: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    labels: Option<&'a [String]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    assignees: Option<&'a [String]>,
}

/// As sets: GitHub applies a name asked for twice once.
fn same_names(wanted: &[String], applied: &[String]) -> bool {
    let wanted: HashSet<String> = wanted.iter().map(|n| n.trim().to_lowercase()).collect();
    let applied: HashSet<String> = applied.iter().cloned().collect();
    wanted == applied
}

pub async fn update(target: &Served, key: &str, title: Option<&str>, body: Option<&str>, labels: Option<&[String]>, assignees: Option<&[String]>) -> Result<(), TrackerError> {
    let (at, _) = verified_issue(target, key, "update").await?;
    let out = rest(target, "update", "PATCH", &at.path(), &UpdateBody { title, body, labels, assignees }).await?;
    // Without triage access GitHub drops labels and assignees and still answers with success.
    let applied: Fields<GhRestIssue> = serde_json::from_str(&out).unwrap_or_default();
    let (Some(applied_labels), Some(applied_assignees)) = (applied.labels.as_ref(), applied.assignees.as_ref()) else { return Ok(()) };
    let applied_labels: Vec<String> = applied_labels.iter().map(|l| l.name().trim().to_lowercase()).collect();
    let applied_assignees: Vec<String> = applied_assignees.iter().filter_map(|a| trimmed(&a.login)).map(|l| l.to_lowercase()).collect();
    if labels.is_some_and(|l| !same_names(l, &applied_labels)) || assignees.is_some_and(|a| !same_names(a, &applied_assignees)) {
        return Err(invalid("update", "GitHub did not apply the labels or assignees. Changing them needs triage access to the repository."));
    }
    Ok(())
}

#[derive(Serialize)]
struct StateBody<'a> {
    state: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    state_reason: Option<String>,
}

pub async fn set_state(target: &Served, key: &str, state: &str) -> Result<(), TrackerError> {
    let (at, found) = verified_issue(target, key, "setState").await?;
    let wanted = issue_states().into_iter().find(|s| s.name == state).ok_or_else(|| invalid("setState", format!("A GitHub issue has no \"{state}\" state.")))?;
    let reason = wanted.close_reason.as_deref().map(|r| r.replace('-', "_"));
    // GitHub ignores `state_reason` unless the state changes, and would answer with success.
    let closed = found.state.as_deref().is_some_and(|s| s.eq_ignore_ascii_case("CLOSED"));
    if let Some(reason) = &reason {
        if closed && found.state_reason.as_deref().map(str::to_lowercase).as_deref() != Some(reason.as_str()) {
            return Err(invalid("setState", "GitHub cannot change why a closed issue was closed. Reopen it, then close it again."));
        }
    }
    rest(target, "setState", "PATCH", &at.path(), &StateBody { state: wanted.category.as_str(), state_reason: reason }).await.map(|_| ())
}

pub async fn set_reaction(target: &Served, key: &str, subject_id: Option<&str>, content: &IssueReactionContent, reacted: bool) -> Result<(), TrackerError> {
    let subject = match subject_id {
        None => {
            let (_, found) = verified_issue(target, key, "setReaction").await?;
            text(&found.id)
        }
        Some(comment_id) => {
            let at = issue_at(&target.scope, key, "setReaction")?;
            // Node ids are global: prove the comment belongs to this issue before writing.
            let answer: GhIssueAnswer = graphql(
                target,
                "setReaction",
                COMMENT_SCOPE_QUERY,
                &[("owner", Var::Text(at.owner)), ("name", Var::Text(at.name)), ("number", Var::number(at.number)), ("commentId", Var::Text(comment_id))],
            )
            .await?;
            let expected = trimmed(&answer.repository.issue.id);
            let actual = trimmed(&answer.node.issue.id);
            if expected.is_none() || answer.node.typename.as_deref() != Some("IssueComment") || actual != expected {
                return Err(invalid("setReaction", "The comment does not belong to the selected issue."));
            }
            comment_id.to_owned()
        }
    };
    let name = github_reaction(content).ok_or_else(|| invalid("setReaction", "GitHub has no such reaction."))?;
    let mutation = if reacted { ADD_REACTION } else { REMOVE_REACTION };
    let _: Fields<GhViewer> = graphql(target, "setReaction", mutation, &[("subjectId", Var::Text(&subject)), ("content", Var::Text(name))]).await?;
    Ok(())
}

/// `blank_issues_enabled` of a `config.yml`, read by its line (default true).
pub fn blank_issues_enabled(config: &str) -> bool {
    for line in config.lines() {
        let line = line.split('#').next().unwrap_or_default().trim();
        if let Some(value) = line.strip_prefix("blank_issues_enabled:") {
            return !matches!(value.trim().trim_matches(['"', '\'']).to_lowercase().as_str(), "false" | "no" | "off");
        }
    }
    true
}

pub async fn list_templates(target: &Served) -> Result<IssueTemplateList, TrackerError> {
    let (owner, name) = repository(&target.scope, "listTemplates")?;
    let answer: GhIssueAnswer = graphql(target, "listTemplates", TEMPLATES_QUERY, &[("owner", Var::Text(owner)), ("name", Var::Text(name))]).await?;
    let repo = &answer.repository;
    let mut templates: Vec<IssueTemplate> = rows(&repo.issue_templates)
        .filter_map(|t| {
            let key = trimmed(&t.filename)?;
            Some(IssueTemplate {
                key,
                name: text(&t.name),
                about: text(&t.about),
                title: text(&t.title),
                body: text(&t.body),
                labels: rows(&t.labels.nodes).filter_map(|n| trimmed(&n.name)).collect(),
                assignees: rows(&t.assignees.nodes).filter_map(|n| trimmed(&n.login)).collect(),
            })
        })
        .filter(|t| !t.name.trim().is_empty())
        .collect();
    templates.sort_by(|a, b| a.key.cmp(&b.key));
    let mut blank = true;
    if repo.template_directory.typename.as_deref() == Some("Tree") {
        for entry in rows(&repo.template_directory.entries) {
            let Some(file) = trimmed(&entry.name) else { continue };
            let lower = file.to_lowercase();
            if lower != "config.yml" && lower != "config.yaml" {
                continue;
            }
            let blob = &entry.object;
            if blob.typename.as_deref() != Some("Blob") || blob.is_truncated.0 == Some(true) || blob.text.is_none() {
                return Err(invalid("listTemplates", "GitHub returned incomplete issue template contents."));
            }
            blank = blank_issues_enabled(blob.text.as_deref().unwrap_or_default());
        }
    }
    Ok(IssueTemplateList { templates, external_templates: None, blank_issues_enabled: blank })
}

pub async fn list_candidates(target: &Served) -> Result<IssueCandidates, TrackerError> {
    let (owner, name) = repository(&target.scope, "listCandidates")?;
    let answer: GhIssueAnswer = graphql(target, "listCandidates", CANDIDATES_QUERY, &[("owner", Var::Text(owner)), ("name", Var::Text(name))]).await?;
    let repo = &answer.repository;
    if repo.labels.nodes.is_none() && repo.assignable_users.nodes.is_none() {
        return Err(not_found("listCandidates"));
    }
    Ok(IssueCandidates {
        labels: rows(&repo.labels.nodes)
            .filter_map(|l| Some(IssueLabelCandidate { name: trimmed(&l.name)?, color: trimmed(&l.color), description: l.description.0.clone() }))
            .collect(),
        assignees: actors(&repo.assignable_users.nodes).into_iter().map(|actor| IssueAssigneeCandidate { id: actor.login.clone(), actor }).collect(),
        truncated: repo.labels.page_info.has_next_page.0 == Some(true) || repo.assignable_users.page_info.has_next_page.0 == Some(true),
    })
}

// ---------------------------------------------------------------------------
// Issue URLs
// ---------------------------------------------------------------------------

/// Whether a host name is GitHub's (`isGitHubHost`): github.com, a `*.ghe.com`
/// tenant, or a name with a `github` label.
pub fn is_github_host(hostname: &str) -> bool {
    let host = hostname.to_lowercase();
    host == "github.com" || host.ends_with(".ghe.com") || host.split('.').any(|label| label == "github")
}

/// `/<owner>/<repo>/issues/<n>` of a URL (`parseIssuePath`): host, scope and
/// key, lower case. `known` is set when a project on the URL's host vouches for it.
pub fn parse_issue_url(url: &url::Url, known: bool) -> Option<(String, String, String)> {
    if !matches!(url.scheme(), "https" | "http") {
        return None;
    }
    let host = url.host_str()?.to_lowercase();
    if !known && !is_github_host(&host) {
        return None;
    }
    let mut parts = url.path().trim_start_matches('/').split('/');
    let (owner, repo, issues, number) = (parts.next()?, parts.next()?, parts.next()?, parts.next()?);
    if owner.is_empty() || repo.is_empty() || issues != "issues" || number.is_empty() || !number.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let number: u64 = number.parse().ok().filter(|n: &u64| *n > 0 && *n <= 9_007_199_254_740_991)?;
    Some((host, format!("{owner}/{repo}").to_lowercase(), number.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use domovoi_model::typed::IssueListSort;

    fn query() -> ListQuery {
        ListQuery {
            state: IssueListState::Open,
            involvement: IssueInvolvement::All,
            limit: 50,
            query: None,
            labels: vec![],
            assignee: None,
            sort: IssueListSort::Updated,
            cursor: None,
        }
    }

    #[test]
    fn the_search_quotes_what_it_is_given() {
        assert_eq!(search_terms("o/r", &query(), None), "is:issue repo:o/r is:open sort:updated-desc");
        let q = ListQuery {
            state: IssueListState::All,
            involvement: IssueInvolvement::Mentioned,
            labels: vec!["good first issue".into()],
            assignee: Some("bob".into()),
            sort: IssueListSort::Created,
            query: Some(" crash  \"quoted\" ".into()),
            ..query()
        };
        assert_eq!(
            search_terms(" o/r ", &q, Some("me")),
            r#"is:issue repo:o/r mentions:me label:"good first issue" assignee:"bob" sort:created-desc "crash" "\"quoted\"""#
        );
    }

    #[test]
    fn keys_and_scopes_github_can_address() {
        assert_eq!(issue_number("12"), Some(12));
        for bad in ["0", "012", "-1", "1.5", "", "x", "2147483648"] {
            assert_eq!(issue_number(bad), None, "{bad}");
        }
        assert!(repository("acme/calc.py", "x").is_ok());
        for bad in ["acme", "-acme/x", "acme/..", "acme/x/y", "ac me/x"] {
            assert!(repository(bad, "x").is_err(), "{bad}");
        }
    }

    #[test]
    fn issue_urls_are_read_as_upstream_reads_them() {
        let read = |u: &str, known| parse_issue_url(&url::Url::parse(u).unwrap(), known);
        assert_eq!(read("https://github.com/Acme/Calc/issues/7#issuecomment-1", false), Some(("github.com".into(), "acme/calc".into(), "7".into())));
        assert_eq!(read("https://github.com/acme/calc/pull/7", false), None);
        assert_eq!(read("https://git.example.com/acme/calc/issues/7", false), None);
        assert_eq!(read("https://git.example.com/acme/calc/issues/7", true).map(|r| r.0), Some("git.example.com".into()));
        assert_eq!(read("https://github.example.com/a/b/issues/3", false).map(|r| r.2), Some("3".into()));
        assert_eq!(read("https://github.com/a/b/issues/0", false), None);
    }

    #[test]
    fn states_and_config() {
        assert_eq!(issue_state(Some("CLOSED"), Some("NOT_PLANNED")).name, "Not planned");
        assert_eq!(issue_state(Some("CLOSED"), None).name, "Closed");
        assert_eq!(issue_state(Some("OPEN"), Some("REOPENED")).name, "Open");
        assert!(!blank_issues_enabled("blank_issues_enabled: false # no\ncontact_links: []"));
        assert!(blank_issues_enabled("contact_links: []"));
        assert!(blank_issues_enabled("blank_issues_enabled: true"));
    }
}
