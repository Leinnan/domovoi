//! Linear as an issue tracker (t3code `issue-tracker-linear`: `LinearIssueTracker`,
//! `LinearApi` and its `LinearApi.graphql` operations): Linear's GraphQL API
//! over HTTPS, signed with the personal API key `issueTrackers.connect` stored
//! (`crate::secrets`). A scope is a team key (`ENG`), an issue key its
//! identifier (`ENG-123`). A workflow state is closed when its type is
//! `completed`, `canceled` or `duplicate`.
//!
//! `DOMOVOI_LINEAR_API_URL` points the API elsewhere (t3code's
//! `T3CODE_LINEAR_API_URL`), which is how the tests reach a local stand-in.
//!
//! Limits: as upstream, a listing's comment counts are 0 (Linear's
//! connections report no totals), a team's states, labels and members past
//! the first page are not read (`truncated` says so), and an issue carries one
//! assignee.

#![expect(clippy::disallowed_types, clippy::disallowed_macros, reason = "Linear's GraphQL wire: the variables and the issue filter a request carries")]

use super::{IssuePage, ListQuery, TrackerError, TrackerIssue, TrackerReason};
use crate::gh::lenient::{Fields, Loose, Rows, rows, trimmed};
use domovoi_model::typed::{
    IssueActor, IssueAssigneeCandidate, IssueCandidates, IssueCapabilities, IssueComment, IssueCommentsPage, IssueCreateResult, IssueEvent, IssueEventKind,
    IssueExternalTemplate, IssueInvolvement, IssueLabel, IssueLabelCandidate, IssueLinkedPullRequest, IssueListSort, IssueListState, IssueReaction,
    IssueReactionContent, IssueState, IssueStateCategory, IssueSummary, IssueTemplate, IssueTemplateList, IssueTrackerCredential, IssueTrackerScope,
    IssueViewerPermissions, PullRequestState, StandaloneIssueTrackerDescriptor,
};
use serde::Deserialize;
use serde::de::{DeserializeOwned, IgnoredAny};
use serde_json::{Value, json};
use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

pub const KIND: &str = "linear";
pub const HOST: &str = "linear.app";
const DEFAULT_API_URL: &str = "https://api.linear.app/graphql";
const COMMENTS_PAGE: u64 = 50;
const CLOSED_STATE_TYPES: [&str; 3] = ["completed", "canceled", "duplicate"];
/// The order Linear lists a workflow in; within a type its own `position` decides.
const STATE_TYPE_ORDER: [&str; 7] = ["triage", "backlog", "unstarted", "started", "completed", "canceled", "duplicate"];

pub const CAPABILITIES: IssueCapabilities = IssueCapabilities::ALL;

/// Everything granted: Linear reports no per-issue permissions and refuses with a message.
const VIEWER_PERMISSIONS: IssueViewerPermissions = IssueViewerPermissions { set_state: true, comment: true, edit: true, labels: true, assignees: true };

pub fn descriptor() -> StandaloneIssueTrackerDescriptor {
    StandaloneIssueTrackerDescriptor {
        tracker: KIND.into(),
        display_name: "Linear".into(),
        host: HOST.into(),
        scope_noun: "team".into(),
        credential: IssueTrackerCredential { label: "Personal API key".into(), help_url: Some("https://linear.app/settings/account/security".into()) },
    }
}

// ---------------------------------------------------------------------------
// The operations (`LinearApi.graphql`)
// ---------------------------------------------------------------------------

const FRAGMENTS: &str = "fragment LinearUser on User { id name displayName email avatarUrl } \
fragment LinearBot on ActorBot { name userDisplayName avatarUrl } \
fragment LinearWorkflowState on WorkflowState { id name type position } \
fragment LinearLabel on IssueLabel { id name color } \
fragment LinearTeamLabel on IssueLabel { ...LinearLabel description } \
fragment LinearReaction on Reaction { id emoji user { ...LinearUser } } \
fragment LinearComment on Comment { id body createdAt url user { ...LinearUser } botActor { ...LinearBot } reactions { ...LinearReaction } } \
fragment LinearIssue on Issue { id identifier number title url createdAt updatedAt completedAt canceledAt state { ...LinearWorkflowState } team { key } creator { ...LinearUser } botActor { ...LinearBot } assignee { ...LinearUser } labels { nodes { ...LinearLabel } } } \
fragment LinearPageInfo on PageInfo { hasNextPage endCursor }";

const VIEWER: &str = "query T3Viewer { viewer { ...LinearUser } organization { urlKey } teams(first: 250) { nodes { id key name } } }";
const ISSUES: &str = "query T3Issues($first: Int!, $after: String, $filter: IssueFilter!, $orderBy: PaginationOrderBy!) { issues(first: $first, after: $after, filter: $filter, orderBy: $orderBy) { nodes { ...LinearIssue } pageInfo { ...LinearPageInfo } } }";
const ISSUE: &str = "query T3Issue($id: String!, $comments: Int!) { issue(id: $id) { ...LinearIssue description team { states(first: 100) { nodes { ...LinearWorkflowState } } } \
comments(first: $comments, orderBy: createdAt) { nodes { ...LinearComment } pageInfo { ...LinearPageInfo } } reactions { ...LinearReaction } \
attachments(first: 50) { nodes { url title sourceType metadata } } \
history(first: 100, orderBy: createdAt) { nodes { id createdAt actor { ...LinearUser } botActor { ...LinearBot } fromState { ...LinearWorkflowState } toState { ...LinearWorkflowState } \
fromAssignee { ...LinearUser } toAssignee { ...LinearUser } addedLabels { ...LinearLabel } removedLabels { ...LinearLabel } fromTitle toTitle } } } }";
const COMMENTS: &str = "query T3Comments($id: String!, $first: Int!, $after: String) { issue(id: $id) { comments(first: $first, after: $after, orderBy: createdAt) { nodes { ...LinearComment } pageInfo { ...LinearPageInfo } } } }";
const TEAM: &str = "query T3Team($key: String!) { teams(first: 1, filter: { key: { eq: $key } }) { nodes { id key name \
states(first: 100) { nodes { ...LinearWorkflowState } pageInfo { ...LinearPageInfo } } labels(first: 250) { nodes { ...LinearTeamLabel } pageInfo { ...LinearPageInfo } } \
members(first: 250) { nodes { ...LinearUser } pageInfo { ...LinearPageInfo } } templates { nodes { id name description content type hasFormFields templateData } } } } \
issueLabels(first: 250, filter: { team: { null: true } }) { nodes { ...LinearTeamLabel } pageInfo { ...LinearPageInfo } } }";
const ISSUE_CREATE: &str = "mutation T3IssueCreate($input: IssueCreateInput!) { issueCreate(input: $input) { success issue { identifier url } } }";
const ISSUE_UPDATE: &str = "mutation T3IssueUpdate($id: String!, $input: IssueUpdateInput!) { issueUpdate(id: $id, input: $input) { success } }";
const COMMENT_CREATE: &str = "mutation T3CommentCreate($input: CommentCreateInput!) { commentCreate(input: $input) { success } }";
const REACTION_CREATE: &str = "mutation T3ReactionCreate($input: ReactionCreateInput!) { reactionCreate(input: $input) { success } }";
const REACTION_DELETE: &str = "mutation T3ReactionDelete($id: String!) { reactionDelete(id: $id) { success } }";
const ISSUE_REACTIONS: &str = "query T3IssueReactions($id: String!) { issue(id: $id) { reactions { ...LinearReaction } } }";
const COMMENT_REACTIONS: &str = "query T3CommentReactions($id: String!) { comment(id: $id) { issue { id } reactions { ...LinearReaction } } }";

/// The fragments an operation spreads, and only those: GraphQL refuses an unused one.
fn document(operation: &str) -> String {
    let fragments: Vec<(&str, &str)> = FRAGMENTS
        .split("fragment ")
        .filter_map(|f| {
            let f = f.trim();
            f.split_once(' ').map(|(name, _)| (name, f))
        })
        .collect();
    let mut wanted: Vec<&str> = vec![];
    let mut pending = vec![operation];
    while let Some(text) = pending.pop() {
        for (name, body) in &fragments {
            if !wanted.contains(name) && text.contains(&format!("...{name} ")) {
                wanted.push(name);
                pending.push(body);
            }
        }
    }
    let mut doc = operation.to_owned();
    for (name, body) in &fragments {
        if wanted.contains(name) {
            doc.push_str(" fragment ");
            doc.push_str(body);
        }
    }
    doc
}

// ---------------------------------------------------------------------------
// What Linear answers, as far as it is read
// ---------------------------------------------------------------------------

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct PageInfo {
    has_next_page: Loose<bool>,
    end_cursor: Loose<String>,
}

#[derive(Deserialize)]
#[serde(default, rename_all = "camelCase", bound(deserialize = "T: Deserialize<'de> + Default"))]
struct Conn<T> {
    nodes: Rows<T>,
    page_info: Fields<PageInfo>,
}

impl<T> Default for Conn<T> {
    fn default() -> Self {
        Conn { nodes: Loose(None), page_info: Fields(PageInfo::default()) }
    }
}

impl<T> Conn<T> {
    fn next_cursor(&self) -> Option<String> {
        if self.page_info.has_next_page.0 == Some(true) { trimmed(&self.page_info.end_cursor) } else { None }
    }
    fn more(&self) -> bool {
        self.page_info.has_next_page.0 == Some(true)
    }
}

#[derive(Deserialize, Default, Clone)]
#[serde(default, rename_all = "camelCase")]
struct LinUser {
    id: Loose<String>,
    name: Loose<String>,
    display_name: Loose<String>,
    email: Loose<String>,
    avatar_url: Loose<String>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct LinBot {
    name: Loose<String>,
    user_display_name: Loose<String>,
    avatar_url: Loose<String>,
}

#[derive(Deserialize, Default, Clone)]
#[serde(default)]
struct LinState {
    id: Loose<String>,
    name: Loose<String>,
    #[serde(rename = "type")]
    kind: Loose<String>,
    position: Loose<f64>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct LinLabel {
    id: Loose<String>,
    name: Loose<String>,
    color: Loose<String>,
    description: Loose<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct LinReaction {
    id: Loose<String>,
    emoji: Loose<String>,
    user: Fields<LinUser>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct LinComment {
    id: Loose<String>,
    body: Loose<String>,
    created_at: Loose<String>,
    url: Loose<String>,
    user: Fields<LinUser>,
    bot_actor: Fields<LinBot>,
    reactions: Rows<LinReaction>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct LinTeamRef {
    id: Loose<String>,
    key: Loose<String>,
    name: Loose<String>,
    states: Fields<Conn<LinState>>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct AttachmentMetadata {
    status: Loose<String>,
    draft: Loose<bool>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct LinAttachment {
    url: Loose<String>,
    title: Loose<String>,
    metadata: Fields<AttachmentMetadata>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct LinHistory {
    id: Loose<String>,
    created_at: Loose<String>,
    actor: Fields<LinUser>,
    bot_actor: Fields<LinBot>,
    from_state: Fields<LinState>,
    to_state: Fields<LinState>,
    from_assignee: Fields<LinUser>,
    to_assignee: Fields<LinUser>,
    added_labels: Rows<LinLabel>,
    removed_labels: Rows<LinLabel>,
    from_title: Loose<String>,
    to_title: Loose<String>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct LinIssue {
    id: Loose<String>,
    identifier: Loose<String>,
    title: Loose<String>,
    url: Loose<String>,
    created_at: Loose<String>,
    updated_at: Loose<String>,
    completed_at: Loose<String>,
    canceled_at: Loose<String>,
    state: Fields<LinState>,
    team: Fields<LinTeamRef>,
    creator: Fields<LinUser>,
    bot_actor: Fields<LinBot>,
    assignee: Fields<LinUser>,
    labels: Fields<Conn<LinLabel>>,
    description: Loose<String>,
    comments: Fields<Conn<LinComment>>,
    reactions: Rows<LinReaction>,
    attachments: Fields<Conn<LinAttachment>>,
    history: Fields<Conn<LinHistory>>,
}

/// `templateData`: sent as a JSON string, and once as an object.
#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct TemplateData {
    title: Loose<String>,
    description: Loose<String>,
    description_data: Option<IgnoredAny>,
    label_ids: Loose<Vec<String>>,
    assignee_id: Loose<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RawTemplateData {
    Text(String),
    Object(Fields<TemplateData>),
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct LinTemplate {
    id: Loose<String>,
    name: Loose<String>,
    description: Loose<String>,
    content: Loose<String>,
    #[serde(rename = "type")]
    kind: Loose<String>,
    has_form_fields: Loose<bool>,
    template_data: Loose<RawTemplateData>,
}

impl LinTemplate {
    fn data(&self) -> TemplateData {
        match &self.template_data.0 {
            Some(RawTemplateData::Text(text)) => serde_json::from_str::<Fields<TemplateData>>(text).map(|f| f.0).unwrap_or_default(),
            Some(RawTemplateData::Object(fields)) => {
                TemplateData { title: fields.title.clone(), description: fields.description.clone(), description_data: fields.description_data, label_ids: fields.label_ids.clone(), assignee_id: fields.assignee_id.clone() }
            }
            None => TemplateData::default(),
        }
    }
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct LinTeam {
    id: Loose<String>,
    key: Loose<String>,
    name: Loose<String>,
    states: Fields<Conn<LinState>>,
    labels: Fields<Conn<LinLabel>>,
    members: Fields<Conn<LinUser>>,
    templates: Fields<Conn<LinTemplate>>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct Organization {
    url_key: Loose<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Success {
    success: Loose<bool>,
    issue: Fields<LinIssue>,
}

/// Every operation's `data`, as far as it is read.
#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct Data {
    viewer: Fields<LinUser>,
    organization: Fields<Organization>,
    teams: Fields<Conn<LinTeam>>,
    issues: Fields<Conn<LinIssue>>,
    issue: Loose<Fields<LinIssue>>,
    comment: Fields<LinCommentSubject>,
    issue_labels: Fields<Conn<LinLabel>>,
    issue_create: Fields<Success>,
    issue_update: Fields<Success>,
    comment_create: Fields<Success>,
    reaction_create: Fields<Success>,
    reaction_delete: Fields<Success>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct LinCommentSubject {
    issue: Fields<LinIssue>,
    reactions: Rows<LinReaction>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct ErrorExtensions {
    code: Loose<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct GraphqlError {
    message: Loose<String>,
    extensions: Fields<ErrorExtensions>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Answer<T: Default> {
    data: Loose<Fields<T>>,
    errors: Rows<GraphqlError>,
}

// ---------------------------------------------------------------------------
// The API
// ---------------------------------------------------------------------------

fn api_url() -> String {
    std::env::var("DOMOVOI_LINEAR_API_URL").ok().filter(|u| !u.trim().is_empty()).unwrap_or_else(|| DEFAULT_API_URL.to_owned())
}

fn client() -> Option<&'static reqwest::Client> {
    static CLIENT: OnceLock<Option<reqwest::Client>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            // The process installs ring at start; a test run has not.
            let _ = rustls::crypto::ring::default_provider().install_default();
            reqwest::Client::builder().timeout(Duration::from_secs(30)).build().ok()
        })
        .as_ref()
}

fn fail(operation: &str, reason: TrackerReason, detail: impl Into<String>) -> TrackerError {
    TrackerError::new(KIND, operation, reason, detail)
}

fn now_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)
}

/// Linear's reset headers are UTC epoch milliseconds (not `Retry-After` seconds).
fn reset_at(value: Option<&str>, now: u64) -> Option<u64> {
    let value = value?.trim();
    if value.is_empty() || !value.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    value.parse::<u64>().ok().filter(|at| *at > now)
}

/// Linear's answer as the tracker's error (`toTrackerError`), or its data.
fn read_answer<T: DeserializeOwned + Default>(operation: &str, status: u16, reset: Option<u64>, body: &str) -> Result<T, TrackerError> {
    let answer: Option<Answer<T>> = serde_json::from_str::<Fields<Answer<T>>>(body).ok().map(|a| a.0);
    let errors: Vec<&GraphqlError> = answer.as_ref().map(|a| rows(&a.errors).collect()).unwrap_or_default();
    let has_code = |code: &str| errors.iter().any(|e| e.extensions.code.as_deref() == Some(code));
    if status == 429 || has_code("RATELIMITED") {
        let mut e = fail(operation, TrackerReason::RateLimited, "Linear's rate limit was reached.");
        e.retry_at = reset;
        return Err(e);
    }
    if status == 401 || status == 403 || has_code("AUTHENTICATION_ERROR") {
        return Err(fail(operation, TrackerReason::Unauthenticated, "Linear rejected the API key."));
    }
    if let Some(first) = errors.first() {
        let message = first.message.as_deref().map(str::trim).filter(|m| !m.is_empty()).unwrap_or("Linear refused the request.");
        return Err(fail(operation, TrackerReason::Failed, message));
    }
    match answer.and_then(|a| a.data.0) {
        Some(data) if (200..300).contains(&status) => Ok(data.0),
        _ if !(200..300).contains(&status) => Err(fail(operation, TrackerReason::Failed, format!("Linear returned HTTP {status} with an unexpected body."))),
        _ => Err(fail(operation, TrackerReason::Failed, "Linear returned an unexpected body.")),
    }
}

/// One GraphQL operation signed with `token`.
async fn call(operation: &str, token: &str, query: &str, variables: Value) -> Result<Data, TrackerError> {
    let client = client().ok_or_else(|| fail(operation, TrackerReason::Failed, "Linear could not be reached."))?;
    let request = json!({ "query": document(query), "variables": variables });
    // A personal API key goes in as it is; `Bearer` is for OAuth tokens.
    let response = client
        .post(api_url())
        .header("authorization", token)
        .header("content-type", "application/json")
        .body(request.to_string())
        .send()
        .await
        .map_err(|_| fail(operation, TrackerReason::Failed, "Linear could not be reached."))?;
    let status = response.status().as_u16();
    let header = |name: &str| response.headers().get(name).and_then(|v| v.to_str().ok()).map(str::to_owned);
    let reset = reset_at(header("x-ratelimit-requests-reset").or_else(|| header("x-ratelimit-complexity-reset")).as_deref(), now_ms());
    let body = response.text().await.map_err(|_| fail(operation, TrackerReason::Failed, "Linear could not be reached."))?;
    read_answer(operation, status, reset, &body)
}

/// The stored API key, or the failure every read reports without one.
fn stored_token(userdata: &Path, operation: &str) -> Result<String, TrackerError> {
    match crate::secrets::tracker_token(userdata, KIND) {
        Ok(Some(token)) => Ok(token),
        Ok(None) => Err(fail(operation, TrackerReason::Unauthenticated, "Linear is not connected on this server.")),
        Err(_) => Err(fail(operation, TrackerReason::Failed, "The stored credential could not be read.")),
    }
}

async fn stored(userdata: &Path, operation: &str, query: &str, variables: Value) -> Result<Data, TrackerError> {
    let token = stored_token(userdata, operation)?;
    call(operation, &token, query, variables).await
}

fn applied(operation: &str, result: &Success) -> Result<(), TrackerError> {
    if result.success.0 == Some(true) { Ok(()) } else { Err(fail(operation, TrackerReason::Failed, "Linear did not apply the change.")) }
}

// ---------------------------------------------------------------------------
// Reading Linear's answer as the contract's records
// ---------------------------------------------------------------------------

fn text(value: &Loose<String>) -> String {
    value.as_deref().unwrap_or_default().to_owned()
}

fn state(raw: &LinState) -> IssueState {
    let kind = text(&raw.kind);
    if CLOSED_STATE_TYPES.contains(&kind.as_str()) {
        IssueState { category: IssueStateCategory::Closed, name: text(&raw.name), close_reason: Some(kind) }
    } else {
        IssueState { category: IssueStateCategory::Open, name: text(&raw.name), close_reason: None }
    }
}

fn is_closed(raw: &LinState) -> bool {
    raw.kind.as_deref().is_some_and(|k| CLOSED_STATE_TYPES.contains(&k))
}

fn in_workflow_order(states: &Rows<LinState>) -> Vec<LinState> {
    let mut states: Vec<LinState> = rows(states).cloned().collect();
    let rank = |s: &LinState| STATE_TYPE_ORDER.iter().position(|t| Some(*t) == s.kind.as_deref()).map_or(-1, |i| i as i64);
    states.sort_by(|a, b| rank(a).cmp(&rank(b)).then(a.position.unwrap_or(0.0).total_cmp(&b.position.unwrap_or(0.0))));
    states
}

/// The login is Linear's handle (`displayName`), falling back to the id.
fn to_actor(user: &LinUser) -> Option<IssueActor> {
    let login = trimmed(&user.display_name).or_else(|| trimmed(&user.id))?;
    Some(IssueActor { is_bot: None, login, name: user.name.0.clone(), avatar_url: user.avatar_url.0.clone() })
}

/// The person, else the integration that acted.
fn actor_of(user: &LinUser, bot: &LinBot) -> Option<IssueActor> {
    if user.id.is_some() {
        return to_actor(user);
    }
    let name = trimmed(&bot.user_display_name).or_else(|| trimmed(&bot.name))?;
    Some(IssueActor { is_bot: Some(true), login: trimmed(&bot.name).unwrap_or_else(|| name.clone()), name: Some(name), avatar_url: bot.avatar_url.0.clone() })
}

/// How a user shows in a picker and an event.
fn user_name(user: &LinUser) -> String {
    trimmed(&user.display_name).or_else(|| trimmed(&user.name)).or_else(|| trimmed(&user.email)).unwrap_or_else(|| text(&user.id))
}

fn summary(issue: &LinIssue, comment_count: u64) -> IssueSummary {
    let identifier = text(&issue.identifier);
    IssueSummary {
        tracker: KIND.into(),
        host: HOST.into(),
        scope: text(&issue.team.key),
        key: identifier.clone(),
        display_key: identifier,
        title: text(&issue.title),
        url: text(&issue.url),
        author: actor_of(&issue.creator, &issue.bot_actor),
        state: state(&issue.state),
        created_at: text(&issue.created_at).as_str().into(),
        updated_at: text(&issue.updated_at).as_str().into(),
        closed_at: trimmed(&issue.completed_at).or_else(|| trimmed(&issue.canceled_at)).map(|at| at.as_str().into()),
        assignees: if issue.assignee.id.is_some() { to_actor(&issue.assignee).into_iter().collect() } else { vec![] },
        labels: rows(&issue.labels.nodes).filter_map(|l| Some(IssueLabel { name: trimmed(&l.name)?, color: l.color.0.clone(), description: None })).collect(),
        comment_count,
    }
}

/// Linear reacts with the emoji, and answers with its short name: a read knows both.
const EMOJI: [(IssueReactionContent, &str, &str); 8] = [
    (IssueReactionContent::ThumbsUp, "\u{1F44D}", "+1"),
    (IssueReactionContent::ThumbsDown, "\u{1F44E}", "-1"),
    (IssueReactionContent::Laugh, "\u{1F604}", "smile"),
    (IssueReactionContent::Hooray, "\u{1F389}", "tada"),
    (IssueReactionContent::Confused, "\u{1F615}", "confused"),
    (IssueReactionContent::Heart, "\u{2764}\u{FE0F}", "heart"),
    (IssueReactionContent::Rocket, "\u{1F680}", "rocket"),
    (IssueReactionContent::Eyes, "\u{1F440}", "eyes"),
];

fn content_of(emoji: &str) -> Option<IssueReactionContent> {
    EMOJI.iter().find(|(_, e, name)| *e == emoji || *name == emoji).map(|(c, _, _)| c.clone())
}

fn emoji_of(content: &IssueReactionContent) -> Option<&'static str> {
    EMOJI.iter().find(|(c, _, _)| c == content).map(|(_, e, _)| *e)
}

fn reactions(raw: &Rows<LinReaction>, viewer_id: Option<&str>) -> Vec<IssueReaction> {
    let mut grouped: Vec<IssueReaction> = vec![];
    for reaction in rows(raw) {
        let Some(content) = reaction.emoji.as_deref().and_then(content_of) else { continue };
        let has_user = reaction.user.id.is_some();
        let mine = has_user && reaction.user.id.as_deref() == viewer_id;
        let actor: Vec<String> = if !has_user || mine { vec![] } else { vec![user_name(&reaction.user)] };
        match grouped.iter_mut().find(|g| g.content == content) {
            Some(group) => {
                group.count += 1;
                group.actors.extend(actor);
                group.viewer_has_reacted |= mine;
            }
            None => grouped.push(IssueReaction { content, count: 1, actors: actor, viewer_has_reacted: mine }),
        }
    }
    grouped
}

fn comment(raw: &LinComment, viewer_id: Option<&str>) -> IssueComment {
    IssueComment {
        id: text(&raw.id),
        author: actor_of(&raw.user, &raw.bot_actor),
        body: text(&raw.body),
        created_at: text(&raw.created_at).as_str().into(),
        url: raw.url.0.clone(),
        reactions: Some(reactions(&raw.reactions, viewer_id)),
    }
}

fn events(history: &Rows<LinHistory>) -> Vec<IssueEvent> {
    let mut events = vec![];
    for entry in rows(history) {
        let actor = actor_of(&entry.actor, &entry.bot_actor);
        let mut add = |kind: IssueEventKind, detail: Option<String>, suffix: &str| {
            events.push(IssueEvent { id: format!("{}:{suffix}", text(&entry.id)), kind, actor: actor.clone(), created_at: text(&entry.created_at).as_str().into(), detail });
        };
        if entry.to_state.id.is_some() && entry.to_state.id != entry.from_state.id {
            let was_closed = entry.from_state.id.is_some() && is_closed(&entry.from_state);
            let closed = is_closed(&entry.to_state);
            let kind = match (was_closed, closed) {
                (false, true) => IssueEventKind::Closed,
                (true, false) => IssueEventKind::Reopened,
                _ => IssueEventKind::StateChanged,
            };
            add(kind, trimmed(&entry.to_state.name), "state");
        }
        for label in rows(&entry.added_labels) {
            add(IssueEventKind::Labeled, trimmed(&label.name), &format!("label+{}", text(&label.id)));
        }
        for label in rows(&entry.removed_labels) {
            add(IssueEventKind::Unlabeled, trimmed(&label.name), &format!("label-{}", text(&label.id)));
        }
        if entry.from_assignee.id.is_some() && entry.from_assignee.id != entry.to_assignee.id {
            add(IssueEventKind::Unassigned, Some(user_name(&entry.from_assignee)), "unassigned");
        }
        if entry.to_assignee.id.is_some() && entry.to_assignee.id != entry.from_assignee.id {
            add(IssueEventKind::Assigned, Some(user_name(&entry.to_assignee)), "assigned");
        }
        if entry.to_title.is_some() && entry.to_title != entry.from_title {
            add(IssueEventKind::Renamed, trimmed(&entry.to_title), "title");
        }
    }
    events
}

/// `https://github.com/<owner>/<repo>/pull/<n>` or `https://gitlab.com/<path>/-/merge_requests/<n>`.
fn pull_request_link(url: &str) -> Option<(String, u64)> {
    let rest = url.strip_prefix("https://")?;
    let path = rest.split(['?', '#']).next()?.trim_end_matches('/');
    if let Some(path) = path.strip_prefix("github.com/") {
        let parts: Vec<&str> = path.split('/').collect();
        if let [owner, repo, "pull", number] = parts.as_slice() {
            return Some((format!("{owner}/{repo}"), number.parse().ok().filter(|n| *n > 0)?));
        }
    } else if let Some(path) = path.strip_prefix("gitlab.com/") {
        let (project, number) = path.rsplit_once("/-/merge_requests/")?;
        return Some((project.to_owned(), number.parse().ok().filter(|n| *n > 0)?));
    }
    None
}

/// Linear's GitHub and GitLab integrations record change requests as attachments.
fn linked_pull_requests(attachments: &Rows<LinAttachment>) -> Vec<IssueLinkedPullRequest> {
    let mut seen = std::collections::HashSet::new();
    rows(attachments)
        .filter_map(|a| {
            let url = trimmed(&a.url)?;
            let (repository, number) = pull_request_link(&url)?;
            if !seen.insert(url.clone()) {
                return None;
            }
            let status = a.metadata.status.as_deref();
            Some(IssueLinkedPullRequest {
                repository,
                number,
                title: trimmed(&a.title).unwrap_or_else(|| url.clone()),
                state: match status {
                    Some("merged") => PullRequestState::Merged,
                    Some("closed") => PullRequestState::Closed,
                    _ => PullRequestState::Open,
                },
                is_draft: status == Some("draft") || a.metadata.draft.0 == Some(true),
                url,
                closes_issue: false,
            })
        })
        .collect()
}

/// `ENG-123`, or `None` for anything that is not a Linear identifier.
pub fn parse_identifier(key: &str) -> Option<String> {
    let key = key.trim();
    let (team, number) = key.split_once('-')?;
    let team_ok = team.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) && team.chars().all(|c| c.is_ascii_alphanumeric());
    let number_ok = !number.is_empty() && !number.starts_with('0') && number.chars().all(|c| c.is_ascii_digit());
    (team_ok && number_ok).then(|| key.to_uppercase())
}

/// `https://linear.app/<workspace>/issue/<TEAM>-<n>[/<slug>]` (`parseLinearIssueUrl`):
/// `(workspace, team, identifier)`.
pub fn parse_issue_url(url: &url::Url) -> Option<(String, String, String)> {
    if url.scheme() != "https" || url.port().is_some() || !url.host_str()?.eq_ignore_ascii_case(HOST) {
        return None;
    }
    let mut parts = url.path().split('/').skip(1);
    let (workspace, resource, key) = (parts.next()?, parts.next()?, parts.next()?);
    if workspace.is_empty() || resource != "issue" {
        return None;
    }
    let (team, number) = key.split_once('-')?;
    let team_ok = team.chars().next().is_some_and(|c| c.is_ascii_uppercase()) && team.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
    let number_ok = !number.is_empty() && !number.starts_with('0') && number.chars().all(|c| c.is_ascii_digit());
    (team_ok && number_ok).then(|| (workspace.to_owned(), team.to_owned(), key.to_owned()))
}

// ---------------------------------------------------------------------------
// The tracker
// ---------------------------------------------------------------------------

/// Who a credential signs in as, its workspace and the teams it reaches (`probeCredential`).
pub async fn probe(token: &str) -> Result<(String, Option<String>, Vec<IssueTrackerScope>), TrackerError> {
    let data = call("viewer", token, VIEWER, json!({})).await?;
    let scopes = rows(&data.teams.nodes).filter_map(|t| Some(IssueTrackerScope { key: trimmed(&t.key)?, name: trimmed(&t.name)? })).collect();
    Ok((user_name(&data.viewer), trimmed(&data.organization.url_key), scopes))
}

async fn viewer_user(userdata: &Path) -> Result<LinUser, TrackerError> {
    Ok(stored(userdata, "viewer", VIEWER, json!({})).await?.viewer.0)
}

/// The viewer's handle, which involvement filters compare against.
pub async fn viewer(userdata: &Path) -> Result<String, TrackerError> {
    let user = viewer_user(userdata).await?;
    to_actor(&user).map(|a| a.login).ok_or_else(|| fail("viewer", TrackerReason::Failed, "Linear returned an unexpected body."))
}

fn involvement_filter(involvement: &IssueInvolvement, viewer: Option<&str>) -> Option<(&'static str, Value)> {
    let viewer = viewer?;
    let eq = json!({ "displayName": { "eq": viewer } });
    match involvement {
        IssueInvolvement::Assigned => Some(("assignee", eq)),
        IssueInvolvement::Authored => Some(("creator", eq)),
        IssueInvolvement::Mentioned => Some(("subscribers", json!({ "some": eq }))),
        _ => None,
    }
}

/// The `IssueFilter` a listing sends.
pub fn issue_filter(scope: &str, q: &ListQuery, viewer: Option<&str>) -> Value {
    let mut filter = serde_json::Map::new();
    filter.insert("team".into(), json!({ "key": { "eq": scope.trim().to_uppercase() } }));
    if let Some((field, value)) = involvement_filter(&q.involvement, viewer) {
        filter.insert(field.into(), value);
    }
    match q.state {
        IssueListState::Closed => {
            filter.insert("state".into(), json!({ "type": { "in": CLOSED_STATE_TYPES } }));
        }
        IssueListState::Open => {
            filter.insert("state".into(), json!({ "type": { "nin": CLOSED_STATE_TYPES } }));
        }
        _ => {}
    }
    // Each is its own clause: an assignee filter must narrow "assigned to me", not replace it.
    let mut clauses: Vec<Value> = vec![];
    if let Some(assignee) = &q.assignee {
        clauses.push(json!({ "assignee": { "displayName": { "eq": assignee } } }));
    }
    clauses.extend(q.labels.iter().map(|name| json!({ "labels": { "some": { "name": { "eq": name } } } })));
    if !clauses.is_empty() {
        filter.insert("and".into(), Value::Array(clauses));
    }
    if let Some(query) = &q.query {
        // `ENG-12`, `#12` or `12` asks for one issue of the team by number.
        let trimmed = query.trim();
        let digits = trimmed.trim_start_matches('#');
        let digits = match digits.split_once('-') {
            Some((team, n)) if team.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) && team.chars().all(|c| c.is_ascii_alphanumeric()) => n,
            _ => digits,
        };
        let mut any = vec![json!({ "title": { "containsIgnoreCase": query } }), json!({ "description": { "containsIgnoreCase": query } })];
        if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) {
            if let Ok(number) = digits.parse::<u64>() {
                any.push(json!({ "number": { "eq": number } }));
            }
        }
        filter.insert("or".into(), Value::Array(any));
    }
    Value::Object(filter)
}

pub async fn list_issues(userdata: &Path, scope: &str, q: &ListQuery, viewer: Option<&str>) -> Result<IssuePage, TrackerError> {
    let order_by = if q.sort == IssueListSort::Created { "createdAt" } else { "updatedAt" };
    let variables = json!({ "first": q.limit, "after": q.cursor, "filter": issue_filter(scope, q, viewer), "orderBy": order_by });
    let data = stored(userdata, "listIssues", ISSUES, variables).await?;
    Ok(IssuePage { items: rows(&data.issues.nodes).map(|issue| summary(issue, 0)).collect(), next_cursor: data.issues.next_cursor() })
}

/// The issue by identifier, `None` where Linear has none (an error, not null).
async fn read_issue(userdata: &Path, identifier: &str, comments: u64) -> Result<Option<LinIssue>, TrackerError> {
    match stored(userdata, "getIssue", ISSUE, json!({ "id": identifier, "comments": comments })).await {
        Ok(data) => Ok(data.issue.0.map(|f| f.0).filter(|issue| issue.id.is_some())),
        Err(e) if e.reason == TrackerReason::Failed && e.detail.to_lowercase().contains("not found") => Ok(None),
        Err(e) => Err(e),
    }
}

fn not_found(operation: &str) -> TrackerError {
    fail(operation, TrackerReason::NotFound, "Linear has no issue by that identifier, or this account cannot see it.")
}

fn require_identifier(operation: &str, key: &str) -> Result<String, TrackerError> {
    parse_identifier(key).ok_or_else(|| not_found(operation))
}

pub async fn get_issue(userdata: &Path, key: &str) -> Result<TrackerIssue, TrackerError> {
    let identifier = require_identifier("getIssue", key)?;
    let (issue, me) = tokio::join!(read_issue(userdata, &identifier, COMMENTS_PAGE), viewer_user(userdata));
    let me = me.ok();
    let issue = issue?.ok_or_else(|| not_found("getIssue"))?;
    let viewer_id = me.as_ref().and_then(|m| m.id.0.clone());
    let comments: Vec<IssueComment> = rows(&issue.comments.nodes).map(|c| comment(c, viewer_id.as_deref())).collect();
    Ok(TrackerIssue {
        summary: summary(&issue, comments.len() as u64),
        body: text(&issue.description),
        viewer: me.as_ref().and_then(to_actor).map(|a| a.login),
        viewer_permissions: VIEWER_PERMISSIONS,
        states: in_workflow_order(&issue.team.states.nodes).iter().map(state).collect(),
        reactions: Some(reactions(&issue.reactions, viewer_id.as_deref())),
        comments_cursor: issue.comments.next_cursor(),
        comments,
        events: events(&issue.history.nodes),
        linked_pull_requests: linked_pull_requests(&issue.attachments.nodes),
    })
}

pub async fn get_comments(userdata: &Path, key: &str, cursor: &str) -> Result<IssueCommentsPage, TrackerError> {
    let identifier = require_identifier("getComments", key)?;
    let data = stored(userdata, "getComments", COMMENTS, json!({ "id": identifier, "first": COMMENTS_PAGE, "after": cursor })).await?;
    let Some(issue) = data.issue.0.map(|f| f.0).filter(|i| i.comments.nodes.is_some()) else {
        return Err(fail("getComments", TrackerReason::NotFound, "This issue no longer exists."));
    };
    let viewer_id = if rows(&issue.comments.nodes).any(|c| rows(&c.reactions).next().is_some()) { viewer_user(userdata).await.ok().and_then(|u| u.id.0) } else { None };
    Ok(IssueCommentsPage { comments: rows(&issue.comments.nodes).map(|c| comment(c, viewer_id.as_deref())).collect(), next_cursor: issue.comments.next_cursor() })
}

/// The team with the workspace-wide labels its issues may also carry.
struct TeamScope {
    team: LinTeam,
    workspace_labels: Rows<LinLabel>,
    truncated: bool,
}

impl TeamScope {
    fn labels(&self) -> impl Iterator<Item = &LinLabel> {
        rows(&self.team.labels.nodes).chain(rows(&self.workspace_labels))
    }
}

async fn require_team(userdata: &Path, operation: &str, scope: &str) -> Result<TeamScope, TrackerError> {
    let key = scope.trim().to_uppercase();
    let data = stored(userdata, operation, TEAM, json!({ "key": key })).await?;
    let truncated = data.issue_labels.more();
    let workspace_labels = data.issue_labels.0.nodes;
    let team = data.teams.0.nodes.0.and_then(|nodes| nodes.into_iter().next()).map(|f| f.0).filter(|t| t.id.is_some());
    let Some(team) = team else { return Err(fail(operation, TrackerReason::Failed, format!("No Linear team has the key \"{scope}\"."))) };
    let truncated = truncated || team.states.more() || team.labels.more() || team.members.more();
    Ok(TeamScope { team, workspace_labels, truncated })
}

/// The issue a write names, refused unless it sits in the bound team.
async fn require_team_issue(userdata: &Path, operation: &str, scope: &str, key: &str) -> Result<LinIssue, TrackerError> {
    let identifier = require_identifier(operation, key)?;
    // One comment, not none: Linear refuses a page of zero.
    let issue = read_issue(userdata, &identifier, 1).await?.ok_or_else(|| not_found(operation))?;
    let team = scope.trim().to_uppercase();
    let actual = text(&issue.team.key);
    if actual != team {
        return Err(fail(operation, TrackerReason::Failed, format!("{} belongs to Linear team {actual}, not {team}.", text(&issue.identifier))));
    }
    Ok(issue)
}

fn label_ids(operation: &str, scope: &TeamScope, names: &[String]) -> Result<Vec<String>, TrackerError> {
    names
        .iter()
        .map(|name| {
            scope
                .labels()
                .find(|l| l.name.as_deref().is_some_and(|n| n.eq_ignore_ascii_case(name.trim())))
                .and_then(|l| l.id.0.clone())
                .ok_or_else(|| fail(operation, TrackerReason::Failed, format!("Linear has no label named \"{name}\" for this team.")))
        })
        .collect()
}

/// One assignee at most: a Linear issue has a single assignee.
fn assignee_id(operation: &str, scope: &TeamScope, assignees: &[String]) -> Result<Option<String>, TrackerError> {
    let Some(first) = assignees.first() else { return Ok(None) };
    if assignees.len() > 1 {
        return Err(fail(operation, TrackerReason::Failed, "A Linear issue has one assignee at most."));
    }
    rows(&scope.team.members.nodes)
        .find(|u| {
            u.id.as_deref() == Some(first.as_str())
                || u.email.as_deref().is_some_and(|e| e.eq_ignore_ascii_case(first))
                || u.display_name.as_deref().is_some_and(|d| d.eq_ignore_ascii_case(first))
        })
        .and_then(|u| u.id.0.clone())
        .map(Some)
        .ok_or_else(|| fail(operation, TrackerReason::Failed, format!("\"{first}\" is not a member of this Linear team.")))
}

pub async fn create(userdata: &Path, scope: &str, title: &str, body: &str, labels: &[String], assignees: &[String]) -> Result<IssueCreateResult, TrackerError> {
    let team = require_team(userdata, "create", scope).await?;
    let labels = label_ids("create", &team, labels)?;
    let assignee = assignee_id("create", &team, assignees)?;
    let mut input = json!({ "teamId": text(&team.team.id), "title": title, "description": body, "labelIds": labels });
    if let (Some(assignee), Some(fields)) = (assignee, input.as_object_mut()) {
        fields.insert("assigneeId".into(), Value::String(assignee));
    }
    let data = stored(userdata, "createIssue", ISSUE_CREATE, json!({ "input": input })).await?;
    let created = &data.issue_create;
    match (created.success.0, trimmed(&created.issue.identifier), trimmed(&created.issue.url)) {
        (Some(true), Some(identifier), Some(url)) => Ok(IssueCreateResult { key: identifier.clone(), display_key: identifier, url }),
        _ => Err(fail("createIssue", TrackerReason::Failed, "Linear did not create the issue.")),
    }
}

pub async fn comment_on(userdata: &Path, scope: &str, key: &str, body: &str) -> Result<(), TrackerError> {
    let issue = require_team_issue(userdata, "comment", scope, key).await?;
    let data = stored(userdata, "createComment", COMMENT_CREATE, json!({ "input": { "issueId": text(&issue.id), "body": body } })).await?;
    applied("createComment", &data.comment_create)
}

pub async fn update(userdata: &Path, scope: &str, key: &str, title: Option<&str>, body: Option<&str>, labels: Option<&[String]>, assignees: Option<&[String]>) -> Result<(), TrackerError> {
    let issue = require_team_issue(userdata, "update", scope, key).await?;
    let mut input = serde_json::Map::new();
    if let Some(title) = title {
        input.insert("title".into(), json!(title));
    }
    if let Some(body) = body {
        input.insert("description".into(), json!(body));
    }
    if labels.is_some() || assignees.is_some() {
        let team = require_team(userdata, "update", scope).await?;
        if let Some(labels) = labels {
            input.insert("labelIds".into(), json!(label_ids("update", &team, labels)?));
        }
        if let Some(assignees) = assignees {
            input.insert("assigneeId".into(), json!(assignee_id("update", &team, assignees)?));
        }
    }
    let data = stored(userdata, "updateIssue", ISSUE_UPDATE, json!({ "id": text(&issue.identifier), "input": input })).await?;
    applied("updateIssue", &data.issue_update)
}

pub async fn set_state(userdata: &Path, scope: &str, key: &str, wanted: &str) -> Result<(), TrackerError> {
    let issue = require_team_issue(userdata, "setState", scope, key).await?;
    let team = require_team(userdata, "setState", scope).await?;
    let found = rows(&team.team.states.nodes)
        .find(|s| s.name.as_deref() == Some(wanted))
        .and_then(|s| s.id.0.clone())
        .ok_or_else(|| fail("setState", TrackerReason::Failed, format!("This team's workflow has no \"{wanted}\" state.")))?;
    let data = stored(userdata, "updateIssue", ISSUE_UPDATE, json!({ "id": text(&issue.identifier), "input": { "stateId": found } })).await?;
    applied("updateIssue", &data.issue_update)
}

pub async fn set_reaction(userdata: &Path, scope: &str, key: &str, subject_id: Option<&str>, content: &IssueReactionContent, reacted: bool) -> Result<(), TrackerError> {
    let issue = require_team_issue(userdata, "setReaction", scope, key).await?;
    let issue_id = text(&issue.id);
    let emoji = emoji_of(content).ok_or_else(|| fail("setReaction", TrackerReason::Failed, "Linear has no such reaction."))?;
    let target = match subject_id {
        None => json!({ "issueId": issue_id }),
        Some(comment) => json!({ "commentId": comment }),
    };
    // Comment ids are global: prove the comment belongs to this issue before writing.
    let subject = if subject_id.is_none() && reacted {
        None
    } else {
        Some(match subject_id {
            None => {
                let data = stored(userdata, "listReactions", ISSUE_REACTIONS, json!({ "id": issue_id })).await?;
                (Some(issue_id.clone()), data.issue.0.map(|f| f.0.reactions).unwrap_or_default())
            }
            Some(comment) => {
                let data = stored(userdata, "listReactions", COMMENT_REACTIONS, json!({ "id": comment })).await?;
                let subject = data.comment.0;
                (subject.issue.id.0.clone(), subject.reactions)
            }
        })
    };
    if let Some((owner, _)) = &subject {
        if owner.as_deref() != Some(issue_id.as_str()) {
            return Err(fail("setReaction", TrackerReason::Failed, "The comment does not belong to the selected issue."));
        }
    }
    let Some((_, existing)) = subject.filter(|_| !reacted) else {
        let mut input = target;
        if let Some(fields) = input.as_object_mut() {
            fields.insert("emoji".into(), json!(emoji));
        }
        let data = stored(userdata, "createReaction", REACTION_CREATE, json!({ "input": input })).await?;
        return applied("createReaction", &data.reaction_create);
    };
    let viewer_id = viewer_user(userdata).await?.id.0;
    let own = rows(&existing).find(|r| r.emoji.as_deref().and_then(content_of).as_ref() == Some(content) && r.user.id.0.is_some() && r.user.id.0 == viewer_id);
    if let Some(id) = own.and_then(|r| r.id.0.clone()) {
        let data = stored(userdata, "deleteReaction", REACTION_DELETE, json!({ "id": id })).await?;
        applied("deleteReaction", &data.reaction_delete)?;
    }
    Ok(())
}

pub async fn list_templates(userdata: &Path, scope: &str) -> Result<IssueTemplateList, TrackerError> {
    let team = require_team(userdata, "listTemplates", scope).await?;
    let team_key = text(&team.team.key);
    let mut templates = vec![];
    let mut external = vec![];
    for template in rows(&team.team.templates.nodes) {
        if template.kind.as_deref() != Some("issue") {
            continue;
        }
        let data = template.data();
        let body = template.content.0.clone().or_else(|| data.description.0.clone());
        let id = text(&template.id);
        if template.has_form_fields.0 == Some(true) || (body.is_none() && data.description_data.is_some()) {
            let mut url = url::Url::parse("https://linear.app/").map_err(|_| fail("listTemplates", TrackerReason::Failed, "Linear returned an unexpected body."))?;
            url.path_segments_mut().map_err(|()| fail("listTemplates", TrackerReason::Failed, "Linear returned an unexpected body."))?.extend(["team", &team_key, "new"]);
            url.query_pairs_mut().append_pair("template", &id);
            external.push(IssueExternalTemplate { name: text(&template.name), about: text(&template.description), url: url.to_string() });
            continue;
        }
        let names: Vec<String> = data
            .label_ids
            .iter()
            .flatten()
            .filter_map(|wanted| team.labels().find(|l| l.id.as_deref() == Some(wanted.as_str())).and_then(|l| l.name.0.clone()))
            .collect();
        templates.push(IssueTemplate {
            key: id,
            name: text(&template.name),
            about: text(&template.description),
            title: text(&data.title),
            body: body.unwrap_or_default(),
            labels: names,
            assignees: trimmed(&data.assignee_id).into_iter().collect(),
        });
    }
    Ok(IssueTemplateList { templates, external_templates: Some(external), blank_issues_enabled: true })
}

pub async fn list_candidates(userdata: &Path, scope: &str) -> Result<IssueCandidates, TrackerError> {
    let team = require_team(userdata, "listCandidates", scope).await?;
    Ok(IssueCandidates {
        labels: team.labels().filter_map(|l| Some(IssueLabelCandidate { name: trimmed(&l.name)?, color: l.color.0.clone(), description: l.description.0.clone() })).collect(),
        assignees: rows(&team.team.members.nodes).filter_map(|u| Some(IssueAssigneeCandidate { actor: to_actor(u)?, id: trimmed(&u.id)? })).collect(),
        truncated: team.truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn documents_carry_the_fragments_they_spread() {
        let doc = document(ISSUES);
        assert!(doc.contains("fragment LinearIssue on Issue") && doc.contains("fragment LinearUser") && doc.contains("fragment LinearPageInfo"));
        assert!(!doc.contains("fragment LinearComment"), "{doc}");
        assert_eq!(document(ISSUE_UPDATE), ISSUE_UPDATE);
        assert!(document(TEAM).contains("fragment LinearTeamLabel") && document(TEAM).contains("fragment LinearLabel on"));
    }

    #[test]
    fn identifiers_and_urls() {
        assert_eq!(parse_identifier(" eng-12 "), Some("ENG-12".into()));
        for bad in ["ENG", "ENG-0", "12", "E-N-1", "-1"] {
            assert_eq!(parse_identifier(bad), None, "{bad}");
        }
        let read = |u: &str| parse_issue_url(&url::Url::parse(u).unwrap());
        assert_eq!(read("https://linear.app/acme/issue/ENG-7/fix-it"), Some(("acme".into(), "ENG".into(), "ENG-7".into())));
        assert_eq!(read("https://linear.app/acme/issue/eng-7"), None);
        assert_eq!(read("http://linear.app/acme/issue/ENG-7"), None);
        assert_eq!(read("https://linear.app/acme/project/ENG-7"), None);
        assert_eq!(pull_request_link("https://github.com/o/r/pull/9?x"), Some(("o/r".into(), 9)));
        assert_eq!(pull_request_link("https://gitlab.com/g/s/p/-/merge_requests/3"), Some(("g/s/p".into(), 3)));
        assert_eq!(pull_request_link("https://github.com/o/r/issues/9"), None);
    }

    #[test]
    fn failures_read_as_upstream_reads_them() {
        let read = |status, body: &str| read_answer::<Data>("x", status, Some(5), body).err().map(|e| (e.reason, e.detail, e.retry_at));
        assert_eq!(read(429, "").map(|e| (e.0, e.2)), Some((TrackerReason::RateLimited, Some(5))));
        let limited = r#"{"errors":[{"message":"slow down","extensions":{"code":"RATELIMITED"}}]}"#;
        assert_eq!(read(400, limited).map(|e| e.0), Some(TrackerReason::RateLimited));
        assert_eq!(read(401, "{}").map(|e| e.1), Some("Linear rejected the API key.".into()));
        let refused = r#"{"errors":[{"message":"Entity not found: Issue"}],"data":null}"#;
        assert_eq!(read(200, refused).map(|e| e.1), Some("Entity not found: Issue".into()));
        assert_eq!(read(502, "<html>").map(|e| e.1), Some("Linear returned HTTP 502 with an unexpected body.".into()));
        assert_eq!(read(200, "nope").map(|e| e.1), Some("Linear returned an unexpected body.".into()));
        assert!(read_answer::<Data>("x", 200, None, r#"{"data":{"viewer":{"id":"u1"}}}"#).is_ok());
        assert_eq!(reset_at(Some("2000"), 1000), Some(2000));
        assert_eq!(reset_at(Some("500"), 1000), None);
    }

    #[test]
    fn a_listing_filter_narrows_as_upstream_does() {
        let q = ListQuery {
            state: IssueListState::Open,
            involvement: IssueInvolvement::Assigned,
            limit: 10,
            query: Some("ENG-12".into()),
            labels: vec!["bug".into()],
            assignee: Some("ann".into()),
            sort: IssueListSort::Updated,
            cursor: None,
        };
        let filter = issue_filter("eng", &q, Some("me"));
        assert_eq!(
            filter,
            json!({
                "team": { "key": { "eq": "ENG" } },
                "assignee": { "displayName": { "eq": "me" } },
                "state": { "type": { "nin": ["completed", "canceled", "duplicate"] } },
                "and": [{ "assignee": { "displayName": { "eq": "ann" } } }, { "labels": { "some": { "name": { "eq": "bug" } } } }],
                "or": [{ "title": { "containsIgnoreCase": "ENG-12" } }, { "description": { "containsIgnoreCase": "ENG-12" } }, { "number": { "eq": 12 } }],
            })
        );
    }

    #[test]
    fn reactions_keep_the_viewer_out_of_the_actors() {
        let raw: Rows<LinReaction> = serde_json::from_value(json!([
            { "id": "r1", "emoji": "+1", "user": { "id": "me", "displayName": "me" } },
            { "id": "r2", "emoji": "\u{1F44D}", "user": { "id": "u2", "displayName": "ann" } },
            { "id": "r3", "emoji": "\u{1F937}", "user": { "id": "u2" } },
        ]))
        .unwrap();
        let got = reactions(&raw, Some("me"));
        assert_eq!(got, vec![IssueReaction { content: IssueReactionContent::ThumbsUp, count: 2, actors: vec!["ann".into()], viewer_has_reacted: true }]);
    }
}
