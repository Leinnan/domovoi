//! The Git tab: the branch and how it stands against its upstream, what
//! changed and its committing, the branches, the worktrees and the
//! project's scripts.

use domovoi_core::Event;
use domovoi_core::view::GitPanelView;
use domovoi_desktop_host::palette;
use windows_reactor::{
    Border, Button, ButtonStyle, FontWeight, Grid, GridLength, HorizontalAlignment, Orientation, ScrollBarVisibility, ScrollViewer, StackPanel, TextBlock, TextBox,
    TextTrimming, TextWrapping, Thickness, TooltipExt as _, VerticalAlignment, View,
};

use super::{Act, Shown, caption, card, does, heading, mono, nothing, tell, tell_icon};
use crate::core_link::CoreLink;
use crate::{theme, widgets};

/// The most changed files and branches listed.
const FILES: usize = 20;
const BRANCHES: usize = 12;

pub fn view(core: &CoreLink, shown: &Shown<'_>) -> View {
    let git = &shown.panel.git;
    if git.loading && !git.is_repo {
        return nothing("\u{E895}", "Checking the repository…", "Its state is on its way from the server.", None);
    }
    if !git.is_repo {
        return nothing("\u{E8B7}", "Not a Git repository", "Branches, worktrees and pull requests need this project to be one.", Some(tell(core, "Refresh", Event::RefreshGit).horizontal_alignment(HorizontalAlignment::Center).into()));
    }
    let mut cards: Vec<View> = vec![branch(core, git, shown.dark)];
    if let Some(error) = &git.error {
        cards.push(Border::new().padding(Thickness::uniform(8.0)).corner_radius(6.0).background(theme::tint(palette::project_rgb("red", shown.dark), shown.dark)).content(TextBlock::new().text(error).font_size(theme::CAPTION + 1.0).text_wrapping(TextWrapping::WrapWholeWords).is_text_selection_enabled(true)).into());
    }
    if !git.progress.is_empty() {
        cards.push(card(git.progress.iter().map(|step| caption(step).text_wrapping(TextWrapping::WrapWholeWords).into()).collect()));
    }
    if let Some(request) = &git.pull_request {
        cards.push(card(vec![
            Grid::new()
                .columns([GridLength::Auto, GridLength::STAR, GridLength::Auto])
                .column_spacing(8.0)
                .children((
                    widgets::line_icon("git-pull-request", 16.0, shown.dark).vertical_alignment(VerticalAlignment::Center),
                    StackPanel::new().grid_column(1).orientation(Orientation::Vertical).children(vec![
                        TextBlock::new().text(format!("#{} {}", request.number, request.title)).font_size(theme::CAPTION + 1.0).font_weight(FontWeight::SEMI_BOLD).text_trimming(TextTrimming::CharacterEllipsis).into(),
                        caption(&[request.state.as_str(), request.checks.as_str()].into_iter().filter(|part| !part.is_empty()).collect::<Vec<_>>().join(" · ")).into(),
                    ]),
                    does(shown.on_act, "Open", Act::Open(request.url.clone())).grid_column(2).vertical_alignment(VerticalAlignment::Center),
                ))
                .into(),
        ]));
    }
    cards.push(changes(git, shown));
    cards.push(branches(core, git, shown));
    cards.push(worktrees(core, git, shown));
    if !git.scripts.is_empty() {
        let mut lines: Vec<View> = vec![heading("Project scripts").into()];
        for script in &git.scripts {
            let (core, id) = (core.clone(), script.id.clone());
            lines.push(
                Button::new()
                    .style(ButtonStyle::Subtle)
                    .horizontal_alignment(HorizontalAlignment::Stretch)
                    .horizontal_content_alignment(HorizontalAlignment::Left)
                    .automation_name(format!("Run {}", script.name))
                    .content(StackPanel::new().orientation(Orientation::Horizontal).spacing(8.0).children(vec![
                        theme::glyph(if script.icon == "test" { "\u{E73E}" } else { "\u{E768}" }, 12.0).foreground(theme::accent()).into(),
                        StackPanel::new().orientation(Orientation::Vertical).children(vec![TextBlock::new().text(&script.name).font_size(theme::CAPTION + 1.0).into(), mono(&script.command).opacity(0.6).into()]).into(),
                    ]))
                    .on_click(move || core.send(Event::RunScript { id: id.clone() }))
                    .tooltip("Run it in a new terminal of this thread"),
            );
        }
        cards.push(card(lines));
    }
    ScrollViewer::new()
        .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
        .horizontal_scroll_bar_visibility(ScrollBarVisibility::Disabled)
        .content(StackPanel::new().orientation(Orientation::Vertical).spacing(8.0).margin(Thickness::new(10.0, 10.0, 14.0, 10.0)).children(cards))
        .into()
}

/// The branch, how far it is ahead of and behind its upstream, Pull and Refresh.
fn branch(core: &CoreLink, git: &GitPanelView, dark: bool) -> View {
    let mut cells: Vec<View> = vec![
        widgets::line_icon("git-branch", 16.0, dark).vertical_alignment(VerticalAlignment::Center).into(),
        TextBlock::new()
            .grid_column(1)
            .text(git.branch.as_deref().unwrap_or("(detached)"))
            .font_family(theme::MONO)
            .font_weight(FontWeight::SEMI_BOLD)
            .text_trimming(TextTrimming::CharacterEllipsis)
            .vertical_alignment(VerticalAlignment::Center)
            .into(),
    ];
    let mut right: Vec<View> = Vec::new();
    if git.ahead > 0 {
        right.push(mono(&format!("↑{}", git.ahead)).foreground(theme::rgb(palette::project_rgb("blue", dark))).vertical_alignment(VerticalAlignment::Center).tooltip("Commits not pushed yet"));
    }
    if git.behind > 0 {
        right.push(mono(&format!("↓{}", git.behind)).foreground(theme::rgb(palette::project_rgb("amber", dark))).vertical_alignment(VerticalAlignment::Center).tooltip("Commits not pulled yet"));
    }
    if git.has_upstream {
        right.push(tell(core, "Pull", Event::GitPull).is_enabled(!git.busy).into());
    }
    right.push(tell_icon(core, "\u{E72C}", "Refresh the repository's state", Event::RefreshGit).tooltip("Refresh"));
    cells.push(StackPanel::new().grid_column(2).orientation(Orientation::Horizontal).spacing(6.0).children(right).into());
    card(vec![Grid::new().columns([GridLength::Auto, GridLength::STAR, GridLength::Auto]).column_spacing(8.0).children(cells).into()])
}

/// What changed, and its committing and pushing.
fn changes(git: &GitPanelView, shown: &Shown<'_>) -> View {
    let (added, removed) = (palette::project_rgb("green", shown.dark), palette::project_rgb("red", shown.dark));
    let counts = |insertions: u32, deletions: u32| -> View {
        StackPanel::new()
            .grid_column(1)
            .orientation(Orientation::Horizontal)
            .spacing(6.0)
            .children(vec![mono(&format!("+{insertions}")).foreground(theme::rgb(added)).into(), mono(&format!("−{deletions}")).foreground(theme::rgb(removed)).into()])
            .into()
    };
    let mut lines: Vec<View> = vec![Grid::new().columns([GridLength::STAR, GridLength::Auto]).children((heading("Changes"), counts(git.insertions, git.deletions))).into()];
    if git.changed_files.is_empty() {
        lines.push(caption("Nothing to commit.").into());
    } else {
        for file in git.changed_files.iter().take(FILES) {
            lines.push(Grid::new().columns([GridLength::STAR, GridLength::Auto]).column_spacing(8.0).children((mono(&file.path).text_trimming(TextTrimming::CharacterEllipsis), counts(file.insertions, file.deletions))).into());
        }
        if git.changed_files.len() > FILES {
            lines.push(caption(&format!("… and {} more", git.changed_files.len() - FILES)).into());
        }
        let on_act = shown.on_act.clone();
        lines.push(
            TextBox::new()
                .automation_name("Commit message")
                .placeholder_text("Commit message (empty: one is written for you)")
                .text(&shown.state.commit)
                .on_text_changed(move |text| on_act.call(Act::Commit(text)))
                .into(),
        );
        lines.push(
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(4.0)
                .children(vec![
                    does(shown.on_act, "Commit", Act::Git("commit")).is_enabled(!git.busy).into(),
                    does(shown.on_act, "Commit & Push", Act::Git("commit_push")).style(ButtonStyle::Accent).is_enabled(!git.busy && git.has_remote).into(),
                    does(shown.on_act, "Commit, Push & PR", Act::Git("commit_push_pr")).is_enabled(!git.busy && git.has_remote).into(),
                ])
                .into(),
        );
    }
    if git.has_remote && git.ahead > 0 {
        lines.push(
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(4.0)
                .children(vec![does(shown.on_act, "Push", Act::Git("push")).is_enabled(!git.busy).into(), does(shown.on_act, "Create PR", Act::Git("create_pr")).is_enabled(!git.busy).into()])
                .into(),
        );
    }
    card(lines)
}

/// A new branch, and the ones there are: pressing one switches to it.
fn branches(core: &CoreLink, git: &GitPanelView, shown: &Shown<'_>) -> View {
    let on_act = shown.on_act.clone();
    let mut lines: Vec<View> = vec![
        heading("Branches").into(),
        Grid::new()
            .columns([GridLength::STAR, GridLength::Auto])
            .column_spacing(4.0)
            .children((
                TextBox::new().automation_name("New branch name").placeholder_text("New branch name").text(&shown.state.branch).on_text_changed(move |text| on_act.call(Act::Branch(text))),
                does(shown.on_act, "Create", Act::CreateBranch).grid_column(1).is_enabled(!shown.state.branch.trim().is_empty() && !git.busy),
            ))
            .into(),
    ];
    for name in git.branches.iter().take(BRANCHES) {
        let current = git.branch.as_deref() == Some(name.as_str());
        let mut cells: Vec<View> = vec![
            widgets::line_icon("git-branch", 13.0, shown.dark).opacity(if current { 1.0 } else { 0.55 }).vertical_alignment(VerticalAlignment::Center).into(),
            mono(name).grid_column(1).text_trimming(TextTrimming::CharacterEllipsis).vertical_alignment(VerticalAlignment::Center).into(),
        ];
        if current {
            cells.push(caption("Current").grid_column(2).into());
        }
        let (core, switch_to) = (core.clone(), name.clone());
        let button = Button::new()
            .style(ButtonStyle::Subtle)
            .horizontal_alignment(HorizontalAlignment::Stretch)
            .horizontal_content_alignment(HorizontalAlignment::Stretch)
            .automation_name(if current { format!("{name}, the current branch") } else { format!("Switch to {name}") })
            .is_enabled(!current && !git.busy)
            .content(Grid::new().columns([GridLength::Auto, GridLength::STAR, GridLength::Auto]).column_spacing(8.0).children(cells))
            .on_click(move || core.send(Event::SwitchBranch { name: switch_to.clone() }));
        lines.push(if current { Border::new().corner_radius(4.0).background(theme::selected()).content(button).into() } else { button.into() });
    }
    if git.branches.len() > BRANCHES {
        lines.push(caption(&format!("… and {} more", git.branches.len() - BRANCHES)).into());
    }
    card(lines)
}

/// The repository's worktrees, and the form that makes another.
fn worktrees(core: &CoreLink, git: &GitPanelView, shown: &Shown<'_>) -> View {
    let form = shown.state.worktree.as_ref();
    let mut lines: Vec<View> = vec![
        Grid::new()
            .columns([GridLength::STAR, GridLength::Auto])
            .children((heading("Worktrees"), does(shown.on_act, if form.is_some() { "Cancel" } else { "New Worktree" }, Act::Worktree).grid_column(1).style(ButtonStyle::Subtle)))
            .into(),
    ];
    if let Some((base, branch)) = form {
        let (on_base, on_branch) = (shown.on_act.clone(), shown.on_act.clone());
        lines.push(TextBox::new().automation_name("Base branch").placeholder_text("Base branch (main, for one)").text(base).on_text_changed(move |text| on_base.call(Act::WorktreeBase(text))).into());
        lines.push(TextBox::new().automation_name("New branch").placeholder_text("New branch (empty: the base branch itself)").text(branch).on_text_changed(move |text| on_branch.call(Act::WorktreeBranch(text))).into());
        lines.push(does(shown.on_act, "Create Worktree", Act::CreateWorktree).style(ButtonStyle::Accent).is_enabled(!base.trim().is_empty() && !git.busy).into());
    }
    if git.worktrees.is_empty() {
        lines.push(caption("No worktrees.").into());
    }
    for tree in &git.worktrees {
        let mut about: Vec<View> = vec![TextBlock::new().text(&tree.branch).font_family(theme::MONO).font_size(theme::CAPTION + 1.0).font_weight(FontWeight::SEMI_BOLD).into(), caption(&tree.path).text_trimming(TextTrimming::CharacterEllipsis).into()];
        if !tree.threads.is_empty() {
            about.push(caption(&format!("Used by {}", tree.threads.join(", "))).text_trimming(TextTrimming::CharacterEllipsis).into());
        }
        let end: View = if tree.current {
            caption("This thread").grid_column(1).vertical_alignment(VerticalAlignment::Center).into()
        } else {
            tell_icon(core, "\u{E74D}", &format!("Remove the worktree {}", tree.branch), Event::RemoveWorktree { path: tree.path.clone(), force: false }).grid_column(1).tooltip("Remove the worktree (kept if it has changes)")
        };
        lines.push(Grid::new().columns([GridLength::STAR, GridLength::Auto]).column_spacing(8.0).children((StackPanel::new().orientation(Orientation::Vertical).children(about), end)).into());
    }
    card(lines)
}
