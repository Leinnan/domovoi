//! Left sidebar: primary thread creation, search, status/project grouping,
//! unread filter, cleanly grouped thread sections with single-line right-click
//! rows, and a two-slot workspace/server footer.

use domovoi_core::Event;
use domovoi_core::view::{ProjectIconView, SidebarProjectView, SidebarSectionView, SidebarView, ThreadRowView};
use iced::widget::{Column, Space, button, column, container, mouse_area, row, stack, text};
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

    // Row 1: Primary creation group (New Thread + Scratch Thread).
    let new_thread_btn = w::with_tooltip(
        button(
            row![
                w::themed_icon(Icon::Plus, ICON_SMALL, |c| c.accent),
                text("New Thread").size(TEXT_UI).font(fonts::MEDIUM),
            ]
            .spacing(ICON_GAP)
            .align_y(Alignment::Center),
        )
        .width(Length::Fill)
        .padding([6.0, M])
        .style(w::secondary_style)
        .on_press(core(Event::NewThread { project_id: None })),
        &format!("New thread ({})", w::shortcut_label("Mod+N")),
    );
    let creation_bar = row![
        new_thread_btn,
        w::icon_button(
            Icon::MessageSquare,
            &format!("Scratch thread ({})", w::shortcut_label("Mod+Shift+N")),
            Some(core(Event::OpenScratch)),
        ),
    ]
    .spacing(XS)
    .align_y(Alignment::Center);

    // Row 2: Search threads input with inline clear when active.
    let mut search_bar = row![
        w::input("Search threads…", &app.search_input)
            .on_input(Message::SearchChanged)
            .width(Length::Fill),
    ]
    .spacing(XS)
    .align_y(Alignment::Center);
    if !app.search_input.is_empty() {
        search_bar = search_bar.push(w::icon_button_compact(
            Icon::X,
            "Clear search",
            Some(Message::SearchChanged(String::new())),
        ));
    }

    // Row 3: Grouping & filter group (Status/Project + Unread + Attention jump).
    let mut filter_bar = row![
        w::segmented(
            &[(true, Some(Icon::Inbox), "Status"), (false, Some(Icon::Folder), "Project")],
            by_status,
            |status| core(Event::SetSidebarGrouping { grouping: if status { "status" } else { "project" }.into() }),
        ),
        Space::new().width(Length::Fill),
        w::icon_button_tinted(
            Icon::Bell,
            if sb.unread_only { "Showing unread only" } else { "Filter unread & waiting" },
            Some(core(Event::SetUnreadOnly { on: !sb.unread_only })),
            sb.unread_only,
        ),
    ]
    .spacing(XS)
    .align_y(Alignment::Center);
    if sb.threads_needing_attention > 0 {
        let n = sb.threads_needing_attention;
        filter_bar = filter_bar.push(w::with_tooltip(
            button(icon_capsule(Icon::Bell, n.to_string(), |c| c.warning))
                .padding(0)
                .style(w::ghost_style)
                .on_press_maybe(next_needing_you(sb)),
            &format!("{n} {} you — go to the next one", if n == 1 { "thread needs" } else { "threads need" }),
        ));
    }

    let mut list = Column::new().spacing(SECTION_GAP).padding([S, S]);

    if !sb.search_query.is_empty() && !sb.search_results.is_empty() {
        let mut group = Column::new()
            .spacing(XXS)
            .push(group_header(Icon::Search, |c| c.accent, "Message matches", Some(sb.search_results.len())));
        for hit in &sb.search_results {
            let selected = sb.selected_thread_id.as_deref() == Some(hit.thread_id.as_str());
            group = group.push(
                w::list_row(
                    column![
                        container(text(&hit.title).size(TEXT_UI).font(fonts::MEDIUM).wrapping(text::Wrapping::None))
                            .width(Length::Fill)
                            .clip(true),
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
        list = list.push(group);
    }

    let mut listed_any = !sb.search_results.is_empty();
    if app.all_terminal_servers {
        listed_any = true;
        let mut group = Column::new()
            .spacing(XXS)
            .push(group_header(Icon::Globe, |c| c.info, "All Servers", Some(app.vm.connection.environments.len())));
        for environment in &app.vm.connection.environments {
            let server = app.terminal_servers.get(&environment.id);
            let state = match server {
                Some(server) if server.stale => " · Stale",
                Some(server) if server.loaded => "",
                _ => " · Loading",
            };
            group = group.push(
                container(w::muted(format!("{}{state}", environment.label), TEXT_CAPTION).font(fonts::SEMIBOLD))
                    .padding(Padding { top: XS, bottom: XXS, left: S, right: S }),
            );
            if let Some(server) = server {
                for terminal in &server.terminals {
                    let (status, tone): (&str, w::Tone) = match terminal.agent.as_ref().map(|agent| agent.activity.as_str()) {
                        Some("needsInput") => ("Needs input", |c| c.warning),
                        Some("working") => ("Working", |c| c.success),
                        Some("finished") => ("Finished", |c| c.info),
                        _ if terminal.exit_code.is_some_and(|code| code != 0) => ("Failed", |c| c.danger),
                        _ => continue,
                    };
                    let name = terminal.title.as_deref().unwrap_or(&terminal.label);
                    group = group.push(
                        w::list_row(
                            row![
                                w::themed_icon(Icon::Terminal, ICON_SMALL, tone),
                                container(text(name).size(TEXT_SMALL).wrapping(text::Wrapping::None))
                                    .width(Length::Fill)
                                    .clip(true),
                                w::badge_capsule(status, Some(tone)),
                            ]
                            .spacing(S)
                            .align_y(Alignment::Center),
                            false,
                            false,
                        )
                        .on_press_maybe((!server.stale).then(|| core(Event::OpenTerminalOnEnvironment {
                            environment_id: environment.id.clone(),
                            thread_id: terminal.thread_id.clone(),
                            terminal_id: terminal.terminal_id.clone(),
                        }))),
                    );
                }
            }
        }
        list = list.push(group);
    }
    if !app.all_terminal_servers && !sb.terminal_attention.is_empty() {
        listed_any = true;
        let mut group = Column::new()
            .spacing(XXS)
            .push(group_header(Icon::Terminal, |c| c.warning, "Terminal activity", Some(sb.terminal_attention.len())));
        for terminal in &sb.terminal_attention {
            let (status, tone): (&str, w::Tone) = match terminal.agent.as_ref().map(|agent| agent.activity.as_str()) {
                Some("needsInput") => ("Needs input", |c| c.warning),
                Some("finished") => ("Finished", |c| c.info),
                _ => ("Working", |c| c.success),
            };
            let name = terminal.title.as_deref().unwrap_or(&terminal.label);
            let mut row_content = row![
                w::themed_icon(Icon::Terminal, ICON_SMALL, tone),
                column![
                    container(text(name).size(TEXT_UI).font(fonts::MEDIUM).wrapping(text::Wrapping::None))
                        .width(Length::Fill)
                        .clip(true),
                    w::colored(status, TEXT_CAPTION, tone),
                ]
                .spacing(XXS)
                .width(Length::Fill),
            ]
            .spacing(S)
            .align_y(Alignment::Center);
            if let Some(thread_id) = &terminal.agent_thread_id {
                row_content = row_content.push(w::icon_button_compact(
                    Icon::MessageSquare,
                    "Open conversation",
                    Some(core(Event::SelectThread { thread_id: thread_id.clone() })),
                ));
            }
            group = group.push(
                w::list_row(row_content, false, true).on_press(core(Event::OpenTerminalAttention {
                    thread_id: terminal.thread_id.clone(),
                    terminal_id: terminal.terminal_id.clone(),
                })),
            );
        }
        list = list.push(group);
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
            let mut snoozed_group = Column::new()
                .spacing(XXS)
                .push(group_header(Icon::Clock, |c| c.info, "Snoozed", Some(sb.snoozed.len())));
            for t in &sb.snoozed {
                snoozed_group = snoozed_group.push(thread_row(app, t, true));
            }
            list = list.push(snoozed_group);
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

    // Two-slot footer: Workspace Add menu on the left, server scope toggle on the right.
    let add_menu_btn = button(
        row![
            w::themed_icon(Icon::Plus, ICON_SMALL, |c| c.accent),
            text("Add…").size(TEXT_SMALL).font(fonts::MEDIUM),
            w::themed_icon(Icon::ChevronDown, ICON_XS, |c| c.muted),
        ]
        .spacing(XS)
        .align_y(Alignment::Center),
    )
    .padding(SMALL_BUTTON_PAD)
    .style(w::secondary_style)
    .on_press(Message::Overlay(Some(Overlay::SidebarAddMenu)));

    let all_servers = app.all_terminal_servers;
    let servers_btn = button(
        row![
            w::themed_icon(if all_servers { Icon::Server } else { Icon::Globe }, ICON_SMALL, move |c| {
                if all_servers { c.accent } else { c.muted }
            }),
            text(if all_servers { "This Server" } else { "All Servers" }).size(TEXT_SMALL).font(fonts::MEDIUM),
        ]
        .spacing(XS)
        .align_y(Alignment::Center),
    )
    .padding(SMALL_BUTTON_PAD)
    .style(w::bar_chip_style(all_servers))
    .on_press(Message::ToggleAllTerminalServers);

    let footer = container(
        row![add_menu_btn, Space::new().width(Length::Fill), servers_btn]
            .spacing(S)
            .align_y(Alignment::Center),
    )
    .padding([S, CHROME_PAD_X])
    .width(Length::Fill);

    column![
        container(column![creation_bar, search_bar, filter_bar].spacing(S)).padding([M, CHROME_PAD_X]),
        w::divider(),
        w::scroll(list).height(Length::Fill),
        w::divider(),
        footer,
    ]
    .height(Length::Fill)
    .into()
}

/// Consistent non-collapsible section header matching `status_section` geometry.
fn group_header<'a>(ic: Icon, tone: w::Tone, title: &'a str, count: Option<usize>) -> Element<'a, Message> {
    let mut r = row![
        w::themed_icon(ic, ICON_SMALL, tone),
        text(title).size(TEXT_SMALL).font(fonts::SEMIBOLD).width(Length::Fill),
    ]
    .spacing(TIGHT)
    .align_y(Alignment::Center);
    if let Some(n) = count {
        r = r.push(w::badge_capsule(n.to_string(), None));
    }
    container(r).padding([TIGHT, S]).width(Length::Fill).into()
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
                .padding(Padding { left: SIDEBAR_ICON_COL + S, ..Padding::ZERO }),
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
                container(text(&proj.title).size(TEXT_UI).font(fonts::SEMIBOLD).wrapping(text::Wrapping::None))
                    .width(Length::Fill)
                    .clip(true),
            ]
            .spacing(TIGHT)
            .align_y(Alignment::Center),
        )
        .width(Length::Fill)
        .padding([TIGHT, S])
        .style(w::ghost_style)
        .on_press(core(Event::ToggleProject { project_id: proj.id.clone() })),
        w::icon_button_compact(
            Icon::Plus,
            "New thread in project",
            Some(core(Event::NewThread { project_id: Some(proj.id.clone()) })),
        ),
        w::icon_button_compact(
            Icon::Ellipsis,
            "Project options",
            Some(Message::Overlay(Some(Overlay::ProjectMenu(proj.id.clone())))),
        ),
    ]
    .spacing(XXS)
    .align_y(Alignment::Center);

    let mut col = Column::new().spacing(XXS).push(header);
    if proj.expanded {
        let mut nested = Column::new().spacing(XXS);
        for t in &proj.threads {
            nested = nested.push(thread_row(app, t, false));
        }
        if proj.hidden_count > 0 {
            nested = nested.push(
                container(w::small_button(
                    format!("Show {} more", proj.hidden_count),
                    w::ghost_style,
                    Some(core(Event::ShowAllThreads { project_id: proj.id.clone() })),
                ))
                .padding(Padding { left: SIDEBAR_STATUS_COL + S, ..Padding::ZERO }),
            );
        }
        if proj.earlier_count > 0 {
            let open = app.earlier_shown.contains(&proj.id);
            nested = nested.push(
                container(w::small_button(
                    if open {
                        format!("Hide earlier sessions ({})", proj.earlier_count)
                    } else {
                        format!("Earlier sessions ({})", proj.earlier_count)
                    },
                    w::ghost_style,
                    Some(Message::ToggleEarlier(proj.id.clone())),
                ))
                .padding(Padding { left: SIDEBAR_STATUS_COL + S, ..Padding::ZERO }),
            );
            if open {
                for t in &proj.earlier {
                    nested = nested.push(thread_row(app, t, false));
                }
            }
        }
        col = col.push(container(nested).padding(Padding { left: M, ..Padding::ZERO }).width(Length::Fill));
    }
    col.into()
}

/// Project avatar tile with an optional status dot badged cleanly at the bottom-right corner.
fn project_avatar_badged<'a>(
    icon_view: &'a ProjectIconView,
    status_tone: Option<w::Tone>,
    images: &'a std::collections::HashMap<String, iced::widget::image::Handle>,
) -> Element<'a, Message> {
    let tile = w::project_icon_tile(icon_view, SIDEBAR_ICON_COL, images);
    let Some(tone) = status_tone else {
        return container(tile).width(SIDEBAR_ICON_COL).height(SIDEBAR_ICON_COL).into();
    };
    let badge = container(w::dot(tone, 7.0))
        .padding(1.5)
        .style(|t: &Theme| container::Style {
            background: Some(Background::Color(colors(t).surface)),
            border: Border { radius: RADIUS_PILL.into(), ..Border::default() },
            ..Default::default()
        });
    stack![
        tile,
        container(badge)
            .width(SIDEBAR_ICON_COL)
            .height(SIDEBAR_ICON_COL)
            .align_x(Alignment::End)
            .align_y(Alignment::End),
    ]
    .width(SIDEBAR_ICON_COL)
    .height(SIDEBAR_ICON_COL)
    .into()
}

fn thread_row<'a>(app: &'a App, t: &'a ThreadRowView, show_project: bool) -> Element<'a, Message> {
    let selected = app.vm.sidebar.selected_thread_id.as_deref() == Some(t.id.as_str());
    let title = if t.title.is_empty() { "New thread" } else { t.title.as_str() };

    let planning = t.plan_mode && t.status == "working";
    let working_tone: w::Tone = if planning { |c| c.violet } else { |c| c.success };
    let now = now_ms();

    let status_tone: Option<w::Tone> = match t.status.as_str() {
        "working" => Some(working_tone),
        "approval" => Some(if t.question { |c| c.info } else { |c| c.warning }),
        "failed" => Some(|c| c.danger),
        _ if t.unread => Some(|c| c.accent),
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
    title_line = title_line.push(
        container(
            text(title)
                .size(TEXT_UI)
                .font(if t.unread || selected { fonts::SEMIBOLD } else { fonts::INTER })
                .wrapping(text::Wrapping::None),
        )
        .width(Length::Fill)
        .clip(true),
    );
    if snoozed {
        title_line = title_line.push(w::with_tooltip(w::themed_icon(Icon::Clock, ICON_XS, |c| c.info), "Snoozed"));
    }
    title_line = title_line.push(right_el);

    let mut sub_line = row![
        container(w::muted(meta, TEXT_CAPTION).wrapping(text::Wrapping::None))
            .width(Length::Fill)
            .clip(true),
    ]
    .spacing(XS)
    .align_y(Alignment::Center);
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

    // Fixed-width leading indicator column keeps all thread titles aligned vertically.
    let leading: Element<'a, Message> = if show_project {
        project_avatar_badged(&t.project_icon, status_tone, &app.images)
    } else {
        let mark: Element<'a, Message> = match status_tone {
            Some(tone) => w::dot(tone, 7.0),
            None => Space::new().into(),
        };
        container(mark).width(SIDEBAR_STATUS_COL).center_x(SIDEBAR_STATUS_COL).into()
    };

    let row_body = row![leading, column![title_line, sub_line].spacing(XXS).width(Length::Fill)]
        .spacing(S)
        .align_y(Alignment::Center);

    let row_btn = w::list_row(row_body, selected, true).on_press(core(Event::SelectThread { thread_id: t.id.clone() }));
    mouse_area(row_btn)
        .on_right_press(Message::Overlay(Some(Overlay::ThreadMenu(t.id.clone()))))
        .into()
}
