//! What the open thread's page keeps of its own, and what its parts share
//! when they draw: which rows are opened, how much of a long thread is
//! shown, the find box's text, whether the page follows the latest row.

use std::collections::HashSet;
use std::rc::Rc;

use domovoi_core::view::{DiagramView, ImageView, ItemView};
use windows_reactor::Callback;

use crate::media::Media;

/// How many entries a thread shows at first, and how many more each time earlier ones are asked for.
pub const WINDOW: usize = 60;

/// What the page keeps of its own.
pub struct State {
    /// Rows and groups opened or shut against how they start.
    toggled: HashSet<String>,
    /// The plan and checklist above the composer are open.
    pub progress_open: bool,
    /// How many entries from the end are drawn.
    pub shown: usize,
    /// The find box's text (the window's copy).
    pub find: String,
    /// The page is at its end, and stays there as rows come.
    pub following: bool,
    /// Rows came while the page was not at its end.
    pub unseen: bool,
    /// The picture viewer: the pictures of the row it was opened from, and the one shown.
    pub viewer: Option<(Vec<ImageView>, usize)>,
}

impl Default for State {
    fn default() -> Self {
        Self { toggled: HashSet::new(), progress_open: false, shown: WINDOW, find: String::new(), following: true, unseen: false, viewer: None }
    }
}

/// What the page's parts tell the window, besides what goes straight to the core.
#[derive(Clone, Debug)]
pub enum Act {
    /// A row or group opened or shut (its id).
    Toggle(String),
    /// Text for the clipboard.
    Copy(String),
    /// A page for the browser.
    Open(String),
    /// A picture opened in the viewer: its row's pictures, and which.
    View(Vec<ImageView>, usize),
    /// The viewer moved by this many pictures, or (`None`) shut.
    Viewer(Option<i32>),
    /// Save a plan's text into the workspace (its row).
    SavePlan(String),
    /// Put the files back as they were before a turn: its checkpoint, and what the checkpoint is called.
    Revert(String, String),
    /// Draw earlier rows too.
    Earlier,
    /// The plan and checklist opened or shut.
    Progress,
    /// The find box's text.
    Find(Rc<str>),
    /// Go to the page's end and follow it.
    Latest,
    /// A file of the workspace, opened in the Files tab.
    OpenFile(String),
    /// A page an edit wrote, opened as an artifact.
    Preview(String),
    /// What a turn changed (its diff source), in the Diff tab.
    Diff(String),
    /// Bring a row into what is drawn, and open what folds it away.
    Reveal(String),
}

impl State {
    /// Whether `id` is open, given how it starts.
    pub fn open(&self, id: &str, starts_open: bool) -> bool {
        starts_open != self.toggled.contains(id)
    }

    pub fn toggle(&mut self, id: String) {
        if !self.toggled.remove(&id) {
            self.toggled.insert(id);
        }
    }

    /// Another thread is on the page: nothing kept of the one before.
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Moves the viewer by `delta` pictures (staying within them), or shuts it.
    pub fn viewer(&mut self, delta: Option<i32>) {
        match (&mut self.viewer, delta) {
            (Some((pictures, at)), Some(delta)) => *at = (*at as i64 + i64::from(delta)).clamp(0, pictures.len() as i64 - 1) as usize,
            _ => self.viewer = None,
        }
    }
}

/// What every part needs to draw.
#[derive(Clone, Copy)]
pub struct Draw<'a> {
    /// The transcript's text size.
    pub size: f64,
    pub dark: bool,
    pub media: &'a Media,
    pub diagrams: &'a [DiagramView],
    pub state: &'a State,
    pub on_act: &'a Callback<Act>,
}

/// A thread's rows as the page lists them: a row on its own, or the steps
/// of one turn folded under a summary (`ItemView::group`).
#[derive(Debug, PartialEq)]
pub enum Entry<'a> {
    Row(&'a ItemView),
    Group(&'a [Rc<ItemView>]),
}

/// The rows as entries, steps of one group together.
pub fn entries(rows: &[Rc<ItemView>]) -> Vec<Entry<'_>> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < rows.len() {
        let group = &rows[at].group;
        if group.is_empty() {
            out.push(Entry::Row(&rows[at]));
            at += 1;
        } else {
            let end = rows[at..].iter().position(|row| row.group != *group).map_or(rows.len(), |n| at + n);
            out.push(Entry::Group(&rows[at..end]));
            at = end;
        }
    }
    out
}

/// The entries to draw of `all`: the last `shown`, and how many are left out before them.
pub fn window<'e, 'a>(all: &'e [Entry<'a>], shown: usize) -> (usize, &'e [Entry<'a>]) {
    let hidden = all.len().saturating_sub(shown);
    (hidden, &all[hidden..])
}

/// The key a Mermaid diagram is asked for under: one per source and theme.
pub fn diagram_key(source: &str, dark: bool) -> String {
    let hash = source.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |acc, byte| (acc ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3));
    format!("{hash:016x}:{}", if dark { "d" } else { "l" })
}

/// The Mermaid diagrams finished in `rows`: each one's key and source.
pub fn diagrams_in<'a>(rows: impl Iterator<Item = &'a ItemView>, dark: bool) -> Vec<(String, &'a str)> {
    let blocks = rows.flat_map(|row| row.blocks.iter());
    blocks.filter(|b| b.kind == "code" && b.fence_closed && b.language.eq_ignore_ascii_case("mermaid")).map(|b| (diagram_key(&b.text, dark), b.text.as_str())).collect()
}

/// A span of time as a person says it: `45s`, `1m 12s`, `2h 5m`.
pub fn duration(ms: i64) -> String {
    let seconds = (ms / 1000).max(0);
    match seconds {
        ..60 => format!("{seconds}s"),
        60..3600 => format!("{}m {}s", seconds / 60, seconds % 60),
        _ => format!("{}h {}m", seconds / 3600, seconds % 3600 / 60),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use domovoi_core::view::MarkdownBlock;
    use std::sync::Arc;

    fn row(id: &str, group: &str) -> Rc<ItemView> {
        Rc::new(ItemView { id: id.into(), group: group.into(), ..ItemView::default() })
    }

    #[test]
    fn steps_of_one_group_are_one_entry() {
        let rows = vec![row("a", ""), row("b", "g1"), row("c", "g1"), row("d", "g2"), row("e", ""), row("f", "g3")];
        let entries = entries(&rows);
        let shape: Vec<usize> = entries.iter().map(|e| if let Entry::Group(steps) = e { steps.len() } else { 0 }).collect();
        assert_eq!(shape, [0, 2, 1, 0, 1]);
        let (hidden, shown) = window(&entries, 2);
        assert_eq!((hidden, shown.len()), (3, 2));
        assert_eq!(window(&entries, 60).0, 0);
    }

    #[test]
    fn a_row_opens_and_shuts_against_how_it_starts() {
        let mut state = State::default();
        assert!(state.open("a", true) && !state.open("b", false));
        state.toggle("a".into());
        state.toggle("b".into());
        assert!(!state.open("a", true) && state.open("b", false));
        state.toggle("a".into());
        assert!(state.open("a", true));
    }

    #[test]
    fn the_viewer_stays_within_its_pictures() {
        let picture = |name: &str| ImageView { name: name.into(), url: format!("http://s/{name}") };
        let mut state = State { viewer: Some((vec![picture("a"), picture("b")], 0)), ..State::default() };
        state.viewer(Some(-1));
        assert_eq!(state.viewer.as_ref().map(|(_, at)| *at), Some(0));
        state.viewer(Some(1));
        state.viewer(Some(1));
        assert_eq!(state.viewer.as_ref().map(|(_, at)| *at), Some(1));
        state.viewer(None);
        assert!(state.viewer.is_none());
    }

    #[test]
    fn only_finished_mermaid_blocks_are_diagrams_and_a_theme_is_a_key_of_its_own() {
        let block = |language: &str, fence_closed: bool| MarkdownBlock { kind: "code".into(), language: language.into(), text: "graph TD; a-->b".into(), fence_closed, ..MarkdownBlock::default() };
        let row = ItemView { blocks: Arc::new(vec![block("mermaid", true), block("Mermaid", false), block("rust", true)]), ..ItemView::default() };
        let found = diagrams_in(std::iter::once(&row), true);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].0, diagram_key("graph TD; a-->b", true));
        assert_ne!(diagram_key("graph TD; a-->b", true), diagram_key("graph TD; a-->b", false));
    }

    #[test]
    fn a_span_of_time_reads_as_a_person_says_it() {
        assert_eq!(duration(45_000), "45s");
        assert_eq!(duration(72_000), "1m 12s");
        assert_eq!(duration(7_500_000), "2h 5m");
        assert_eq!(duration(-5), "0s");
    }
}
