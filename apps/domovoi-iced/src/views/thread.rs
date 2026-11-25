//! Main thread view, new-thread setup view, connection banner, transcript cards
//! (grouped steps, markdown, diffs, plans, checkpoints, subagents), interactive
//! approvals, questions and secret requests, linked pull requests and
//! issues, and queued messages.

use domovoi_core::Event;
use domovoi_core::view::{ItemView, RequestView, SecretRequestView, ThreadPageView, WorkLinkView};
use iced::widget::{Column, Space, button, canvas, column, container, row, text};
use iced::{Alignment, Background, Border, Element, Length, Padding, Theme};

use crate::app::{App, Message, Overlay};
use crate::ui::fonts;
use crate::ui::icons::{Icon, step_icon};
use crate::ui::theme::{alpha, colors};
use crate::ui::tokens::*;
use crate::ui::widgets as w;
use crate::scroll;
use crate::views::{image_viewer, markdown};

fn core(e: Event) -> Message {
    Message::Core(Box::new(e))
}

/// Connection status banner shown above threads and pages when not cleanly connected.
pub fn status_strip(app: &App) -> Element<'_, Message> {
    let c = &app.vm.connection;
    if c.state == "connected" && !c.showing_saved {
        return Space::new().into();
    }
    let (tone, msg): (w::Tone, String) = if c.server_changed {
        (|h| h.danger, "This server no longer recognizes this device.".into())
    } else if c.offline {
        (|h| h.warning, "Offline — messages queue until your network returns.".into())
    } else if c.state == "reconnecting" {
        let extra = if c.reconnect_in_seconds > 0 && !c.attempting {
            format!("Retrying in {}s…", c.reconnect_in_seconds)
        } else {
            "Reconnecting…".into()
        };
        (|h| h.warning, extra)
    } else if c.state == "connecting" || c.state == "pairing" {
        (|h| h.info, "Connecting to server…".into())
    } else if let Some(err) = &c.error {
        (|h| h.danger, err.clone())
    } else {
        (|h| h.muted, "Disconnected.".into())
    };

    let mut r = row![
        w::themed_icon(Icon::WifiOff, ICON_SMALL, tone),
        text(msg).size(TEXT_SMALL).width(Length::Fill),
    ]
    .spacing(S)
    .align_y(Alignment::Center);

    if c.server_changed {
        r = r.push(w::small_button("Pair again", w::primary_style, Some(Message::ShowAddServer(true))));
    } else {
        r = r.push(w::small_button("Reconnect", w::secondary_style, Some(core(Event::Reconnect))));
    }

    container(r).padding([TIGHT, M]).width(Length::Fill).style(w::banner_style(tone)).into()
}

pub fn thread_view<'a>(app: &'a App, thread: &'a ThreadPageView) -> Element<'a, Message> {
    let mut main = Column::new().spacing(0.0).height(Length::Fill);
    main = main.push(thread_header(app, thread));
    main = main.push(w::divider());

    if let Some(find) = &thread.find {
        main = main.push(find_bar(app, find));
        main = main.push(w::divider());
    }

    // Banners: waiting children, usage limit reset (plan and tasks sit above the composer).
    let mut banners = Column::new().spacing(XS).padding([S, L]).max_width(TRANSCRIPT_MAX_WIDTH);
    let mut has_banners = false;

    for child in &thread.waiting_children {
        has_banners = true;
        banners = banners.push(
            container(
                row![
                    w::themed_icon(Icon::Bell, ICON_SMALL, |c| c.warning),
                    w::muted(if child.relation == "subagent" { "Subagent waiting:" } else { "Thread waiting:" }, TEXT_SMALL),
                    text(if child.title.is_empty() { "Untitled" } else { child.title.as_str() }).size(TEXT_SMALL).font(fonts::MEDIUM).width(Length::Fill),
                    w::small_button("Open", w::secondary_style, Some(core(Event::SelectThread { thread_id: child.thread_id.clone() }))),
                ]
                .spacing(S)
                .align_y(Alignment::Center),
            )
            .padding([TIGHT, M])
            .style(w::banner_style(|c| c.warning)),
        );
    }

    if !thread.links.is_empty() {
        has_banners = true;
        banners = banners.push(row(thread.links.iter().map(work_link_chip)).spacing(XS).wrap());
    }

    if !thread.limit_resets.is_empty() {
        has_banners = true;
        let mut r = row![
            w::themed_icon(Icon::Clock, ICON_SMALL, |c| c.warning),
            text(format!("Usage limit reached · resets {}", thread.limit_resets)).size(TEXT_SMALL).width(Length::Fill),
        ]
        .spacing(S)
        .align_y(Alignment::Center);
        if thread.limit_plan.is_empty() {
            r = r
                .push(w::small_button("Resume at reset", w::primary_style, Some(core(Event::PlanForLimitReset { snooze: false }))))
                .push(w::small_button("Snooze until reset", w::secondary_style, Some(core(Event::PlanForLimitReset { snooze: true }))));
        } else {
            r = r
                .push(w::badge_capsule(format!("Scheduled: {}", thread.limit_plan), Some(|c| c.info)))
                .push(w::small_button("Cancel", w::ghost_style, Some(core(Event::CancelLimitPlan))));
        }
        banners = banners.push(container(r).padding([TIGHT, M]).style(w::banner_style(|c| c.warning)));
    }

    if has_banners {
        main = main.push(container(banners).center_x(Length::Fill));
    }

    // Transcript body
    let mut transcript = Column::new().spacing(M).padding([L, L]).max_width(TRANSCRIPT_MAX_WIDTH).width(Length::Fill);

    if thread.loading && thread.items.is_empty() {
        transcript = transcript.push(w::muted("Loading thread…", TEXT_UI));
    } else if thread.items.is_empty() && thread.pending_sends.is_empty() && thread.queued.is_empty() && !thread.working && thread.background_tasks.is_empty() {
        transcript = transcript.push(w::empty_state(
            Icon::MessageSquare,
            "Ready to work",
            "Ask your coding agent to inspect files, write code, or run commands.",
            None,
        ));
    } else {
        let mut idx = 0;
        while idx < thread.items.len() {
            let item = &thread.items[idx];
            if !item.group.is_empty() {
                let grp = &item.group;
                let start = idx;
                while idx < thread.items.len() && thread.items[idx].group == *grp {
                    idx += 1;
                }
                transcript = transcript.push(container(tool_group_view(app, &thread.items[start..idx])).id(scroll::item_id(&thread.items[start].id)));
            } else {
                let mut el = find_mark(item.find_matches, item.find_active, item_view(app, item));
                // A reply says when its turn ran in its own footer; other last rows get one here.
                if item.run_duration_ms > 0 && item.kind != "assistant" {
                    el = column![el, turn_footer(item)].spacing(XS).into();
                }
                transcript = transcript.push(container(el).id(scroll::item_id(&item.id)));
                idx += 1;
            }
        }
    }

    // Working indicator & background tasks
    if thread.working || !thread.background_tasks.is_empty() {
        transcript = transcript.push(working_row(app, thread));
    }
    if !thread.background_tasks.is_empty() {
        transcript = transcript.push(background_activity(app, &thread.background_tasks));
    }

    // Interactive pending approvals / questions
    for req in &thread.pending_requests {
        transcript = transcript.push(pending_request_card(app, req));
    }

    for asked in &thread.secret_requests {
        transcript = transcript.push(secret_request_card(app, asked));
    }

    // Queued messages
    if !thread.queued.is_empty() {
        transcript = transcript.push(queued_messages_card(app, thread));
    }

    // Offline / outbox pending sends
    for pending in &thread.pending_sends {
        transcript = transcript.push(
            container(
                row![
                    w::themed_icon(Icon::Clock, ICON_SMALL, |c| c.muted),
                    text(&pending.text).size(TEXT_SMALL).width(Length::Fill),
                    w::badge_capsule(&pending.state, None),
                    w::small_button("Cancel", w::ghost_style, Some(core(Event::CancelSend { id: pending.id.clone() }))),
                ]
                .spacing(S)
                .align_y(Alignment::Center),
            )
            .padding(S)
            .style(w::card_style),
        );
    }

    let scroller = w::scroll(container(transcript).center_x(Length::Fill))
        .id(scroll::transcript_id())
        .anchor_bottom()
        .on_scroll(Message::TranscriptScrolled)
        .height(Length::Fill);
    let mut body = iced::widget::stack![scroller];
    if !app.transcript.following {
        let (label, kind) = if app.transcript.unseen { ("New messages", w::Kind::Primary) } else { ("Jump to latest", w::Kind::Secondary) };
        body = body.push(
            container(w::icon_label_button(Icon::ArrowDown, label, kind, Some(Message::JumpToLatest)))
                .center_x(Length::Fill)
                .align_bottom(Length::Fill)
                .padding(M),
        );
    }
    main = main.push(body);
    if thread.plan.is_some() || thread.tasks.is_some() {
        main = main.push(work_progress(app, thread));
    }
    main = main.push(crate::views::composer::view(app));
    main.into()
}

/// The latest plan and checklist at a glance ("Plan · Tasks 2/5 · Write the
/// tests"), opening on the plan's text, the to-dos and Implement.
fn work_progress<'a>(app: &'a App, t: &'a ThreadPageView) -> Element<'a, Message> {
    let open = app.progress_open.as_deref() == Some(t.id.as_str());
    let label = [t.plan.as_ref().map(|p| p.label.as_str()), t.tasks.as_ref().map(|k| k.label.as_str())]
        .into_iter()
        .flatten()
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join(" · ");
    let mut strip = row![
        w::themed_icon(Icon::ListTodo, ICON_SMALL, |c| c.accent),
        text(if label.is_empty() { "Plan and checklist".to_owned() } else { label }).size(TEXT_SMALL).font(fonts::MEDIUM).width(Length::Fill),
    ]
    .spacing(S)
    .align_y(Alignment::Center);
    if let Some(pct) = t.tasks.as_ref().and_then(|k| (k.done * 100).checked_div(k.total)) {
        strip = strip.push(container(w::percent_bar(pct, 0, |c| c.info)).width(Length::Fixed(64.0)));
    }
    strip = strip.push(w::themed_icon(if open { Icon::ChevronDown } else { Icon::ChevronUp }, ICON_XS, |c| c.muted));
    let strip = w::with_tooltip(
        button(strip).width(Length::Fill).padding([TIGHT, M]).style(w::chip_style(open)).on_press(Message::ToggleProgress),
        if open { "Hide plan and checklist" } else { "Show plan and checklist" },
    );

    let mut col = Column::new().spacing(XS).max_width(TRANSCRIPT_MAX_WIDTH);
    if open {
        let mut details = Column::new().spacing(S);
        if let Some(plan) = &t.plan {
            let mut hdr = row![
                w::themed_icon(Icon::ClipboardList, ICON, |c| c.violet),
                text(if plan.label.is_empty() { "Plan" } else { plan.label.as_str() }).size(TEXT_UI).font(fonts::SEMIBOLD).width(Length::Fill),
            ]
            .spacing(S)
            .align_y(Alignment::Center);
            if plan.can_implement && !plan.plan_id.is_empty() {
                hdr = hdr.push(w::small_button("Implement Plan", w::primary_style, Some(Message::ImplementPlan(plan.plan_id.clone()))));
            }
            details = details.push(hdr);
            if !plan.blocks.is_empty() {
                details = details.push(markdown::render_blocks(&plan.blocks, app.prefs.text_size, app.is_dark(), &app.vm.diagrams, &app.images, Message::Markdown));
            }
        }
        if let Some(tasks) = &t.tasks {
            if t.plan.is_some() {
                details = details.push(w::divider());
            }
            let mut hdr = row![
                w::themed_icon(Icon::ListTodo, ICON_SMALL, |c| c.info),
                text(format!("Checklist · {}/{}", tasks.done, tasks.total)).size(TEXT_UI).font(fonts::SEMIBOLD),
            ]
            .spacing(S)
            .align_y(Alignment::Center);
            if tasks.previous_run {
                // The list is from an earlier turn, not the one under way.
                hdr = hdr.push(w::badge_capsule("Previous run", None));
            }
            details = details.push(hdr);
            if let Some(explanation) = tasks.explanation.as_deref().filter(|e| !e.is_empty()) {
                details = details.push(w::muted(explanation, TEXT_SMALL));
            }
            details = details.push(todo_steps(&tasks.todos));
        }
        col = col.push(w::card(w::scroll(details).height(Length::Shrink)).max_height(360.0).width(Length::Fill));
    }
    col = col.push(strip);
    container(col).center_x(Length::Fill).padding(Padding { top: XS, bottom: 0.0, left: L, right: L }).into()
}

/// A to-do list: done, under way and waiting steps.
fn todo_steps(todos: &[domovoi_core::view::TodoView]) -> Column<'_, Message> {
    let mut col = Column::new().spacing(XS);
    for todo in todos {
        let (ic, tone): (Icon, w::Tone) = match todo.status.as_str() {
            "completed" => (Icon::CircleCheck, |c| c.success),
            "in_progress" => (Icon::Loader, |c| c.accent),
            _ => (Icon::CircleDot, |c| c.faint),
        };
        let label = if todo.status == "completed" { w::muted(&todo.text, TEXT_SMALL) } else { text(&todo.text).size(TEXT_SMALL) };
        col = col.push(row![w::themed_icon(ic, ICON_SMALL, tone), label.width(Length::Fill)].spacing(XS).align_y(Alignment::Center));
    }
    col
}

fn thread_header<'a>(app: &'a App, t: &'a ThreadPageView) -> Element<'a, Message> {
    let mut left = row![
        text(if t.title.is_empty() { "New thread" } else { t.title.as_str() })
            .size(TEXT_BODY)
            .font(fonts::SEMIBOLD),
    ]
    .spacing(S)
    .align_y(Alignment::Center);

    if !t.project_title.is_empty() {
        left = left.push(w::badge_capsule(&t.project_title, None));
    }
    if let Some(branch) = &t.branch {
        left = left.push(w::badge_capsule(format!("⑂ {branch}"), Some(|c| c.info)));
    }
    if let Some(parent) = &t.parent {
        left = left.push(
            w::small_button(
                format!("↑ {}", parent.title),
                w::ghost_style,
                Some(core(Event::SelectThread { thread_id: parent.thread_id.clone() })),
            ),
        );
    }

    let mut right = row![].spacing(XS).align_y(Alignment::Center);
    if t.can_merge_back {
        right = right.push(w::small_button("Merge back", w::secondary_style, Some(core(Event::MergeBack))));
    }
    if t.can_fork {
        right = right.push(w::icon_button(
            Icon::GitBranch,
            "Fork thread",
            Some(core(Event::ForkThread { thread_id: t.id.clone() })),
        ));
    }
    if t.artifact_count > 0 {
        right = right.push(w::small_button(
            format!("Artifacts ({})", t.artifact_count),
            w::ghost_style,
            Some(core(Event::ShowPanel { tab: "artifacts".into() })),
        ));
    }
    if let Some(usage) = &t.usage {
        right = right.push(usage_meter(app, usage));
    }
    right = right.push(w::icon_button(
        Icon::Search,
        "Find in thread (Mod+F)",
        Some(core(Event::FindInThread { query: app.find_input.clone() })),
    ));
    right = right.push(w::icon_button(
        Icon::RefreshCw,
        "Reload thread",
        Some(core(Event::ReloadThread)),
    ));
    right = right.push(w::icon_button(Icon::Ellipsis, "Thread actions", Some(Message::Overlay(Some(Overlay::ThreadMenu(t.id.clone()))))));

    container(row![left, Space::new().width(Length::Fill), right].spacing(S).align_y(Alignment::Center))
        .padding([S, L])
        .width(Length::Fill)
        .style(w::surface)
        .into()
}

fn find_bar<'a>(app: &'a App, find: &'a domovoi_core::view::FindView) -> Element<'a, Message> {
    let matches_label = if find.query.is_empty() {
        String::new()
    } else if find.total == 0 {
        "No matches".into()
    } else {
        format!("{} of {}", find.active, find.total)
    };
    container(
        row![
            w::input("Find in thread…", &app.find_input)
                .on_input(Message::FindQueryChanged)
                .on_submit(core(Event::FindNext { forward: true }))
                .width(Length::Fixed(260.0)),
            w::muted(matches_label, TEXT_SMALL),
            w::icon_button(Icon::ChevronUp, "Previous match", (find.total > 0).then(|| core(Event::FindNext { forward: false }))),
            w::icon_button(Icon::ChevronDown, "Next match", (find.total > 0).then(|| core(Event::FindNext { forward: true }))),
            Space::new().width(Length::Fill),
            w::icon_button(Icon::X, "Close find (Esc)", Some(core(Event::CloseFind))),
        ]
        .spacing(XS)
        .align_y(Alignment::Center),
    )
    .padding([TIGHT, L])
    .style(w::surface)
    .into()
}

fn tool_group_view<'a>(app: &'a App, items: &'a [ItemView]) -> Element<'a, Message> {
    let first = &items[0];
    let grp_id = first.group.clone();
    // A group holding the active find match opens so the match shows.
    let expanded = app.expanded_groups.contains(&grp_id) || items.iter().any(|i| i.find_active);
    let summary = if first.group_summary.is_empty() {
        format!("{} steps", items.len())
    } else {
        first.group_summary.clone()
    };
    let chev = if expanded { Icon::ChevronDown } else { Icon::ChevronRight };
    let header = button(
        row![
            w::themed_icon(chev, ICON_XS, |c| c.muted),
            w::themed_icon(Icon::Layers, ICON_SMALL, |c| c.muted),
            w::muted(summary, TEXT_SMALL).font(fonts::MEDIUM).width(Length::Fill),
        ]
        .spacing(S)
        .align_y(Alignment::Center),
    )
    .width(Length::Fill)
    .padding([TIGHT, S])
    .style(w::ghost_style)
    .on_press(Message::ToggleGroup(grp_id));

    let mut col = Column::new().spacing(XS).push(header);

    // While folded, the images the steps looked at still show (open, each step shows its own).
    if !expanded && !first.group_images.is_empty() {
        col = col.push(container(image_viewer::group_strip(app, &first.group_images)).padding(Padding { left: L, ..Padding::ZERO }));
    }

    if expanded {
        let mut inner = Column::new().spacing(XS).padding(Padding { left: L, ..Padding::ZERO });
        for item in items {
            inner = inner.push(container(find_mark(item.find_matches, item.find_active, item_view(app, item))).id(scroll::item_id(&item.id)));
        }
        col = col.push(inner);
    }
    let last = &items[items.len() - 1];
    if last.run_duration_ms > 0 {
        col = col.push(container(turn_footer(last)).padding([0.0, S]));
    }

    container(col).padding(XS).width(Length::Fill).style(w::surface).into()
}

fn item_view<'a>(app: &'a App, item: &'a ItemView) -> Element<'a, Message> {
    let dark = app.is_dark();
    let body_size = app.prefs.text_size;
    match item.kind.as_str() {
        "user" => {
            let mut bubble_col = Column::new().spacing(S);
            if !item.text.is_empty() {
                bubble_col = bubble_col.push(text(&item.text).size(body_size));
            }
            if !item.images.is_empty() {
                bubble_col = bubble_col.push(image_viewer::entry_images(app, &item.images, 200.0));
            }
            let actions = row![
                Space::new().width(Length::Fill),
                w::icon_button(Icon::Copy, "Copy message", Some(Message::CopyText(item.text.clone()))),
                w::icon_button(Icon::Pencil, "Edit & resend", Some(core(Event::EditMessage { text: item.text.clone() }))),
            ]
            .spacing(XXS);
            let bubble = container(column![bubble_col, actions].spacing(XS))
                .padding([M, L])
                .max_width(680)
                .style(|t: &Theme| {
                    let c = colors(t);
                    container::Style {
                        background: Some(Background::Color(alpha(c.accent, if c.dark { 0.16 } else { 0.11 }))),
                        text_color: Some(c.text),
                        border: Border { radius: RADIUS_BUBBLE.into(), width: 1.0, color: alpha(c.accent, 0.30) },
                        ..Default::default()
                    }
                });
            container(bubble).width(Length::Fill).align_x(Alignment::End).into()
        }
        "assistant" => {
            let content: Element<'a, Message> = if item.blocks.is_empty() {
                text(&item.text).size(body_size).into()
            } else {
                markdown::render_blocks(&item.blocks, body_size, dark, &app.vm.diagrams, &app.images, Message::Markdown)
            };
            let mut col = Column::new().spacing(XS).push(content);
            if item.streaming {
                col = col.push(w::colored("▍", body_size, |c| c.accent));
            }
            if !item.streaming && (item.artifact_id.is_some() || item.can_fork_here || item.run_duration_ms > 0) {
                let mut footer = row![].spacing(S).align_y(Alignment::Center);
                if item.run_duration_ms > 0 {
                    footer = footer.push(turn_footer(item));
                }
                footer = footer.push(Space::new().width(Length::Fill));
                if !item.text.is_empty() {
                    footer = footer.push(w::icon_button(Icon::Copy, "Copy reply", Some(Message::CopyText(item.text.clone()))));
                }
                if item.can_fork_here {
                    footer = footer.push(w::small_button(
                        "Fork here",
                        w::ghost_style,
                        Some(core(Event::ForkFromItem { item_id: item.id.clone() })),
                    ));
                }
                col = col.push(footer);
            }
            col.width(Length::Fill).into()
        }
        "reasoning" => {
            let open = app.expanded_items.contains(&item.id) || !item.collapsed;
            let chev = if open { Icon::ChevronDown } else { Icon::ChevronRight };
            let header = button(
                row![
                    w::themed_icon(chev, ICON_XS, |c| c.faint),
                    w::themed_icon(Icon::Lightbulb, ICON_SMALL, |c| c.muted),
                    w::muted(if item.title.is_empty() { "Reasoning" } else { item.title.as_str() }, TEXT_SMALL).font(fonts::MEDIUM),
                ]
                .spacing(XS)
                .align_y(Alignment::Center),
            )
            .padding([XS, S])
            .style(w::ghost_style)
            .on_press(Message::ToggleItem(item.id.clone()));
            let mut col = Column::new().spacing(XS).push(header);
            if open && !item.text.is_empty() {
                col = col.push(container(w::muted(&item.text, TEXT_SMALL)).padding([XS, M]));
            }
            col.into()
        }
        "plan" => {
            let mut col = Column::new().spacing(S);
            let mut hdr = row![
                w::themed_icon(Icon::ClipboardList, ICON, |c| c.violet),
                text(if item.title.is_empty() { "Proposed Plan" } else { item.title.as_str() })
                    .size(TEXT_UI)
                    .font(fonts::SEMIBOLD)
                    .width(Length::Fill),
            ]
            .spacing(S)
            .align_y(Alignment::Center);
            if item.superseded {
                hdr = hdr.push(w::badge_capsule("Superseded", None));
            }
            if item.can_implement && !item.plan_id.is_empty() {
                hdr = hdr.push(w::small_button(
                    "Implement Plan",
                    w::primary_style,
                    Some(core(Event::ImplementPlan { plan_id: item.plan_id.clone() })),
                ));
            }
            hdr = hdr.push(w::small_button(
                "Save…",
                w::secondary_style,
                Some(Message::Overlay(Some(Overlay::SavePlanDialog {
                    item_id: item.id.clone(),
                    path: String::new(),
                }))),
            ));
            col = col.push(hdr);
            if !item.blocks.is_empty() {
                col = col.push(markdown::render_blocks(&item.blocks, body_size, dark, &app.vm.diagrams, &app.images, Message::Markdown));
            } else if !item.text.is_empty() {
                col = col.push(text(&item.text).size(body_size));
            }
            container(col).padding(M).width(Length::Fill).style(w::banner_style(|c| c.violet)).into()
        }
        "todo" => {
            let mut col = Column::new().spacing(XS).push(
                row![
                    w::themed_icon(Icon::ListTodo, ICON_SMALL, |c| c.info),
                    text(if item.title.is_empty() { "Tasks" } else { item.title.as_str() }).size(TEXT_SMALL).font(fonts::SEMIBOLD),
                ]
                .spacing(XS)
                .align_y(Alignment::Center),
            );
            col = col.push(todo_steps(&item.todos));
            w::card(col).width(Length::Fill).into()
        }
        "checkpoint" => {
            let Some(cp) = &item.checkpoint else {
                return w::muted(&item.title, TEXT_SMALL).into();
            };
            let mut col = Column::new().spacing(XS);
            let mut hdr = row![
                w::themed_icon(Icon::GitCommit, ICON_SMALL, |c| c.info),
                text(&cp.label).size(TEXT_SMALL).font(fonts::SEMIBOLD).width(Length::Fill),
            ]
            .spacing(S)
            .align_y(Alignment::Center);
            // A turn that changed nothing has no counts or diff to show.
            if cp.files_changed > 0 {
                let source = if cp.diff_source.is_empty() { format!("turn:{}", cp.turn_count) } else { cp.diff_source.clone() };
                hdr = hdr
                    .push(w::muted(format!("{} file{}", cp.files_changed, if cp.files_changed == 1 { "" } else { "s" }), TEXT_CAPTION))
                    .push(w::colored(format!("+{}", cp.additions), TEXT_CAPTION, |c| c.success))
                    .push(w::colored(format!("−{}", cp.deletions), TEXT_CAPTION, |c| c.danger))
                    .push(w::small_button("View diff", w::secondary_style, Some(Message::ViewDiff(source))));
            }
            if cp.can_revert {
                hdr = hdr.push(w::small_button(
                    "Revert",
                    w::ghost_style,
                    Some(core(Event::Revert { checkpoint_id: cp.id.clone() })),
                ));
            }
            col = col.push(hdr);
            for f in &cp.files {
                col = col.push(
                    row![
                        text(&f.path).size(TEXT_CAPTION).font(fonts::MONO).width(Length::Fill),
                        w::colored(format!("+{}", f.insertions), TEXT_CAPTION, |c| c.success),
                        w::colored(format!("−{}", f.deletions), TEXT_CAPTION, |c| c.danger),
                    ]
                    .spacing(XS),
                );
            }
            w::card(col).width(Length::Fill).into()
        }
        "subagent" => {
            let mut r = row![
                w::themed_icon(Icon::Bot, ICON, |c| c.accent),
                column![
                    text(&item.title).size(TEXT_UI).font(fonts::SEMIBOLD),
                    w::muted(if item.progress.is_empty() { item.status.as_str() } else { item.progress.as_str() }, TEXT_SMALL),
                ]
                .spacing(XXS)
                .width(Length::Fill),
            ]
            .spacing(S)
            .align_y(Alignment::Center);
            if !item.child_thread_id.is_empty() {
                r = r.push(w::small_button(
                    "Open",
                    w::secondary_style,
                    Some(core(Event::SelectThread { thread_id: item.child_thread_id.clone() })),
                ));
            }
            if item.can_stop {
                r = r.push(w::small_button(
                    "Stop",
                    w::danger_style,
                    Some(core(Event::StopSubagent { subagent_id: item.id.clone() })),
                ));
            }
            w::card(r).width(Length::Fill).into()
        }
        "error" => {
            let mut r = row![
                w::themed_icon(Icon::CircleAlert, ICON, |c| c.danger),
                column![
                    text(if item.title.is_empty() { "Error" } else { item.title.as_str() })
                        .size(TEXT_UI)
                        .font(fonts::SEMIBOLD),
                    text(&item.text).size(TEXT_SMALL),
                ]
                .spacing(XXS)
                .width(Length::Fill),
            ]
            .spacing(S)
            .align_y(Alignment::Center);
            if item.can_retry {
                r = r.push(w::small_button("Retry", w::primary_style, Some(core(Event::Retry))));
            }
            container(r).padding(M).width(Length::Fill).style(w::banner_style(|c| c.danger)).into()
        }
        "artifact" => artifact_card(app, item),
        "notice" => container(
            row![w::themed_icon(Icon::Info, ICON_SMALL, |c| c.info), w::muted(&item.text, TEXT_SMALL)]
                .spacing(S)
                .align_y(Alignment::Center),
        )
        .padding([TIGHT, M])
        .style(w::banner_style(|c| c.info))
        .into(),
        _ => step_row_view(app, item),
    }
}

fn step_row_view<'a>(app: &'a App, item: &'a ItemView) -> Element<'a, Message> {
    let is_open = app.expanded_items.contains(&item.id) || (!item.collapsed && item.expandable);
    let ic = step_icon(&item.icon);
    let tone: w::Tone = match item.status.as_str() {
        "failed" => |c| c.danger,
        "running" => |c| c.accent,
        _ => |c| c.muted,
    };

    let mut hdr = row![].spacing(S).align_y(Alignment::Center);
    if item.expandable {
        hdr = hdr.push(w::themed_icon(if is_open { Icon::ChevronDown } else { Icon::ChevronRight }, ICON_XS, |c| c.faint));
    }
    hdr = hdr.push(w::themed_icon(ic, ICON_SMALL, tone));
    if !item.verb.is_empty() {
        hdr = hdr.push(w::muted(&item.verb, TEXT_SMALL).font(fonts::MEDIUM));
    }
    hdr = hdr.push(
        text(&item.title)
            .size(TEXT_SMALL)
            .font(if item.mono { fonts::MONO } else { fonts::MEDIUM })
            .width(Length::Fill),
    );
    if !item.subtitle.is_empty() {
        hdr = hdr.push(w::faint(&item.subtitle, TEXT_CAPTION));
    }
    if !item.badge.is_empty() {
        let badge_tone: Option<w::Tone> = if item.badge_tone == "failed" { Some(|c| c.danger) } else { None };
        hdr = hdr.push(w::badge_capsule(&item.badge, badge_tone));
    }
    // A failed exit is in the badge ("exit 101"); a clean one gets a quiet check.
    if item.kind == "command" && item.status != "running" && item.exit_code == Some(0) {
        hdr = hdr.push(w::with_tooltip(w::themed_icon(Icon::Check, ICON_XS, |c| c.success), "Exited with code 0"));
    }
    // Web pages an edit wrote open as their source in the Artifacts panel.
    let pages: Vec<&String> = if item.kind == "file_change" && !item.streaming { item.artifact_paths.iter().filter(|p| is_html(p)).collect() } else { Vec::new() };
    for path in &pages {
        let label = if pages.len() == 1 { "Open preview".to_owned() } else { format!("Open {}", path.rsplit(['/', '\\']).next().unwrap_or(path)) };
        hdr = hdr.push(w::with_tooltip(w::small_button(label, w::ghost_style, Some(Message::PreviewArtifactFile((*path).clone()))), path));
    }
    if let Some(art_id) = &item.artifact_id
        && let Some(tid) = app.vm.thread.as_ref().map(|t| t.id.clone()) {
            hdr = hdr.push(w::small_button(
                "Open artifact",
                w::secondary_style,
                Some(core(Event::OpenArtifact {
                    thread_id: tid,
                    artifact_id: art_id.clone(),
                    version: Some(item.artifact_version),
                    dark: app.is_dark(),
                })),
            ));
        }

    let header_el: Element<'a, Message> = if item.expandable {
        button(hdr).width(Length::Fill).padding([XS, S]).style(w::ghost_style).on_press(Message::ToggleItem(item.id.clone())).into()
    } else {
        container(hdr).padding([XS, S]).width(Length::Fill).into()
    };

    let mut col = Column::new().spacing(XS).push(header_el);
    if !item.images.is_empty() {
        // What the agent looked at shows without opening the step.
        col = col.push(container(image_viewer::entry_images(app, &item.images, 120.0)).padding(Padding { left: M, right: M, bottom: XS, top: 0.0 }));
    }
    if is_open {
        let mut details = Column::new().spacing(XS).padding([XS, M]);
        for field in item.fields.iter() {
            details = details.push(
                row![
                    w::faint(&field.label, TEXT_CAPTION).width(Length::Fixed(100.0)),
                    text(&field.value).size(TEXT_CAPTION).font(if field.mono { fonts::MONO } else { fonts::INTER }).width(Length::Fill),
                ]
                .spacing(S),
            );
        }
        if let Some(output) = &item.output
            && !output.is_empty() {
                details = details.push(
                    container(text(output.as_ref()).font(fonts::MONO).size(TEXT_CODE))
                        .padding(S)
                        .width(Length::Fill)
                        .style(w::sunken),
                );
            }
        if let Some(diff) = &item.diff {
            details = details.push(crate::views::panel::diff::diff_file_body(diff, true));
        }
        for link in item.links.iter() {
            details = details.push(
                button(
                    row![
                        w::themed_icon(Icon::ExternalLink, ICON_XS, |c| c.accent),
                        w::colored(&link.title, TEXT_SMALL, |c| c.accent),
                        w::faint(&link.host, TEXT_CAPTION),
                    ]
                    .spacing(XS)
                    .align_y(Alignment::Center),
                )
                .padding([2.0, XS])
                .style(w::ghost_style)
                .on_press(Message::OpenUrl(link.url.clone())),
            );
        }
        if item.truncated {
            details = details.push(w::small_button(
                "Show full output",
                w::secondary_style,
                Some(core(Event::ExpandItem { id: item.id.clone() })),
            ));
        }
        col = col.push(details);
    }

    container(col).width(Length::Fill).style(w::card_style).into()
}

/// A linked pull request (`#12 · Fix login`, with its state) or issue
/// (`ENG-123 · Login fails`): opens in the browser.
fn work_link_chip(link: &WorkLinkView) -> Element<'_, Message> {
    let icon = if link.kind == "pull_request" { Icon::GitPullRequest } else { Icon::CircleDot };
    let mut chip = row![w::themed_icon(icon, ICON_XS, |c| c.muted), text(&link.label).size(TEXT_SMALL).font(fonts::MEDIUM)].spacing(XS).align_y(Alignment::Center);
    if !link.title.is_empty() {
        chip = chip.push(w::muted(&link.title, TEXT_SMALL));
    }
    if !link.state.is_empty() {
        let tone: w::Tone = match link.state.as_str() {
            "merged" => |c| c.violet,
            "closed" => |c| c.danger,
            "draft" => |c| c.muted,
            _ => |c| c.success,
        };
        chip = chip.push(w::badge_capsule(link.state.clone(), Some(tone)));
    }
    let tip = if link.kind == "pull_request" { "Open the pull request in the browser" } else { "Open the issue in the browser" };
    w::with_tooltip(button(chip).padding([TIGHT, S]).style(w::chip_style(false)).on_press(Message::OpenUrl(link.url.clone())), tip)
}

/// A secret the agent asked for: a password field, Save and Decline. What
/// is typed stays in this window (`App::secret_drafts`) until it is sent.
fn secret_request_card<'a>(app: &'a App, asked: &'a SecretRequestView) -> Element<'a, Message> {
    let mut col = Column::new().spacing(S).push(
        row![
            w::themed_icon(Icon::Key, ICON, |c| c.warning),
            text(format!("The agent asks for {}", asked.label)).size(TEXT_UI).font(fonts::SEMIBOLD).width(Length::Fill),
        ]
        .spacing(S)
        .align_y(Alignment::Center),
    );
    if !asked.reason.is_empty() {
        col = col.push(text(&asked.reason).size(TEXT_UI));
    }
    if let Some(url) = &asked.url {
        col = col.push(w::small_button("Get one", w::ghost_style, Some(Message::OpenUrl(url.clone()))));
    }
    let typed = app.secret_drafts.get(&asked.item_id).map_or("", |v| v.0.as_str());
    let ready = !typed.trim().is_empty() && !asked.sending;
    let id = asked.item_id.clone();
    let mut field = w::input(if asked.placeholder.is_empty() { "Secret value" } else { asked.placeholder.as_str() }, typed).secure(true);
    if !asked.sending {
        field = field.on_input(move |value| Message::SecretChanged { item_id: id.clone(), value: crate::app::SecretText(value) });
        if ready {
            field = field.on_submit(Message::SecretSubmit { item_id: asked.item_id.clone() });
        }
    }
    col = col.push(field);
    col = col.push(w::faint(&asked.hint, TEXT_CAPTION));
    let decline = (!asked.sending).then(|| core(Event::DeclineSecret { item_id: asked.item_id.clone() }));
    col = col.push(
        row![
            w::label_button(if asked.sending { "Saving…" } else { "Save Secret" }, w::primary_style, ready.then(|| Message::SecretSubmit { item_id: asked.item_id.clone() })),
            w::label_button("Decline", w::ghost_style, decline),
        ]
        .spacing(S),
    );
    container(col).padding(M).width(Length::Fill).style(w::card_style).into()
}

fn pending_request_card<'a>(app: &'a App, req: &'a RequestView) -> Element<'a, Message> {
    let is_question = !req.questions.is_empty();
    let tone: w::Tone = if is_question { |c| c.info } else { |c| c.warning };
    let mut col = Column::new().spacing(S).push(
        row![
            w::themed_icon(if is_question { Icon::CircleHelp } else { Icon::Shield }, ICON, tone),
            text(&req.prompt).size(TEXT_UI).font(fonts::SEMIBOLD).width(Length::Fill),
        ]
        .spacing(S)
        .align_y(Alignment::Center),
    );

    if let Some(detail) = &req.detail
        && !detail.is_empty() {
            col = col.push(container(text(detail).font(fonts::MONO).size(TEXT_CODE)).padding(S).width(Length::Fill).style(w::sunken));
        }

    for f in &req.files {
        col = col.push(crate::views::panel::diff::diff_file_body(f, true));
    }

    if is_question {
        let req_drafts = app.question_drafts.get(&req.id);
        for q in &req.questions {
            let q_draft = req_drafts.and_then(|m| m.get(&q.id));
            let mut q_col = Column::new().spacing(XS);
            if !q.header.is_empty() {
                q_col = q_col.push(w::heading(&q.header));
            }
            let mut q_line = row![text(&q.question).size(TEXT_UI).font(fonts::MEDIUM)].spacing(S).align_y(Alignment::Center);
            if q.required {
                q_line = q_line.push(w::faint("Required", TEXT_CAPTION));
            }
            q_col = q_col.push(q_line);
            for opt in &q.options {
                let checked = q_draft.is_some_and(|d| d.selected.iter().any(|v| v == &opt.value));
                q_col = q_col.push(
                    w::list_row(
                        row![
                            w::themed_icon(
                                if checked { Icon::CircleCheck } else { Icon::CircleDot },
                                ICON_SMALL,
                                move |c| if checked { c.accent } else { c.muted },
                            ),
                            column![
                                text(&opt.label).size(TEXT_UI).font(fonts::MEDIUM),
                                if !opt.description.is_empty() {
                                    w::muted(&opt.description, TEXT_SMALL)
                                } else {
                                    w::muted("", TEXT_SMALL)
                                },
                            ]
                            .spacing(1.0)
                            .width(Length::Fill),
                        ]
                        .spacing(S)
                        .align_y(Alignment::Center),
                        checked,
                        !opt.description.is_empty(),
                    )
                    .on_press(Message::QuestionToggleOption {
                        request_id: req.id.clone(),
                        question_id: q.id.clone(),
                        value: opt.value.clone(),
                        multi: q.multi_select,
                    }),
                );
            }
            if q.allow_custom_answer || q.options.is_empty() {
                let custom = q_draft.map(|d| d.custom.as_str()).unwrap_or("");
                let rid = req.id.clone();
                let qid = q.id.clone();
                q_col = q_col.push(
                    w::input("Custom answer…", custom)
                        .on_input(move |t| Message::QuestionCustomChanged {
                            request_id: rid.clone(),
                            question_id: qid.clone(),
                            text: t,
                        })
                        .on_submit(Message::QuestionSubmit { request_id: req.id.clone() }),
                );
            }
            col = col.push(q_col);
        }
        // Submit waits for an answer to each required question.
        let answered = app.questions_answered(req);
        let submit = w::label_button("Submit Answer", w::primary_style, answered.then(|| Message::QuestionSubmit { request_id: req.id.clone() }));
        col = col.push(
            row![
                if answered { submit.into() } else { w::with_tooltip(submit, "Answer the required questions first") },
                w::label_button("Dismiss", w::ghost_style, Some(core(Event::DismissUserInput { request_id: req.id.clone() }))),
            ]
            .spacing(S),
        );
    } else {
        let session_label = if req.session_label.is_empty() { "Allow for Session" } else { req.session_label.as_str() };
        col = col.push(
            row![
                w::label_button(
                    "Allow",
                    w::primary_style,
                    Some(core(Event::Respond { request_id: req.id.clone(), decision: "accept".into() })),
                ),
                w::label_button(
                    session_label,
                    w::secondary_style,
                    Some(core(Event::Respond { request_id: req.id.clone(), decision: "acceptForSession".into() })),
                ),
                w::label_button(
                    "Decline",
                    w::danger_style,
                    Some(core(Event::Respond { request_id: req.id.clone(), decision: "decline".into() })),
                ),
            ]
            .spacing(S),
        );
    }

    container(col).padding(L).width(Length::Fill).style(w::banner_style(tone)).into()
}

fn queued_messages_card<'a>(app: &'a App, thread: &'a ThreadPageView) -> Element<'a, Message> {
    let mut col = Column::new().spacing(XS);
    let mut hdr = row![
        w::themed_icon(Icon::Clock, ICON_SMALL, |c| c.info),
        text(format!("Queued ({})", thread.queued.len())).size(TEXT_SMALL).font(fonts::SEMIBOLD).width(Length::Fill),
    ]
    .spacing(S)
    .align_y(Alignment::Center);
    if thread.queue_held {
        hdr = hdr.push(w::small_button("Resume Queue", w::primary_style, Some(core(Event::ResumeQueue))));
    }
    col = col.push(hdr);

    for q in &thread.queued {
        if let Some((edit_id, edit_text)) = &app.editing_queued
            && edit_id == &q.run_id {
                col = col.push(
                    row![
                        w::input("Edit queued message…", edit_text)
                            .on_input(Message::EditQueuedChanged)
                            .on_submit(Message::CommitEditQueued)
                            .width(Length::Fill),
                        w::small_button("Save", w::primary_style, Some(Message::CommitEditQueued)),
                        w::small_button("Cancel", w::ghost_style, Some(Message::CancelEditQueued)),
                    ]
                    .spacing(XS)
                    .align_y(Alignment::Center),
                );
                continue;
            }
        let mut r = row![text(&q.text).size(TEXT_SMALL).width(Length::Fill)].spacing(XS).align_y(Alignment::Center);
        if q.can_steer {
            r = r.push(w::small_button("Steer now", w::secondary_style, Some(core(Event::SteerQueued { run_id: q.run_id.clone() }))));
        }
        if !q.first {
            r = r.push(w::icon_button(Icon::ArrowUp, "Move up", Some(core(Event::MoveQueued { run_id: q.run_id.clone(), delta: -1 }))));
        }
        if !q.last {
            r = r.push(w::icon_button(Icon::ArrowDown, "Move down", Some(core(Event::MoveQueued { run_id: q.run_id.clone(), delta: 1 }))));
        }
        r = r
            .push(w::icon_button(
                Icon::Pencil,
                "Edit",
                Some(Message::StartEditQueued { run_id: q.run_id.clone(), text: q.text.clone() }),
            ))
            .push(w::icon_button(Icon::X, "Cancel", Some(core(Event::CancelQueued { run_id: q.run_id.clone() }))));
        col = col.push(r);
    }

    w::card(col).width(Length::Fill).into()
}

pub fn new_thread_view(app: &App) -> Element<'_, Message> {
    let comp = &app.vm.composer;
    let projects = &app.vm.sidebar.projects;
    let selected_proj = app
        .vm
        .sidebar
        .selected_project_id
        .as_deref()
        .and_then(|id| projects.iter().find(|p| p.id == id));

    let mut center_col = Column::new().spacing(L).max_width(640).align_x(Alignment::Center);
    center_col = center_col.push(
        column![
            w::themed_icon(Icon::Sparkles, 32.0, |c| c.accent),
            text(
                selected_proj
                    .map(|p| format!("New thread in {}", p.title))
                    .unwrap_or_else(|| "Start a new thread".into())
            )
            .size(TEXT_TITLE)
            .font(fonts::SEMIBOLD),
            w::muted("Choose a project and workspace mode, then send your first prompt.", TEXT_UI),
        ]
        .spacing(XS)
        .align_x(Alignment::Center),
    );

    if !comp.pr_worktree.is_empty() {
        center_col = center_col.push(
            container(
                row![w::themed_icon(Icon::GitPullRequest, ICON_SMALL, |c| c.info), text(&comp.pr_worktree).size(TEXT_SMALL)]
                    .spacing(S)
                    .align_y(Alignment::Center),
            )
            .padding([TIGHT, M])
            .style(w::banner_style(|c| c.info)),
        );
    }

    if !projects.is_empty() {
        let mut proj_row = row![].spacing(XS).align_y(Alignment::Center);
        for p in projects.iter().take(6) {
            let active = app.vm.sidebar.selected_project_id.as_deref() == Some(p.id.as_str());
            proj_row = proj_row.push(
                button(
                    row![w::project_icon_tile(&p.icon, 16.0, &app.images), text(&p.title).size(TEXT_SMALL)]
                        .spacing(XS)
                        .align_y(Alignment::Center),
                )
                .padding([4.0, S])
                .style(w::chip_style(active))
                .on_press(core(Event::NewThread { project_id: Some(p.id.clone()) })),
            );
        }
        center_col = center_col.push(proj_row);
    }

    // Workspace mode: local vs worktree
    let is_worktree = comp.workspace_mode == "worktree";
    let ws_picker = w::segmented(
        &[(false, Some(Icon::Folder), "Local folder"), (true, Some(Icon::GitBranch), "New worktree")],
        is_worktree,
        |wt| core(Event::SetWorkspaceMode { mode: if wt { "worktree" } else { "local" }.into() }),
    );
    let mut ws_row = row![ws_picker].spacing(S).align_y(Alignment::Center);
    if is_worktree && !comp.branches.is_empty() {
        for br in comp.branches.iter().take(4) {
            let active = comp.base_branch.as_deref() == Some(br.as_str());
            ws_row = ws_row.push(
                button(text(br).size(TEXT_SMALL).font(fonts::MONO))
                    .padding([4.0, S])
                    .style(w::chip_style(active))
                    .on_press(core(Event::SetBaseBranch { branch: br.clone() })),
            );
        }
    }
    center_col = center_col.push(ws_row);

    column![
        container(center_col).center(Length::Fill).padding(XL),
        crate::views::composer::view(app),
    ]
    .height(Length::Fill)
    .into()
}

/// The core reads only HTML files for `OpenArtifactFile`.
fn is_html(path: &str) -> bool {
    path.rsplit_once('.').is_some_and(|(_, ext)| ext.eq_ignore_ascii_case("html") || ext.eq_ignore_ascii_case("htm"))
}

/// When a finished turn started and how long it took: "14:02 · 1m 12s".
fn turn_footer(item: &ItemView) -> Element<'_, Message> {
    let started = crate::ui::time::local_clock_ms(item.run_started_ms);
    let took = format_duration_ms(item.run_duration_ms);
    let label = if started.is_empty() { took } else { format!("{started} · {took}") };
    w::faint(label, TEXT_CAPTION).into()
}

fn format_duration_ms(ms: i64) -> String {
    let secs = (ms / 1000).max(0);
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m {}s", secs / 60, secs % 60)
    } else {
        format!("{}h {}m", secs / 3600, secs % 3600 / 60)
    }
}

/// "Writing a reply · 1m 12s": what the turn under way is doing, from the
/// kind of its live item, with a pulsing mark (steady while it waits for you).
fn working_row<'a>(app: &'a App, t: &'a ThreadPageView) -> Element<'a, Message> {
    let approval = t.status == "approval";
    let (title, ic) = if approval {
        ("Waiting for you", Icon::CircleHelp)
    } else {
        match t.working_activity.as_ref().and_then(|a| a.kind.as_deref()) {
            Some("starting") => ("Starting", Icon::Sparkles),
            Some("assistant") => ("Writing a reply", Icon::MessageSquare),
            Some("command") => ("Running a command", Icon::Terminal),
            Some("file_change") => ("Editing files", Icon::Pencil),
            Some("tool") => ("Using a tool", Icon::Wrench),
            Some("reasoning") => ("Thinking", Icon::Lightbulb),
            _ if !t.working => ("Working in the background", Icon::Clock),
            _ => ("Working", Icon::Sparkles),
        }
    };
    let tone: w::Tone = if approval { |c| c.warning } else { |c| c.accent };
    // A slow breath on the mark's fill while the agent works (the window
    // redraws every second during a turn; see `App::subscription`).
    let pulse = if approval { 0.13 } else { 0.08 + 0.07 * (1.0 + (app.clock_ms as f32 / 1000.0 * std::f32::consts::PI).sin()) / 2.0 };
    let mark = container(w::themed_icon(ic, ICON_SMALL, tone)).center(Length::Fixed(28.0)).style(move |th: &Theme| container::Style {
        background: Some(Background::Color(alpha(tone(colors(th)), pulse))),
        border: Border { radius: RADIUS_CHIP.into(), ..Border::default() },
        ..Default::default()
    });
    let mut r = row![mark, text(title).size(TEXT_UI).font(fonts::SEMIBOLD)].spacing(S).align_y(Alignment::Center);
    if !approval && t.working && t.working_since_ms > 0 {
        r = r.push(w::faint("·", TEXT_UI)).push(w::muted(format_duration_ms(app.clock_ms - t.working_since_ms), TEXT_UI).font(fonts::MONO));
    }
    if !t.background.is_empty() {
        r = r.push(w::faint(&t.background, TEXT_SMALL));
    }
    if let Some(step) = t.working_activity.as_ref().and_then(|a| a.step_item_id.as_ref()).filter(|_| !approval) {
        r = r.push(w::with_tooltip(w::small_button("Activity ↑", w::ghost_style, Some(Message::RevealItem(step.clone()))), "Shows the current run's transcript steps"));
    }
    container(r).padding([XS, 0.0]).into()
}

/// The provider's background work (subagents, commands, monitors); a row
/// with a child thread opens it.
fn background_activity<'a>(app: &'a App, tasks: &'a [domovoi_core::view::BackgroundTaskView]) -> Element<'a, Message> {
    const PREVIEW: usize = 2;
    let expanded = app.background_expanded;
    let mut col = Column::new().spacing(XS).push(w::muted(format!("Background activity · {}", tasks.len()), TEXT_SMALL).font(fonts::SEMIBOLD));
    for task in tasks.iter().take(if expanded { tasks.len() } else { PREVIEW }) {
        let ic = match task.kind.as_str() {
            "subagent" => Icon::Bot,
            "command" => Icon::Terminal,
            "monitor" => Icon::Eye,
            _ => Icon::Settings,
        };
        let detail = match (task.status.as_str(), task.progress.as_deref()) {
            ("pending", _) => "Starting".to_owned(),
            ("waiting", Some(p)) => format!("Waiting · {p}"),
            ("waiting", None) => "Waiting".to_owned(),
            (_, Some(p)) => p.to_owned(),
            _ => "Running in the background".to_owned(),
        };
        let mut r = row![
            w::themed_icon(ic, ICON_SMALL, |c| c.accent),
            column![text(&task.label).size(TEXT_UI).font(fonts::MEDIUM), w::muted(detail, TEXT_SMALL)].spacing(XXS).width(Length::Fill),
        ]
        .spacing(S)
        .align_y(Alignment::Center);
        if let Some(child) = &task.child_thread_id {
            r = r.push(w::themed_icon(Icon::ExternalLink, ICON_SMALL, |c| c.accent));
            col = col.push(button(r).padding([XXS, 0.0]).style(w::ghost_style).on_press(core(Event::SelectThread { thread_id: child.clone() })));
        } else {
            col = col.push(container(r).padding([XXS, 0.0]));
        }
    }
    if tasks.len() > PREVIEW {
        let label = if expanded { "Show less".to_owned() } else { format!("Show all {}", tasks.len()) };
        col = col.push(w::small_button(label, w::ghost_style, Some(Message::ToggleBackgroundTasks)));
    }
    w::card(col).into()
}

/// The thread's usage at a glance: a ring for how full the context window is
/// (with a tick where the agent compacts on its own), else for the fullest
/// provider limit; a dot when a limit runs low. Opens the details.
fn usage_meter<'a>(app: &'a App, u: &'a domovoi_core::view::SessionUsageView) -> Element<'a, Message> {
    let (ring, label, label_tone): (Option<UsageRing>, String, w::Tone) = if let Some(ctx) = &u.context {
        let tone: w::Tone = if ctx.critical {
            |c| c.danger
        } else if ctx.high {
            |c| c.warning
        } else {
            |c| c.accent
        };
        let ring = (ctx.max_tokens > 0).then_some(UsageRing { percent: ctx.percent, tick: ctx.compact_at_percent, tone, alert: None });
        let label = if ctx.max_tokens > 0 { format!("{}%", ctx.percent) } else { ctx.label.clone() };
        (ring, label, if ctx.high { tone } else { |c| c.muted })
    } else if let Some(win) = &u.fullest {
        let tone = w::limit_tone(win);
        (Some(UsageRing { percent: win.percent, tick: 0, tone, alert: None }), format!("{}%", win.percent), if win.high { tone } else { |c| c.muted })
    } else {
        (None, String::new(), |c| c.muted)
    };
    let alert: Option<w::Tone> = match u.limit_alert.as_str() {
        "danger" => Some(|c| c.danger),
        "caution" => Some(|c| c.warning),
        _ => None,
    };
    let ring = ring.unwrap_or(UsageRing { percent: 0, tick: 0, tone: |c| c.muted, alert: None });
    let mut r = row![iced::widget::canvas(UsageRing { alert, ..ring }).width(18.0).height(18.0)].spacing(XS).align_y(Alignment::Center);
    if !label.is_empty() {
        r = r.push(w::colored(label, TEXT_CAPTION, label_tone).font(fonts::MONO));
    }
    let mut tip: Vec<String> = Vec::new();
    if let Some(ctx) = &u.context {
        tip.push(if ctx.max_tokens > 0 { format!("Context: {} ({}%)", ctx.label, ctx.percent) } else { format!("Context: {}", ctx.label) });
    }
    if let (Some(win), Some(lim)) = (&u.fullest, &u.limits) {
        let resets = if win.resets.is_empty() { String::new() } else { format!(", resets {}", win.resets) };
        tip.push(format!("{} {}: {}% used{resets}", lim.provider, win.label, win.percent));
    } else if let (false, Some(lim)) = (u.limits_notice.is_empty(), &u.limits) {
        tip.push(format!("{} limits: {}", lim.provider, u.limits_notice));
    }
    w::with_tooltip(
        button(r).padding([3.0, S]).style(w::chip_style(matches!(app.overlay, Some(Overlay::ContextPopover)))).on_press(Message::Overlay(Some(Overlay::ContextPopover))),
        &tip.join("\n"),
    )
}

#[derive(Clone, Copy)]
struct UsageRing {
    percent: u32,
    /// Where the agent compacts on its own (0 or 100: none).
    tick: u32,
    tone: w::Tone,
    alert: Option<w::Tone>,
}

impl canvas::Program<Message> for UsageRing {
    type State = ();

    fn draw(&self, _state: &(), renderer: &iced::Renderer, theme: &Theme, bounds: iced::Rectangle, _cursor: iced::mouse::Cursor) -> Vec<canvas::Geometry> {
        use std::f32::consts::{FRAC_PI_2, TAU};
        let c = colors(theme);
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let center = iced::Point::new(bounds.width / 2.0, bounds.height / 2.0);
        let radius = bounds.width.min(bounds.height) / 2.0 - 2.5;
        let stroke = |color| canvas::Stroke::default().with_width(3.0).with_color(color).with_line_cap(canvas::LineCap::Round);
        frame.stroke(&canvas::Path::circle(center, radius), stroke(alpha(c.muted, 0.25)));
        let arc = |from: f32, to: f32| {
            canvas::Path::new(|b| b.arc(canvas::path::Arc { center, radius, start_angle: iced::Radians(from * TAU - FRAC_PI_2), end_angle: iced::Radians(to * TAU - FRAC_PI_2) }))
        };
        let p = self.percent.min(100) as f32 / 100.0;
        if p > 0.0 {
            frame.stroke(&arc(0.0, p), stroke((self.tone)(c)));
        }
        if (1..100).contains(&self.tick) {
            let at = self.tick as f32 / 100.0;
            frame.stroke(&arc(at - 0.012, at + 0.012), canvas::Stroke::default().with_width(3.0).with_color(alpha(c.text, 0.75)));
        }
        if let Some(alert) = self.alert {
            let dot = iced::Point::new(bounds.width - 3.0, 3.0);
            frame.fill(&canvas::Path::circle(dot, 3.5), c.surface);
            frame.fill(&canvas::Path::circle(dot, 2.5), alert(c));
        }
        vec![frame.into_geometry()]
    }
}

/// Rows matching the thread's find query: a count, and an outline on the
/// active match.
fn find_mark<'a>(matches: u32, active: bool, row: Element<'a, Message>) -> Element<'a, Message> {
    if matches == 0 {
        return row;
    }
    let badge = container(text(matches.to_string()).size(TEXT_CAPTION).font(fonts::MONO_BOLD)).padding([1.0, XS]).style(move |t: &Theme| {
        let c = colors(t);
        container::Style {
            background: Some(Background::Color(alpha(c.warning, if active { 0.9 } else { 0.35 }))),
            text_color: Some(if active { c.bg } else { c.text }),
            border: Border { radius: RADIUS_PILL.into(), ..Border::default() },
            ..Default::default()
        }
    });
    let framed = container(row).padding(XXS).width(Length::Fill).style(move |t: &Theme| container::Style {
        border: Border { radius: RADIUS_CARD.into(), width: if active { 2.0 } else { 0.0 }, color: alpha(colors(t).warning, 0.8) },
        ..Default::default()
    });
    column![row![Space::new().width(Length::Fill), badge], framed].spacing(XXS).into()
}

/// An artifact the agent made (HTML, React, SVG, Mermaid diagram): opens
/// in the side panel; "Generating…" while it streams.
fn artifact_card<'a>(app: &'a App, item: &'a ItemView) -> Element<'a, Message> {
    let kind = if item.artifact_kind.is_empty() { "html" } else { item.artifact_kind.as_str() };
    let kind_label = match kind {
        "html" => "HTML page",
        "react" => "React component",
        "svg" => "SVG image",
        "mermaid" => "Diagram",
        other => other,
    };
    let title = if !item.artifact_title.is_empty() {
        item.artifact_title.as_str()
    } else if !item.title.is_empty() {
        item.title.as_str()
    } else {
        kind_label
    };
    let subtitle = if item.streaming {
        format!("Generating {}…", kind_label.to_lowercase())
    } else {
        let mut s = kind_label.to_owned();
        if item.artifact_version > 1 {
            s.push_str(&format!(" · v{}", item.artifact_version));
        }
        if item.artifact_hosted_url.is_some() {
            s.push_str(" · claude.ai");
        }
        s
    };
    let open = match (&item.artifact_id, app.vm.thread.as_ref()) {
        (Some(id), Some(t)) => core(Event::OpenArtifact {
            thread_id: t.id.clone(),
            artifact_id: id.clone(),
            version: (item.artifact_version > 0).then_some(item.artifact_version),
            dark: app.is_dark(),
        }),
        _ => core(Event::ShowPanel { tab: "artifacts".into() }),
    };
    let mut r = row![
        container(w::themed_icon(if item.streaming { Icon::Loader } else { Icon::LayoutDashboard }, ICON, |c| c.accent)).center(Length::Fixed(36.0)).style(|t: &Theme| container::Style {
            background: Some(Background::Color(alpha(colors(t).accent, 0.12))),
            border: Border { radius: RADIUS_CHIP.into(), ..Border::default() },
            ..Default::default()
        }),
        column![text(title).size(TEXT_UI).font(fonts::SEMIBOLD), w::muted(subtitle, TEXT_SMALL)].spacing(XXS).width(Length::Fill),
    ]
    .spacing(M)
    .align_y(Alignment::Center);
    if let Some(url) = &item.artifact_hosted_url {
        r = r.push(w::icon_button(Icon::ExternalLink, "Open on claude.ai", Some(Message::OpenUrl(url.clone()))));
    }
    r = r.push(w::themed_icon(Icon::ChevronRight, ICON_SMALL, |c| c.faint));
    button(r).padding(M).width(Length::Fill).style(w::secondary_style).on_press(open).into()
}
