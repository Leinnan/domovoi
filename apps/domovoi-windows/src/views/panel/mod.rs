//! The panel beside the thread: what changed, the thread's artifacts, its
//! terminals, the workspace's files and its repository, one at a time.
//!
//! The core holds what the panel shows (`PanelView`). What is typed and not
//! sent yet, and what is folded shut, is kept here (`State`).

pub mod artifacts;
pub mod diff;
pub mod files;
pub mod git;
pub mod terminal;

use std::collections::HashSet;
use std::rc::Rc;

use domovoi_core::Event;
use domovoi_core::view::{ImageView, PanelView};
use windows_reactor::{
    Border, Button, ButtonStyle, Callback, CharacterEventInfo, ElementRef, FontWeight, Grid, GridLength, HorizontalAlignment, KeyEventInfo, Orientation, RoutedCallback,
    StackPanel, TextBlock, TextWrapping, Thickness, TooltipExt as _, VerticalAlignment, View,
};

use crate::core_link::CoreLink;
use crate::media::Media;
use crate::{theme, widgets};

/// The tabs in order: what the core calls each, its name, its drawing and its shortcut.
pub const TABS: [(&str, &str, &str, &str); 5] = [
    ("diff", "Diff", "diff", "Ctrl+Shift+D"),
    ("artifacts", "Artifacts", "sparkles", "Ctrl+Shift+A"),
    ("terminal", "Terminal", "terminal", "Ctrl+Shift+T"),
    ("files", "Files", "folder", "Ctrl+Shift+E"),
    ("git", "Git", "git-branch", "Ctrl+Alt+G"),
];

/// The tab shown for what the core names (an unknown one is the diff).
pub fn tab(named: &str) -> &'static str {
    TABS.iter().map(|(id, ..)| *id).find(|id| *id == named).unwrap_or("diff")
}

/// What the panel keeps of its own.
#[derive(Default)]
pub struct State {
    /// The files of the diff folded shut.
    pub folded: HashSet<String>,
    /// The file search as typed.
    pub file_query: String,
    /// The file being changed: its path and its text as typed.
    pub editing: Option<(String, String)>,
    /// The commit message and the new branch's name as typed.
    pub commit: String,
    pub branch: String,
    /// The new worktree's form, when open: the branch it starts from and its own.
    pub worktree: Option<(String, String)>,
    /// What is asked about the open artifact, as typed.
    pub ask: String,
    /// The terminal has the keyboard.
    pub terminal_focused: bool,
    /// The size last asked for, and of which terminal.
    asked_size: (String, u32, u32),
}

/// What is done in the panel that is not one event for the core.
#[derive(Clone, Debug)]
pub enum Act {
    /// A file of the diff folded or unfolded.
    Fold(String),
    FileQuery(Rc<str>),
    /// Start changing the previewed file (`true`) or give the change up.
    Edit(bool),
    EditText(Rc<str>),
    Save,
    Commit(Rc<str>),
    Branch(Rc<str>),
    CreateBranch,
    /// `commit`, `push`, `create_pr`, `commit_push` or `commit_push_pr`.
    Git(&'static str),
    /// The new worktree's form opened or shut; its fields; its making.
    Worktree,
    WorktreeBase(Rc<str>),
    WorktreeBranch(Rc<str>),
    CreateWorktree,
    Ask(Rc<str>),
    SendAsk,
    Copy(String),
    Open(String),
    /// A picture opened over the window.
    View(ImageView),
    /// The terminal's screen to the clipboard, and the clipboard into the terminal.
    CopyScreen,
    Paste,
    /// A key for the terminal.
    Key { key: &'static str, text: String, ctrl: bool, alt: bool },
    /// The wheel over the terminal, in notches (away from the user is positive).
    Wheel(i32),
    /// The terminal was clicked, and gained or lost the keyboard.
    Clicked,
    Focused(bool),
}

/// What the window does after an act.
pub enum After {
    Nothing,
    Copy(String),
    Open(String),
    View(ImageView),
    /// Give the terminal the keyboard.
    FocusTerminal,
    /// Read the clipboard's text for the terminal.
    Paste,
}

impl State {
    /// Does `act`, telling the core what it means, and says what is left for the window.
    pub fn apply(&mut self, act: Act, core: &CoreLink, panel: &PanelView) -> After {
        match act {
            Act::Fold(path) => {
                if !self.folded.remove(&path) {
                    self.folded.insert(path);
                }
            }
            Act::FileQuery(text) => {
                if *text != *self.file_query {
                    self.file_query = text.to_string();
                    core.send(Event::SetFileQuery { query: self.file_query.clone() });
                }
            }
            Act::Edit(true) => self.editing = panel.files.preview.as_ref().filter(|p| p.editable).map(|p| (p.path.clone(), p.text.clone())),
            Act::Edit(false) => self.editing = None,
            Act::EditText(text) => {
                if let Some((_, typed)) = &mut self.editing {
                    *typed = text.to_string();
                }
            }
            Act::Save => {
                if let Some((path, text)) = self.editing.take() {
                    core.send(Event::SaveFile { path, text });
                }
            }
            Act::Commit(text) => self.commit = text.to_string(),
            Act::Branch(text) => self.branch = text.to_string(),
            Act::CreateBranch => {
                let name = std::mem::take(&mut self.branch).trim().to_owned();
                if !name.is_empty() {
                    core.send(Event::CreateBranch { name });
                }
            }
            Act::Git(action) => {
                // The message goes with a commit; a push or a pull request takes none.
                let message = if action.starts_with("commit") { std::mem::take(&mut self.commit) } else { String::new() };
                core.send(Event::GitAction { action: action.into(), message });
            }
            Act::Worktree => self.worktree = if self.worktree.is_some() { None } else { Some((panel.git.branch.clone().unwrap_or_default(), String::new())) },
            Act::WorktreeBase(text) => {
                if let Some((base, _)) = &mut self.worktree {
                    *base = text.to_string();
                }
            }
            Act::WorktreeBranch(text) => {
                if let Some((_, branch)) = &mut self.worktree {
                    *branch = text.to_string();
                }
            }
            Act::CreateWorktree => {
                if let Some((base, branch)) = self.worktree.take_if(|(base, _)| !base.trim().is_empty()) {
                    core.send(Event::CreateWorktree { base: base.trim().to_owned(), branch: branch.trim().to_owned() });
                }
            }
            Act::Ask(text) => self.ask = text.to_string(),
            Act::SendAsk => {
                let prompt = std::mem::take(&mut self.ask).trim().to_owned();
                if let Some(artifact) = panel.artifacts.active.as_ref().filter(|_| !prompt.is_empty()) {
                    core.send(Event::AskAboutArtifact { artifact_id: artifact.id.clone(), prompt });
                }
            }
            Act::Copy(text) => return After::Copy(text),
            Act::Open(url) => return After::Open(url),
            Act::View(picture) => return After::View(picture),
            Act::CopyScreen => {
                if let Some(screen) = &panel.terminal.active {
                    return After::Copy(terminal::plain_text(screen));
                }
            }
            Act::Paste => return After::Paste,
            Act::Key { key, text, ctrl, alt } => core.send(Event::TerminalKey { key: key.into(), text, ctrl, alt }),
            Act::Wheel(notches) => {
                if notches != 0 {
                    core.send(Event::TerminalScroll { lines: notches * 3 });
                }
            }
            Act::Clicked => return After::FocusTerminal,
            Act::Focused(focused) => self.terminal_focused = focused,
        }
        After::Nothing
    }

    /// After the panel changed: a file no longer previewed is no longer being changed.
    pub fn sync(&mut self, panel: &PanelView) {
        let previewed = panel.files.preview.as_ref().map(|p| p.path.as_str());
        if self.editing.as_ref().is_some_and(|(path, _)| Some(path.as_str()) != previewed) {
            self.editing = None;
        }
    }

    /// Asks for the terminal to be as many rows and columns as fit a panel
    /// `width` by `height`, once for each size.
    pub fn fit(&mut self, core: &CoreLink, panel: &PanelView, width: f64, height: f64) {
        let Some(screen) = panel.terminal.active.as_ref().filter(|_| panel.open && tab(&panel.tab) == "terminal") else { return };
        let (rows, cols) = terminal::grid(width, height);
        let wanted = (screen.id.clone(), rows, cols);
        if (screen.rows, screen.cols) != (rows, cols) && self.asked_size != wanted {
            core.send(Event::TerminalResize { rows, cols });
            self.asked_size = wanted;
        }
    }
}

/// What the tabs draw from.
#[derive(Clone, Copy)]
pub struct Shown<'a> {
    pub panel: &'a PanelView,
    /// The open thread.
    pub thread_id: Option<&'a str>,
    pub state: &'a State,
    pub media: &'a Media,
    pub dark: bool,
    pub on_act: &'a Callback<Act>,
}

/// What the terminal needs of the window.
pub struct Hooks<'a> {
    pub terminal: &'a ElementRef<Border>,
    pub on_keys: RoutedCallback<KeyEventInfo>,
    pub on_chars: RoutedCallback<CharacterEventInfo>,
}

fn caption(text: &str) -> TextBlock {
    TextBlock::new().text(text).font_size(theme::CAPTION).opacity(0.65)
}

fn mono(text: &str) -> TextBlock {
    TextBlock::new().text(text).font_family(theme::MONO).font_size(theme::CAPTION)
}

fn heading(text: &str) -> TextBlock {
    TextBlock::new().text(text).font_weight(FontWeight::SEMI_BOLD).vertical_alignment(VerticalAlignment::Center)
}

/// A small button with only a glyph on it. Its tooltip is added last, by whoever places it.
fn icon(glyph: &str, name: &str) -> Button {
    Button::new().style(ButtonStyle::Subtle).automation_name(name).content(theme::glyph(glyph, 13.0))
}

/// A button that tells the core `event`.
fn tell(core: &CoreLink, label: &str, event: Event) -> Button {
    let core = core.clone();
    Button::new().content(TextBlock::new().text(label).font_size(theme::CAPTION)).on_click(move || core.send(event.clone()))
}

/// An icon button that tells the core `event`.
fn tell_icon(core: &CoreLink, glyph: &str, name: &str, event: Event) -> Button {
    let core = core.clone();
    icon(glyph, name).on_click(move || core.send(event.clone()))
}

/// A button that does `act` here.
fn does(on_act: &Callback<Act>, label: &str, act: Act) -> Button {
    let on_act = on_act.clone();
    Button::new().content(TextBlock::new().text(label).font_size(theme::CAPTION)).on_click(move || on_act.call(act.clone()))
}

/// An icon button that does `act` here.
fn does_icon(on_act: &Callback<Act>, glyph: &str, name: &str, act: Act) -> Button {
    let on_act = on_act.clone();
    icon(glyph, name).on_click(move || on_act.call(act.clone()))
}

/// A group of lines set apart.
fn card(lines: Vec<View>) -> View {
    Border::new()
        .padding(Thickness::uniform(10.0))
        .background(theme::code())
        .corner_radius(6.0)
        .content(StackPanel::new().orientation(Orientation::Vertical).spacing(6.0).children(lines))
        .into()
}

/// What a tab says when it has nothing to show.
fn nothing(glyph: &str, title: &str, text: &str, more: Option<View>) -> View {
    let mut lines: Vec<View> = vec![
        theme::glyph(glyph, 28.0).opacity(0.5).horizontal_alignment(HorizontalAlignment::Center).into(),
        TextBlock::new().text(title).font_weight(FontWeight::SEMI_BOLD).horizontal_alignment(HorizontalAlignment::Center).into(),
        caption(text).text_wrapping(TextWrapping::WrapWholeWords).horizontal_alignment(HorizontalAlignment::Center).into(),
    ];
    lines.extend(more);
    StackPanel::new()
        .orientation(Orientation::Vertical)
        .spacing(8.0)
        .margin(Thickness::uniform(24.0))
        .vertical_alignment(VerticalAlignment::Center)
        .horizontal_alignment(HorizontalAlignment::Center)
        .children(lines)
        .into()
}

/// A file's name: what follows the last separator of `path`.
fn file_name(path: &str) -> &str {
    path.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next().filter(|name| !name.is_empty()).unwrap_or(path)
}

/// The panel: its tabs, then the one shown.
pub fn panel(core: &CoreLink, shown: &Shown<'_>, hooks: Hooks<'_>) -> View {
    let current = tab(&shown.panel.tab);
    let mut bar: Vec<View> = Vec::new();
    for (id, label, drawing, keys) in TABS {
        let selected = id == current;
        // Only the tab shown has its name beside its glyph: five names do not fit a narrow panel.
        let mut cells: Vec<View> = vec![widgets::line_icon(drawing, 15.0, shown.dark).opacity(if selected { 1.0 } else { 0.7 }).vertical_alignment(VerticalAlignment::Center).into()];
        if selected {
            cells.push(TextBlock::new().text(label).font_size(theme::CAPTION + 1.0).font_weight(FontWeight::SEMI_BOLD).vertical_alignment(VerticalAlignment::Center).into());
        }
        let core = core.clone();
        let button = Button::new()
            .style(ButtonStyle::Subtle)
            .automation_name(format!("{label} panel"))
            .content(StackPanel::new().orientation(Orientation::Horizontal).spacing(6.0).children(cells))
            .on_click(move || core.send(Event::ShowPanel { tab: id.into() }))
            .tooltip(format!("{label} ({keys})"));
        bar.push(Border::new().corner_radius(4.0).background(if selected { theme::selected() } else { theme::clear() }).content(button).into());
    }
    let content = match current {
        "artifacts" => artifacts::view(core, shown),
        "terminal" => terminal::view(core, shown, hooks),
        "files" => files::view(core, shown),
        "git" => git::view(core, shown),
        _ => diff::view(core, shown),
    };
    Grid::new()
        .rows([GridLength::Auto, GridLength::STAR])
        .children((
            Grid::new().columns([GridLength::STAR, GridLength::Auto]).margin(Thickness::new(8.0, 6.0, 6.0, 6.0)).children((
                StackPanel::new().orientation(Orientation::Horizontal).spacing(2.0).children(bar),
                tell_icon(core, "\u{E711}", "Close the panel", Event::TogglePanel).grid_column(1).tooltip("Close the panel (Ctrl+J)"),
            )),
            Border::new().grid_row(1).border_brush(theme::stroke()).border_thickness(Thickness::new(0.0, 1.0, 0.0, 0.0)).content(content),
        ))
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unknown_tab_is_the_diff_and_a_path_ends_in_its_name() {
        assert_eq!(tab("git"), "git");
        assert_eq!(tab(""), "diff");
        assert_eq!(tab("no-such-tab"), "diff");
        assert_eq!(file_name("src/views/panel.rs"), "panel.rs");
        assert_eq!(file_name("C:\\work\\repo\\"), "repo");
        assert_eq!(file_name("README.md"), "README.md");
    }
}
