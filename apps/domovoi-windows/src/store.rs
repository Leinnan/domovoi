//! The view model as the window draws it: the core's sections, kept current
//! with its deltas, each an `Rc`. A delta names only the sections that
//! changed, so only those are replaced; reactor skips a component whose input
//! is the same `Rc` as before.
//!
//! The open thread is kept as its header and its rows apart. A streamed token
//! changes one row, and only that row is replaced (`ThreadPatch`).

use std::collections::HashMap;
use std::rc::Rc;

use domovoi_core::delta::{ThreadChange, ThreadPatch, ViewDelta};
use domovoi_core::view::{
    AgentSessionsView, ComposerView, ConnectionView, DiagramView, FolderBrowserView, IssuesView, ItemView, PaletteView, PanelView, PullRequestsView, SettingsView, SidebarView,
    ThreadPageView, ToastView, UsageView,
};

#[derive(Default)]
pub struct Store {
    pub connection: Rc<ConnectionView>,
    pub sidebar: Rc<SidebarView>,
    pub thread: Option<Thread>,
    pub composer: Rc<ComposerView>,
    pub toast: Option<Rc<ToastView>>,
    pub route: Rc<str>,
    /// The server's folders, as the folder dialog walks them.
    pub folder_browser: Rc<FolderBrowserView>,
    /// Folders with other agents' sessions, for the import dialog.
    pub agent_sessions: Rc<AgentSessionsView>,
    /// What Settings shows of the server (each section once it was asked for).
    pub settings: Rc<SettingsView>,
    /// The Mermaid diagrams asked for, with their pictures as they come.
    pub diagrams: Rc<Vec<DiagramView>>,
    /// The panel beside the thread: the diff, artifacts, terminals, files and repository.
    pub panel: Rc<PanelView>,
    /// The command palette, while it is open.
    pub palette: Option<Rc<PaletteView>>,
    /// The pages beside the threads: the projects' pull requests, and what the agents used.
    pub issues: Rc<IssuesView>,
    pub pull_requests: Rc<PullRequestsView>,
    pub usage: Rc<UsageView>,
}

/// The open thread.
#[derive(Default)]
pub struct Thread {
    /// The page apart from its rows (`items` is empty).
    pub page: Rc<ThreadPageView>,
    /// Its rows in order; a row that did not change is the `Rc` it was.
    pub rows: Vec<Rc<ItemView>>,
}

impl Thread {
    fn patch(&mut self, patch: ThreadPatch) {
        if let Some(header) = patch.header {
            self.page = Rc::new(header);
        }
        let mut changed: HashMap<String, ItemView> = patch.rows.into_iter().map(|row| (row.id.clone(), row)).collect();
        self.rows = match patch.order {
            Some(order) => {
                let mut kept: HashMap<String, Rc<ItemView>> = std::mem::take(&mut self.rows).into_iter().map(|row| (row.id.clone(), row)).collect();
                order.into_iter().filter_map(|id| changed.remove(&id).map(Rc::new).or_else(|| kept.remove(&id))).collect()
            }
            None => std::mem::take(&mut self.rows).into_iter().map(|row| changed.remove(&row.id).map_or(row, Rc::new)).collect(),
        };
    }
}

/// Which parts a delta replaced.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Changed {
    pub connection: bool,
    pub sidebar: bool,
    pub thread: bool,
    pub composer: bool,
    pub settings: bool,
    pub diagrams: bool,
    pub panel: bool,
    pub palette: bool,
}

impl Store {
    pub fn apply(&mut self, delta: ViewDelta) -> Changed {
        let changed = Changed {
            connection: delta.connection.is_some(),
            sidebar: delta.sidebar.is_some(),
            thread: delta.thread.is_some(),
            composer: delta.composer.is_some(),
            settings: delta.settings.is_some(),
            diagrams: delta.diagrams.is_some(),
            panel: delta.panel.is_some(),
            palette: delta.palette.is_some(),
        };
        if let Some(connection) = delta.connection {
            self.connection = Rc::new(connection);
        }
        if let Some(sidebar) = delta.sidebar {
            self.sidebar = Rc::new(sidebar);
        }
        match delta.thread {
            None => {}
            Some(ThreadChange::Closed) => self.thread = None,
            Some(ThreadChange::Page(patch)) => self.thread.get_or_insert_with(Thread::default).patch(*patch),
        }
        if let Some(composer) = delta.composer {
            self.composer = Rc::new(composer);
        }
        if let Some(change) = delta.toast {
            self.toast = change.toast.map(Rc::new);
        }
        if let Some(route) = delta.route {
            self.route = route.into();
        }
        if let Some(folders) = delta.folder_browser {
            self.folder_browser = Rc::new(folders);
        }
        if let Some(sessions) = delta.agent_sessions {
            self.agent_sessions = Rc::new(sessions);
        }
        if let Some(settings) = delta.settings {
            self.settings = Rc::new(settings);
        }
        if let Some(diagrams) = delta.diagrams {
            self.diagrams = Rc::new(diagrams);
        }
        if let Some(panel) = delta.panel {
            self.panel = Rc::new(panel);
        }
        if let Some(change) = delta.palette {
            self.palette = change.palette.map(Rc::new);
        }
        if let Some(issues) = delta.issues { self.issues = Rc::new(issues); }
        if let Some(requests) = delta.pull_requests {
            self.pull_requests = Rc::new(requests);
        }
        if let Some(usage) = delta.usage {
            self.usage = Rc::new(usage);
        }
        changed
    }

    /// The pairing form is shown instead of the threads.
    pub fn needs_pairing(&self) -> bool {
        let c = &self.connection;
        c.needs_pairing || c.server_changed || c.environments.is_empty() && c.state != "connected"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use domovoi_core::view::ViewModel;

    #[test]
    fn only_the_parts_a_delta_names_are_replaced() {
        let mut store = Store::default();
        let before = ViewModel::default();
        let mut after = before.clone();
        after.connection.state = "connected".into();
        let changed = store.apply(ViewDelta::between(Some(&before), &after));
        assert_eq!(changed, Changed { connection: true, ..Changed::default() });
        assert_eq!(store.connection.state, "connected");
        let sidebar = Rc::clone(&store.sidebar);
        let mut later = after.clone();
        later.connection.error = Some("gone".into());
        store.apply(ViewDelta::between(Some(&after), &later));
        assert!(Rc::ptr_eq(&sidebar, &store.sidebar), "the sidebar is untouched, so the same Rc");
        assert_eq!(store.connection.error.as_deref(), Some("gone"));
    }

    #[test]
    fn a_new_device_pairs_first() {
        assert!(Store::default().needs_pairing());
    }

    fn row(id: &str, text: &str) -> ItemView {
        ItemView { id: id.into(), text: text.into(), ..ItemView::default() }
    }

    fn page(id: &str, title: &str, items: Vec<ItemView>) -> ViewModel {
        ViewModel { thread: Some(ThreadPageView { id: id.into(), title: title.into(), items, ..ThreadPageView::default() }), ..ViewModel::default() }
    }

    fn texts(store: &Store) -> Vec<String> {
        store.thread.as_ref().map(|t| t.rows.iter().map(|r| r.text.clone()).collect()).unwrap_or_default()
    }

    #[test]
    fn a_row_that_did_not_change_is_the_same_rc() {
        let mut store = Store::default();
        let first = page("t1", "One", vec![row("a", "hello"), row("b", "wor")]);
        store.apply(ViewDelta::between(None, &first));
        assert_eq!(texts(&store), ["hello", "wor"]);
        let thread = store.thread.as_ref().expect("a thread is open");
        let (kept, header) = (Rc::clone(&thread.rows[0]), Rc::clone(&thread.page));

        // A token streams into the last row: the first row and the header stay.
        let second = page("t1", "One", vec![row("a", "hello"), row("b", "world")]);
        store.apply(ViewDelta::between(Some(&first), &second));
        let thread = store.thread.as_ref().expect("still open");
        assert_eq!(texts(&store), ["hello", "world"]);
        assert!(Rc::ptr_eq(&kept, &thread.rows[0]));
        assert!(Rc::ptr_eq(&header, &thread.page));

        // Rows come, go and move; the title changes.
        let third = page("t1", "Renamed", vec![row("c", "new"), row("a", "hello")]);
        store.apply(ViewDelta::between(Some(&second), &third));
        let thread = store.thread.as_ref().expect("still open");
        assert_eq!(texts(&store), ["new", "hello"]);
        assert!(Rc::ptr_eq(&kept, &thread.rows[1]));
        assert_eq!(thread.page.title, "Renamed");

        // Another thread comes whole; then none.
        let other = page("t2", "Two", vec![row("a", "another thread's row")]);
        store.apply(ViewDelta::between(Some(&third), &other));
        assert_eq!(texts(&store), ["another thread's row"]);
        store.apply(ViewDelta::between(Some(&other), &ViewModel::default()));
        assert!(store.thread.is_none());
    }
}
