use crux_core::{Command, render::render};
use domovoi_model::typed::{
    IssueDetail, IssueListResult, IssueRef, IssueResolveResult, IssueSummary, PullRequestState,
    ThreadShell,
};
use serde_json::{Value, json};

use crate::{
    app::Event,
    model::{Connection, Model},
    requests::issue_request,
    rpc::Route,
    update::{Cmd, call, select_thread, start_new_thread},
    view::{IssueDetailView, IssueRowView, IssuesView},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IssueDraft {
    pub issue: IssueRef,
    pub label: String,
    pub assign: bool,
    pub workspace: Option<String>,
}

#[derive(Default)]
pub struct Issues {
    project: Option<String>,
    filter: String,
    list: IssueListResult,
    detail: Option<IssueDetail>,
    list_revision: u32,
    detail_revision: u32,
    loading: bool,
    notice: Option<String>,
}

pub fn draft(model: &Model) -> Option<&IssueDraft> {
    model
        .selected_thread
        .is_none()
        .then(|| model.composer.issue_drafts.get(&model.draft_key()))
        .flatten()
}

fn issue_ref(project: &str, summary: &IssueSummary) -> IssueRef {
    IssueRef {
        project_id: project.into(),
        tracker: summary.tracker.clone(),
        host: Some(summary.host.clone()),
        scope: summary.scope.clone(),
        key: summary.key.clone(),
    }
}

fn label(summary: &IssueSummary) -> String {
    format!("{} {}", summary.display_key, summary.title)
}

pub fn linked_thread<'a>(model: &'a Model, issue: &IssueRef) -> Option<&'a ThreadShell> {
    let key = domovoi_model::typed::ThreadIssueKey {
        tracker: issue.tracker.clone(),
        host: issue.host.clone()?,
        scope: issue.scope.clone(),
        key: issue.key.clone(),
    };
    crate::sidebar::live_threads(model).find(|thread| {
        thread
            .issues
            .iter()
            .flatten()
            .any(|link| link.is_focus() && link.key.same_issue(&key))
    })
}

fn attach(model: &mut Model, issue: IssueRef, summary: &IssueSummary) -> Cmd {
    if let Some(id) = linked_thread(model, &issue).map(|thread| thread.id.as_str().to_owned()) {
        model.composer.issue_picker = None;
        return select_thread(model, id);
    }
    let key = model.draft_key();
    model.composer.issue_drafts.insert(
        key,
        IssueDraft {
            issue,
            label: label(summary),
            assign: false,
            workspace: None,
        },
    );
    model.composer.issue_picker = None;
    model.composer.existing_worktree = None;
    render()
}

fn list(model: &mut Model, append: bool) -> Cmd {
    let page = &mut model.pages.issues;
    let Some(project) = page.project.as_ref() else {
        return render();
    };
    page.list_revision += 1;
    if !append {
        page.list = IssueListResult::default();
        page.detail = None;
        page.detail_revision += 1;
    }
    let payload = json!({
        "projectId": project,
        "state": if page.filter.is_empty() { "open" } else { &page.filter },
        "limit": 40,
        "cursors": append.then_some(&page.list.next_cursors),
    });
    let route = Route::Issues(page.list_revision, append);
    page.loading = model.connection == Connection::Connected;
    page.notice = (!page.loading).then(|| "Connect to the server to load issues.".into());
    call(model, issue_request("issues.list", payload), route).and(render())
}

pub fn update(model: &mut Model, event: Event) -> Cmd {
    match event {
        Event::ConnectIssueTracker { token } => {
            if token.as_ref().is_some_and(|token| token.trim().is_empty())
                || model.rpc.has(&Route::IssueTrackerMutation)
            {
                return Command::done();
            }
            call(
                model,
                issue_request(
                    "issueTrackers.connect",
                    json!({ "tracker": "linear", "token": token }),
                ),
                Route::IssueTrackerMutation,
            )
        }
        Event::BindProjectIssueTracker { project_id, scope } => {
            if model.project(&project_id).is_none()
                || model.rpc.has(&Route::IssueTrackerMutation)
                || scope.as_ref().is_some_and(|key| {
                    !linear_settings(model).is_some_and(|linear| {
                        linear
                            .linear_teams
                            .iter()
                            .any(|scope| &scope.linear_team_key == key)
                    })
                })
            {
                return Command::done();
            }
            let binding = scope.map(|scope| json!({ "tracker": "linear", "scope": scope }));
            call(
                model,
                issue_request(
                    "issueTrackers.bindProject",
                    json!({ "projectId": project_id, "binding": binding }),
                ),
                Route::IssueTrackerMutation,
            )
        }
        Event::LoadIssues { project_id } => {
            model.route = "issues".into();
            model.pages.issues.project = Some(project_id);
            list(model, false)
        }
        Event::SetIssueFilter { filter }
            if matches!(filter.as_str(), "open" | "closed" | "all") =>
        {
            model.pages.issues.filter = filter;
            list(model, false)
        }
        Event::LoadMoreIssues
            if !model.pages.issues.loading && !model.pages.issues.list.next_cursors.is_empty() =>
        {
            list(model, true)
        }
        Event::SelectIssue { index } => {
            let page = &mut model.pages.issues;
            let Some(entry) = page.list.entries.get(index as usize) else {
                return Command::done();
            };
            let issue = issue_ref(entry.project_id.as_str(), &entry.summary);
            page.detail = None;
            page.detail_revision += 1;
            let route = Route::IssueDetail(page.detail_revision);
            call(model, issue_request("issues.detail", json!(issue)), route).and(render())
        }
        Event::StartIssueThread { index } if !model.composer.sending => {
            let Some(entry) = model.pages.issues.list.entries.get(index as usize).cloned() else {
                return Command::done();
            };
            let project = entry.project_id.as_str();
            let cmd = start_new_thread(model, Some(project.to_owned()));
            model.composer.issue_handoff = model.composer.issue_handoff.wrapping_add(1);
            cmd.and(attach(
                model,
                issue_ref(project, &entry.summary),
                &entry.summary,
            ))
        }
        Event::OpenIssuePicker if model.route == "new-thread" => {
            model.composer.issue_picker = Some(String::new());
            render()
        }
        Event::SetIssuePickerQuery { query } if model.composer.issue_picker.is_some() => {
            model.composer.issue_picker = Some(query);
            render()
        }
        Event::AttachIssue if model.route == "new-thread" && !model.composer.sending => {
            let Some(query) = model
                .composer
                .issue_picker
                .as_deref()
                .map(str::trim)
                .filter(|query| !query.is_empty())
                .map(str::to_owned)
            else {
                return Command::done();
            };
            let Some(project) = model.selected_project.clone() else {
                return Command::done();
            };
            call(
                model,
                issue_request(
                    "issues.resolve",
                    json!({ "projectId": project, "reference": query }),
                ),
                Route::IssueResolve(model.draft_key(), query),
            )
        }
        Event::DetachIssue if !model.composer.sending => {
            model.composer.issue_picker = None;
            model.composer.issue_drafts.remove(&model.draft_key());
            render()
        }
        Event::SetAssignIssueToMe { assign } => {
            if let Some(draft) = model.composer.issue_drafts.get_mut(&model.draft_key()) {
                draft.assign = assign;
            }
            render()
        }
        _ => Command::done(),
    }
}

pub fn reply(model: &mut Model, route: Route, result: Result<Value, String>) -> Vec<Cmd> {
    let page = &mut model.pages.issues;
    match route {
        Route::Issues(revision, append) if revision == page.list_revision => {
            page.loading = false;
            match result.and_then(|value| {
                serde_json::from_value::<IssueListResult>(value).map_err(|err| err.to_string())
            }) {
                Ok(mut list) => {
                    page.notice = list
                        .errors
                        .first()
                        .map(|error| error.message.clone())
                        .or_else(|| {
                            list.trackers
                                .iter()
                                .find(|tracker| !tracker.configured)
                                .and_then(|tracker| tracker.detail.clone())
                        });
                    if append {
                        let mut more = std::mem::take(&mut list.entries);
                        more.retain(|new| {
                            !page.list.entries.iter().any(|entry| {
                                new.project_id == entry.project_id
                                    && new.summary.host == entry.summary.host
                                    && new.summary.scope == entry.summary.scope
                                    && new.summary.key == entry.summary.key
                            })
                        });
                        list.entries = std::mem::take(&mut page.list.entries);
                        list.entries.extend(more);
                    }
                    page.list = list;
                }
                Err(error) => page.notice = Some(error),
            }
        }
        Route::IssueDetail(revision) if revision == page.detail_revision => {
            match result
                .and_then(|value| serde_json::from_value(value).map_err(|err| err.to_string()))
            {
                Ok(detail) => page.detail = Some(detail),
                Err(error) => page.notice = Some(error),
            }
        }
        Route::IssueResolve(key, reference)
            if model.route == "new-thread"
                && !model.composer.sending
                && key == model.draft_key()
                && model.composer.issue_picker.as_deref().map(str::trim)
                    == Some(reference.as_str()) =>
        {
            match result.and_then(|value| {
                serde_json::from_value::<IssueResolveResult>(value).map_err(|err| err.to_string())
            }) {
                Ok(resolved) => {
                    if issue_reference(&model.draft()) == Some(reference.as_str()) {
                        model.composer.drafts.remove(&key);
                        model.composer.draft_revision += 1;
                        model.drafts_dirty = true;
                    }
                    return vec![attach(model, resolved.issue, &resolved.summary)];
                }
                Err(error) => model.toast("error", error),
            }
        }
        _ => {}
    }
    vec![render()]
}

pub fn view(model: &Model) -> IssuesView {
    let page = &model.pages.issues;
    IssuesView {
        issue_project: page.project.clone(),
        issue_filter: if page.filter.is_empty() {
            "open".into()
        } else {
            page.filter.clone()
        },
        issue_rows: page
            .list
            .entries
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                let thread =
                    linked_thread(model, &issue_ref(entry.project_id.as_str(), &entry.summary));
                IssueRowView {
                    issue_index: index as u32,
                    issue_label: label(&entry.summary),
                    issue_url: entry.summary.url.clone(),
                    linked_thread_id: thread.map(|thread| thread.id.as_str().to_owned()),
                    linked_thread_title: thread.map(|thread| thread.title.clone()),
                }
            })
            .collect(),
        issue_detail: page.detail.as_ref().map(|detail| IssueDetailView {
            issue_body: detail.body.clone(),
            issue_comments: detail
                .comments
                .iter()
                .map(|comment| comment.body.clone())
                .collect(),
            closing_pull_request_url: detail
                .linked_pull_requests
                .iter()
                .find(|pr| pr.closes_issue && pr.state == PullRequestState::Open)
                .map(|pr| pr.url.clone()),
        }),
        issues_loading: page.loading,
        issues_notice: page.notice.clone(),
        more_issues: !page.list.next_cursors.is_empty(),
    }
}

pub fn available(model: &Model) -> bool {
    let Some(project) = model
        .selected_project
        .as_deref()
        .and_then(|id| model.project(id))
    else {
        return false;
    };
    model
        .pages
        .issue_trackers
        .bindings
        .contains_key(project.id.as_str())
        || project
            .repository_identity
            .as_ref()
            .and_then(Option::as_ref)
            .and_then(|identity| identity.provider.as_deref())
            .is_some_and(|provider| matches!(provider, "github" | "gitlab"))
}

pub fn load_trackers(model: &mut Model) -> Cmd {
    if model.rpc.has(&Route::IssueTrackers) {
        return Command::done();
    }
    call(
        model,
        issue_request("issueTrackers.list", json!({})),
        Route::IssueTrackers,
    )
}

pub fn linear_settings(model: &Model) -> Option<crate::view::LinearSettingsView> {
    use crate::view::{LinearSettingsView, LinearTeamView};
    let tracker = model
        .pages
        .issue_trackers
        .trackers
        .iter()
        .find(|tracker| tracker.descriptor.tracker == "linear")?;
    Some(LinearSettingsView {
        linear_account: tracker.account.clone(),
        linear_notice: tracker.detail.clone(),
        linear_connected: tracker.connected,
        linear_teams: tracker
            .scopes
            .iter()
            .map(|scope| LinearTeamView {
                linear_team_key: scope.key.clone(),
                linear_team_name: scope.name.clone(),
            })
            .collect(),
    })
}

/// A whole draft that identifies an issue. A number by itself could be an ordinary prompt.
pub fn issue_reference(text: &str) -> Option<&str> {
    let text = text.trim();
    if text.is_empty() || text.chars().any(char::is_whitespace) {
        return None;
    }
    let numbered = text
        .strip_prefix('#')
        .is_some_and(|number| !number.is_empty() && number.bytes().all(|c| c.is_ascii_digit()));
    let linear = text.rsplit_once('-').is_some_and(|(team, number)| {
        !team.is_empty()
            && team
                .bytes()
                .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
            && !number.is_empty()
            && number.bytes().all(|c| c.is_ascii_digit())
    });
    let url = text.starts_with("https://")
        && ["/issues/", "/issue/"]
            .iter()
            .any(|segment| text.contains(segment));
    (numbered || linear || url).then_some(text)
}

pub fn disconnected(model: &mut Model) {
    if model.pages.issues.loading {
        model.pages.issues.loading = false;
        model.pages.issues.notice = Some("The connection dropped while loading issues.".into());
    }
}
