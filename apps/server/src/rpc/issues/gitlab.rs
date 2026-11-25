//! GitLab as an issue tracker (t3code `GitLabIssueTracker` and
//! `gitLabIssueJson`), over `glab api` (`crate::glab`). A scope is a project's
//! full path (`group/sub/project`), a key an issue's iid. GitLab pages by
//! offset, so every cursor is a page number; a comments cursor carries the
//! awards cursor of the same page after a colon. Notes, label events, linked
//! merge requests and awards are separate endpoints, joined into the detail.
//! GitLab's REST enums are read as plain strings and normalized, and a row
//! that cannot be read is skipped rather than failing its page.
//!
//! The viewer is granted everything (`VIEWER_PERMISSIONS`): REST never says
//! whether the viewer opened an issue, and its author may close, reopen and
//! edit it whatever their role, so GitLab explains any refusal itself.
//!
//! Limits: as upstream, `mentioned` narrows nothing (GitLab has no such
//! filter), at most 20 templates are read, and only the first page of label
//! events, linked merge requests and issue awards is read. A failure's detail
//! is what `glab` said rather than upstream's fixed sentence (except for a
//! missing tool, a missing login and a rate limit, which are upstream's).

use super::{IssuePage, ListQuery, Served, TrackerError, TrackerIssue, TrackerReason};
use crate::gh::lenient::{Fields, Loose, Rows, rows, trimmed};
use crate::gh::{GhError, GhReason};
use crate::glab::{self, Api};
use domovoi_model::typed::{
    IssueActor, IssueAssigneeCandidate, IssueCandidates, IssueCapabilities, IssueComment, IssueCommentsPage, IssueCreateResult, IssueEvent, IssueEventKind,
    IssueInvolvement, IssueLabel, IssueLabelCandidate, IssueLinkedPullRequest, IssueListSort, IssueListState, IssueReaction, IssueReactionContent, IssueState,
    IssueStateCategory, IssueSummary, IssueTemplate, IssueTemplateList, IssueViewerPermissions, PullRequestState,
};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

pub const KIND: &str = "gitlab";

pub const CAPABILITIES: IssueCapabilities = IssueCapabilities::ALL;

/// GitLab's ceiling on `per_page`, which is what every bounded read asks for.
const PAGE_SIZE: u64 = 100;
/// How many pages of system notes the timeline reads before giving up on the rest.
const MAX_ACTIVITY_PAGES: u64 = 20;
/// How many of a scope's templates are read in full; each one is a request of its own.
const TEMPLATE_LIMIT: usize = 20;

const VIEWER_PERMISSIONS: IssueViewerPermissions = IssueViewerPermissions { set_state: true, comment: true, edit: true, labels: true, assignees: true };

/// Awards on every note of the issue: `mergeRequest` is an alias for the
/// issue, so the merge request decoder's shape answers for both.
const ISSUE_AWARD_EMOJI_QUERY: &str = "query($fullPath: ID!, $iid: String!, $cursor: String) { currentUser { username } project(fullPath: $fullPath) { mergeRequest: issue(iid: $iid) { notes(first: 100, after: $cursor) { pageInfo { hasNextPage endCursor } nodes { id awardEmoji { nodes { name user { username } } } } } } } }";

/// GitLab has exactly the two states and records no reason for a close.
pub fn issue_states() -> Vec<IssueState> {
    vec![
        IssueState { category: IssueStateCategory::Open, name: "Open".into(), close_reason: None },
        IssueState { category: IssueStateCategory::Closed, name: "Closed".into(), close_reason: None },
    ]
}

// ---------------------------------------------------------------------------
// What GitLab answers
// ---------------------------------------------------------------------------

#[derive(Deserialize, Default)]
#[serde(default)]
struct GlUser {
    id: Loose<u64>,
    username: Loose<String>,
    name: Loose<String>,
    avatar_url: Loose<String>,
}

fn actor(raw: &GlUser) -> Option<IssueActor> {
    Some(IssueActor { is_bot: None, login: trimmed(&raw.username)?, name: trimmed(&raw.name), avatar_url: trimmed(&raw.avatar_url) })
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct GlLabel {
    name: Loose<String>,
    color: Loose<String>,
    description: Loose<String>,
}

/// `with_labels_details=true` answers label objects; hosts that ignore it still send names.
#[derive(Deserialize)]
#[serde(untagged)]
enum GlLabelRef {
    Name(String),
    Detail(Fields<GlLabel>),
}

fn labels(raw: &Loose<Vec<GlLabelRef>>) -> Vec<IssueLabel> {
    raw.iter()
        .flatten()
        .filter_map(|label| match label {
            GlLabelRef::Name(name) => {
                let name = name.trim();
                (!name.is_empty()).then(|| IssueLabel { name: name.to_owned(), color: None, description: None })
            }
            GlLabelRef::Detail(l) => Some(IssueLabel { name: trimmed(&l.name)?, color: trimmed(&l.color), description: Some(trimmed(&l.description)) }),
        })
        .collect()
}

/// An issue as REST answers it (`RawIssueSchema`).
#[derive(Deserialize, Default)]
#[serde(default)]
struct GlIssue {
    iid: Loose<u64>,
    title: Loose<String>,
    web_url: Loose<String>,
    description: Loose<String>,
    author: Fields<GlUser>,
    state: Loose<String>,
    created_at: Loose<String>,
    updated_at: Loose<String>,
    closed_at: Loose<String>,
    assignees: Rows<GlUser>,
    labels: Loose<Vec<GlLabelRef>>,
    user_notes_count: Loose<u64>,
}

/// `toSummary`; `None` for a row without what upstream's schema requires.
fn summary(raw: &GlIssue, target: &Served) -> Option<IssueSummary> {
    let iid = raw.iid.0?;
    let states = issue_states();
    // GitLab calls the open one `opened`.
    let closed = raw.state.as_deref().is_some_and(|s| s.trim().eq_ignore_ascii_case("closed"));
    Some(IssueSummary {
        tracker: KIND.into(),
        host: target.host.clone(),
        scope: target.scope.clone(),
        key: iid.to_string(),
        display_key: format!("#{iid}"),
        title: raw.title.0.clone()?,
        url: raw.web_url.0.clone()?,
        author: actor(&raw.author),
        state: states[usize::from(closed)].clone(),
        created_at: raw.created_at.0.clone()?.into(),
        updated_at: raw.updated_at.0.clone()?.into(),
        closed_at: trimmed(&raw.closed_at).map(Into::into),
        assignees: rows(&raw.assignees).filter_map(actor).collect(),
        labels: labels(&raw.labels),
        comment_count: raw.user_notes_count.unwrap_or(0),
    })
}

/// A note (`RawNoteSchema`): `system` marks the ones GitLab writes itself, which are events.
#[derive(Deserialize, Default)]
#[serde(default)]
struct GlNote {
    id: Loose<u64>,
    body: Loose<String>,
    author: Fields<GlUser>,
    created_at: Loose<String>,
    system: Loose<bool>,
}

/// `toSystemEvent`. A label note names its labels by id, which is why
/// labellings come from `/resource_label_events` instead and are dropped here.
fn system_event(body: &str) -> Option<(IssueEventKind, Option<String>)> {
    let text = body.trim();
    let rest = |prefix: &str| text.strip_prefix(prefix).map(|r| r.trim_start_matches('@').trim().to_owned()).filter(|r| !r.is_empty());
    match text {
        "closed" => return Some((IssueEventKind::Closed, None)),
        "reopened" => return Some((IssueEventKind::Reopened, None)),
        _ => {}
    }
    if text.starts_with("assigned to ") {
        return Some((IssueEventKind::Assigned, rest("assigned to ")));
    }
    if text.starts_with("unassigned ") {
        return Some((IssueEventKind::Unassigned, rest("unassigned ")));
    }
    if text.starts_with("mentioned in ") {
        return Some((IssueEventKind::Referenced, text.strip_prefix("mentioned in ").map(str::trim).filter(|r| !r.is_empty()).map(str::to_owned)));
    }
    if text.starts_with("changed title") {
        // GitLab marks the new title with its own inline-diff braces.
        let title = text.split_once("{+").and_then(|(_, after)| after.rsplit_once("+}")).map(|(title, _)| title.trim().to_owned()).filter(|t| !t.is_empty());
        return Some((IssueEventKind::Renamed, title));
    }
    None
}

/// One page of notes, split into what people wrote and what GitLab wrote about the issue.
struct Notes {
    comments: Vec<IssueComment>,
    events: Vec<IssueEvent>,
    next: Option<u64>,
}

/// `decodeIssueNotesJson` with the page `glab api --include` said comes next.
fn notes_of(raw: &Rows<GlNote>, next: Option<Option<u64>>, page: u64) -> Notes {
    let mut comments = vec![];
    let mut events = vec![];
    for note in rows(raw) {
        let (Some(id), Some(created_at)) = (note.id.0, note.created_at.0.clone()) else { continue };
        let body = note.body.as_deref().unwrap_or("");
        if note.system.0 == Some(true) {
            if let Some((kind, detail)) = system_event(body) {
                // Notes and label events are numbered separately, so each carries where it came from.
                events.push(IssueEvent { id: format!("note-{id}"), kind, actor: actor(&note.author), created_at: created_at.into(), detail });
            }
            continue;
        }
        comments.push(IssueComment { id: id.to_string(), author: actor(&note.author), body: body.to_owned(), created_at: created_at.into(), url: None, reactions: None });
    }
    let count = raw.as_ref().map_or(0, Vec::len) as u64;
    // Without headers a full page is taken as the sign of more.
    let next = next.unwrap_or_else(|| (count >= PAGE_SIZE).then_some(page + 1));
    Notes { comments, events, next }
}

/// `splitPagedResponse`: `glab api --include` output as its body and GitLab's
/// next page (`Some(None)`: the last page; `None`: no header to say).
fn split_paged(raw: &str) -> (&str, Option<Option<u64>>) {
    if !raw.starts_with("HTTP/") {
        return (raw, None);
    }
    let (headers, body) = match raw.find("\r\n\r\n").map(|at| (at, 4)).or_else(|| raw.find("\n\n").map(|at| (at, 2))) {
        Some((at, gap)) => (&raw[..at], &raw[at + gap..]),
        None => (raw, ""),
    };
    let next = headers.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.trim().eq_ignore_ascii_case("x-next-page").then(|| value.trim().parse::<u64>().ok().filter(|n| *n > 0 && *n < 10_000_000))
    });
    (body, next)
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct GlLabelEvent {
    id: Loose<u64>,
    action: Loose<String>,
    created_at: Loose<String>,
    user: Fields<GlUser>,
    label: Option<Fields<GlLabel>>,
}

/// `decodeLabelEventsJson`.
fn label_events(raw: &Rows<GlLabelEvent>) -> Vec<IssueEvent> {
    rows(raw)
        .filter_map(|e| {
            let kind = match e.action.as_deref()?.trim().to_lowercase().as_str() {
                "add" => IssueEventKind::Labeled,
                "remove" => IssueEventKind::Unlabeled,
                _ => return None,
            };
            Some(IssueEvent {
                id: format!("label-{}", e.id.0?),
                kind,
                actor: actor(&e.user),
                created_at: e.created_at.0.clone()?.into(),
                // Null once the label itself has been deleted.
                detail: e.label.as_ref().and_then(|l| trimmed(&l.name)),
            })
        })
        .collect()
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct GlReferences {
    full: Loose<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct GlLinkedMergeRequest {
    iid: Loose<u64>,
    title: Loose<String>,
    web_url: Loose<String>,
    state: Loose<String>,
    draft: Loose<bool>,
    work_in_progress: Loose<bool>,
    /// `group/project!12`: the only place the merge request names its project.
    references: Fields<GlReferences>,
}

/// `decodeLinkedMergeRequestsJson`.
fn linked_merge_requests(raw: &Rows<GlLinkedMergeRequest>, closes_issue: bool) -> Vec<IssueLinkedPullRequest> {
    rows(raw)
        .filter_map(|mr| {
            let repository = mr.references.full.as_deref()?.split('!').next()?.trim().to_owned();
            let number = mr.iid.0.filter(|n| *n > 0)?;
            let state = match mr.state.as_deref().unwrap_or("").trim().to_lowercase().as_str() {
                "merged" => PullRequestState::Merged,
                "closed" => PullRequestState::Closed,
                // `locked` is an open merge request whose discussion is locked.
                _ => PullRequestState::Open,
            };
            (!repository.is_empty()).then_some(())?;
            Some(IssueLinkedPullRequest {
                repository,
                number,
                title: trimmed(&mr.title)?,
                url: trimmed(&mr.web_url)?,
                state,
                is_draft: mr.draft.0.or(mr.work_in_progress.0).unwrap_or(false),
                closes_issue,
            })
        })
        .collect()
}

/// GitLab's award name of each reaction (`GITLAB_AWARD_BY_CONTENT`).
const AWARDS: [(IssueReactionContent, &str); 8] = [
    (IssueReactionContent::ThumbsUp, "thumbsup"),
    (IssueReactionContent::ThumbsDown, "thumbsdown"),
    (IssueReactionContent::Laugh, "laughing"),
    (IssueReactionContent::Hooray, "tada"),
    (IssueReactionContent::Confused, "confused"),
    (IssueReactionContent::Heart, "heart"),
    (IssueReactionContent::Rocket, "rocket"),
    (IssueReactionContent::Eyes, "eyes"),
];

fn award_name(content: &IssueReactionContent) -> Option<&'static str> {
    AWARDS.iter().find(|(c, _)| c == content).map(|(_, name)| *name)
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct Username {
    username: Loose<String>,
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
fn reactions<'a>(awards: impl Iterator<Item = &'a Award>, viewer: Option<&str>) -> Vec<IssueReaction> {
    let viewer = viewer.map(str::to_lowercase);
    let mut groups: Vec<IssueReaction> = vec![];
    for award in awards {
        let name = award.name.as_deref().unwrap_or("").trim().to_lowercase();
        let Some(content) = AWARDS.iter().find(|(_, n)| *n == name).map(|(c, _)| c.clone()) else { continue };
        let Some(user) = trimmed(&award.user.username) else { continue };
        let index = match groups.iter().position(|g| g.content == content) {
            Some(i) => i,
            None => {
                groups.push(IssueReaction { content, count: 0, actors: vec![], viewer_has_reacted: false });
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
struct AwardNodes {
    nodes: Rows<Award>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct AwardNote {
    id: Loose<String>,
    award_emoji: Fields<AwardNodes>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct PageInfo {
    has_next_page: Loose<bool>,
    end_cursor: Loose<String>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct AwardNotes {
    page_info: Fields<PageInfo>,
    nodes: Rows<AwardNote>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct AwardIssue {
    notes: Fields<AwardNotes>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct AwardProject {
    merge_request: Option<Fields<AwardIssue>>,
}

#[derive(Deserialize, Default)]
#[serde(default, rename_all = "camelCase")]
struct AwardData {
    current_user: Fields<Username>,
    project: Fields<AwardProject>,
}

/// One page of note awards: reactions by note id, and where the next page starts.
fn note_awards(data: &AwardData) -> (HashMap<String, Vec<IssueReaction>>, Option<String>) {
    let viewer = trimmed(&data.current_user.username);
    let Some(notes) = data.project.merge_request.as_ref().map(|i| &i.notes) else { return (HashMap::new(), None) };
    let by_note = rows(&notes.nodes)
        .filter_map(|n| {
            let id = trimmed(&n.id)?.rsplit('/').next()?.to_owned();
            let found = reactions(rows(&n.award_emoji.nodes), viewer.as_deref());
            (id.chars().all(|c| c.is_ascii_digit()) && !found.is_empty()).then_some((id, found))
        })
        .collect();
    let next = (notes.page_info.has_next_page.0 == Some(true)).then(|| trimmed(&notes.page_info.end_cursor)).flatten();
    (by_note, next)
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct GlTemplateEntry {
    key: Loose<String>,
    name: Loose<String>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct GlTemplate {
    content: Loose<String>,
}

// ---------------------------------------------------------------------------
// Failures and addressing
// ---------------------------------------------------------------------------

/// `reasonFor`: GitLab's issue tracker reports a missing tool, a missing
/// login and a rate limit; anything else failed.
fn reason_of(error: &GhError) -> TrackerReason {
    match error.reason {
        GhReason::MissingTool => TrackerReason::MissingTool,
        GhReason::Unauthenticated => TrackerReason::Unauthenticated,
        GhReason::RateLimited => TrackerReason::RateLimited,
        _ => TrackerReason::Failed,
    }
}

fn fail(operation: &str) -> impl Fn(GhError) -> TrackerError + '_ {
    move |error| TrackerError::new(KIND, operation, reason_of(&error), error.detail)
}

fn refused(operation: &str, detail: impl Into<String>) -> TrackerError {
    TrackerError::new(KIND, operation, TrackerReason::Failed, detail)
}

fn not_found(operation: &str) -> TrackerError {
    TrackerError::new(KIND, operation, TrackerReason::NotFound, "GitLab could not find the requested issue.")
}

fn project(target: &Served) -> String {
    format!("projects/{}", glab::project_path(&target.scope))
}

/// An issue key is its iid; anything else names no GitLab issue (`parseNumber`).
pub fn issue_number(key: &str) -> Option<u64> {
    let key = key.trim();
    (!key.is_empty() && !key.starts_with('0') && key.chars().all(|c| c.is_ascii_digit())).then(|| key.parse().ok()).flatten()
}

fn issue_path(target: &Served, key: &str, operation: &str) -> Result<(u64, String), TrackerError> {
    let number = issue_number(key).ok_or_else(|| not_found(operation))?;
    Ok((number, format!("{}/issues/{number}", project(target))))
}

/// A page cursor this tracker handed out (`parsePage`).
fn parse_page(cursor: Option<&str>) -> Option<u64> {
    match cursor {
        None => Some(1),
        Some(c) => (!c.is_empty() && c.len() <= 7 && !c.starts_with('0') && c.chars().all(|ch| ch.is_ascii_digit())).then(|| c.parse().ok()).flatten(),
    }
}

/// `parseCommentsCursor`: the next REST page of notes and, after a colon,
/// where GraphQL's awards for that page start.
fn parse_comments_cursor(cursor: &str) -> Option<(u64, Option<String>)> {
    let (page, awards) = match cursor.split_once(':') {
        Some((page, awards)) if !awards.is_empty() && awards.chars().count() <= 512 => (page, Some(awards.to_owned())),
        Some(_) => return None,
        None => (cursor, None),
    };
    Some((parse_page(Some(page))?, awards))
}

fn comments_cursor(notes_next: Option<u64>, awards_next: Option<String>) -> Option<String> {
    let notes = notes_next?;
    Some(match awards_next {
        Some(awards) => format!("{notes}:{awards}"),
        None => notes.to_string(),
    })
}

async fn get<T: serde::de::DeserializeOwned>(target: &Served, operation: &str, path: &str) -> Result<T, TrackerError> {
    let out = glab::api(Some(&target.workspace_root), &target.host, &Api::get(path)).await.map_err(fail(operation))?;
    glab::read_json(&out).map_err(|_| refused(operation, format!("GitLab returned an unreadable {operation} response.")))
}

async fn write<B: Serialize>(target: &Served, operation: &str, method: &str, path: &str, body: Option<&B>) -> Result<String, TrackerError> {
    let request = match body {
        Some(body) => Api::send(method, path, body).map_err(|_| refused(operation, "The request could not be written."))?,
        None => Api::call(method, path),
    };
    glab::api(Some(&target.workspace_root), &target.host, &request).await.map_err(fail(operation))
}

fn paged(path: &str, page: u64) -> String {
    let page = page.to_string();
    format!("{path}?{}", glab::query(&[("per_page", "100"), ("page", &page)]))
}

/// One page of an issue's notes, with the page GitLab says comes next.
async fn notes_page(target: &Served, path: &str, page: u64, operation: &str, only_activity: bool) -> Result<Notes, TrackerError> {
    let page_text = page.to_string();
    let mut params = vec![("per_page", "100"), ("page", page_text.as_str()), ("order_by", "created_at"), ("sort", "asc")];
    if only_activity {
        params.push(("activity_filter", "only_activity"));
    }
    let endpoint = format!("{path}/notes?{}", glab::query(&params));
    let out = glab::run(Some(&target.workspace_root), &target.host, &["api", &endpoint, "--include"], None, glab::SLOW_TIMEOUT).await.map_err(fail(operation))?;
    let (body, next) = split_paged(&out);
    let raw: Rows<GlNote> = glab::read_json(body).map_err(|_| refused(operation, format!("GitLab returned an unreadable {operation} response.")))?;
    Ok(notes_of(&raw, next, page))
}

/// Every system note, wherever it falls among the comments, for the issue's timeline.
async fn activity(target: &Served, path: &str) -> Result<Vec<IssueEvent>, TrackerError> {
    let mut events = vec![];
    let mut page = 1;
    for _ in 0..MAX_ACTIVITY_PAGES {
        let notes = notes_page(target, path, page, "getIssue", true).await?;
        events.extend(notes.events);
        match notes.next {
            Some(next) => page = next,
            None => break,
        }
    }
    Ok(events)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct AwardVariables<'a> {
    full_path: &'a str,
    iid: String,
    cursor: Option<&'a str>,
}

async fn awards(target: &Served, number: u64, operation: &str, cursor: Option<&str>) -> Result<(HashMap<String, Vec<IssueReaction>>, Option<String>), TrackerError> {
    let vars = AwardVariables { full_path: target.scope.trim(), iid: number.to_string(), cursor };
    let data: AwardData = glab::graphql(Some(&target.workspace_root), &target.host, ISSUE_AWARD_EMOJI_QUERY, vars).await.map_err(fail(operation))?;
    Ok(note_awards(&data))
}

// ---------------------------------------------------------------------------
// The tracker
// ---------------------------------------------------------------------------

/// The signed-in account; an answer naming nobody is a failure, since every comparison needs a login.
pub async fn viewer(target: &Served) -> Result<String, TrackerError> {
    glab::read_identity(Some(&target.workspace_root), &target.host).await.map(|(_, login)| login).map_err(fail("getViewer"))
}

/// `listIssues`.
pub async fn list_issues(target: &Served, q: &ListQuery, viewer: Option<&str>) -> Result<IssuePage, TrackerError> {
    let page = parse_page(q.cursor.as_deref()).ok_or_else(|| refused("listIssues", "The cursor was not one GitLab handed out."))?;
    let per_page = q.limit.clamp(1, PAGE_SIZE);
    let viewer = viewer.unwrap_or("");
    let mut params: Vec<(&str, String)> = vec![(
        "state",
        match q.state {
            IssueListState::Open => "opened".into(),
            IssueListState::Closed => "closed".into(),
            _ => "all".into(),
        },
    )];
    match q.involvement {
        IssueInvolvement::Authored => params.push(("author_username", viewer.to_owned())),
        // A named assignee takes the one assignee parameter GitLab reads; the viewer is kept by dropping rows.
        IssueInvolvement::Assigned if q.assignee.is_none() => params.push(("assignee_username", viewer.to_owned())),
        _ => {}
    }
    if let Some(text) = q.query.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
        params.push(("search", text.to_owned()));
    }
    if !q.labels.is_empty() {
        params.push(("labels", q.labels.join(",")));
    }
    if let Some(assignee) = &q.assignee {
        params.push(("assignee_username", assignee.clone()));
    }
    params.extend([
        ("order_by", if q.sort == IssueListSort::Created { "created_at".into() } else { "updated_at".into() }),
        ("sort", "desc".into()),
        ("with_labels_details", "true".into()),
        ("per_page", per_page.to_string()),
        ("page", page.to_string()),
    ]);
    let pairs: Vec<(&str, &str)> = params.iter().map(|(k, v)| (*k, v.as_str())).collect();
    let raw: Rows<GlIssue> = get(target, "listIssues", &format!("{}/issues?{}", project(target), glab::query(&pairs))).await?;
    let also_assigned_to = (q.involvement == IssueInvolvement::Assigned && q.assignee.is_some()).then(|| viewer.to_lowercase());
    let items = rows(&raw)
        .filter_map(|issue| summary(issue, target))
        .filter(|issue| also_assigned_to.as_ref().is_none_or(|v| issue.assignees.iter().any(|a| a.login.to_lowercase() == *v)))
        .collect();
    let count = raw.as_ref().map_or(0, Vec::len) as u64;
    Ok(IssuePage { items, next_cursor: (count >= per_page).then(|| (page + 1).to_string()) })
}

/// `getIssue`: the issue, its comments and timeline, the merge requests that
/// close or mention it, and its awards.
pub async fn get_issue(target: &Served, key: &str) -> Result<TrackerIssue, TrackerError> {
    let operation = "getIssue";
    let (number, path) = issue_path(target, key, operation)?;
    let number_text = number.to_string();
    let found_path = format!("{}/issues?{}", project(target), glab::query(&[("iids[]", &number_text), ("with_labels_details", "true"), ("per_page", "1")]));
    let label_events_path = paged(&format!("{path}/resource_label_events"), 1);
    let closed_by_path = paged(&format!("{path}/closed_by"), 1);
    let related_path = paged(&format!("{path}/related_merge_requests"), 1);
    let awards_path = paged(&format!("{path}/award_emoji"), 1);
    let (found, login, notes, system, label_events_read, closing, related, note_awards_read, issue_awards) = tokio::join!(
        get::<Rows<GlIssue>>(target, operation, &found_path),
        viewer(target),
        notes_page(target, &path, 1, operation, false),
        activity(target, &path),
        get::<Rows<GlLabelEvent>>(target, operation, &label_events_path),
        get::<Rows<GlLinkedMergeRequest>>(target, operation, &closed_by_path),
        get::<Rows<GlLinkedMergeRequest>>(target, operation, &related_path),
        awards(target, number, operation, None),
        get::<Rows<Award>>(target, operation, &awards_path)
    );
    let found = found?;
    let issue = rows(&found).next().ok_or_else(|| not_found(operation))?;
    let issue_summary = summary(issue, target).ok_or_else(|| refused(operation, "GitLab returned an unreadable getIssue response."))?;
    let login = login.map_err(|e| TrackerError { operation: operation.into(), ..e })?;
    let (notes, system, label_events_read, closing, related) = (notes?, system?, label_events_read?, closing?, related?);
    let (by_note, awards_next) = note_awards_read?;
    let issue_awards = issue_awards?;
    let mut events = system;
    events.extend(label_events(&label_events_read));
    events.sort_by(|a, b| a.created_at.as_str().cmp(b.created_at.as_str()));
    // A merge request that closes the issue outranks its sighting as a mere mention.
    let mut linked = linked_merge_requests(&closing, true);
    let closing_keys: HashSet<String> = linked.iter().map(|l| format!("{}!{}", l.repository, l.number)).collect();
    linked.extend(linked_merge_requests(&related, false).into_iter().filter(|l| !closing_keys.contains(&format!("{}!{}", l.repository, l.number))));
    Ok(TrackerIssue {
        summary: issue_summary,
        body: issue.description.as_deref().unwrap_or("").to_owned(),
        reactions: Some(reactions(rows(&issue_awards), Some(&login))),
        viewer: Some(login),
        viewer_permissions: VIEWER_PERMISSIONS,
        states: issue_states(),
        comments: notes.comments.into_iter().map(|c| IssueComment { reactions: Some(by_note.get(&c.id).cloned().unwrap_or_default()), ..c }).collect(),
        comments_cursor: comments_cursor(notes.next, awards_next),
        events,
        linked_pull_requests: linked,
    })
}

/// `getComments`: a later page of notes, joined with its own awards.
pub async fn get_comments(target: &Served, key: &str, cursor: &str) -> Result<IssueCommentsPage, TrackerError> {
    let operation = "getComments";
    let (page, awards_cursor) = parse_comments_cursor(cursor).ok_or_else(|| refused(operation, "The cursor was not one GitLab handed out."))?;
    let (number, path) = issue_path(target, key, operation)?;
    // Without its awards cursor a later page's awards cannot be found.
    let read_awards = async {
        if awards_cursor.is_none() && page > 1 { Ok((HashMap::new(), None)) } else { awards(target, number, operation, awards_cursor.as_deref()).await }
    };
    let (notes, awarded) = tokio::join!(notes_page(target, &path, page, operation, false), read_awards);
    let (notes, (by_note, awards_next)) = (notes?, awarded?);
    Ok(IssueCommentsPage {
        comments: notes.comments.into_iter().map(|c| IssueComment { reactions: Some(by_note.get(&c.id).cloned().unwrap_or_default()), ..c }).collect(),
        next_cursor: comments_cursor(notes.next, awards_next),
    })
}

/// GitLab writes assignees by numeric id, which is what the candidates carry
/// as `id`; a username an issue names is looked up.
async fn assignee_ids(target: &Served, operation: &str, assignees: &[String]) -> Result<Vec<u64>, TrackerError> {
    let mut ids = vec![];
    for assignee in assignees {
        if let Some(id) = issue_number(assignee) {
            ids.push(id);
            continue;
        }
        let users: Rows<GlUser> = get(target, operation, &format!("users?{}", glab::query(&[("username", assignee)]))).await?;
        let id = rows(&users).find_map(|u| u.id.0).ok_or_else(|| refused(operation, format!("GitLab has no user named {assignee}.")))?;
        ids.push(id);
    }
    Ok(ids)
}

#[derive(Serialize)]
struct CreateBody<'a> {
    title: &'a str,
    description: &'a str,
    labels: String,
    assignee_ids: Vec<u64>,
}

/// `create`.
pub async fn create(target: &Served, title: &str, body: &str, labels: &[String], assignees: &[String]) -> Result<IssueCreateResult, TrackerError> {
    let assignee_ids = assignee_ids(target, "create", assignees).await?;
    let request = CreateBody { title, description: body, labels: labels.join(","), assignee_ids };
    let out = write(target, "create", "POST", &format!("{}/issues", project(target)), Some(&request)).await?;
    let created: Fields<GlIssue> = glab::read_json(&out).map_err(|_| refused("create", "GitLab returned an unreadable create response."))?;
    let (Some(number), Some(url)) = (created.iid.0, created.web_url.0.clone()) else {
        return Err(refused("create", "GitLab returned an unreadable create response."));
    };
    Ok(IssueCreateResult { key: number.to_string(), display_key: format!("#{number}"), url })
}

#[derive(Serialize)]
struct NoteBody<'a> {
    body: &'a str,
}

/// `comment`.
pub async fn comment(target: &Served, key: &str, body: &str) -> Result<(), TrackerError> {
    let (_, path) = issue_path(target, key, "comment")?;
    write(target, "comment", "POST", &format!("{path}/notes"), Some(&NoteBody { body })).await.map(|_| ())
}

#[derive(Serialize)]
struct UpdateBody<'a> {
    #[serde(skip_serializing_if = "Option::is_none")]
    title: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    labels: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    assignee_ids: Option<Vec<u64>>,
}

/// `update`: only the fields given are written.
pub async fn update(target: &Served, key: &str, title: Option<&str>, body: Option<&str>, labels: Option<&[String]>, assignees: Option<&[String]>) -> Result<(), TrackerError> {
    let (_, path) = issue_path(target, key, "update")?;
    let assignee_ids = match assignees {
        Some(assignees) => Some(assignee_ids(target, "update", assignees).await?),
        None => None,
    };
    let change = UpdateBody { title, description: body, labels: labels.map(|l| l.join(",")), assignee_ids };
    write(target, "update", "PUT", &path, Some(&change)).await.map(|_| ())
}

#[derive(Serialize)]
struct StateBody {
    state_event: &'static str,
}

/// `setState`: one of the two states, by name.
pub async fn set_state(target: &Served, key: &str, state: &str) -> Result<(), TrackerError> {
    let wanted = issue_states().into_iter().find(|s| s.name == state).ok_or_else(|| refused("setState", format!("A GitLab issue has no \"{state}\" state.")))?;
    let (_, path) = issue_path(target, key, "setState")?;
    let state_event = if wanted.category == IssueStateCategory::Closed { "close" } else { "reopen" };
    write(target, "setState", "PUT", &path, Some(&StateBody { state_event })).await.map(|_| ())
}

/// `setReaction`: an award on the issue or one of its notes. GitLab deletes
/// an award by its id, so the reader's own award of that name is found first;
/// nothing to delete is success.
pub async fn set_reaction(target: &Served, key: &str, subject_id: Option<&str>, content: &IssueReactionContent, reacted: bool) -> Result<(), TrackerError> {
    let operation = "setReaction";
    let (_, path) = issue_path(target, key, operation)?;
    if subject_id.is_some_and(|id| issue_number(id).is_none()) {
        return Err(refused(operation, "The comment id names no GitLab note."));
    }
    let award = award_name(content).ok_or_else(|| refused(operation, "Unknown reaction."))?;
    let subject = match subject_id {
        Some(note) => format!("{path}/notes/{note}/award_emoji"),
        None => format!("{path}/award_emoji"),
    };
    if reacted {
        return write::<()>(target, operation, "POST", &format!("{subject}?{}", glab::query(&[("name", award)])), None).await.map(|_| ());
    }
    let login = viewer(target).await.map_err(|e| TrackerError { operation: operation.into(), ..e })?;
    for page in 1.. {
        let listed: Rows<Award> = get(target, operation, &paged(&subject, page)).await?;
        let own = rows(&listed).find(|a| {
            a.name.as_deref().map(|n| n.trim().to_lowercase()).as_deref() == Some(award) && trimmed(&a.user.username).as_deref() == Some(login.as_str())
        });
        if let Some(id) = own.and_then(|a| a.id.0) {
            return write::<()>(target, operation, "DELETE", &format!("{subject}/{id}"), None).await.map(|_| ());
        }
        // An empty raw page establishes exhaustion even when the host caps page size.
        if listed.as_ref().is_none_or(Vec::is_empty) {
            return Ok(());
        }
    }
    Ok(())
}

/// `listTemplates`: each template's markdown is what a description starts
/// out as; GitLab has no switch that forbids an issue filed from nothing.
pub async fn list_templates(target: &Served) -> Result<IssueTemplateList, TrackerError> {
    let operation = "listTemplates";
    let base = format!("{}/templates/issues", project(target));
    let entries: Rows<GlTemplateEntry> = get(target, operation, &base).await?;
    let mut templates = vec![];
    for entry in rows(&entries).take(TEMPLATE_LIMIT) {
        let Some(key) = trimmed(&entry.key) else { continue };
        let name = trimmed(&entry.name).unwrap_or_else(|| key.clone());
        let template: Fields<GlTemplate> = get(target, operation, &format!("{base}/{}", glab::encode(&key))).await?;
        templates.push(IssueTemplate { key, name, about: String::new(), title: String::new(), body: text(&template.content), labels: vec![], assignees: vec![] });
    }
    Ok(IssueTemplateList { templates, external_templates: None, blank_issues_enabled: true })
}

fn text(value: &Loose<String>) -> String {
    value.as_deref().unwrap_or("").to_owned()
}

/// `listCandidates`: the project's labels and the people with access to it.
/// A member without a numeric id is dropped, as GitLab assigns by id alone.
pub async fn list_candidates(target: &Served) -> Result<IssueCandidates, TrackerError> {
    let operation = "listCandidates";
    let labels_path = paged(&format!("{}/labels", project(target)), 1);
    let members_path = paged(&format!("{}/members/all", project(target)), 1);
    let (labels_read, members) = tokio::join!(get::<Rows<GlLabel>>(target, operation, &labels_path), get::<Rows<GlUser>>(target, operation, &members_path));
    let (labels_read, members) = (labels_read?, members?);
    let labels: Vec<IssueLabelCandidate> = rows(&labels_read)
        .filter_map(|l| Some(IssueLabelCandidate { name: trimmed(&l.name)?, color: trimmed(&l.color), description: l.description.0.clone() }))
        .collect();
    let assignees: Vec<IssueAssigneeCandidate> = rows(&members).filter_map(|u| Some(IssueAssigneeCandidate { id: u.id.0?.to_string(), actor: actor(u)? })).collect();
    let truncated = labels.len() as u64 >= PAGE_SIZE || assignees.len() as u64 >= PAGE_SIZE;
    Ok(IssueCandidates { labels, assignees, truncated })
}

// ---------------------------------------------------------------------------
// Issue URLs
// ---------------------------------------------------------------------------

/// `/<group>/<project>/-/issues/<n>` (or `work_items`) of a URL on a GitLab
/// host (`ISSUE_PATH`): host, scope and key. `known` is set when a project
/// on the URL's host vouches for it; otherwise the host's name must say GitLab.
pub fn parse_issue_url(url: &url::Url, known: bool) -> Option<(String, String, String)> {
    if !matches!(url.scheme(), "https" | "http") {
        return None;
    }
    let host = url.host_str()?.to_lowercase();
    if !known && !glab::recognizes_host(&host) {
        return None;
    }
    let path = url.path().trim_start_matches('/');
    let (project, rest) = path.split_once("/-/")?;
    let parts: Vec<&str> = project.split('/').collect();
    if parts.len() < 2 || parts.iter().any(|p| p.is_empty()) {
        return None;
    }
    let mut rest = rest.split('/');
    let (kind, number) = (rest.next()?, rest.next()?);
    if !matches!(kind, "issues" | "work_items") {
        return None;
    }
    let number = issue_number(number)?;
    Some((host, project.to_lowercase(), number.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn served() -> Served {
        Served {
            project_id: "p".into(),
            project_title: "P".into(),
            workspace_root: "/nowhere".into(),
            tracker: KIND.into(),
            host: "gitlab.com".into(),
            scope: "group/one".into(),
            kind: super::super::Kind::GitLab,
            userdata: std::path::PathBuf::from("/nowhere"),
        }
    }

    #[test]
    fn issues_map_like_upstream() {
        let raw: Fields<GlIssue> = serde_json::from_value(json!({
            "iid": 4, "title": "Crash", "web_url": "https://gitlab.com/group/one/-/issues/4", "state": "closed",
            "created_at": "1", "updated_at": "2", "closed_at": "3", "author": { "username": "me", "name": "Me" },
            "assignees": [{ "id": 7, "username": "bob" }, { "id": 8 }], "labels": ["bug", { "name": "ui", "color": "#f00" }], "user_notes_count": 2
        }))
        .unwrap();
        let s = json!(summary(&raw, &served()).unwrap());
        assert_eq!(s["displayKey"], "#4");
        assert_eq!(s["state"], json!({ "category": "closed", "name": "Closed" }));
        assert_eq!(s["assignees"], json!([{ "login": "bob", "name": null, "avatarUrl": null }]));
        assert_eq!(s["labels"], json!([{ "name": "bug", "color": null }, { "name": "ui", "color": "#f00", "description": null }]));
        assert_eq!(s["commentCount"], 2);
        let opened: Fields<GlIssue> = serde_json::from_value(json!({ "iid": 1, "title": "t", "web_url": "u", "state": "opened", "created_at": "1", "updated_at": "1" })).unwrap();
        assert_eq!(summary(&opened, &served()).unwrap().state.name, "Open");
        let partial: Fields<GlIssue> = serde_json::from_value(json!({ "iid": 1, "title": "t" })).unwrap();
        assert!(summary(&partial, &served()).is_none(), "a row without what the schema requires is skipped");
    }

    #[test]
    fn notes_split_into_comments_and_events() {
        let raw: Rows<GlNote> = serde_json::from_value(json!([
            { "id": 1, "body": "Hello", "created_at": "1", "author": { "username": "me" } },
            { "id": 2, "body": "closed", "created_at": "2", "system": true },
            { "id": 3, "body": "assigned to @bob", "created_at": "3", "system": true },
            { "id": 4, "body": "changed title from **{-Old-}** to **{+New title+}**", "created_at": "4", "system": true },
            { "id": 5, "body": "added ~7 label", "created_at": "5", "system": true },
            { "id": 6, "body": "mentioned in merge request !3", "created_at": "6", "system": true },
            { "body": "no id", "created_at": "7" }
        ]))
        .unwrap();
        let notes = notes_of(&raw, None, 1);
        assert_eq!(notes.comments.iter().map(|c| c.id.as_str()).collect::<Vec<_>>(), ["1"]);
        assert_eq!(
            json!(notes.events.iter().map(|e| (e.id.clone(), e.kind.clone(), e.detail.clone())).collect::<Vec<_>>()),
            json!([["note-2", "closed", null], ["note-3", "assigned", "bob"], ["note-4", "renamed", "New title"], ["note-6", "referenced", "merge request !3"]])
        );
        assert_eq!(notes.next, None, "a short page without headers is the last");
        assert_eq!(notes_of(&raw, Some(Some(3)), 1).next, Some(3));
    }

    #[test]
    fn paged_answers_carry_the_next_page() {
        let raw = "HTTP/2.0 200 OK\r\nX-Next-Page: 2\r\nX-Page: 1\r\n\r\n[{\"id\": 1}]";
        assert_eq!(split_paged(raw), ("[{\"id\": 1}]", Some(Some(2))));
        assert_eq!(split_paged("HTTP/1.1 200 OK\nx-next-page: \n\n[]"), ("[]", Some(None)));
        assert_eq!(split_paged("HTTP/1.1 200 OK\n\n[]"), ("[]", None));
        assert_eq!(split_paged("[]"), ("[]", None));
    }

    #[test]
    fn label_events_and_linked_merge_requests() {
        let raw: Rows<GlLabelEvent> = serde_json::from_value(json!([
            { "id": 1, "action": "add", "created_at": "1", "user": { "username": "me" }, "label": { "name": "bug" } },
            { "id": 2, "action": "remove", "created_at": "2", "label": null },
            { "id": 3, "action": "rename", "created_at": "3" }
        ]))
        .unwrap();
        assert_eq!(json!(label_events(&raw).iter().map(|e| (e.id.clone(), e.kind.clone(), e.detail.clone())).collect::<Vec<_>>()), json!([["label-1", "labeled", "bug"], ["label-2", "unlabeled", null]]));
        let linked: Rows<GlLinkedMergeRequest> = serde_json::from_value(json!([
            { "iid": 3, "title": "Fix", "web_url": "https://gitlab.com/group/one/-/merge_requests/3", "state": "locked", "draft": true, "references": { "full": "group/one!3" } },
            { "iid": 4, "title": "No reference", "web_url": "u" }
        ]))
        .unwrap();
        assert_eq!(
            json!(linked_merge_requests(&linked, true)),
            json!([{ "repository": "group/one", "number": 3, "title": "Fix", "url": "https://gitlab.com/group/one/-/merge_requests/3", "state": "open", "isDraft": true, "closesIssue": true }])
        );
    }

    #[test]
    fn awards_group_by_reaction() {
        let data: AwardData = serde_json::from_value::<Fields<AwardData>>(json!({ "currentUser": { "username": "me" }, "project": { "mergeRequest": { "notes": {
            "pageInfo": { "hasNextPage": true, "endCursor": "abc" },
            "nodes": [{ "id": "gid://gitlab/Note/1", "awardEmoji": { "nodes": [{ "name": "thumbsup", "user": { "username": "me" } }, { "name": "thumbsup", "user": { "username": "bob" } }] } }]
        } } } }))
        .unwrap()
        .0;
        let (by_note, next) = note_awards(&data);
        assert_eq!(json!(by_note["1"]), json!([{ "content": "thumbs-up", "count": 2, "actors": ["bob"], "viewerHasReacted": true }]));
        assert_eq!(next.as_deref(), Some("abc"));
        assert_eq!(award_name(&IssueReactionContent::Laugh), Some("laughing"));
    }

    #[test]
    fn cursors_keys_and_urls() {
        assert_eq!(parse_page(None), Some(1));
        assert_eq!(parse_page(Some("3")), Some(3));
        for bad in ["0", "x", "12345678", "", "01"] {
            assert_eq!(parse_page(Some(bad)), None, "{bad}");
        }
        assert_eq!(parse_comments_cursor("2:eyJ"), Some((2, Some("eyJ".into()))));
        assert_eq!(parse_comments_cursor("2"), Some((2, None)));
        assert_eq!(parse_comments_cursor("2:"), None);
        assert_eq!(comments_cursor(Some(2), Some("a".into())).as_deref(), Some("2:a"));
        assert_eq!(comments_cursor(None, Some("a".into())), None);
        assert_eq!(issue_number("12"), Some(12));
        assert_eq!(issue_number("012"), None);
        let read = |u: &str, known| parse_issue_url(&url::Url::parse(u).unwrap(), known);
        assert_eq!(read("https://gitlab.com/Group/Sub/One/-/issues/7#note_1", false), Some(("gitlab.com".into(), "group/sub/one".into(), "7".into())));
        assert_eq!(read("https://gitlab.example.com/g/p/-/work_items/3", false).map(|r| r.2), Some("3".into()));
        assert_eq!(read("https://code.example.com/g/p/-/issues/3", false), None);
        assert_eq!(read("https://code.example.com/g/p/-/issues/3", true).map(|r| r.0), Some("code.example.com".into()));
        assert_eq!(read("https://gitlab.com/p/-/issues/3", false), None, "a project sits in a group");
        assert_eq!(read("https://gitlab.com/g/p/-/merge_requests/3", false), None);
    }
}
