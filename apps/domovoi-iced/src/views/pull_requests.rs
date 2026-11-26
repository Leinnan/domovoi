//! Pull Requests page: split master-detail view with checks, markdown description,
//! worktree thread launch, merge/close/reopen/draft actions, reviews, and comments.

use domovoi_core::Event;
use iced::widget::{Column, column, container, row, text};
use iced::{Alignment, Element, Length};

use crate::app::{App, Message};
use crate::ui::fonts;
use crate::ui::icons::Icon;
use crate::ui::tokens::*;
use crate::ui::widgets as w;
use crate::views::markdown;

fn core(e: Event) -> Message {
    Message::Core(Box::new(e))
}

pub fn view(app: &App) -> Element<'_, Message> {
    let prs = &app.vm.pull_requests;

    let top = container(
        row![
            w::themed_icon(Icon::GitPullRequest, ICON, |c| c.accent),
            text("Pull Requests").size(TEXT_BODY).font(fonts::SEMIBOLD).width(Length::Fill),
            w::small_button("Refresh", w::secondary_style, (!prs.loading).then(|| core(Event::LoadPullRequests))),
        ]
        .spacing(S)
        .align_y(Alignment::Center),
    )
    .height(SUB_BAR_HEIGHT)
    .align_y(Alignment::Center)
    .padding([0.0, CONTENT_GUTTER])
    .width(Length::Fill)
    .style(w::surface);

    // Master list
    let mut left = Column::new().spacing(XS).padding(S);
    if let Some(notice) = &prs.notice {
        left = left.push(container(w::muted(notice, TEXT_SMALL)).padding(S).width(Length::Fill).style(w::banner_style(|c| c.info)));
    }
    if let Some(err) = &prs.error {
        left = left.push(container(w::colored(err, TEXT_SMALL, |c| c.danger)).padding(S).width(Length::Fill).style(w::banner_style(|c| c.danger)));
    }
    if prs.loading && prs.items.is_empty() {
        left = left.push(w::muted("Loading pull requests…", TEXT_UI));
    } else if prs.items.is_empty() {
        left = left.push(w::faint("No open pull requests found.", TEXT_UI));
    } else {
        let selected_key = prs.selected.as_ref().map(|d| d.key.as_str());
        for pr in &prs.items {
            let selected = selected_key == Some(pr.key.as_str());
            let state_tone: w::Tone = match pr.state.as_str() {
                "merged" => |c| c.violet,
                "closed" => |c| c.danger,
                _ if pr.draft => |c| c.muted,
                _ => |c| c.success,
            };
            let mut item_col = Column::new()
                .spacing(XXS)
                .width(Length::Fill)
                .push(
                    row![
                        text(format!("#{} {}", pr.number, pr.title)).size(TEXT_UI).font(fonts::MEDIUM).width(Length::Fill),
                        w::badge_capsule(if pr.draft { "Draft" } else { pr.state.as_str() }, Some(state_tone)),
                    ]
                    .spacing(XS)
                    .align_y(Alignment::Center),
                )
                .push(w::muted(format!("{} · {} · {}", pr.repository, pr.author, pr.age), TEXT_CAPTION));
            if !pr.checks.is_empty() {
                item_col = item_col.push(w::faint(&pr.checks, TEXT_CAPTION));
            }
            left = left.push(
                w::list_row(item_col, selected, true)
                    .on_press(core(Event::SelectPullRequest { key: pr.key.clone() })),
            );
        }
    }

    // Detail pane
    let right: Element<'_, Message> = if let Some(d) = &prs.selected {
        let mut col = Column::new().spacing(L).padding(CONTENT_GUTTER);
        col = col.push(
            row![
                column![
                    text(format!("#{} {}", d.number, d.title)).size(TEXT_TITLE).font(fonts::SEMIBOLD),
                    row![
                        w::badge_capsule(if d.draft { "Draft" } else { d.state.as_str() }, Some(|c| c.info)),
                        w::muted(format!("{} → {}", d.head, d.base), TEXT_SMALL).font(fonts::MONO),
                        w::colored(format!("+{}", d.additions), TEXT_SMALL, |c| c.success),
                        w::colored(format!("−{}", d.deletions), TEXT_SMALL, |c| c.danger),
                        w::muted(format!("{} files", d.changed_files), TEXT_SMALL),
                    ]
                    .spacing(S)
                    .align_y(Alignment::Center),
                ]
                .spacing(XS)
                .width(Length::Fill),
                w::icon_label_button(
                    Icon::GitBranch,
                    "Start Thread",
                    w::Kind::Primary,
                    (!prs.busy).then(|| core(Event::StartPullRequestThread { key: d.key.clone() })),
                ),
                w::icon_label_button(Icon::ExternalLink, "Browser", w::Kind::Secondary, Some(Message::OpenUrl(d.url.clone()))),
            ]
            .spacing(S)
            .align_y(Alignment::Start),
        );

        // Merge & state actions
        let merge_method = if app.pr_review.merge_method.is_empty() { "squash" } else { app.pr_review.merge_method.as_str() };
        let mut act_row = row![
            w::segmented(
                &[("squash", None, "Squash"), ("merge", None, "Merge"), ("rebase", None, "Rebase")],
                merge_method,
                |m| Message::PrMergeMethodChanged(m.into()),
            ),
            w::small_button(
                "Merge PR",
                w::primary_style,
                (!prs.busy && d.state == "open" && !d.draft).then(|| core(Event::PullRequestAction {
                    key: d.key.clone(),
                    action: "merge".into(),
                    merge_method: merge_method.into(),
                })),
            ),
        ]
        .spacing(S)
        .align_y(Alignment::Center);

        if d.state == "open" {
            act_row = act_row
                .push(w::small_button(
                    if d.draft { "Mark Ready" } else { "Convert to Draft" },
                    w::secondary_style,
                    (!prs.busy).then(|| core(Event::PullRequestAction {
                        key: d.key.clone(),
                        action: if d.draft { "ready" } else { "draft" }.into(),
                        merge_method: String::new(),
                    })),
                ))
                .push(w::small_button(
                    "Close",
                    w::danger_style,
                    (!prs.busy).then(|| core(Event::PullRequestAction {
                        key: d.key.clone(),
                        action: "close".into(),
                        merge_method: String::new(),
                    })),
                ));
        } else if d.state == "closed" {
            act_row = act_row.push(w::small_button(
                "Reopen",
                w::secondary_style,
                (!prs.busy).then(|| core(Event::PullRequestAction {
                    key: d.key.clone(),
                    action: "reopen".into(),
                    merge_method: String::new(),
                })),
            ));
        }
        col = col.push(act_row);

        // Checks
        if !d.checks.is_empty() {
            let mut ch_col = Column::new().spacing(XS).push(w::heading("Checks"));
            for ch in &d.checks {
                let tone: w::Tone = match ch.status.as_str() {
                    "success" | "pass" | "completed" => |c| c.success,
                    "failure" | "failed" | "error" => |c| c.danger,
                    _ => |c| c.warning,
                };
                let mut r = row![
                    w::dot(tone, 7.0),
                    text(&ch.name).size(TEXT_SMALL).width(Length::Fill),
                    w::colored(&ch.status, TEXT_CAPTION, tone),
                ]
                .spacing(S)
                .align_y(Alignment::Center);
                if !ch.url.is_empty() {
                    r = r.push(w::icon_button(Icon::ExternalLink, "Open check", Some(Message::OpenUrl(ch.url.clone()))));
                }
                ch_col = ch_col.push(r);
            }
            col = col.push(w::card(ch_col).width(Length::Fill));
        }

        // Description
        if !d.body.is_empty() {
            col = col.push(
                w::card(markdown::render_blocks(
                    &d.body,
                    app.prefs.text_size,
                    app.is_dark(),
                    &app.vm.diagrams,
                    &app.images,
                    Message::Markdown,
                ))
                .width(Length::Fill),
            );
        }

        // Review & Comment forms
        let verdict = if app.pr_review.verdict.is_empty() { "comment" } else { app.pr_review.verdict.as_str() };
        col = col.push(
            w::card(
                column![
                    w::heading("Submit Review"),
                    w::segmented(
                        &[("approve", None, "Approve"), ("request-changes", None, "Request changes"), ("comment", None, "Comment")],
                        verdict,
                        |v| Message::PrReviewVerdictChanged(v.into()),
                    ),
                    w::input("Review summary…", &app.pr_review.body).on_input(Message::PrReviewBodyChanged),
                    w::small_button(
                        "Submit Review",
                        w::primary_style,
                        (!prs.busy).then(|| Message::SubmitPrReview(d.key.clone())),
                    ),
                ]
                .spacing(S),
            )
            .width(Length::Fill),
        );

        col = col.push(
            w::card(
                column![
                    w::heading("Comment"),
                    row![
                        w::input("Leave a comment…", &app.pr_review.comment)
                            .on_input(Message::PrCommentChanged)
                            .on_submit(Message::SubmitPrComment(d.key.clone()))
                            .width(Length::Fill),
                        w::small_button(
                            "Post",
                            w::secondary_style,
                            (!prs.busy && !app.pr_review.comment.trim().is_empty()).then(|| Message::SubmitPrComment(d.key.clone())),
                        ),
                    ]
                    .spacing(XS),
                ]
                .spacing(S),
            )
            .width(Length::Fill),
        );

        w::scroll(col).height(Length::Fill).into()
    } else {
        w::empty_state(
            Icon::GitPullRequest,
            "Select a pull request",
            "Choose a pull request on the left to inspect checks, review, or start a worktree thread.",
            None,
        )
    };

    column![
        top,
        w::divider(),
        row![
            container(w::scroll(left).height(Length::Fill)).width(Length::Fixed(340.0)).height(Length::Fill).style(w::surface),
            w::vdivider(),
            container(right).width(Length::Fill).height(Length::Fill).style(w::page),
        ]
        .height(Length::Fill),
    ]
    .height(Length::Fill)
    .into()
}
