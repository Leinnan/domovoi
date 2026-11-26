//! Left sidebar: thread search, unread filter, status/project grouping,
//! thread rows with status badges, and project/session creation actions.

use domovoi_core::Event;
use domovoi_core::view::{SidebarProjectView, SidebarSectionView, SidebarView, ThreadRowView};
use iced::widget::{Column, Space, button, column, container, row, text};
use iced::{Alignment, Background, Border, Element, Length, Padding, Theme};

use crate::app::{App, Message, Overlay, now_ms};
use crate::ui::fonts;
use crate::ui::icons::Icon;
use crate::ui::theme::{alpha, colors};
use crate::ui::tokens::*;
use crate::ui::widgets as w;

fn core(e: Event) -> Message {
    Message::Core(Box::new(e))
}

pub fn view(app: &App) -> Element<'_, Message> {
    let sb = &app.vm.sidebar;
    let by_status = sb.grouping != "project";

    let search_bar = row![
        w::input("Search threads…", &app.search_input)
            .on_input(Message::SearchChanged)
            .width(Length::Fill),
        w::icon_button_tinted(
            Icon::Bell,
            if sb.unread_only { "Showing unread only" } else { "Filter unread & waiting" },
            Some(core(Event::SetUnreadOnly { on: !sb.unread_only })),
            sb.unread_only,
        ),
    ]
    .spacing(XS)
    .align_y(Alignment::Center);

    let mut grouping_bar = row![
        w::segmented(
            &[(true, Some(Icon::Inbox), "Status"), (false, Some(Icon::Folder), "Project")],
            by_status,
            |status| core(Event::SetSidebarGrouping { grouping: if status { "status" } else { "project" }.into() }),
        ),
        Space::new().width(Length::Fill),
    ]
    .spacing(XS)
    .align_y(Alignment::Center);
    if sb.threads_needing_attention > 0 {
        let n = sb.threads_needing_attention;
        grouping_bar = grouping_bar.push(w::with_tooltip(
            button(icon_capsule(Icon::Bell, n.to_string(), |c| c.warning))
                .padding(0)
                .style(w::ghost_style)
                .on_press_maybe(next_needing_you(sb)),
            &format!("{n} {} you — go to the next one", if n == 1 { "thread needs" } else { "threads need" }),
        ));
    }
    let grouping_bar = grouping_bar.push(row![
        w::icon_button(Icon::Plus, "New thread (Mod+N)", Some(core(Event::NewThread { project_id: None }))),
        w::icon_button(Icon::MessageSquare, "Scratch thread (Mod+Shift+N)", Some(core(Event::OpenScratch))),
    ]
    .spacing(XS)
    .align_y(Alignment::Center));

    let mut list = Column::new().spacing(XXS).padding([0.0, XS]);

    if !sb.search_query.is_empty() && !sb.search_results.is_empty() {
        list = list.push(w::section("Message matches"));
        for hit in &sb.search_results {
            let selected = sb.selected_thread_id.as_deref() == Some(hit.thread_id.as_str());
            list = list.push(
                w::list_row(
                    column![
                        text(&hit.title).size(TEXT_UI).font(fonts::MEDIUM),
                        w::muted(&hit.snippet, TEXT_SMALL),
                        w::faint(&hit.project_title, TEXT_CAPTION),
                    ]
                    .spacing(XXS)
                    .width(Length::Fill),
                    selected,
                    true,
                )
                .on_press(core(Event::SelectThread { thread_id: hit.thread_id.clone() })),
            );
        }
    }

    let mut listed_any = !sb.search_results.is_empty();
    if app.all_terminal_servers {
        listed_any = true;
        list = list.push(w::section("All Servers"));
        for environment in &app.vm.connection.environments {
            let server = app.terminal_servers.get(&environment.id);
            let state = match server {
                Some(server) if server.stale => " · Stale",
                Some(server) if server.loaded => "",
                _ => " · Loading",
            };
            list = list.push(w::muted(format!("{}{state}", environment.label), TEXT_SMALL));
            if let Some(server) = server {
                for terminal in &server.terminals {
                    let status = match terminal.agent.as_ref().map(|agent| agent.activity.as_str()) {
                        Some("needsInput") => "Needs input",
                        Some("working") => "Working",
                        Some("finished") => "Finished",
                        _ if terminal.exit_code.is_some_and(|code| code != 0) => "Process failed",
                        _ => continue,
                    };
                    list = list.push(w::small_button(
                        format!("{} · {status}", terminal.title.as_deref().unwrap_or(&terminal.label)),
                        w::ghost_style,
                        (!server.stale).then(|| core(Event::OpenTerminalOnEnvironment {
                            environment_id: environment.id.clone(), thread_id: terminal.thread_id.clone(), terminal_id: terminal.terminal_id.clone(),
                        })),
                    ));
                }
            }
        }
    }
    if !app.all_terminal_servers && !sb.terminal_attention.is_empty() {
        listed_any = true;
        list = list.push(w::section("Terminal activity"));
        for terminal in &sb.terminal_attention {
            let status = match terminal.agent.as_ref().map(|agent| agent.activity.as_str()) {
                Some("needsInput") => "Needs input",
                Some("finished") => "Finished",
                _ => "Working",
            };
            list = list.push(w::list_row(
                column![
                    text(terminal.title.as_deref().unwrap_or(&terminal.label)).size(TEXT_UI),
                    w::muted(status, TEXT_SMALL),
                ].width(Length::Fill),
                false,
                true,
            ).on_press(core(Event::OpenTerminalAttention { thread_id: terminal.thread_id.clone(), terminal_id: terminal.terminal_id.clone() })));
            if let Some(thread_id) = &terminal.agent_thread_id {
                list = list.push(w::small_button("Open conversation", w::ghost_style,
                    Some(core(Event::SelectThread { thread_id: thread_id.clone() }))));
            }
        }
    }

    if by_status {
        for sec in &sb.sections {
            listed_any = true;
            list = list.push(status_section(app, sec));
        }
    } else {
        for proj in &sb.projects {
            listed_any = true;
            list = list.push(project_section(app, proj));
        }
        if !sb.snoozed.is_empty() && sb.search_query.is_empty() {
            listed_any = true;
            list = list.push(w::section("Snoozed"));
            for t in &sb.snoozed {
                list = list.push(thread_row(app, t, true));
            }
        }
    }

    if !listed_any {
        let note = if !sb.search_query.is_empty() {
            format!("No threads match “{}”.", sb.search_query)
        } else if sb.unread_only {
            "All caught up — no unread threads.".into()
        } else {
            "No threads yet. Start a new thread or add a project.".into()
        };
        list = list.push(container(w::faint(note, TEXT_SMALL)).padding([L, M]));
    }

    let footer = container(
        row![
            w::small_button(
                "+ Project",
                w::ghost_style,
                Some(Message::Overlay(Some(Overlay::NewProject { name: String::new(), path: String::new() }))),
            ),
            w::small_button(if app.all_terminal_servers { "This Server" } else { "All Servers" }, w::ghost_style, Some(Message::ToggleAllTerminalServers)),
            w::small_button("Browse", w::ghost_style, Some(Message::Overlay(Some(Overlay::FolderBrowser)))),
            w::small_button(
                "Import",
                w::ghost_style,
                Some(Message::Overlay(Some(Overlay::ImportSessions { selected: Vec::new() }))),
            ),
        ]
        .spacing(XS)
        .align_y(Alignment::Center),
    )
    .padding([TIGHT, S])
    .width(Length::Fill);

    column![
        container(column![search_bar, grouping_bar].spacing(S)).padding([S, S]),
        w::divider(),
        w::scroll(list).height(Length::Fill),
        w::divider(),
        footer,
    ]
    .height(Length::Fill)
    .into()
}

/// The next thread waiting on you after the selected one (wrapping around).
fn next_needing_you(sb: &SidebarView) -> Option<Message> {
    let waiting: Vec<&ThreadRowView> = sb.surface_threads.iter().filter(|t| t.status == "approval").collect();
    let current = waiting.iter().position(|t| sb.selected_thread_id.as_deref() == Some(t.id.as_str()));
    let next = waiting.get(current.map_or(0, |i| (i + 1) % waiting.len()))?;
    Some(core(Event::SelectThread { thread_id: next.id.clone() }))
}

/// A tinted capsule with a leading icon (a pull request's state, the waiting count).
fn icon_capsule<'a>(i: Icon, label: String, tone: w::Tone) -> Element<'a, Message> {
    container(
        row![
            w::themed_icon(i, ICON_XS, tone),
            text(label).size(TEXT_CAPTION).font(fonts::MEDIUM).style(move |t: &Theme| text::Style { color: Some(tone(colors(t))) }),
        ]
        .spacing(XXS)
        .align_y(Alignment::Center),
    )
    .padding([2.0, TIGHT])
    .style(move |t: &Theme| container::Style {
        background: Some(Background::Color(alpha(tone(colors(t)), 0.16))),
        border: Border { radius: RADIUS_PILL.into(), ..Border::default() },
        ..Default::default()
    })
    .into()
}

/// A span of time at minute granularity: `3m`, `2h`, `4d` (empty under a minute).
/// Elapsed time rounds down; a countdown (`round_up`) rounds up, so 1h59m left reads `2h`.
fn short_span(ms: i64, round_up: bool) -> String {
    let unit = |u: i64| if round_up { (ms + u - 1) / u } else { ms / u };
    match ms / 60_000 {
        ..1 => String::new(),
        1..60 => format!("{}m", unit(60_000)),
        60..1440 => format!("{}h", unit(3_600_000)),
        _ => format!("{}d", unit(86_400_000)),
    }
}

/// A pull request state's icon and tint, as GitHub colours it.
fn pull_request_look(state: &str) -> (Icon, w::Tone) {
    match state {
        "open" => (Icon::GitPullRequest, |c| c.success),
        "merged" => (Icon::GitMerge, |c| c.violet),
        "closed" => (Icon::CircleX, |c| c.danger),
        _ => (Icon::GitPullRequest, |c| c.muted),
    }
}

fn status_section<'a>(app: &'a App, sec: &'a SidebarSectionView) -> Element<'a, Message> {
    let (ic, tone): (Icon, w::Tone) = match sec.kind.as_str() {
        "inbox" => (Icon::Inbox, |c| c.accent),
        "running" => (Icon::Play, |c| c.success),
        "inactive" => (Icon::Clock, |c| c.info),
        "archived" => (Icon::Archive, |c| c.muted),
        _ => (Icon::Bell, |c| c.warning),
    };
    let chev = if sec.expanded { Icon::ChevronDown } else { Icon::ChevronRight };
    let header = button(
        row![
            w::themed_icon(chev, ICON_XS, |c| c.faint),
            w::themed_icon(ic, ICON_SMALL, tone),
            text(&sec.title).size(TEXT_SMALL).font(fonts::SEMIBOLD).width(Length::Fill),
            w::badge_capsule(sec.count.to_string(), None),
        ]
        .spacing(TIGHT)
        .align_y(Alignment::Center),
    )
    .width(Length::Fill)
    .padding([TIGHT, S])
    .style(w::ghost_style)
    .on_press(core(Event::ToggleSection { section: sec.kind.clone() }));

    let mut col = Column::new().spacing(XXS).push(header);
    if sec.expanded {
        for t in &sec.rows {
            col = col.push(thread_row(app, t, true));
        }
        if sec.hidden_count > 0 {
            col = col.push(
                container(w::small_button(
                    format!("Show {} more", sec.hidden_count),
                    w::ghost_style,
                    Some(core(Event::ShowAllInSection { section: sec.kind.clone() })),
                ))
                .padding(Padding { left: L, ..Padding::ZERO }),
            );
        }
    }
    col.into()
}

fn project_section<'a>(app: &'a App, proj: &'a SidebarProjectView) -> Element<'a, Message> {
    let chev = if proj.expanded { Icon::ChevronDown } else { Icon::ChevronRight };
    let header = row![
        button(
            row![
                w::themed_icon(chev, ICON_XS, |c| c.faint),
                w::project_icon_tile(&proj.icon, 20.0, &app.images),
                text(&proj.title).size(TEXT_UI).font(fonts::SEMIBOLD).width(Length::Fill),
            ]
            .spacing(TIGHT)
            .align_y(Alignment::Center),
        )
        .width(Length::Fill)
        .padding([TIGHT, S])
        .style(w::ghost_style)
        .on_press(core(Event::ToggleProject { project_id: proj.id.clone() })),
        w::icon_button(
            Icon::Plus,
            "New thread in project",
            Some(core(Event::NewThread { project_id: Some(proj.id.clone()) })),
        ),
        w::icon_button(
            Icon::Ellipsis,
            "Project options",
            Some(Message::Overlay(Some(Overlay::ProjectMenu(proj.id.clone())))),
        ),
    ]
    .spacing(XXS)
    .align_y(Alignment::Center);

    let mut col = Column::new().spacing(XXS).push(header);
    if proj.expanded {
        for t in &proj.threads {
            col = col.push(thread_row(app, t, false));
        }
        if proj.hidden_count > 0 {
            col = col.push(
                container(w::small_button(
                    format!("Show {} more", proj.hidden_count),
                    w::ghost_style,
                    Some(core(Event::ShowAllThreads { project_id: proj.id.clone() })),
                ))
                .padding(Padding { left: L, ..Padding::ZERO }),
            );
        }
        if proj.earlier_count > 0 {
            let open = app.earlier_shown.contains(&proj.id);
            col = col.push(
                container(w::small_button(
                    if open {
                        format!("Hide earlier sessions ({})", proj.earlier_count)
                    } else {
                        format!("Earlier sessions ({})", proj.earlier_count)
                    },
                    w::ghost_style,
                    Some(Message::ToggleEarlier(proj.id.clone())),
                ))
                .padding(Padding { left: L, ..Padding::ZERO }),
            );
            if open {
                for t in &proj.earlier {
                    col = col.push(thread_row(app, t, false));
                }
            }
        }
    }
    col.into()
}

fn thread_row<'a>(app: &'a App, t: &'a ThreadRowView, show_project: bool) -> Element<'a, Message> {
    let selected = app.vm.sidebar.selected_thread_id.as_deref() == Some(t.id.as_str());
    let title = if t.title.is_empty() { "New thread" } else { t.title.as_str() };

    let planning = t.plan_mode && t.status == "working";
    let working_tone: w::Tone = if planning { |c| c.violet } else { |c| c.success };
    let now = now_ms();

    let status_dot: Option<Element<'a, Message>> = match t.status.as_str() {
        "working" => Some(w::dot(working_tone, 7.0)),
        "approval" => Some(w::dot(move |c: &crate::ui::theme::Hearth| if t.question { c.info } else { c.warning }, 7.0)),
        "failed" => Some(w::dot(|c| c.danger, 7.0)),
        _ if t.unread => Some(w::dot(|c| c.accent, 6.0)),
        _ => None,
    };

    let mut meta_parts: Vec<String> = Vec::new();
    if show_project && !t.project_title.is_empty() {
        meta_parts.push(t.project_title.clone());
    }
    if !t.model_label.is_empty() {
        meta_parts.push(t.model_label.clone());
    }
    if let Some(branch) = &t.branch {
        meta_parts.push(format!("⑂ {branch}"));
    }
    let meta = meta_parts.join(" · ");

    let snoozed = t.snoozed_until_ms > 0;
    // The core's label ages only when it renders; count a running turn here so the 5 s tick keeps it current.
    let right_label = if t.status == "working" && t.working_since_ms > 0 {
        let doing = if planning { "Planning" } else { "Working" };
        match short_span(now - t.working_since_ms, false) {
            elapsed if elapsed.is_empty() => doing.to_owned(),
            elapsed => format!("{doing} {elapsed}"),
        }
    } else if !t.status_label.is_empty() {
        t.status_label.clone()
    } else if snoozed {
        match short_span(t.snoozed_until_ms - now, true) {
            left if left.is_empty() => "Waking".to_owned(),
            left => format!("Wakes in {left}"),
        }
    } else {
        t.age.clone()
    };
    let right_el: Element<'a, Message> = match t.status.as_str() {
        "approval" => w::colored(right_label, TEXT_CAPTION, if t.question { |c| c.info } else { |c| c.warning }).into(),
        "working" => w::colored(right_label, TEXT_CAPTION, working_tone).font(if planning { fonts::MEDIUM } else { fonts::INTER }).into(),
        "failed" => w::colored(right_label, TEXT_CAPTION, |c| c.danger).into(),
        _ if snoozed => w::colored(right_label, TEXT_CAPTION, |c| c.info).into(),
        _ => w::faint(right_label, TEXT_CAPTION).into(),
    };

    let mut title_line = row![].spacing(XS).align_y(Alignment::Center);
    if t.pinned {
        title_line = title_line.push(w::themed_icon(Icon::Pin, ICON_XS, |c| c.accent));
    }
    title_line = title_line
        .push(text(title).size(TEXT_UI).font(if t.unread || selected { fonts::SEMIBOLD } else { fonts::INTER }).width(Length::Fill));
    if snoozed {
        title_line = title_line.push(w::with_tooltip(w::themed_icon(Icon::Clock, ICON_XS, |c| c.info), "Snoozed"));
    }
    title_line = title_line.push(right_el);

    let mut sub_line = row![container(w::muted(meta, TEXT_CAPTION).wrapping(text::Wrapping::None)).width(Length::Fill).clip(true)].spacing(XS).align_y(Alignment::Center);
    if t.plan_ready {
        sub_line = sub_line.push(w::badge_capsule("Plan", Some(|c| c.violet)));
    }
    if !t.pull_request.is_empty() {
        let (ic, tone) = pull_request_look(&t.pull_request_state);
        let tip = if t.pull_request_state.is_empty() {
            format!("Pull request {}", t.pull_request)
        } else {
            format!("Pull request {}, {}", t.pull_request, t.pull_request_state)
        };
        sub_line = sub_line.push(w::with_tooltip(icon_capsule(ic, t.pull_request.clone(), tone), &tip));
    }
    if t.relation == "fork" {
        sub_line = sub_line.push(w::with_tooltip(w::themed_icon(Icon::GitBranch, ICON_XS, |c| c.muted), "Forked from another thread"));
    }
    if t.in_worktree {
        sub_line = sub_line.push(w::with_tooltip(w::themed_icon(Icon::Layers, ICON_XS, |c| c.muted), "In its own worktree"));
    }
    if !t.source.is_empty() {
        sub_line = sub_line.push(w::badge_capsule(t.source.clone(), None));
    }

    let mut row_body = row![].spacing(S).align_y(Alignment::Center);
    if show_project {
        row_body = row_body.push(w::project_icon_tile(&t.project_icon, 24.0, &app.images));
    }
    if let Some(dot) = status_dot {
        row_body = row_body.push(dot);
    }
    row_body = row_body.push(column![title_line, sub_line].spacing(XXS).width(Length::Fill));

    row![
        w::list_row(row_body, selected, true).on_press(core(Event::SelectThread { thread_id: t.id.clone() })),
        w::icon_button(
            Icon::Ellipsis,
            "Thread actions",
            Some(Message::Overlay(Some(Overlay::ThreadMenu(t.id.clone())))),
        ),
    ]
    .spacing(XXS)
    .align_y(Alignment::Center)
    .into()
}
