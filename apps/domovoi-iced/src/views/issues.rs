use domovoi_core::{
    Event,
    view::{PullRequestSheetView, ThreadPageView},
};
use iced::{
    Element, Length,
    widget::{Column, column, row, scrollable, text, text_editor},
};

use crate::{
    app::{App, Message},
    ui::{tokens::*, widgets as w},
};

fn core(event: Event) -> Message {
    Message::Core(Box::new(event))
}

pub fn view(app: &App) -> Element<'_, Message> {
    let page = &app.vm.issues;
    let mut list = column![text("Issues").size(TEXT_TITLE)].spacing(S).padding(L);
    let mut projects = row![].spacing(XS);
    for project in &app.vm.sidebar.projects {
        projects = projects.push(w::small_button(
            &project.title,
            w::secondary_style,
            Some(core(Event::LoadIssues {
                project_id: project.id.clone(),
            })),
        ));
    }
    list = list.push(projects.wrap());
    if page.issue_project.is_some() {
        list = list.push(w::segmented(
            &[("open", None, "Open"), ("closed", None, "Closed"), ("all", None, "All")],
            page.issue_filter.as_str(),
            |filter| core(Event::SetIssueFilter { filter: filter.into() }),
        ));
    }
    if let Some(notice) = &page.issues_notice {
        list = list.push(w::muted(notice, TEXT_SMALL));
    }
    if page.issues_loading {
        list = list.push(w::muted("Loading issues…", TEXT_SMALL));
    } else if page.issue_project.is_some() && page.issue_rows.is_empty() {
        list = list.push(w::muted("No issues found.", TEXT_SMALL));
    }
    for issue in &page.issue_rows {
        let action = match &issue.linked_thread_id {
            Some(id) => w::small_button(
                issue.linked_thread_title.as_deref().unwrap_or("Open thread"),
                w::secondary_style,
                Some(core(Event::SelectThread { thread_id: id.clone() })),
            ),
            None => w::small_button(
                "Start thread",
                w::primary_style,
                Some(core(Event::StartIssueThread { index: issue.issue_index })),
            ),
        };
        list = list.push(
            row![
                w::small_button(&issue.issue_label, w::ghost_style, Some(core(Event::SelectIssue { index: issue.issue_index }))),
                action,
                w::small_button("Open issue", w::ghost_style, Some(Message::OpenUrl(issue.issue_url.clone()))),
            ]
            .spacing(XS)
            .wrap(),
        );
    }
    if page.more_issues {
        list = list.push(w::small_button(
            "Load more",
            w::secondary_style,
            (!page.issues_loading).then(|| core(Event::LoadMoreIssues)),
        ));
    }
    let mut detail = Column::new().spacing(S).padding(L);
    if let Some(issue) = &page.issue_detail {
        detail = detail.push(text(&issue.issue_body).size(TEXT_UI));
        if let Some(url) = &issue.closing_pull_request_url {
            detail = detail.push(w::small_button(
                "Open closing pull request",
                w::secondary_style,
                Some(Message::OpenUrl(url.clone())),
            ));
        }
        for comment in &issue.issue_comments {
            detail = detail.push(w::divider()).push(text(comment).size(TEXT_UI));
        }
    }
    row![
        scrollable(list).width(Length::FillPortion(3)),
        w::vdivider(),
        scrollable(detail).width(Length::FillPortion(2))
    ]
    .height(Length::Fill)
    .into()
}

pub fn chip(app: &App) -> Element<'_, Message> {
    let composer = &app.vm.composer;
    let mut col = Column::new().spacing(XS);
    if let Some(issue) = &composer.issue_chip {
        col = col.push(
            row![
                w::badge_capsule(issue, None),
                w::small_button("Remove issue", w::ghost_style, Some(core(Event::DetachIssue))),
                iced::widget::checkbox(composer.issue_assign_to_me)
                    .label("Assign to me")
                    .on_toggle(|assign| core(Event::SetAssignIssueToMe { assign })),
            ]
            .spacing(XS)
            .wrap(),
        );
    } else if let Some(query) = &composer.issue_picker {
        col = col.push(
            row![
                w::input("Issue URL or #123", query)
                    .on_input(|query| core(Event::SetIssuePickerQuery { query }))
                    .on_submit(core(Event::AttachIssue)),
                w::small_button("Attach", w::primary_style, Some(core(Event::AttachIssue))),
                w::small_button("Cancel", w::ghost_style, Some(core(Event::DetachIssue))),
            ]
            .spacing(XS),
        );
    } else if composer.issue_available {
        col = col.push(w::small_button("Attach issue", w::secondary_style, Some(core(Event::OpenIssuePicker))));
    }
    col.into()
}

pub fn status(thread: &ThreadPageView) -> Element<'_, Message> {
    let mut controls = row![].spacing(XS);
    if let Some(pr) = &thread.pull_request_status {
        controls = controls.push(w::small_button(
            &pr.pr_status_label,
            w::secondary_style,
            Some(Message::OpenUrl(pr.pr_status_url.clone())),
        ));
        for label in [&pr.pr_status_checks, &pr.pr_status_review].into_iter().flatten() {
            controls = controls.push(w::badge_capsule(label, None));
        }
        if pr.pr_status_has_comments {
            controls = controls.push(w::small_button(
                "Send review to agent",
                w::ghost_style,
                Some(core(Event::HandReviewCommentsToAgent)),
            ));
        }
        if pr.pr_status_following {
            controls = controls.push(w::small_button("Stop following", w::ghost_style, Some(core(Event::StopFollowingPullRequest))));
        }
    } else if thread.can_create_pull_request {
        controls = controls.push(w::small_button(
            "Create pull request",
            w::secondary_style,
            Some(core(Event::PreparePullRequest)),
        ));
    }
    controls.wrap().into()
}

pub fn sheet<'a>(app: &'a App, sheet: &'a PullRequestSheetView) -> Element<'a, Message> {
    let field = |field: &str, value| core(Event::SetPullRequestSheetField { field: field.into(), value });
    let mut form = column![text("Create pull request").size(TEXT_TITLE)].spacing(S);
    if let Some(branch) = &sheet.pr_sheet_branch {
        form = form.push(w::muted(format!("{branch} → {}", sheet.pr_sheet_base), TEXT_SMALL));
    }
    if let Some(notice) = &sheet.pr_sheet_notice {
        form = form.push(w::muted(notice, TEXT_SMALL));
    }
    if let Some(url) = &sheet.pr_sheet_existing_url {
        form = form.push(w::small_button(
            "Open existing pull request",
            w::primary_style,
            Some(Message::OpenUrl(url.clone())),
        ));
    } else {
        form = form.push(w::input("Title", &sheet.pr_sheet_title).on_input(move |value| field("title", value)));
        form = form.push(
            text_editor(&app.pr_body)
                .placeholder("Description")
                .height(130)
                .on_action(Message::EditPullRequestBody),
        );
        if !sheet.pr_sheet_bases.is_empty() {
            form = form.push(iced::widget::pick_list(
                sheet.pr_sheet_bases.as_slice(),
                Some(&sheet.pr_sheet_base),
                move |value| field("base", value),
            ));
        }
        if sheet.pr_sheet_supports_draft {
            form = form.push(
                iced::widget::checkbox(sheet.pr_sheet_draft)
                    .label("Draft")
                    .on_toggle(move |draft| field("draft", draft.to_string())),
            );
        }
        for file in &sheet.pr_sheet_files {
            form = form.push(iced::widget::checkbox(file.pr_file_selected).label(&file.pr_file_path).on_toggle(|_| {
                core(Event::TogglePullRequestFile {
                    path: file.pr_file_path.clone(),
                })
            }));
        }
    }
    let footer = row![
        w::small_button("Close", w::ghost_style, Some(core(Event::ClosePullRequestSheet))),
        w::small_button(
            if sheet.pr_sheet_loading { "Working…" } else { "Create" },
            w::primary_style,
            sheet.pr_sheet_can_create.then(|| core(Event::CreatePullRequest))
        ),
    ]
    .spacing(S);
    w::modal(
        column![scrollable(form).height(Length::Fill), footer].spacing(M).width(580).height(500),
        core(Event::ClosePullRequestSheet),
        w::Placement::Center,
    )
}
