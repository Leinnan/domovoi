//! What crosses FFI on each render: the parts of the view model that changed
//! since the view sent before, not the whole of it.
//!
//! At a long thread the view model is megabytes (2000 rows: 5.8 MB), and a
//! streamed token changes one row of it. `FfiApp` (the app the shells' core
//! runs) builds the full `ViewModel` as `DomovoiApp` does, compares it with the
//! one it built last time, and hands over a `ViewDelta`: each section only
//! when it changed, and of the thread page its header, its row order and its
//! changed rows. The shells keep the sections and rows and apply each delta
//! in order (`ViewDelta::apply` is the rule they follow). The first delta
//! after the core starts carries every section.

use crate::view::{
    AgentSessionsView, ComposerView, ConnectionView, FolderBrowserView, ItemView, PaletteView, PanelView, PullRequestsView,
    SettingsView, SidebarView, ThreadPageView, ToastView, UsageView, ViewModel,
};
use crate::{Effect, Event, Model, DomovoiApp};
use crux_core::{App, Command};
use facet::Facet;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Mutex;

/// The view model's changes since the last view; `None` keeps a section.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct ViewDelta {
    pub connection: Option<ConnectionView>,
    pub toast: Option<ToastChange>,
    pub sidebar: Option<SidebarView>,
    pub route: Option<String>,
    pub thread: Option<ThreadChange>,
    pub composer: Option<ComposerView>,
    pub panel: Option<PanelView>,
    pub palette: Option<PaletteChange>,
    pub settings: Option<SettingsView>,
    pub pull_requests: Option<PullRequestsView>,
    pub usage: Option<UsageView>,
    pub agent_sessions: Option<AgentSessionsView>,
    pub folder_browser: Option<FolderBrowserView>,
}

/// The toast shown now (`None`: none).
#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct ToastChange {
    pub toast: Option<ToastView>,
}

/// The palette open now (`None`: closed).
#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct PaletteChange {
    pub palette: Option<PaletteView>,
}

/// How the thread page changed.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[repr(C)]
pub enum ThreadChange {
    /// No thread page any more.
    Closed,
    /// The page's changes; a page for another thread than before comes whole.
    Page(Box<ThreadPatch>),
}

/// A thread page's changes.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct ThreadPatch {
    /// The page apart from its rows (`items` is empty), when any of it changed.
    pub header: Option<ThreadPageView>,
    /// Every row's id in order, when rows came, went or moved.
    pub order: Option<Vec<String>>,
    /// The rows that are new or changed.
    pub rows: Vec<ItemView>,
}

impl ViewDelta {
    /// What changed from `old` (`None`: nothing sent yet, so everything) to `new`.
    pub fn between(old: Option<&ViewModel>, new: &ViewModel) -> Self {
        fn changed<T: PartialEq + Clone>(old: Option<&T>, new: &T) -> Option<T> {
            (old != Some(new)).then(|| new.clone())
        }
        ViewDelta {
            connection: changed(old.map(|v| &v.connection), &new.connection),
            toast: changed(old.map(|v| &v.toast), &new.toast).map(|toast| ToastChange { toast }),
            sidebar: changed(old.map(|v| &v.sidebar), &new.sidebar),
            route: changed(old.map(|v| &v.route), &new.route),
            thread: thread_change(old.map(|v| &v.thread), &new.thread),
            composer: changed(old.map(|v| &v.composer), &new.composer),
            panel: changed(old.map(|v| &v.panel), &new.panel),
            palette: changed(old.map(|v| &v.palette), &new.palette).map(|palette| PaletteChange { palette }),
            settings: changed(old.map(|v| &v.settings), &new.settings),
            pull_requests: changed(old.map(|v| &v.pull_requests), &new.pull_requests),
            usage: changed(old.map(|v| &v.usage), &new.usage),
            agent_sessions: changed(old.map(|v| &v.agent_sessions), &new.agent_sessions),
            folder_browser: changed(old.map(|v| &v.folder_browser), &new.folder_browser),
        }
    }

    /// Applies the delta to the view model it was made against.
    pub fn apply(self, view: &mut ViewModel) {
        macro_rules! set {
            ($($field:ident),*) => { $( if let Some(v) = self.$field { view.$field = v; } )* };
        }
        set!(connection, sidebar, route, composer, panel, settings, pull_requests, usage, agent_sessions, folder_browser);
        if let Some(c) = self.toast {
            view.toast = c.toast;
        }
        if let Some(c) = self.palette {
            view.palette = c.palette;
        }
        match self.thread {
            None => {}
            Some(ThreadChange::Closed) => view.thread = None,
            Some(ThreadChange::Page(patch)) => {
                let page = view.thread.get_or_insert_with(ThreadPageView::default);
                patch.apply(page);
            }
        }
    }
}

impl ThreadPatch {
    /// Applies the patch to the page it was made against (a new page: a default one).
    pub fn apply(self, page: &mut ThreadPageView) {
        let mut items = std::mem::take(&mut page.items);
        if let Some(header) = self.header {
            *page = header;
        }
        let mut changed: HashMap<String, ItemView> = self.rows.into_iter().map(|r| (r.id.clone(), r)).collect();
        match self.order {
            Some(order) => {
                let mut kept: HashMap<String, ItemView> = items.into_iter().map(|r| (r.id.clone(), r)).collect();
                items = order
                    .into_iter()
                    .filter_map(|id| changed.remove(&id).or_else(|| kept.remove(&id)))
                    .collect();
            }
            None => {
                for row in &mut items {
                    if let Some(new) = changed.remove(&row.id) {
                        *row = new;
                    }
                }
            }
        }
        page.items = items;
    }
}

fn thread_change(old: Option<&Option<ThreadPageView>>, new: &Option<ThreadPageView>) -> Option<ThreadChange> {
    let old = old.map(Option::as_ref);
    match (old, new) {
        (Some(None), None) => None,
        (_, None) => Some(ThreadChange::Closed),
        (Some(Some(old)), Some(new)) if old.id == new.id => {
            let patch = thread_patch(Some(old), new);
            (patch.header.is_some() || patch.order.is_some() || !patch.rows.is_empty()).then(|| ThreadChange::Page(Box::new(patch)))
        }
        // Nothing sent yet, nothing open before, or another thread: whole.
        (_, Some(new)) => Some(ThreadChange::Page(Box::new(thread_patch(None, new)))),
    }
}

/// The page apart from its rows.
fn header(page: &ThreadPageView) -> ThreadPageView {
    page.clone_without_items()
}

fn thread_patch(old: Option<&ThreadPageView>, new: &ThreadPageView) -> ThreadPatch {
    let Some(old) = old else {
        return ThreadPatch {
            header: Some(header(new)),
            order: Some(new.items.iter().map(|r| r.id.clone()).collect()),
            rows: new.items.clone(),
        };
    };
    let same_header = old.eq_without_items(new);
    let same_order = old.items.len() == new.items.len() && old.items.iter().zip(&new.items).all(|(a, b)| a.id == b.id);
    let rows = if same_order {
        new.items.iter().zip(&old.items).filter(|(n, o)| n != o).map(|(n, _)| n.clone()).collect()
    } else {
        let before: HashMap<&str, &ItemView> = old.items.iter().map(|r| (r.id.as_str(), r)).collect();
        new.items.iter().filter(|r| before.get(r.id.as_str()) != Some(&r)).cloned().collect()
    };
    ThreadPatch {
        header: (!same_header).then(|| header(new)),
        order: (!same_order).then(|| new.items.iter().map(|r| r.id.clone()).collect()),
        rows,
    }
}

/// Every `ThreadPageView` field but `items`, listed once for the copy and
/// the comparison below (a field added to the page and missed here fails
/// to compile in `header`, whose struct literal names every field).
macro_rules! page_header {
    ($m:ident) => {
        $m!(
            id, title, project_id, project_title, branch, worktree_path, cwd, model_label, provider_instance_id,
            runtime_mode, interaction_mode, status, working, loading, pinned, settled, archived, pending_requests,
            pending_sends, working_since_ms, checkpoints, find, parent, can_merge_back, can_fork, context, queued,
            queue_held, background
        )
    };
}

impl ThreadPageView {
    /// A copy with no rows, without copying them.
    fn clone_without_items(&self) -> ThreadPageView {
        macro_rules! copy {
            ($($f:ident),*) => { ThreadPageView { items: Vec::new(), $($f: self.$f.clone()),* } };
        }
        page_header!(copy)
    }

    /// Whether everything but the rows is the same.
    fn eq_without_items(&self, other: &ThreadPageView) -> bool {
        macro_rules! eq {
            ($($f:ident),*) => { true $(&& self.$f == other.$f)* };
        }
        page_header!(eq)
    }
}

/// The app the shells' core runs: `DomovoiApp`, with its view sent as a
/// `ViewDelta` against the view it sent before.
#[derive(Default)]
pub struct FfiApp {
    /// The view the last delta brought the shell to.
    sent: Mutex<Option<Sent>>,
    /// Every view made from the rows that changed is compared with one
    /// built from nothing (tests).
    checked: bool,
    /// How many views were made from the rows that changed.
    from_rows: std::sync::atomic::AtomicU64,
}

/// The view model the shell holds, and where the model's changes stood
/// when it was built.
struct Sent {
    view: ViewModel,
    mark: (u64, u64),
}

impl FfiApp {
    /// An app that checks each view it makes from the rows that changed
    /// against the view built whole, with nothing cached, and panics on a
    /// difference. Costs a whole render per view: for tests.
    #[must_use]
    pub fn checked() -> Self {
        FfiApp { checked: true, ..FfiApp::default() }
    }

    /// How many views so far were the last one with rows built again,
    /// not the whole view built and compared.
    pub fn views_from_rows(&self) -> u64 {
        self.from_rows.load(std::sync::atomic::Ordering::Relaxed)
    }
}

impl Sent {
    /// The delta when rows' text grew and nothing else changed since this
    /// view: those rows built again, with the view brought up to date.
    /// `None` when more may have changed (the view is then built whole).
    fn grown(&mut self, model: &Model) -> Option<ViewDelta> {
        let ids = model.changes.rows_since(self.mark)?;
        let mut rows: Vec<(usize, ItemView)> = Vec::with_capacity(ids.len());
        if !ids.is_empty() {
            let page = self.view.thread.as_ref()?;
            for id in ids {
                // A row that streams is at the thread's end.
                let at = page.items.iter().rposition(|r| r.id == id)?;
                let row = crate::present::regrown_row(model, id, &page.items[at])?;
                if row != page.items[at] {
                    rows.push((at, row));
                }
            }
        }
        // Nothing failed: the view takes the rows, and the shell gets them.
        self.mark = model.changes.mark();
        if rows.is_empty() {
            return Some(ViewDelta::default());
        }
        let page = self.view.thread.as_mut()?;
        for (at, row) in &rows {
            page.items[*at] = row.clone();
        }
        let patch = ThreadPatch { header: None, order: None, rows: rows.into_iter().map(|(_, row)| row).collect() };
        Some(ViewDelta { thread: Some(ThreadChange::Page(Box::new(patch))), ..ViewDelta::default() })
    }
}

/// The view built whole from the model alone: no row or markdown kept from
/// an earlier render.
fn built_from_nothing(model: &Model) -> ViewModel {
    model.item_cache.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clear();
    model.markdown_prefixes.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clear();
    model.approval_diffs.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clear();
    DomovoiApp.view(model)
}

impl App for FfiApp {
    type Event = Event;
    type Model = Model;
    type ViewModel = ViewDelta;
    type Effect = Effect;

    fn update(&self, event: Event, model: &mut Model) -> Command<Effect, Event> {
        #[cfg(test)]
        if crate::ffi::PANIC_IN_NEXT_UPDATE.swap(false, std::sync::atomic::Ordering::SeqCst) {
            panic!("a bug in update (test)");
        }
        DomovoiApp.update(event, model)
    }

    /// The shell applies every view it reads, in order: each delta is
    /// against the one before.
    ///
    /// A reply streaming into a thread changes one row's text a few times a
    /// second: that view is the last one with the row built again, whatever
    /// the thread's length. Anything else builds the whole view and compares
    /// it with the last.
    fn view(&self, model: &Model) -> ViewDelta {
        let mut sent = self.sent.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(last) = sent.as_mut()
            && let Some(delta) = last.grown(model)
        {
            if self.checked {
                assert_eq!(last.view, built_from_nothing(model), "the view made from the rows that changed is the view built whole");
            }
            self.from_rows.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            return delta;
        }
        let new = DomovoiApp.view(model);
        let delta = ViewDelta::between(sent.as_ref().map(|s| &s.view), &new);
        *sent = Some(Sent { view: new, mark: model.changes.mark() });
        delta
    }
}
