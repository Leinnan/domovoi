//! A new thread: where it runs, then its first message in the composer
//! below. Nothing can be sent before a project is chosen.
//!
//! The projects are a grid of tiles with their icons, found by typing: the
//! ones used last come first, then the pinned ones, then the rest. Arrows
//! move through them and Enter takes the one marked, so a project is a few
//! keys away however many there are. The Add menu brings in more: a folder
//! browsed or typed, a new project, or the sessions of other agents.

use std::rc::Rc;

use domovoi_core::Event;
use domovoi_core::view::{ComposerView, SidebarProjectView, SidebarView};
use windows_reactor::{
    Border, Button, ButtonStyle, Callback, ComboBox, DropDownButton, ElementRef, FontWeight, Grid, GridLength, HorizontalAlignment, InfoBar, InfoBarSeverity,
    Key, KeyEventInfo, Menu, MenuExt as _, MenuItem, Orientation, RoutedCallback, ScrollBarVisibility, ScrollViewer, SelectorBar, SelectorBarItem, StackPanel,
    TextBlock, TextBox, TextTrimming, Thickness, TooltipExt as _, VerticalAlignment, View,
};

use crate::core_link::CoreLink;
use crate::theme;
use crate::views::sidebar::Icons;

/// How many projects count as used last.
const RECENT: usize = 4;
/// The widest the page gets, and the least a tile needs.
const WIDTH: f64 = 760.0;
const TILE: f64 = 220.0;

/// What is typed in the search box, and the tile the arrows are on.
#[derive(Default)]
pub struct Picker {
    pub query: String,
    pub marked: usize,
}

/// Projects under one heading (no heading: the matches of a search).
#[derive(Debug, PartialEq)]
pub struct Group<'a> {
    pub title: &'static str,
    pub projects: Vec<&'a SidebarProjectView>,
}

/// Whether `project` is one `query` finds: any part of its name or folder, whatever the case.
fn found(project: &SidebarProjectView, query: &str) -> bool {
    let query = query.trim().to_lowercase();
    query.is_empty() || project.title.to_lowercase().contains(&query) || project.workspace_root.to_lowercase().contains(&query)
}

/// The projects of the threads most worth a glance, then of the listed ones: the ones used last.
fn recent<'a>(sidebar: &'a SidebarView) -> Vec<&'a str> {
    let mut ids: Vec<&str> = Vec::new();
    let rows = sidebar.surface_threads.iter().chain(sidebar.sections.iter().flat_map(|s| &s.rows));
    for id in rows.map(|row| row.project_id.as_str()).filter(|id| !id.is_empty()) {
        if !ids.contains(&id) {
            ids.push(id);
            if ids.len() == RECENT {
                break;
            }
        }
    }
    ids
}

/// The projects to show, grouped. A search gives its matches as one list.
/// The scratch project comes last, wherever it would be.
pub fn groups<'a>(sidebar: &'a SidebarView, query: &str) -> Vec<Group<'a>> {
    let (scratch, projects): (Vec<_>, Vec<_>) = sidebar.projects.iter().filter(|p| found(p, query)).partition(|p| p.is_scratch);
    if !query.trim().is_empty() {
        let all: Vec<_> = projects.into_iter().chain(scratch).collect();
        return if all.is_empty() { Vec::new() } else { vec![Group { title: "", projects: all }] };
    }
    let recent_ids = recent(sidebar);
    let recent: Vec<_> = recent_ids.iter().filter_map(|id| projects.iter().copied().find(|p| p.id == *id)).collect();
    let (pinned, rest): (Vec<_>, Vec<_>) = projects.into_iter().filter(|p| !recent_ids.contains(&p.id.as_str())).partition(|p| p.pinned);
    let rest: Vec<_> = rest.into_iter().chain(scratch).collect();
    let above = !recent.is_empty() || !pinned.is_empty();
    [("Recent", recent), ("Pinned", pinned), (if above { "Other projects" } else { "Projects" }, rest)]
        .into_iter()
        .filter(|(_, projects)| !projects.is_empty())
        .map(|(title, projects)| Group { title, projects })
        .collect()
}

/// The project the arrows are on: the `marked`th shown, counting through the groups.
pub fn marked<'a>(groups: &[Group<'a>], marked: usize) -> Option<&'a SidebarProjectView> {
    groups.iter().flat_map(|g| g.projects.iter().copied()).nth(marked)
}

/// How many projects are shown.
pub fn shown(groups: &[Group<'_>]) -> usize {
    groups.iter().map(|g| g.projects.len()).sum()
}

/// How many tiles fit side by side in a window `window_width` wide.
pub fn columns(window_width: f64) -> usize {
    let page = (window_width - theme::SIDEBAR - 80.0).min(WIDTH);
    ((page / TILE) as usize).clamp(1, 3)
}

/// How tall the list of projects may be in a window `window_height` tall:
/// what the title bar, the page's other parts and the composer leave.
pub fn list_height(window_height: f64) -> f64 {
    (window_height - 470.0).max(110.0)
}

/// What the page tells the window.
pub struct Hooks<'a> {
    pub on_query: Callback<Rc<str>>,
    /// Arrows, Enter and Escape in the search box.
    pub on_keys: RoutedCallback<KeyEventInfo>,
    /// A choice from the Add menu or a tile's menu: `add:browse`, `rename:<project id>`, …
    pub on_menu: Callback<Key>,
    pub search: &'a ElementRef<TextBox>,
    pub columns: usize,
    pub list_height: f64,
}

pub fn new_thread(core: &CoreLink, sidebar: &SidebarView, composer: &ComposerView, picker: &Picker, icons: Icons<'_>, hooks: Hooks<'_>) -> View {
    let chosen = sidebar.selected_project_id.as_deref().and_then(|id| sidebar.projects.iter().find(|p| p.id == id));
    let groups = groups(sidebar, &picker.query);
    let on = marked(&groups, picker.marked).map(|p| p.id.as_str());
    // The mark shows only while a search narrows the list: otherwise it would sit on the first tile for no reason.
    let on = if picker.query.trim().is_empty() && picker.marked == 0 { None } else { on };

    let mut list: Vec<View> = Vec::new();
    for group in &groups {
        if !group.title.is_empty() {
            list.push(caption(group.title).font_weight(FontWeight::SEMI_BOLD).margin(Thickness::new(2.0, 6.0, 0.0, 0.0)).into());
        }
        for row in group.projects.chunks(hooks.columns) {
            let tiles: Vec<View> = row
                .iter()
                .enumerate()
                .map(|(column, project)| tile(core, project, chosen.is_some_and(|c| c.id == project.id), on == Some(project.id.as_str()), column, icons, &hooks.on_menu))
                .collect();
            list.push(Grid::new().columns(vec![GridLength::STAR; hooks.columns]).column_spacing(6.0).children(tiles).into());
        }
    }
    if groups.is_empty() {
        let none = if sidebar.projects.is_empty() { "No projects yet. Add a folder on the server to start." } else { "No matching projects" };
        list.push(caption(none).margin(Thickness::uniform(12.0)).into());
    }

    let add = DropDownButton::new().grid_column(1).automation_name("Add project").content(TextBlock::new().text("Add")).menu(add_menu(sidebar, hooks.on_menu.clone()));
    let search = Border::new().on_preview_key_down(hooks.on_keys).content(
        TextBox::new().element_ref(hooks.search).automation_name("Search projects").placeholder_text("Search projects").text(&picker.query).on_text_changed(hooks.on_query),
    );
    // The list scrolls in the height the window leaves it: the search above
    // it and the workspace below stay in sight.
    let projects = Border::new()
        .grid_row(1)
        .padding(Thickness::uniform(12.0))
        .background(theme::card())
        .border_brush(theme::stroke())
        .border_thickness(Thickness::uniform(1.0))
        .corner_radius(theme::RADIUS)
        .content(Grid::new().rows([GridLength::Auto, GridLength::Auto]).row_spacing(8.0).children((
            Grid::new().columns([GridLength::STAR, GridLength::Auto]).column_spacing(8.0).children((search, add)),
            ScrollViewer::new()
                .grid_row(1)
                .max_height(hooks.list_height)
                .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
                .horizontal_scroll_bar_visibility(ScrollBarVisibility::Disabled)
                .content(StackPanel::new().orientation(Orientation::Vertical).spacing(6.0).children(list)),
        )));

    let mut page: Vec<View> = vec![hero(chosen, icons), projects.into()];
    if chosen.is_some() {
        page.push(Border::new().grid_row(2).content(workspace(core, composer)).into());
    }
    // No scroll viewer around the page: a centred column in one is measured wider than its card.
    Border::new()
        .padding(Thickness::new(20.0, 16.0, 20.0, 12.0))
        .content(Grid::new().max_width(WIDTH).vertical_alignment(VerticalAlignment::Top).rows([GridLength::Auto, GridLength::Auto, GridLength::Auto]).row_spacing(12.0).children(page))
        .into()
}

/// The chosen project's icon, name and folder; before one is chosen, what to do.
fn hero(chosen: Option<&SidebarProjectView>, icons: Icons<'_>) -> View {
    let icon: View = match chosen {
        Some(project) => icons.tile(&project.icon, 52.0),
        None => Border::new().width(52.0).height(52.0).corner_radius(13.0).background(theme::code()).content(theme::glyph("\u{E8F4}", 24.0).opacity(0.6).horizontal_alignment(HorizontalAlignment::Center)).into(),
    };
    let title = match chosen {
        Some(project) if project.is_scratch => "New thread without a project".to_owned(),
        Some(project) => format!("New thread in {}", project.title),
        None => "Start a new thread".to_owned(),
    };
    let under: View = match chosen.filter(|p| !p.workspace_root.is_empty()) {
        Some(project) => caption(&project.workspace_root).font_family(theme::MONO).text_trimming(TextTrimming::CharacterEllipsis).into(),
        None if chosen.is_some() => caption("Write its first message below.").into(),
        None => caption("Choose where it runs, then write its first message below.").into(),
    };
    StackPanel::new()
        .orientation(Orientation::Horizontal)
        .spacing(14.0)
        .children(vec![
            icon,
            StackPanel::new()
                .orientation(Orientation::Vertical)
                .vertical_alignment(VerticalAlignment::Center)
                .spacing(2.0)
                .children(vec![TextBlock::new().text(title).font_size(theme::TITLE).font_weight(FontWeight::SEMI_BOLD).into(), under])
                .into(),
        ])
        .into()
}

/// One project: its icon, name and folder. Pressed, the thread runs there.
/// The button beside it opens the project's menu.
fn tile(core: &CoreLink, project: &SidebarProjectView, chosen: bool, marked: bool, column: usize, icons: Icons<'_>, on_menu: &Callback<Key>) -> View {
    let title = if project.is_scratch { "No project (scratch)" } else { project.title.as_str() };
    let mut name: Vec<View> = vec![TextBlock::new().text(title).font_weight(FontWeight::SEMI_BOLD).text_trimming(TextTrimming::CharacterEllipsis).into()];
    if !project.workspace_root.is_empty() {
        name.push(caption(&project.workspace_root).text_trimming(TextTrimming::CharacterEllipsis).into());
    }
    let mut marks: Vec<View> = Vec::new();
    if project.pinned {
        marks.push(theme::glyph("\u{E718}", 11.0).opacity(0.6).into());
    }
    if chosen {
        marks.push(theme::glyph("\u{E73E}", 12.0).foreground(theme::accent()).into());
    }
    let pick = {
        let (core, id) = (core.clone(), project.id.clone());
        Button::new()
            .style(ButtonStyle::Subtle)
            .horizontal_alignment(HorizontalAlignment::Stretch)
            .horizontal_content_alignment(HorizontalAlignment::Stretch)
            .automation_name(title)
            .content(Grid::new().columns([GridLength::Auto, GridLength::STAR, GridLength::Auto]).column_spacing(10.0).children((
                icons.tile(&project.icon, 28.0),
                StackPanel::new().grid_column(1).orientation(Orientation::Vertical).vertical_alignment(VerticalAlignment::Center).children(name),
                StackPanel::new().grid_column(2).orientation(Orientation::Horizontal).spacing(6.0).vertical_alignment(VerticalAlignment::Center).children(marks),
            )))
            .on_click(move || core.send(Event::NewThread { project_id: Some(id.clone()) }))
    };
    let pick: View = if project.workspace_root.is_empty() { pick.into() } else { pick.tooltip(&project.workspace_root) };
    let mut parts: Vec<View> = vec![pick];
    if !project.is_scratch {
        let id = &project.id;
        let menu = Menu::new(
            [
                MenuItem::item(format!("new:{id}"), "New Thread"),
                MenuItem::item(format!("icon:{id}"), "Change Icon…"),
                MenuItem::item(format!("rename:{id}"), "Rename…"),
                MenuItem::separator("separator"),
                MenuItem::item(format!("remove:{id}"), "Remove Project…"),
            ],
            on_menu.clone(),
        );
        parts.push(
            Button::new()
                .grid_column(1)
                .style(ButtonStyle::Subtle)
                .vertical_alignment(VerticalAlignment::Stretch)
                .automation_name(format!("More for {title}"))
                .content(theme::glyph("\u{E712}", 12.0).opacity(0.7))
                .menu(menu),
        );
    }
    let fill = if chosen || marked { theme::selected() } else { theme::card() };
    Border::new()
        .grid_column(column as i32)
        .background(fill)
        .border_brush(if chosen || marked { theme::accent() } else { theme::stroke() })
        .border_thickness(Thickness::uniform(1.0))
        .corner_radius(theme::RADIUS)
        .content(Grid::new().columns([GridLength::STAR, GridLength::Auto]).children(parts))
        .into()
}

/// The ways to bring a project in.
fn add_menu(sidebar: &SidebarView, on_menu: Callback<Key>) -> Menu {
    let mut items = vec![
        MenuItem::item("add:browse", "Browse server folders…"),
        MenuItem::item("add:path", "Add folder by path…"),
        MenuItem::item("add:create", "Create new project…"),
        MenuItem::separator("separator"),
        MenuItem::item("add:import", "Import agent sessions…"),
    ];
    if !sidebar.projects.iter().any(|p| p.is_scratch) {
        items.push(MenuItem::item("add:scratch", "Thread without a project"));
    }
    Menu::new(items, on_menu)
}

/// Where in the project the thread works: the folder itself, or a worktree
/// of its own cut from a branch.
fn workspace(core: &CoreLink, composer: &ComposerView) -> View {
    const LOCAL: &str = "Local folder";
    const WORKTREE: &str = "New worktree";
    let worktree = composer.workspace_mode == "worktree";
    let workspace_required = composer.workspace_required;
    let modes = {
        let core = core.clone();
        SelectorBar::new().items(vec![SelectorBarItem::new().text(LOCAL).is_selected(!worktree && !workspace_required).into(), SelectorBarItem::new().text(WORKTREE).is_selected(worktree).into()]).on_selected_text_changed(
            move |text: Option<Rc<str>>| {
                // Only a change the user made: showing the mode selects it too.
                let picked = text.as_deref() == Some(WORKTREE);
                if text.is_some() && (workspace_required || picked != worktree) {
                    core.send(Event::SetWorkspaceMode { mode: if picked { "worktree" } else { "local" }.into() });
                }
            },
        )
    };
    let mut lines: Vec<View> = vec![TextBlock::new().text("Workspace").font_weight(FontWeight::SEMI_BOLD).into(), modes.into()];
    if let Some(chip) = super::issues::issue_chip(core, composer) {
        lines.push(chip);
    }
    if worktree && !composer.branches.is_empty() {
        let chosen = composer.base_branch.as_ref().and_then(|base| composer.branches.iter().position(|b| b == base));
        let (core, branches) = (core.clone(), composer.branches.clone());
        lines.push(
            Grid::new()
                .columns([GridLength::Auto, GridLength::STAR])
                .column_spacing(10.0)
                .children((
                    caption("Start from branch").vertical_alignment(VerticalAlignment::Center),
                    ComboBox::new()
                        .grid_column(1)
                        .automation_name("Start from branch")
                        .horizontal_alignment(HorizontalAlignment::Left)
                        .min_width(220.0)
                        .items_source(composer.branches.iter().cloned())
                        .selected_index(chosen)
                        .on_selection_changed(move |index: Option<usize>| {
                            if let Some(branch) = index.filter(|i| Some(*i) != chosen).and_then(|i| branches.get(i)) {
                                core.send(Event::SetBaseBranch { branch: branch.clone() });
                            }
                        }),
                ))
                .into(),
        );
    }
    if !composer.pr_worktree.is_empty() {
        lines.push(InfoBar::new().is_open(true).is_closable(false).severity(InfoBarSeverity::Informational).message(&composer.pr_worktree).into());
    }
    card(lines)
}

fn card(lines: Vec<View>) -> View {
    Border::new()
        .padding(Thickness::uniform(12.0))
        .background(theme::card())
        .border_brush(theme::stroke())
        .border_thickness(Thickness::uniform(1.0))
        .corner_radius(theme::RADIUS)
        .content(StackPanel::new().orientation(Orientation::Vertical).spacing(8.0).children(lines))
        .into()
}

fn caption(text: &str) -> TextBlock {
    TextBlock::new().text(text).font_size(theme::CAPTION).opacity(0.65)
}

#[cfg(test)]
mod tests {
    use super::*;
    use domovoi_core::view::{SidebarSectionView, ThreadRowView};

    fn project(id: &str, root: &str) -> SidebarProjectView {
        SidebarProjectView { id: id.into(), title: id.into(), workspace_root: root.into(), ..SidebarProjectView::default() }
    }

    fn row(project: &str) -> ThreadRowView {
        ThreadRowView { project_id: project.into(), ..ThreadRowView::default() }
    }

    fn names<'a>(groups: &'a [Group<'a>]) -> Vec<(&'a str, Vec<&'a str>)> {
        groups.iter().map(|g| (g.title, g.projects.iter().map(|p| p.id.as_str()).collect())).collect()
    }

    fn sidebar() -> SidebarView {
        let mut pinned = project("pinned", r"C:\src\pinned");
        pinned.pinned = true;
        let scratch = SidebarProjectView { id: "scratch".into(), is_scratch: true, ..SidebarProjectView::default() };
        SidebarView {
            projects: vec![project("alpha", r"C:\src\alpha"), scratch, project("beta", r"D:\work\Beta-Site"), pinned, project("gamma", r"C:\src\gamma")],
            surface_threads: vec![row("gamma"), row("gamma"), row("")],
            sections: vec![SidebarSectionView { rows: vec![row("alpha"), row("gone")], ..SidebarSectionView::default() }],
            ..SidebarView::default()
        }
    }

    #[test]
    fn the_projects_used_last_come_first_then_the_pinned_then_the_rest() {
        let sidebar = sidebar();
        let groups = groups(&sidebar, "");
        assert_eq!(names(&groups), [("Recent", vec!["gamma", "alpha"]), ("Pinned", vec!["pinned"]), ("Other projects", vec!["beta", "scratch"])]);
        assert_eq!(shown(&groups), 5);
        assert_eq!(marked(&groups, 2).map(|p| p.id.as_str()), Some("pinned"));
        assert_eq!(marked(&groups, 5), None);
    }

    #[test]
    fn with_no_threads_yet_the_projects_are_one_list() {
        let sidebar = SidebarView { projects: vec![project("alpha", ""), project("beta", "")], ..SidebarView::default() };
        assert_eq!(names(&groups(&sidebar, "  ")), [("Projects", vec!["alpha", "beta"])]);
    }

    #[test]
    fn a_search_finds_names_and_folders_whatever_the_case() {
        let sidebar = sidebar();
        assert_eq!(names(&groups(&sidebar, "BETA")), [("", vec!["beta"])]);
        assert_eq!(names(&groups(&sidebar, r"c:\SRC")), [("", vec!["alpha", "pinned", "gamma"])]);
        assert_eq!(names(&groups(&sidebar, " site ")), [("", vec!["beta"])]);
        assert!(groups(&sidebar, "no such project").is_empty());
    }

    #[test]
    fn a_short_window_still_shows_a_row_or_two_of_projects() {
        assert_eq!(list_height(820.0), 350.0);
        assert_eq!(list_height(480.0), 110.0);
    }

    #[test]
    fn a_narrow_window_has_fewer_tiles_side_by_side() {
        assert_eq!(columns(1280.0), 3);
        assert_eq!(columns(820.0), 2);
        assert_eq!(columns(720.0), 1);
    }
}
