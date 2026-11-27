//! The thread list: by status (Inbox, Running, Inactive, Archived) or under
//! each project, as the core groups it, with the server's name on top.
//!
//! Above the list: the server (pressed, the servers to switch to), how the
//! threads are grouped, Settings and New thread, then a search (thread names,
//! and the server's message matches) and the unread filter. Each thread and
//! project has a menu on the button at its right; what a choice does is in
//! `crate::menus` and the window.

use std::collections::HashSet;
use std::rc::Rc;

use domovoi_core::Event;
use domovoi_core::view::{ConnectionView, ProjectIconView, SidebarProjectView, SidebarSectionView, SidebarView, ThreadPageView, ThreadRowView};
use domovoi_desktop_host::palette;
use windows_reactor::{
    Border, Button, ButtonStyle, Callback, FontWeight, Grid, GridLength, HorizontalAlignment, Image, Key, Menu, MenuExt as _, MenuItem, Orientation, ScrollBarVisibility,
    ScrollViewer, StackPanel, Stretch, TextBlock, TextBox, TextTrimming, TextWrapping, Thickness, TooltipExt as _, VerticalAlignment, View,
};

use crate::companion::{self, Mood};
use crate::core_link::CoreLink;
use crate::media::Media;
use crate::{theme, widgets};

/// What draws a project's icon: the pictures loaded so far, and whether the
/// window is dark (an icon's colour differs by theme).
#[derive(Clone, Copy)]
pub struct Icons<'a> {
    pub media: &'a Media,
    pub dark: bool,
}

impl Icons<'_> {
    pub fn tile(self, icon: &ProjectIconView, size: f64) -> View {
        widgets::project_icon_tile(icon, size, self.media, self.dark)
    }
}

/// What the list tells the window.
pub struct Hooks {
    pub on_settings: Callback<()>,
    /// The search box's text.
    pub on_search: Callback<Rc<str>>,
    /// A choice from a menu, or a button the window acts on: `t-rename:<thread id>`, `earlier:<project id>`, …
    pub on_menu: Callback<Key>,
}

/// What the window keeps for the list.
pub struct Local<'a> {
    /// The search as typed (the window's own copy).
    pub search: &'a str,
    /// The projects whose earlier sessions are shown.
    pub earlier: &'a HashSet<String>,
    /// The time, for how long a thread has worked and when a snoozed one wakes.
    pub now_ms: i64,
    /// The open thread's page: what only it knows goes into that thread's menu.
    pub open: Option<&'a ThreadPageView>,
    /// How the server's companion feels now.
    pub mood: Mood,
}

/// What a row needs besides its thread.
#[derive(Clone, Copy)]
struct Rows<'a> {
    core: &'a CoreLink,
    selected: Option<&'a str>,
    local: &'a Local<'a>,
    on_menu: &'a Callback<Key>,
    dark: bool,
}

pub fn sidebar(core: &CoreLink, connection: &ConnectionView, sidebar: &SidebarView, icons: Icons<'_>, local: &Local<'_>, hooks: Hooks) -> View {
    let rows = Rows { core, selected: sidebar.selected_thread_id.as_deref(), local, on_menu: &hooks.on_menu, dark: icons.dark };
    let by_project = sidebar.grouping == "project";
    let searching = !sidebar.search_query.is_empty();
    let mut parts: Vec<View> = Vec::new();

    if searching && !sidebar.search_results.is_empty() {
        parts.push(heading("Message matches", Some(sidebar.search_results.len() as u32), None, None, None));
        for hit in &sidebar.search_results {
            let (core, id) = (core.clone(), hit.thread_id.clone());
            let lines: Vec<View> = [
                Some(TextBlock::new().text(&hit.title).text_trimming(TextTrimming::CharacterEllipsis).into()),
                (!hit.snippet.is_empty()).then(|| caption(&hit.snippet).max_lines(2).text_wrapping(TextWrapping::WrapWholeWords).into()),
                (!hit.project_title.is_empty()).then(|| caption(&hit.project_title).opacity(0.5).into()),
            ]
            .into_iter()
            .flatten()
            .collect();
            let button = Button::new()
                .style(ButtonStyle::Subtle)
                .horizontal_alignment(HorizontalAlignment::Stretch)
                .horizontal_content_alignment(HorizontalAlignment::Stretch)
                .automation_name(format!("Match in {}", hit.title))
                .content(StackPanel::new().orientation(Orientation::Vertical).children(lines))
                .on_click(move || core.send(Event::SelectThread { thread_id: id.clone() }));
            parts.push(if rows.selected == Some(hit.thread_id.as_str()) { button.background(theme::selected()) } else { button }.into());
        }
    }
    let mut listed = !parts.is_empty();

    if by_project {
        for project in &sidebar.projects {
            listed = true;
            project_section(&mut parts, project, icons, rows);
        }
        if !sidebar.snoozed.is_empty() && !searching {
            listed = true;
            parts.push(heading("Snoozed", Some(sidebar.snoozed.len() as u32), None, None, None));
            parts.extend(sidebar.snoozed.iter().map(|row| thread_row(row, Some(icons), rows)));
        }
    } else {
        for section in &sidebar.sections {
            listed = true;
            status_section(&mut parts, section, icons, rows);
        }
    }
    if !listed {
        let note = if searching {
            format!("No threads match “{}”.", sidebar.search_query)
        } else if sidebar.unread_only {
            "All caught up: no unread threads.".to_owned()
        } else {
            "No threads yet".to_owned()
        };
        parts.push(TextBlock::new().text(note).opacity(0.6).text_wrapping(TextWrapping::WrapWholeWords).margin(Thickness::uniform(12.0)).into());
    }

    Grid::new()
        .rows([GridLength::Auto, GridLength::STAR])
        .children((
            StackPanel::new()
                .orientation(Orientation::Vertical)
                .spacing(8.0)
                .margin(Thickness::uniform(6.0))
                .children(vec![header(core, connection, &sidebar.grouping, (local.mood, icons.dark), &hooks), filters(core, sidebar, local.search, &hooks)]),
            ScrollViewer::new()
                .grid_row(1)
                .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
                .horizontal_scroll_bar_visibility(ScrollBarVisibility::Disabled)
                .content(StackPanel::new().orientation(Orientation::Vertical).spacing(1.0).margin(Thickness::new(6.0, 0.0, 6.0, 6.0)).children(parts)),
        ))
        .into()
}

/// The server's name and state (pressed: the servers this device knows), how
/// the threads are grouped, Settings and New thread.
fn header(core: &CoreLink, connection: &ConnectionView, grouping: &str, face: (Mood, bool), hooks: &Hooks) -> View {
    let state = match connection.state.as_str() {
        "connected" if connection.remote => format!("Connected over {}", if connection.path == "direct" { "a direct link" } else { "a relay" }),
        "connected" => "Connected".to_owned(),
        "reconnecting" if connection.reconnect_in_seconds > 0 => format!("Reconnecting in {} s", connection.reconnect_in_seconds),
        other => capitalized(other),
    };
    let state = if connection.showing_saved { format!("{state} · showing the saved list") } else { state };
    let label = if connection.environment_label.is_empty() { "Domovoi" } else { connection.environment_label.as_str() };
    let mut servers: Vec<MenuItem> =
        connection.environments.iter().map(|server| MenuItem::item(format!("env-select:{}", server.id), format!("{}{}", if server.active { "✓  " } else { "     " }, server.label))).collect();
    if !servers.is_empty() {
        servers.push(MenuItem::separator("separator"));
    }
    servers.push(MenuItem::item("env-add:", "Add Server…"));
    servers.push(MenuItem::item("settings:servers", "Manage Servers…"));
    let server = Button::new()
        .style(ButtonStyle::Subtle)
        .horizontal_alignment(HorizontalAlignment::Stretch)
        .horizontal_content_alignment(HorizontalAlignment::Left)
        .automation_name(format!("Server: {label}"))
        .content(Grid::new().columns([GridLength::Auto, GridLength::STAR]).column_spacing(8.0).children((
            // The server's companion: how it is doing, at a glance.
            Image::new().source_svg(companion::image(&connection.companion.look, face.0, face.1)).stretch(Stretch::Uniform).width(28.0).height(28.0).vertical_alignment(VerticalAlignment::Center),
            StackPanel::new().grid_column(1).orientation(Orientation::Vertical).children(vec![
                TextBlock::new().text(label).font_weight(FontWeight::SEMI_BOLD).text_trimming(TextTrimming::CharacterEllipsis).into(),
                TextBlock::new().text(state).font_size(theme::CAPTION).opacity(0.7).text_trimming(TextTrimming::CharacterEllipsis).into(),
            ]),
        )))
        .menu(Menu::new(servers, hooks.on_menu.clone()));

    let settings = {
        let on_settings = hooks.on_settings.clone();
        icon_button("\u{E713}", "Settings").on_click(move || on_settings.call(())).grid_column(1).tooltip("Settings (Ctrl+,)")
    };
    let new_thread = {
        let core = core.clone();
        icon_button("\u{E710}", "New thread").on_click(move || core.send(Event::NewThread { project_id: None })).grid_column(2).tooltip("New thread (Ctrl+N)")
    };
    // What has no place of its own in the window: how the threads are grouped, the palette's searches and the pages beside the threads.
    let regroup = if grouping == "project" { MenuItem::item("group:status", "Group Threads by Status") } else { MenuItem::item("group:project", "Group Threads by Project") };
    let more = icon_button("\u{E712}", "More").grid_column(3).menu(Menu::new(
        vec![
            regroup,
            MenuItem::separator("separator-1"),
            MenuItem::item("go:palette", "Command Palette…  (Ctrl+K)"),
            MenuItem::item("go:files", "Go to File…  (Ctrl+P)"),
            MenuItem::item("go:content", "Search in Project…  (Ctrl+Shift+F)"),
            MenuItem::separator("separator-2"),
            MenuItem::item("route:issues", "Issues"),
            MenuItem::item("route:pull-requests", "Pull Requests"),
            MenuItem::item("route:usage", "Usage"),
        ],
        hooks.on_menu.clone(),
    ));
    Grid::new().columns([GridLength::STAR, GridLength::Auto, GridLength::Auto, GridLength::Auto]).children((server, settings, new_thread, more)).into()
}

/// The search, the unread filter, and how many threads wait on you.
fn filters(core: &CoreLink, sidebar: &SidebarView, search: &str, hooks: &Hooks) -> View {
    let unread = {
        let (core, on) = (core.clone(), sidebar.unread_only);
        let label = if on { "Showing unread only" } else { "Show unread and waiting only" };
        Button::new()
            .style(if on { ButtonStyle::Accent } else { ButtonStyle::Subtle })
            .automation_name(label)
            .content(theme::glyph("\u{EA8F}", 14.0))
            .on_click(move || core.send(Event::SetUnreadOnly { on: !on }))
            .grid_column(1)
            .tooltip(label)
    };
    let mut parts: Vec<View> = vec![
        TextBox::new().automation_name("Search threads").placeholder_text("Search threads").text(search).on_text_changed(hooks.on_search.clone()).into(),
        unread,
    ];
    let waiting = sidebar.threads_needing_attention;
    if waiting > 0 {
        let on_menu = hooks.on_menu.clone();
        let tip = format!("{waiting} {} you: go to the next one", if waiting == 1 { "thread needs" } else { "threads need" });
        parts.push(
            Button::new()
                .style(ButtonStyle::Subtle)
                .automation_name(&tip)
                .content(StackPanel::new().orientation(Orientation::Horizontal).spacing(4.0).children(vec![
                    theme::glyph("\u{E7BA}", 12.0).foreground(theme::accent()).into(),
                    TextBlock::new().text(waiting.to_string()).foreground(theme::accent()).font_weight(FontWeight::SEMI_BOLD).into(),
                ]))
                .on_click(move || on_menu.call("next-waiting:".into()))
                .grid_column(2)
                .tooltip(&tip),
        );
    }
    Grid::new().columns([GridLength::STAR, GridLength::Auto, GridLength::Auto]).column_spacing(4.0).children(parts).into()
}

/// The next thread waiting on you after the selected one, going round.
pub fn next_waiting(sidebar: &SidebarView) -> Option<&str> {
    let waiting: Vec<&ThreadRowView> = sidebar.surface_threads.iter().filter(|t| t.status == "approval").collect();
    let current = waiting.iter().position(|t| sidebar.selected_thread_id.as_deref() == Some(t.id.as_str()));
    waiting.get(current.map_or(0, |at| (at + 1) % waiting.len())).map(|t| t.id.as_str())
}

/// The thread `id` wherever the list has it (a thread can be in more than one place).
pub fn find_row<'a>(sidebar: &'a SidebarView, id: &str) -> Option<&'a ThreadRowView> {
    let in_sections = sidebar.sections.iter().flat_map(|s| &s.rows);
    let in_projects = sidebar.projects.iter().flat_map(|p| p.threads.iter().chain(&p.earlier));
    in_sections.chain(in_projects).chain(&sidebar.snoozed).chain(&sidebar.settled).chain(&sidebar.surface_threads).find(|row| row.id == id)
}

fn status_section(parts: &mut Vec<View>, section: &SidebarSectionView, icons: Icons<'_>, rows: Rows<'_>) {
    let toggle = {
        let (core, kind) = (rows.core.clone(), section.kind.clone());
        move || core.send(Event::ToggleSection { section: kind.clone() })
    };
    parts.push(heading(&section.title, Some(section.count), Some(section.expanded), None, Some(Box::new(toggle))));
    if !section.expanded {
        return;
    }
    parts.extend(section.rows.iter().map(|row| thread_row(row, Some(icons), rows)));
    if section.hidden_count > 0 {
        parts.push(more(&format!("Show {} more", section.hidden_count), format!("more:{}", section.kind), rows.on_menu));
    }
}

fn project_section(parts: &mut Vec<View>, project: &SidebarProjectView, icons: Icons<'_>, rows: Rows<'_>) {
    let id = &project.id;
    let title = if project.is_scratch { "No project" } else { project.title.as_str() };
    let toggle = {
        let (core, id) = (rows.core.clone(), id.clone());
        move || core.send(Event::ToggleProject { project_id: id.clone() })
    };
    let new_thread = {
        let (core, id) = (rows.core.clone(), id.clone());
        let tip = format!("New thread in {title}");
        icon_button("\u{E710}", &tip).on_click(move || core.send(Event::NewThread { project_id: Some(id.clone()) })).grid_column(1).tooltip(&tip)
    };
    let mut cells = vec![heading(title, None, Some(project.expanded), Some(icons.tile(&project.icon, 20.0)), Some(Box::new(toggle))), new_thread];
    if !project.is_scratch {
        let menu = Menu::new(
            [
                MenuItem::item(format!("new:{id}"), "New Thread"),
                MenuItem::item(format!("icon:{id}"), "Change Icon…"),
                MenuItem::item(format!("rename:{id}"), "Rename…"),
                MenuItem::separator("separator"),
                MenuItem::item(format!("remove:{id}"), "Remove Project…"),
            ],
            rows.on_menu.clone(),
        );
        cells.push(icon_button("\u{E712}", &format!("More for {title}")).grid_column(2).menu(menu));
    }
    parts.push(Grid::new().columns([GridLength::STAR, GridLength::Auto, GridLength::Auto]).margin(Thickness::new(0.0, 6.0, 0.0, 0.0)).children(cells).into());
    if !project.expanded {
        return;
    }
    // Under its project a row says what the thread is doing; the header has the icon.
    parts.extend(project.threads.iter().map(|row| thread_row(row, None, rows)));
    if project.hidden_count > 0 {
        parts.push(more(&format!("Show {} more", project.hidden_count), format!("more-project:{id}"), rows.on_menu));
    }
    if project.earlier_count > 0 {
        let open = rows.local.earlier.contains(id);
        let label = if open { format!("Hide earlier sessions ({})", project.earlier_count) } else { format!("Earlier sessions ({})", project.earlier_count) };
        parts.push(more(&label, format!("earlier:{id}"), rows.on_menu));
        if open {
            parts.extend(project.earlier.iter().map(|row| thread_row(row, None, rows)));
        }
    }
}

/// A small button with only a glyph on it.
fn icon_button(glyph: &str, name: &str) -> Button {
    Button::new().style(ButtonStyle::Subtle).automation_name(name).content(theme::glyph(glyph, 14.0))
}

/// A line under a list that shows more of it.
fn more(label: &str, key: String, on_menu: &Callback<Key>) -> View {
    let on_menu = on_menu.clone();
    Button::new()
        .style(ButtonStyle::Subtle)
        .margin(Thickness::new(24.0, 0.0, 0.0, 0.0))
        .content(TextBlock::new().text(label).font_size(theme::CAPTION).opacity(0.75))
        .on_click(move || on_menu.call(key.as_str().into()))
        .into()
}

fn caption(text: &str) -> TextBlock {
    TextBlock::new().text(text).font_size(theme::CAPTION).opacity(0.65).text_trimming(TextTrimming::CharacterEllipsis)
}

/// A section's or project's title (a project's with its icon). With
/// `expanded` and `toggle` it folds and unfolds when pressed.
fn heading(title: &str, count: Option<u32>, expanded: Option<bool>, icon: Option<View>, toggle: Option<Box<dyn Fn()>>) -> View {
    let text = match count {
        Some(n) => format!("{title}  {n}"),
        None => title.to_owned(),
    };
    let label = TextBlock::new()
        .text(text)
        .font_size(theme::CAPTION)
        .font_weight(FontWeight::SEMI_BOLD)
        .opacity(0.8)
        .vertical_alignment(VerticalAlignment::Center)
        .text_trimming(TextTrimming::CharacterEllipsis);
    let chevron = expanded.map(|open| theme::glyph(if open { "\u{E70D}" } else { "\u{E76C}" }, 10.0).opacity(0.7).into());
    let content = StackPanel::new().orientation(Orientation::Horizontal).spacing(6.0).children([chevron, icon, Some(label.into())].into_iter().flatten().collect::<Vec<View>>());
    match toggle {
        Some(toggle) => Button::new()
            .style(ButtonStyle::Subtle)
            .horizontal_alignment(HorizontalAlignment::Stretch)
            .horizontal_content_alignment(HorizontalAlignment::Left)
            .automation_name(title)
            .content(content)
            .on_click(toggle)
            .into(),
        None => Border::new().padding(Thickness::new(11.0, 10.0, 0.0, 4.0)).content(content).into(),
    }
}

/// A span of time by the minute: `3m`, `2h`, `4d` (empty under a minute).
/// Time gone by rounds down; time left (`round_up`) rounds up, so 1h59m left reads `2h`.
fn short_span(ms: i64, round_up: bool) -> String {
    let unit = |u: i64| if round_up { (ms + u - 1) / u } else { ms / u };
    match ms / 60_000 {
        ..1 => String::new(),
        1..60 => format!("{}m", unit(60_000)),
        60..1440 => format!("{}h", unit(3_600_000)),
        _ => format!("{}d", unit(86_400_000)),
    }
}

/// What a row says at its right: what the thread is doing, when it wakes, or its age.
fn right_label(row: &ThreadRowView, now_ms: i64) -> String {
    if row.status == "working" && row.working_since_ms > 0 {
        // The core's own label ages only when it draws: count here, so each tick keeps it current.
        let doing = if row.plan_mode { "Planning" } else { "Working" };
        match short_span(now_ms - row.working_since_ms, false) {
            elapsed if elapsed.is_empty() => doing.to_owned(),
            elapsed => format!("{doing} {elapsed}"),
        }
    } else if !row.status_label.is_empty() {
        row.status_label.clone()
    } else if row.snoozed_until_ms > 0 {
        match short_span(row.snoozed_until_ms - now_ms, true) {
            left if left.is_empty() => "Waking".to_owned(),
            left => format!("Wakes in {left}"),
        }
    } else {
        row.age.clone()
    }
}

/// A word in a pill beside a thread: `Plan`, a pull request, where it came from.
fn pill(text: &str, rgb: Option<u32>, dark: bool) -> Border {
    let label = TextBlock::new().text(text).font_size(10.5);
    let (label, fill) = match rgb {
        Some(rgb) => (label.foreground(theme::rgb(rgb)), theme::tint(rgb, dark)),
        None => (label.opacity(0.75), theme::code()),
    };
    Border::new().padding(Thickness::xy(6.0, 1.0)).corner_radius(8.0).background(fill).vertical_alignment(VerticalAlignment::Center).content(label)
}

/// What can be done with a thread, as its menu lists it.
pub fn thread_menu(row: &ThreadRowView, open: Option<&ThreadPageView>, on_menu: &Callback<Key>) -> Menu {
    let id = &row.id;
    let item = |what: &str, label: &str| MenuItem::item(format!("{what}:{id}"), label);
    let mut items = vec![
        item("t-rename", "Rename…"),
        if row.pinned { item("t-unpin", "Unpin") } else { item("t-pin", "Pin") },
        if row.settled { item("t-unsettle", "Mark Active") } else { item("t-settle", "Settle") },
        MenuItem::submenu(
            format!("snooze:{id}"),
            "Snooze",
            [item("t-snooze-1h", "For 1 Hour"), item("t-snooze-3h", "For 3 Hours"), item("t-snooze-1d", "Until Tomorrow"), item("t-snooze-7d", "For a Week"), item("t-snooze-wake", "Wake Now")],
        ),
        item("t-unread", "Mark Unread"),
        item("t-retitle", "Regenerate Title"),
        item("t-fork", "Fork Thread"),
        item("t-schedule", "Schedule…"),
        if row.archived { item("t-unarchive", "Unarchive") } else { item("t-archive", "Archive") },
        MenuItem::separator("separator-1"),
    ];
    // Only the open thread's page knows where it works.
    if open.is_some_and(|page| page.id == *id && page.worktree_path.is_some() && page.project_title == "No project") {
        items.push(item("t-promote", "Make This Folder a Project"));
    }
    items.push(item("t-copy", "Copy Thread ID"));
    items.push(item("t-forget", "Forget Allowed Tools"));
    items.push(MenuItem::separator("separator-2"));
    items.push(item("thread-delete", "Delete Thread…"));
    Menu::new(items, on_menu.clone())
}

/// A thread. With `icons` (threads listed by status) it leads with its
/// project's icon, and what it is doing is a badge on that icon's corner;
/// without, with the mark of what it is doing alone.
fn thread_row(row: &ThreadRowView, icons: Option<Icons<'_>>, rows: Rows<'_>) -> View {
    let open = {
        let (core, id) = (rows.core.clone(), row.id.clone());
        move || core.send(Event::SelectThread { thread_id: id.clone() })
    };
    let selected = rows.selected == Some(row.id.as_str());
    let title = if row.title.is_empty() { "New thread" } else { row.title.as_str() };
    let marker = match row.status.as_str() {
        "working" => Some(("\u{E916}", theme::accent())),
        "approval" => Some(("\u{E7BA}", theme::accent())),
        "failed" => Some(("\u{E783}", theme::critical())),
        _ if row.unread => Some(("\u{E91F}", theme::accent())),
        _ => None,
    };
    let mark = |size: f64| marker.clone().map(|(code, brush)| theme::glyph(code, size).foreground(brush));
    let lead: View = match icons {
        Some(icons) => {
            let mut layers: Vec<View> = vec![icons.tile(&row.project_icon, AVATAR)];
            if let Some(mark) = mark(8.0) {
                // On a disc of the window's own colour, so it reads on any icon.
                layers.push(
                    Border::new()
                        .width(14.0)
                        .height(14.0)
                        .corner_radius(7.0)
                        .background(theme::solid())
                        .horizontal_alignment(HorizontalAlignment::Right)
                        .vertical_alignment(VerticalAlignment::Bottom)
                        .margin(Thickness::new(0.0, 0.0, -4.0, -4.0))
                        .content(mark.horizontal_alignment(HorizontalAlignment::Center))
                        .into(),
                );
            }
            Grid::new().width(AVATAR).height(AVATAR).vertical_alignment(VerticalAlignment::Center).children(layers).into()
        }
        None => match mark(10.0) {
            Some(mark) => Border::new().vertical_alignment(VerticalAlignment::Top).margin(Thickness::new(0.0, 3.0, 0.0, 0.0)).content(mark).into(),
            None => Border::new().into(),
        },
    };

    let right = TextBlock::new().grid_column(2).text(right_label(row, rows.local.now_ms)).font_size(theme::CAPTION).vertical_alignment(VerticalAlignment::Center);
    let right = match row.status.as_str() {
        "approval" | "working" => right.foreground(theme::accent()),
        "failed" => right.foreground(theme::critical()),
        _ => right.opacity(0.65),
    };
    let mut title_line: Vec<View> = Vec::new();
    if row.pinned {
        title_line.push(theme::glyph("\u{E718}", 10.0).foreground(theme::accent()).into());
    }
    title_line.push(
        TextBlock::new().grid_column(1).text(title).font_weight(if row.unread || selected { FontWeight::SEMI_BOLD } else { FontWeight::NORMAL }).text_trimming(TextTrimming::CharacterEllipsis).into(),
    );
    title_line.push(right.into());

    let mut about: Vec<&str> = Vec::new();
    if icons.is_some() && !row.project_title.is_empty() {
        about.push(&row.project_title);
    }
    if !row.model_label.is_empty() {
        about.push(&row.model_label);
    }
    let branch = row.branch.as_ref().map(|branch| format!("⑂ {branch}"));
    about.extend(branch.as_deref());
    let mut pills: Vec<View> = Vec::new();
    if row.plan_ready {
        pills.push(pill("Plan", Some(palette::project_rgb("violet", rows.dark)), rows.dark).into());
    }
    if let Some(issue) = &row.issue_badge {
        pills.push(pill(issue, None, rows.dark).into());
    }
    if !row.pull_request.is_empty() {
        let colour = match row.pull_request_state.as_str() {
            "open" => Some("green"),
            "merged" => Some("violet"),
            "closed" => Some("red"),
            _ => None,
        };
        let tip = if row.pull_request_state.is_empty() { format!("Pull request {}", row.pull_request) } else { format!("Pull request {}, {}", row.pull_request, row.pull_request_state) };
        pills.push(pill(&row.pull_request, colour.map(|key| palette::project_rgb(key, rows.dark)), rows.dark).tooltip(tip));
    }
    if row.relation == "fork" {
        pills.push(pill("fork", None, rows.dark).tooltip("Forked from another thread"));
    }
    if row.in_worktree {
        pills.push(pill("worktree", None, rows.dark).tooltip("In its own worktree"));
    }
    if !row.source.is_empty() {
        pills.push(pill(&row.source, None, rows.dark).into());
    }
    let under: Vec<View> = vec![caption(&about.join(" · ")).into(), StackPanel::new().grid_column(1).orientation(Orientation::Horizontal).spacing(4.0).children(pills).into()];

    let body = Grid::new()
        .columns([GridLength::Pixel(if icons.is_some() { AVATAR } else { 18.0 }), GridLength::STAR])
        .column_spacing(if icons.is_some() { 10.0 } else { 6.0 })
        .children((
            lead,
            StackPanel::new().grid_column(1).orientation(Orientation::Vertical).children(vec![
                Grid::new().columns([GridLength::Auto, GridLength::STAR, GridLength::Auto]).column_spacing(4.0).children(title_line).into(),
                Grid::new().columns([GridLength::STAR, GridLength::Auto]).column_spacing(4.0).children(under).into(),
            ]),
        ));
    let button = Button::new()
        .style(ButtonStyle::Subtle)
        .horizontal_alignment(HorizontalAlignment::Stretch)
        .horizontal_content_alignment(HorizontalAlignment::Stretch)
        .automation_name(title)
        .content(body)
        .on_click(open);
    let button = if selected { button.background(theme::selected()) } else { button };
    let menu = icon_button("\u{E712}", &format!("More for {title}")).grid_column(1).vertical_alignment(VerticalAlignment::Stretch).menu(thread_menu(row, rows.local.open, rows.on_menu));
    Grid::new().columns([GridLength::STAR, GridLength::Auto]).children(vec![View::from(button), menu]).into()
}

/// A thread row's project icon.
const AVATAR: f64 = 24.0;

fn capitalized(word: &str) -> String {
    let mut chars = word.chars();
    chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn states_read_as_words() {
        assert_eq!(capitalized("disconnected"), "Disconnected");
        assert_eq!(capitalized(""), "");
    }

    fn row(id: &str, status: &str) -> ThreadRowView {
        ThreadRowView { id: id.into(), status: status.into(), ..ThreadRowView::default() }
    }

    #[test]
    fn the_next_waiting_thread_goes_round() {
        let mut sidebar = SidebarView { surface_threads: vec![row("a", "approval"), row("b", "working"), row("c", "approval")], ..SidebarView::default() };
        assert_eq!(next_waiting(&sidebar), Some("a"));
        sidebar.selected_thread_id = Some("a".into());
        assert_eq!(next_waiting(&sidebar), Some("c"));
        sidebar.selected_thread_id = Some("c".into());
        assert_eq!(next_waiting(&sidebar), Some("a"));
        sidebar.surface_threads.clear();
        assert_eq!(next_waiting(&sidebar), None);
    }

    #[test]
    fn a_row_says_how_long_it_works_and_when_it_wakes() {
        let minute = 60_000;
        let mut working = row("a", "working");
        working.working_since_ms = 1_000_000;
        assert_eq!(right_label(&working, 1_000_000 + 30_000), "Working");
        assert_eq!(right_label(&working, 1_000_000 + 7 * minute), "Working 7m");
        working.plan_mode = true;
        assert_eq!(right_label(&working, 1_000_000 + 125 * minute), "Planning 2h");
        let mut snoozed = row("b", "idle");
        snoozed.snoozed_until_ms = 1_000_000 + 119 * minute;
        assert_eq!(right_label(&snoozed, 1_000_000), "Wakes in 2h", "time left rounds up");
        assert_eq!(right_label(&snoozed, 1_000_000 + 119 * minute), "Waking");
        let mut idle = row("c", "idle");
        idle.age = "3d".into();
        assert_eq!(right_label(&idle, 0), "3d");
        idle.status_label = "Needs you".into();
        assert_eq!(right_label(&idle, 0), "Needs you");
    }

    #[test]
    fn a_thread_is_found_wherever_it_is_listed() {
        let project = SidebarProjectView { earlier: vec![row("old", "idle")], threads: vec![row("p", "idle")], ..SidebarProjectView::default() };
        let sidebar = SidebarView {
            sections: vec![SidebarSectionView { rows: vec![row("s", "idle")], ..SidebarSectionView::default() }],
            projects: vec![project],
            snoozed: vec![row("z", "idle")],
            ..SidebarView::default()
        };
        for id in ["s", "p", "old", "z"] {
            assert_eq!(find_row(&sidebar, id).map(|r| r.id.as_str()), Some(id));
        }
        assert!(find_row(&sidebar, "gone").is_none());
    }
}
