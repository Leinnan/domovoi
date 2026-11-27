use std::collections::HashSet;

use crux_core::{Command, render::render};
use domovoi_model::typed::{
    Actor, CreationSource, PullRequestLink, PullRequestLinkSource, PullRequestPrepareInput,
    PullRequestPrepareResult, PullRequestState, StackedPullRequest, ThreadShell,
    command::{MessageDispatch, ThreadPullRequestWatch},
};
use serde_json::{Value, json};

use crate::{
    app::Event,
    model::{Connection, Model},
    requests,
    rpc::Route,
    update::{Cmd, call, dispatch, uuid},
    view::{PullRequestFileView, PullRequestSheetView, PullRequestStatusView},
};

pub struct Sheet {
    id: String,
    thread: String,
    cwd: String,
    prepared: Option<PullRequestPrepareResult>,
    title: Option<String>,
    body: Option<String>,
    base: Option<String>,
    draft: bool,
    commit_message: Option<String>,
    selected: HashSet<String>,
    loading: bool,
    notice: Option<String>,
    action: Option<String>,
}

fn active_link(shell: &ThreadShell) -> Option<&PullRequestLink> {
    let links = || {
        shell
            .pull_requests
            .iter()
            .flatten()
            .filter(|link| link.source != PullRequestLinkSource::StackDismissed)
    };
    links()
        .find(|link| link.watch.is_some())
        .or_else(|| {
            links().find(|link| {
                link.snapshot
                    .as_ref()
                    .is_some_and(|snapshot| snapshot.state == PullRequestState::Open)
            })
        })
        .or_else(|| links().next())
}

pub fn available(model: &Model) -> bool {
    model.connection == Connection::Connected
        && model.selected_thread.as_deref().is_some_and(|id| {
            model
                .thread_shell(id)
                .is_some_and(|shell| shell.archived_at.is_none())
                && model.thread_cwd(id).is_some()
        })
}

fn prepare(model: &mut Model, generate: bool) -> Cmd {
    let Some(sheet) = model.pages.pull_request_sheet.as_ref() else {
        return Command::done();
    };
    let request = requests::git_prepare_pull_request(&PullRequestPrepareInput {
        thread_id: Some(sheet.thread.as_str().into()),
        cwd: sheet.cwd.clone(),
        base_branch: sheet.base.clone(),
        generate: Some(generate),
    });
    call(
        model,
        request,
        Route::PreparePullRequest(sheet.id.clone(), generate),
    )
    .and(render())
}

pub fn update(model: &mut Model, event: Event) -> Cmd {
    let Some(thread) = model.selected_thread.clone() else {
        return Command::done();
    };
    match event {
        Event::PreparePullRequest if available(model) => {
            if model
                .pages
                .pull_request_sheet
                .as_ref()
                .is_some_and(|sheet| sheet.action.is_some())
            {
                return Command::done();
            }
            let Some(cwd) = model.thread_cwd(&thread) else {
                return Command::done();
            };
            model.pages.pull_request_sheet = Some(Sheet {
                id: uuid(),
                thread,
                cwd,
                prepared: None,
                title: None,
                body: None,
                base: None,
                draft: false,
                commit_message: None,
                selected: HashSet::new(),
                loading: true,
                notice: None,
                action: None,
            });
            prepare(model, false)
        }
        Event::ClosePullRequestSheet => {
            if model
                .pages
                .pull_request_sheet
                .as_ref()
                .is_some_and(|sheet| sheet.action.is_none())
            {
                model.pages.pull_request_sheet = None;
            }
            render()
        }
        Event::StopFollowingPullRequest => {
            let Some(link) = model
                .thread_shell(&thread)
                .and_then(active_link)
                .filter(|link| link.watch.is_some())
            else {
                return Command::done();
            };
            let command = ThreadPullRequestWatch {
                command_id: uuid().into(),
                thread_id: thread.into(),
                host: link.host.clone(),
                repository: link.repository.clone(),
                number: link.number,
                watching: false,
                until: None,
                checks_pending: None,
                link: None,
                policy: None,
            };
            dispatch(model, command, "Stop following pull request")
        }
        Event::HandReviewCommentsToAgent => {
            let Some(link) = model.thread_shell(&thread).and_then(active_link) else {
                return Command::done();
            };
            let text = format!(
                "Read the review comments for pull request #{} in {} on {} using the reviews section of pull_request_read. Address the feedback you agree with in the code. Leave replies and review-thread resolution to me.",
                link.number, link.repository, link.host
            );
            let id = uuid();
            let message_id = uuid();
            let command = MessageDispatch::new(
                id.as_str(),
                thread.as_str(),
                message_id.as_str(),
                text.as_str(),
                Actor::User,
                CreationSource::Web,
            );
            dispatch(model, command, "Hand review comments to agent")
        }
        event => {
            let Some(sheet) = model
                .pages
                .pull_request_sheet
                .as_mut()
                .filter(|sheet| sheet.thread == thread && sheet.action.is_none())
            else {
                return Command::done();
            };
            match event {
                Event::SetPullRequestSheetField { field, value } => match field.as_str() {
                    "title" => sheet.title = Some(value),
                    "body" => sheet.body = Some(value),
                    "base"
                        if sheet
                            .prepared
                            .as_ref()
                            .is_some_and(|pr| pr.base_branches.contains(&value)) =>
                    {
                        sheet.base = Some(value);
                        sheet.id = uuid();
                        sheet.loading = true;
                        return prepare(model, true);
                    }
                    "draft" => sheet.draft = value == "true",
                    _ => {}
                },
                Event::TogglePullRequestFile { path }
                    if sheet
                        .prepared
                        .as_ref()
                        .is_some_and(|pr| pr.dirty_files.iter().any(|file| file.path == path)) =>
                {
                    if !sheet.selected.remove(&path) {
                        sheet.selected.insert(path);
                    }
                }
                Event::CreatePullRequest
                    if model.connection == Connection::Connected && !model.panel.git.busy =>
                {
                    let Some(pr) = sheet.prepared.as_ref().filter(|pr| can_create(sheet, pr))
                    else {
                        return Command::done();
                    };
                    let action_id = uuid();
                    let paths: Vec<_> = pr
                        .dirty_files
                        .iter()
                        .filter(|file| sheet.selected.contains(&file.path))
                        .map(|file| &file.path)
                        .collect();
                    let pull_request = StackedPullRequest {
                        title: title(sheet, pr).trim().to_owned(),
                        body: body(sheet, pr),
                        draft: Some(sheet.draft && pr.supports_draft),
                        base_branch: Some(pr.base_branch.clone()),
                    };
                    let request = requests::git_create_reviewed_pull_request(json!({
                        "actionId": action_id, "cwd": sheet.cwd, "threadId": sheet.thread,
                        "action": if paths.is_empty() { "create_pr" } else { "commit_push_pr" },
                        "featureBranch": pr.on_default_branch,
                        "filePaths": paths, "pullRequest": pull_request,
                        "commitMessage": sheet.commit_message.as_deref().unwrap_or(&pull_request.title),
                    }));
                    sheet.action = Some(action_id.clone());
                    sheet.notice = None;
                    model.panel.git.busy = true;
                    model.panel.git.progress.clear();
                    return call(model, request, Route::GitAction(action_id)).and(render());
                }
                _ => {}
            }
            render()
        }
    }
}

fn title<'a>(sheet: &'a Sheet, pr: &'a PullRequestPrepareResult) -> &'a str {
    sheet.title.as_deref().or(pr.title.as_deref()).unwrap_or("")
}

fn body(sheet: &Sheet, pr: &PullRequestPrepareResult) -> String {
    sheet
        .body
        .clone()
        .or_else(|| pr.body.clone())
        .unwrap_or_else(|| {
            pr.closing
                .iter()
                .map(|closing| closing.reference.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        })
}

fn can_create(sheet: &Sheet, pr: &PullRequestPrepareResult) -> bool {
    sheet.action.is_none()
        && !sheet.loading
        && pr.unavailable.is_none()
        && pr.existing.is_none()
        && (if pr.on_default_branch {
            !sheet.selected.is_empty()
        } else {
            !sheet.selected.is_empty() || pr.commits_ahead_of_base > 0
        })
        && !title(sheet, pr).trim().is_empty()
}

pub fn reply(
    model: &mut Model,
    id: &str,
    generate: bool,
    result: Result<Value, String>,
) -> Vec<Cmd> {
    let worktree = model
        .pages
        .pull_request_sheet
        .as_ref()
        .and_then(|sheet| model.thread_shell(&sheet.thread))
        .is_some_and(|shell| shell.worktree_path.is_some());
    let Some(sheet) = model
        .pages
        .pull_request_sheet
        .as_mut()
        .filter(|sheet| sheet.id == id)
    else {
        return vec![];
    };
    sheet.loading = false;
    match result.and_then(|value| {
        serde_json::from_value::<PullRequestPrepareResult>(value).map_err(|err| err.to_string())
    }) {
        Ok(pr) => {
            if !generate && worktree {
                sheet
                    .selected
                    .extend(pr.dirty_files.iter().map(|file| file.path.clone()));
            }
            let generate_next = !generate && pr.unavailable.is_none() && pr.existing.is_none();
            sheet.notice = pr
                .unavailable
                .as_ref()
                .map(|reason| reason.detail.clone())
                .or(pr.generation_error.clone());
            sheet
                .selected
                .retain(|path| pr.dirty_files.iter().any(|file| &file.path == path));
            sheet.prepared = Some(pr);
            if generate_next {
                sheet.loading = true;
                return vec![prepare(model, true)];
            }
        }
        Err(error) => sheet.notice = Some(error),
    }
    vec![render()]
}

pub fn finish(model: &mut Model, id: &str, error: Option<String>) {
    let Some(sheet) = model
        .pages
        .pull_request_sheet
        .as_mut()
        .filter(|sheet| sheet.action.as_deref() == Some(id))
    else {
        return;
    };
    if let Some(error) = error {
        sheet.action = None;
        sheet.notice = Some(error);
    } else {
        model.pages.pull_request_sheet = None;
    }
}

pub fn view(model: &Model, thread: &str) -> Option<PullRequestSheetView> {
    let sheet = model
        .pages
        .pull_request_sheet
        .as_ref()
        .filter(|sheet| sheet.thread == thread)?;
    let mut view = PullRequestSheetView {
        pr_sheet_loading: sheet.loading || sheet.action.is_some(),
        pr_sheet_notice: sheet.notice.clone(),
        ..Default::default()
    };
    if let Some(pr) = &sheet.prepared {
        view.pr_sheet_title = title(sheet, pr).into();
        view.pr_sheet_body = body(sheet, pr);
        view.pr_sheet_draft = sheet.draft;
        view.pr_sheet_supports_draft = pr.supports_draft;
        view.pr_sheet_base = sheet.base.clone().unwrap_or_else(|| pr.base_branch.clone());
        view.pr_sheet_bases = pr.base_branches.clone();
        view.pr_sheet_branch = pr.new_branch.clone().or_else(|| pr.branch.clone());
        view.pr_sheet_existing_url = pr.existing.as_ref().map(|existing| existing.url.clone());
        view.pr_sheet_files = pr
            .dirty_files
            .iter()
            .map(|file| PullRequestFileView {
                pr_file_path: file.path.clone(),
                pr_file_selected: sheet.selected.contains(&file.path),
            })
            .collect();
        view.pr_sheet_can_create = model.connection == Connection::Connected
            && !model.panel.git.busy
            && can_create(sheet, pr);
    }
    Some(view)
}

pub fn status(shell: &ThreadShell) -> Option<PullRequestStatusView> {
    let link = active_link(shell)?;
    let snapshot = link.snapshot.as_ref();
    Some(PullRequestStatusView {
        pr_status_label: snapshot.map_or_else(
            || format!("#{}", link.number),
            |s| format!("#{} · {} · {}", link.number, s.state.as_str(), s.title),
        ),
        pr_status_url: link.url.clone(),
        pr_status_checks: snapshot
            .and_then(|s| s.checks_state.as_ref()?.as_ref())
            .map(|s| s.as_str().into()),
        pr_status_review: snapshot
            .and_then(|s| s.review_decision.as_ref()?.as_ref())
            .map(|s| s.as_str().into()),
        pr_status_following: link.watch.is_some(),
        pr_status_has_comments: snapshot
            .and_then(|s| s.unresolved_review_threads)
            .is_some_and(|n| n > 0),
    })
}

pub fn disconnected(model: &mut Model) {
    if let Some(sheet) = &mut model.pages.pull_request_sheet
        && (sheet.action.is_some() || sheet.loading)
    {
        let creating = sheet.action.take().is_some();
        sheet.loading = false;
        sheet.prepared = None;
        sheet.notice = Some(if creating {
            "The connection dropped during creation. Reopen the sheet to check the branch before retrying."
        } else {
            "The connection dropped while preparing the pull request. Reopen the sheet to retry."
        }.into());
    }
}

pub fn open_from_git(model: &mut Model, message: &str) -> Cmd {
    let cmd = update(model, Event::PreparePullRequest);
    if let Some(sheet) = model.pages.pull_request_sheet.as_mut().filter(|sheet| sheet.action.is_none()) {
        let message = message.trim();
        sheet.commit_message = (!message.is_empty()).then(|| message.to_owned());
    }
    cmd
}
