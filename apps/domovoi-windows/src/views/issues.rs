use std::rc::Rc;

use domovoi_core::{
    Event,
    view::{ComposerView, IssuesView, PullRequestSheetView, SidebarView, ThreadPageView},
};
use windows_reactor::{
    Border, Button, Callback, ComboBox, HorizontalAlignment, Orientation, ScrollBarVisibility, ScrollViewer, StackPanel, TextBlock, TextBox, TextWrapping,
    Thickness, VerticalAlignment, View,
};

use super::transcript::{Act, Draw};
use crate::{core_link::CoreLink, theme};

fn caption(text: &str) -> TextBlock {
    TextBlock::new().text(text).font_size(theme::CAPTION).opacity(0.65)
}

fn card(lines: Vec<View>) -> View {
    Border::new()
        .padding(Thickness::uniform(12.0))
        .background(theme::code())
        .corner_radius(6.0)
        .content(StackPanel::new().orientation(Orientation::Vertical).spacing(8.0).children(lines))
        .into()
}

fn tell(core: &CoreLink, label: &str, event: Event) -> Button {
    let core = core.clone();
    Button::new()
        .content(TextBlock::new().text(label).font_size(theme::CAPTION + 1.0))
        .on_click(move || core.send(event.clone()))
}

pub fn page(core: &CoreLink, page: &IssuesView, sidebar: &SidebarView, open: &Callback<String>) -> View {
    let mut lines = vec![TextBlock::new().text("Issues").font_size(22.0).into()];
    for project in &sidebar.projects {
        let label = if page.issue_project.as_ref() == Some(&project.id) {
            format!("✓ {}", project.title)
        } else {
            project.title.clone()
        };
        lines.push(
            tell(
                core,
                &label,
                Event::LoadIssues {
                    project_id: project.id.clone(),
                },
            )
            .into(),
        );
    }
    if page.issue_project.is_some() {
        let filters = [("Open", "open"), ("Closed", "closed"), ("All", "all")].map(|(label, filter)| {
            tell(core, label, Event::SetIssueFilter { filter: filter.into() })
                .is_enabled(page.issue_filter != filter)
                .into()
        });
        lines.push(
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(6.0)
                .children(Vec::<View>::from(filters))
                .into(),
        );
    }
    if page.issues_loading {
        lines.push(caption("Loading issues…").into());
    }
    if let Some(notice) = &page.issues_notice {
        lines.push(caption(notice).into());
    }
    for issue in &page.issue_rows {
        let action = if let Some(id) = &issue.linked_thread_id {
            tell(
                core,
                issue.linked_thread_title.as_deref().unwrap_or("Open thread"),
                Event::SelectThread { thread_id: id.clone() },
            )
        } else {
            tell(core, "Start thread", Event::StartIssueThread { index: issue.issue_index })
        };
        lines.push(
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(6.0)
                .children(vec![
                    tell(core, &issue.issue_label, Event::SelectIssue { index: issue.issue_index }).into(),
                    action.into(),
                    url_button(open, "Open issue", &issue.issue_url).into(),
                ])
                .into(),
        );
    }
    if page.more_issues {
        lines.push(tell(core, "Load more", Event::LoadMoreIssues).is_enabled(!page.issues_loading).into());
    }
    if let Some(issue) = &page.issue_detail {
        lines.push(TextBlock::new().text(&issue.issue_body).text_wrapping(TextWrapping::Wrap).into());
        if let Some(url) = &issue.closing_pull_request_url {
            lines.push(url_button(open, "Open closing pull request", url).into());
        }
        for comment in &issue.issue_comments {
            lines.push(TextBlock::new().text(comment).text_wrapping(TextWrapping::Wrap).into());
        }
    }
    ScrollViewer::new()
        .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
        .content(card(lines))
        .into()
}

fn url_button(open: &Callback<String>, label: &str, url: &str) -> Button {
    let (open, url) = (open.clone(), url.to_owned());
    Button::new().content(TextBlock::new().text(label)).on_click(move || open.call(url.clone()))
}

pub fn issue_chip(core: &CoreLink, composer: &ComposerView) -> Option<View> {
    let lines = if let Some(issue) = &composer.issue_chip {
        vec![
            caption(issue).into(),
            tell(core, "Remove issue", Event::DetachIssue).into(),
            tell(
                core,
                if composer.issue_assign_to_me { "✓ Assign to me" } else { "Assign to me" },
                Event::SetAssignIssueToMe {
                    assign: !composer.issue_assign_to_me,
                },
            )
            .into(),
        ]
    } else if let Some(query) = &composer.issue_picker {
        let (link, previous) = (core.clone(), query.clone());
        vec![
            TextBox::new()
                .placeholder_text("Issue URL or #123")
                .text(query)
                .on_text_changed(move |query: Rc<str>| {
                    if &*query != previous {
                        link.send(Event::SetIssuePickerQuery { query: query.to_string() });
                    }
                })
                .into(),
            tell(core, "Attach", Event::AttachIssue).into(),
            tell(core, "Cancel", Event::DetachIssue).into(),
        ]
    } else if composer.issue_available {
        vec![tell(core, "Attach issue", Event::OpenIssuePicker).into()]
    } else {
        return None;
    };
    Some(StackPanel::new().orientation(Orientation::Horizontal).spacing(6.0).children(lines).into())
}

pub fn thread_status(core: &CoreLink, page: &ThreadPageView, draw: &Draw<'_>) -> Option<View> {
    let mut lines = Vec::new();
    if let Some(pr) = &page.pull_request_status {
        let (on_act, url) = (draw.on_act.clone(), pr.pr_status_url.clone());
        lines.push(
            Button::new()
                .content(TextBlock::new().text(&pr.pr_status_label))
                .on_click(move || on_act.call(Act::Open(url.clone())))
                .into(),
        );
        for label in [&pr.pr_status_checks, &pr.pr_status_review].into_iter().flatten() {
            lines.push(caption(label).into());
        }
        if pr.pr_status_has_comments {
            lines.push(tell(core, "Send review to agent", Event::HandReviewCommentsToAgent).into());
        }
        if pr.pr_status_following {
            lines.push(tell(core, "Stop following", Event::StopFollowingPullRequest).into());
        }
    } else if page.can_create_pull_request {
        lines.push(tell(core, "Create pull request", Event::PreparePullRequest).into());
    }
    (!lines.is_empty()).then(|| StackPanel::new().orientation(Orientation::Horizontal).spacing(6.0).children(lines).into())
}

pub fn review_sheet(core: &CoreLink, sheet: &PullRequestSheetView, open: &Callback<String>) -> View {
    let mut lines = vec![TextBlock::new().text("Create pull request").font_size(22.0).into()];
    if let Some(branch) = &sheet.pr_sheet_branch {
        lines.push(caption(&format!("{branch} → {}", sheet.pr_sheet_base)).into());
    }
    if let Some(notice) = &sheet.pr_sheet_notice {
        lines.push(caption(notice).into());
    }
    if let Some(url) = &sheet.pr_sheet_existing_url {
        lines.push(url_button(open, "Open existing pull request", url).into());
    } else {
        for (field, label, previous) in [("title", "Title", &sheet.pr_sheet_title), ("body", "Description", &sheet.pr_sheet_body)] {
            let (core, previous) = (core.clone(), previous.clone());
            lines.push(
                TextBox::new()
                    .placeholder_text(label)
                    .text(&previous)
                    .accepts_return(field == "body")
                    .on_text_changed(move |value: Rc<str>| {
                        if &*value != previous {
                            core.send(Event::SetPullRequestSheetField {
                                field: field.into(),
                                value: value.to_string(),
                            });
                        }
                    })
                    .into(),
            );
        }
        if !sheet.pr_sheet_bases.is_empty() {
            let chosen = sheet.pr_sheet_bases.iter().position(|base| base == &sheet.pr_sheet_base);
            let (link, bases) = (core.clone(), sheet.pr_sheet_bases.clone());
            lines.push(
                ComboBox::new()
                    .automation_name("Base branch")
                    .items_source(bases.clone())
                    .selected_index(chosen)
                    .on_selection_changed(move |index: Option<usize>| {
                        if let Some(base) = index.filter(|index| Some(*index) != chosen).and_then(|index| bases.get(index)) {
                            link.send(Event::SetPullRequestSheetField {
                                field: "base".into(),
                                value: base.clone(),
                            });
                        }
                    })
                    .into(),
            );
        }
        if sheet.pr_sheet_supports_draft {
            lines.push(
                tell(
                    core,
                    if sheet.pr_sheet_draft { "✓ Draft" } else { "Draft" },
                    Event::SetPullRequestSheetField {
                        field: "draft".into(),
                        value: (!sheet.pr_sheet_draft).to_string(),
                    },
                )
                .into(),
            );
        }
        for file in &sheet.pr_sheet_files {
            let label = if file.pr_file_selected {
                format!("✓ {}", file.pr_file_path)
            } else {
                file.pr_file_path.clone()
            };
            lines.push(
                tell(
                    core,
                    &label,
                    Event::TogglePullRequestFile {
                        path: file.pr_file_path.clone(),
                    },
                )
                .into(),
            );
        }
    }
    lines.push(
        StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(6.0)
            .children(vec![
                tell(core, "Close", Event::ClosePullRequestSheet).into(),
                tell(core, if sheet.pr_sheet_loading { "Working…" } else { "Create" }, Event::CreatePullRequest)
                    .is_enabled(sheet.pr_sheet_can_create)
                    .into(),
            ])
            .into(),
    );
    let panel = Border::new()
        .background(theme::card())
        .horizontal_alignment(HorizontalAlignment::Center)
        .vertical_alignment(VerticalAlignment::Center)
        .width(560.0)
        .max_height(560.0)
        .content(
            ScrollViewer::new()
                .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
                .content(card(lines)),
        );
    Border::new().grid_row_span(2).background(theme::scrim()).content(panel).into()
}
