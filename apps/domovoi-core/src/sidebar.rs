//! The thread list: threads by what they need from you (Inbox, Running,
//! Inactive, Archived), or under their projects.

use crate::app::{age_label, millis_from_iso};
use crate::model::Model;
use crate::replies::shell_status;
use crate::view::{ProjectIconView, SearchHitView, SidebarProjectView, SidebarSectionView, SidebarView, ThreadRowView};
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use domovoi_model::typed::{InteractionMode, ParentRelationship, ProjectShell, RuntimeRequestKind, ShellStatus, ThreadShell, Timestamp};

const THREADS_PER_PROJECT: usize = 8;
const EARLIER_PER_PROJECT: usize = 5;
/// Rows a section shows before "show more".
pub const ROWS_PER_SECTION: usize = 50;
/// Rows the glance surfaces (widgets, shortcuts) are given.
const SURFACE_THREADS: usize = 8;

/// Where a thread sits when the list is grouped by status.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Section {
    /// Waiting on you, finished since you looked, or pinned.
    Inbox,
    /// Working.
    Running,
    /// Read and idle, settled, or snoozed.
    // Keep the device-store value readable across upgrades and downgrades.
    #[serde(rename = "hibernating")]
    Inactive,
    Archived,
}

impl Section {
    pub const ALL: [Section; 4] = [Section::Inbox, Section::Running, Section::Inactive, Section::Archived];

    pub fn as_str(self) -> &'static str {
        match self {
            Section::Inbox => "inbox",
            Section::Running => "running",
            Section::Inactive => "inactive",
            Section::Archived => "archived",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Section::Inbox => "Inbox",
            Section::Running => "Running",
            Section::Inactive => "Inactive",
            Section::Archived => "Archived",
        }
    }

    pub fn parse(s: &str) -> Option<Section> {
        Section::ALL.into_iter().find(|section| section.as_str() == s)
    }
}

/// How the thread list is grouped.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Grouping {
    /// By what each thread needs (`Section`).
    #[default]
    Status,
    /// Under project headers.
    Project,
}

impl Grouping {
    pub fn as_str(self) -> &'static str {
        match self {
            Grouping::Status => "status",
            Grouping::Project => "project",
        }
    }

    pub fn parse(s: &str) -> Option<Grouping> {
        [Grouping::Status, Grouping::Project].into_iter().find(|g| g.as_str() == s)
    }
}

/// How this device lays out the thread list; saved on the device, not the server.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct SidebarLayout {
    pub grouping: Grouping,
    pub collapsed_sections: BTreeSet<Section>,
    pub collapsed_projects: BTreeSet<String>,
}

impl Default for SidebarLayout {
    fn default() -> Self {
        SidebarLayout { grouping: Grouping::Status, collapsed_sections: [Section::Inactive, Section::Archived].into(), collapsed_projects: BTreeSet::new() }
    }
}

/// The layout's key in the device's store (not a `cache:` key: those may be purged).
pub const KEY_SIDEBAR_LAYOUT: &str = "sidebarLayout";

pub fn is_settled(t: &ThreadShell) -> bool {
    let overridden = t.settled_override.as_ref().map(|settled| settled.as_str());
    (t.settled_at.is_some() && overridden != Some("active")) || overridden == Some("settled")
}

pub fn is_pinned(t: &ThreadShell) -> bool {
    matches!(t.pinned_at, Some(Some(_)))
}

/// When a thread was last active for its row: its last message from the
/// user, or else its last change.
fn shown_at(t: &ThreadShell) -> &str {
    t.latest_user_message_at.as_ref().unwrap_or(&t.updated_at).as_str()
}

/// When a snoozed thread wakes (Unix ms), while that's ahead; a thread
/// waiting on you isn't hidden by a snooze.
fn snoozed_until(model: &Model, t: &ThreadShell) -> Option<i64> {
    let until = t.snoozed_until.as_ref()?.as_ref().map(|until| crate::app::millis_from_iso(until.as_str())).filter(|u| *u > model.shown_ms)?;
    (shell_status(t) != ShellStatus::Approval).then_some(until)
}

/// Whether a thread ended since it was last looked at (the open one is being looked at).
fn is_unread(model: &Model, t: &ThreadShell) -> bool {
    let visited = t.last_visited_at.as_ref().and_then(|at| at.as_ref()).map_or("", Timestamp::as_str);
    matches!(shell_status(t), ShellStatus::Completed | ShellStatus::Failed) && visited < t.updated_at.as_str() && model.selected_thread.as_deref() != Some(t.id.as_str())
}

/// The section a thread's state puts it in, the first that fits: waiting on
/// you, working, put away (settled or snoozed), worth a look (unread or
/// pinned), and the remaining inactive threads.
fn natural_section(model: &Model, t: &ThreadShell) -> Section {
    if t.archived_at.is_some() {
        return Section::Archived;
    }
    match shell_status(t) {
        ShellStatus::Approval => return Section::Inbox,
        ShellStatus::Working => return Section::Running,
        _ => {}
    }
    if is_settled(t) || snoozed_until(model, t).is_some() {
        Section::Inactive
    } else if is_unread(model, t) || is_pinned(t) {
        Section::Inbox
    } else {
        Section::Inactive
    }
}

/// The section a thread is listed in. The open thread keeps the section it
/// was opened from rather than drop to Inactive as it is read, so its row
/// does not move from under the click.
pub fn section_of(model: &Model, t: &ThreadShell) -> Section {
    let section = natural_section(model, t);
    match &model.held_section {
        Some((id, held)) if section == Section::Inactive && id == t.id.as_str() && model.selected_thread.as_deref() == Some(id.as_str()) => *held,
        _ => section,
    }
}

/// The section to hold a thread in while it is open: where it was before.
pub fn hold_for(model: &Model, thread_id: &str) -> Option<(String, Section)> {
    let t = model.thread_shell(thread_id)?;
    Some((thread_id.to_owned(), natural_section(model, t)))
}

/// How a thread relates to the one it came from: `fork`, `subagent` or empty.
pub fn relation(t: &ThreadShell) -> &str {
    if t.lineage.parent_thread_id.is_none() { "" } else { t.lineage.relationship_to_parent.as_ref().map_or("", ParentRelationship::as_str) }
}

/// `import:<instance>:<session>` threads came from Claude Code, Codex or Pi.
fn thread_source(t: &ThreadShell) -> &'static str {
    let instance = t.id.as_str().strip_prefix("import:").and_then(|r| r.split(':').next()).unwrap_or("");
    match instance {
        "" => "",
        i => crate::present::source_label(i),
    }
}

/// Project titles and model names, looked up once for a list of rows.
pub struct Labels<'a> {
    projects: FxHashMap<&'a str, &'a str>,
    icons: FxHashMap<&'a str, ProjectIconView>,
    models: FxHashMap<(&'a str, &'a str), &'a str>,
}

impl<'a> Labels<'a> {
    pub fn new(model: &'a Model) -> Self {
        let projects = model.projects.iter().map(|p| (p.id.as_str(), p.title.as_str())).collect();
        let models = model
            .providers
            .iter()
            .flat_map(|p| p.models.iter().map(move |m| ((p.instance_id.as_str(), m.slug.as_str()), m.name.as_str())))
            .filter(|(_, name)| !name.is_empty())
            .collect();
        Labels { projects, models, icons: model.projects.iter().map(|p| (p.id.as_str(), crate::project_icons::view(model, p, false))).collect() }
    }

    pub(crate) fn model_label(&self, t: &'a ThreadShell) -> String {
        let selection = &t.model_selection;
        self.models.get(&(selection.instance_id.as_str(), selection.model.as_str())).copied().unwrap_or(selection.model.as_str()).to_owned()
    }
}

/// The thread waits on an answer to a question (not an approval).
fn is_question(t: &ThreadShell) -> bool {
    t.pending_runtime_request.as_ref().is_some_and(|pending| pending.kind == RuntimeRequestKind::UserInput)
}

/// The thread is in plan mode: its turns plan rather than act.
fn is_planning(t: &ThreadShell) -> bool {
    t.interaction_mode == InteractionMode::Plan
}

/// When the thread's turn under way started.
fn working_started_at(t: &ThreadShell) -> Option<&Timestamp> {
    if !matches!(shell_status(t), ShellStatus::Working | ShellStatus::Approval) {
        return None;
    }
    t.activity_run_started_at.as_ref().and_then(Option::as_ref).or(t.latest_run_started_at.as_ref().and_then(Option::as_ref))
}

fn working_since_ms(t: &ThreadShell) -> Option<i64> {
    working_started_at(t).map(|at| millis_from_iso(at.as_str())).filter(|ms| *ms > 0)
}

/// What a thread is doing, when that says more than its age.
pub(crate) fn status_label(model: &Model, t: &ThreadShell) -> String {
    match shell_status(t) {
        ShellStatus::Approval => if is_question(t) { "Question" } else { "Needs you" }.into(),
        ShellStatus::Working => {
            let doing = if is_planning(t) { "Planning" } else { "Working" };
            match working_started_at(t).map(|at| age_label(model.shown_ms, at.as_str())) {
                Some(age) if !age.is_empty() && age != "now" => format!("{doing} {age}"),
                _ => doing.into(),
            }
        }
        ShellStatus::Failed => "Failed".into(),
        _ if t.has_actionable_proposed_plan => "Plan ready".into(),
        _ => String::new(),
    }
}

/// The thread's pull request (linked by hand first, else its branch's) and
/// its state: `("#12", "open")`.
pub(crate) fn pull_request_badge(t: &ThreadShell) -> (String, String) {
    // A stack member the user dismissed stays as a tombstone, not a link.
    if let Some(link) = t.pull_requests.iter().flatten().find(|link| link.source != domovoi_model::typed::PullRequestLinkSource::StackDismissed) {
        let state = link.snapshot.as_ref().map(|s| if s.is_draft && s.state.as_str() == "open" { "draft" } else { s.state.as_str() }).unwrap_or_default();
        return (format!("#{}", link.number), state.to_owned());
    }
    match t.linked_pull_request.clone().flatten().or_else(|| t.branch_pull_request.clone().flatten()) {
        Some(pr) => (format!("#{}", pr.number), String::new()),
        None => (String::new(), String::new()),
    }
}

fn row(model: &Model, labels: &Labels, t: &ThreadShell) -> ThreadRowView {
    let (pull_request, pull_request_state) = pull_request_badge(t);
    ThreadRowView {
        plan_ready: t.has_actionable_proposed_plan && !matches!(shell_status(t), ShellStatus::Working | ShellStatus::Approval),
        plan_mode: is_planning(t),
        pull_request,
        pull_request_state,
        id: t.id.as_str().to_owned(),
        project_id: t.project_id.as_str().to_owned(),
        project_icon: labels.icons.get(t.project_id.as_str()).cloned().unwrap_or_default(),
        project_title: labels.projects.get(t.project_id.as_str()).copied().unwrap_or_default().to_owned(),
        title: t.title.clone(),
        model_label: labels.model_label(t),
        status_label: status_label(model, t),
        status: shell_status(t).as_str().into(),
        working_since_ms: working_since_ms(t).unwrap_or(0),
        question: is_question(t),
        unread: is_unread(model, t),
        pinned: is_pinned(t),
        settled: is_settled(t),
        archived: t.archived_at.is_some(),
        branch: t.branch.clone().filter(|branch| !branch.is_empty()),
        in_worktree: t.worktree_path.is_some(),
        age: age_label(model.shown_ms, shown_at(t)),
        source: thread_source(t).into(),
        snoozed_until_ms: snoozed_until(model, t).unwrap_or(0),
        relation: if relation(t) == "fork" { "fork".into() } else { String::new() },
    }
}

/// Rows for many threads, with their labels looked up once.
pub fn thread_rows<'a>(model: &'a Model, threads: impl IntoIterator<Item = &'a ThreadShell>) -> Vec<ThreadRowView> {
    let labels = Labels::new(model);
    threads.into_iter().map(|t| row(model, &labels, t)).collect()
}

// A subagent's thread is opened from its row in the parent thread.
pub(crate) fn is_live_thread(thread: &ThreadShell) -> bool {
    thread.deleted_at.is_none() && thread.archived_at.is_none() && relation(thread) != "subagent"
}

pub(crate) fn live_threads(model: &Model) -> impl Iterator<Item = &ThreadShell> {
    model.threads.iter().filter(|thread| is_live_thread(thread))
}

pub fn sidebar(model: &Model) -> SidebarView {
    let labels = Labels::new(model);
    let q = model.search.query.trim().to_lowercase();
    let all_live: Vec<&ThreadShell> = live_threads(model).collect();
    let wanted = |t: &ThreadShell| !model.unread_only || shell_status(t) == ShellStatus::Approval || is_unread(model, t);
    let matches = |t: &ThreadShell| (q.is_empty() || t.title.to_lowercase().contains(&q)) && wanted(t);
    let attention = all_live.iter().filter(|t| shell_status(t) == ShellStatus::Approval).count() as u32;
    let title_of = |id: &str| model.thread_shell(id).map(|t| t.title.clone()).unwrap_or_default();
    let mut view = SidebarView {
        grouping: model.sidebar_layout.grouping.as_str().into(),
        surface_threads: surface_threads(model, &labels, &all_live),
        unread_only: model.unread_only,
        selected_thread_id: model.selected_thread.clone(),
        selected_project_id: model.selected_project.clone(),
        search_query: model.search.query.clone(),
        search_results: model
            .search
            .results
            .iter()
            .map(|m| SearchHitView {
                thread_id: m.thread_id.as_str().to_owned(),
                title: title_of(m.thread_id.as_str()),
                project_title: labels.projects.get(m.project_id.as_str()).copied().unwrap_or_default().to_owned(),
                snippet: m.snippet.clone(),
                source: m.source.as_str().to_owned(),
            })
            .collect(),
        searching: model.search.searching,
        threads_needing_attention: attention,
        ..SidebarView::default()
    };
    match model.sidebar_layout.grouping {
        Grouping::Status => {
            view.sections = sections(model, &labels, &all_live, &q, &matches);
            // The projects still, without their threads: for pickers and headers.
            view.projects = model.projects.iter().map(|p| project_header(model, p)).collect();
            sort_projects(&mut view.projects, &all_live);
        }
        Grouping::Project => by_project(model, &labels, &all_live, &q, &matches, &mut view),
    }
    list_launching(model, &labels, &mut view);
    view
}

/// The thread being started, listed first until the server's shell of it is.
fn list_launching(model: &Model, labels: &Labels, view: &mut SidebarView) {
    let Some(l) = model.starting_thread.as_ref() else { return };
    if model.thread_shell(&l.id).is_some() || !model.search.query.trim().is_empty() {
        return;
    }
    let row = ThreadRowView {
        id: l.id.clone(),
        project_id: l.project_id.clone(),
        project_icon: labels.icons.get(l.project_id.as_str()).cloned().unwrap_or_default(),
        project_title: labels.projects.get(l.project_id.as_str()).copied().unwrap_or_default().to_owned(),
        title: l.title.clone(),
        status_label: "Starting".into(),
        status: "working".into(),
        age: "now".into(),
        ..ThreadRowView::default()
    };
    view.surface_threads.insert(0, row.clone());
    // Under Running, where it stays once it starts.
    if model.sidebar_layout.grouping == Grouping::Status && !model.unread_only {
        let running = Section::Running.as_str();
        let at = match view.sections.iter().position(|s| s.kind == running) {
            Some(at) => at,
            None => {
                let at = usize::from(view.sections.first().is_some_and(|s| s.kind == Section::Inbox.as_str()));
                let expanded = !model.sidebar_layout.collapsed_sections.contains(&Section::Running);
                view.sections.insert(at, SidebarSectionView { kind: running.into(), title: Section::Running.title().into(), expanded, ..SidebarSectionView::default() });
                at
            }
        };
        let section = &mut view.sections[at];
        if section.expanded {
            section.rows.insert(0, row.clone());
        }
        section.count += 1;
    }
    if let Some(project) = view.projects.iter_mut().find(|p| p.id == l.project_id) {
        project.threads.insert(0, row);
    }
}

/// The threads most worth a glance: waiting on you, working, unread, then
/// the latest.
fn surface_threads(model: &Model, labels: &Labels, live: &[&ThreadShell]) -> Vec<ThreadRowView> {
    let rank = |t: &ThreadShell| match shell_status(t) {
        ShellStatus::Approval => 0,
        ShellStatus::Working => 1,
        _ if is_unread(model, t) => 2,
        _ => 3,
    };
    let mut threads: Vec<(u8, &ThreadShell)> = live.iter().map(|t| (rank(t), *t)).collect();
    threads.sort_by(|(ra, a), (rb, b)| ra.cmp(rb).then(b.updated_at.as_str().cmp(a.updated_at.as_str())));
    threads.into_iter().take(SURFACE_THREADS).map(|(_, t)| row(model, labels, t)).collect()
}

fn sections(model: &Model, labels: &Labels, live: &[&ThreadShell], q: &str, matches: &dyn Fn(&ThreadShell) -> bool) -> Vec<SidebarSectionView> {
    let mut by: FxHashMap<Section, Vec<&ThreadShell>> = FxHashMap::default();
    for t in live.iter().copied().filter(|t| matches(t)) {
        by.entry(section_of(model, t)).or_default().push(t);
    }
    if !model.unread_only {
        let archived = model.archived.iter().filter(|t| t.deleted_at.is_none() && relation(t) != "subagent" && matches(t));
        by.entry(Section::Archived).or_default().extend(archived);
    }
    let searching = !q.is_empty();
    Section::ALL
        .into_iter()
        .filter_map(|section| {
            let mut threads = by.remove(&section).unwrap_or_default();
            // Only sections with threads (while filtering, with matches).
            if threads.is_empty() || (model.unread_only && section != Section::Inbox) {
                return None;
            }
            sort_section(section, &mut threads);
            let expanded = searching || !model.sidebar_layout.collapsed_sections.contains(&section);
            let limit = if searching || model.show_all_sections.contains(&section) { usize::MAX } else { ROWS_PER_SECTION };
            let rows: Vec<ThreadRowView> = if expanded { threads.iter().take(limit).map(|t| row(model, labels, t)).collect() } else { vec![] };
            Some(SidebarSectionView {
                kind: section.as_str().into(),
                title: section.title().into(),
                count: threads.len() as u32,
                expanded,
                hidden_count: if expanded { threads.len().saturating_sub(rows.len()) as u32 } else { 0 },
                rows,
            })
        })
        .collect()
}

fn sort_section(section: Section, threads: &mut [&ThreadShell]) {
    match section {
        Section::Inbox => {
            let rank = |t: &ThreadShell| if shell_status(t) == ShellStatus::Approval { 0 } else if is_pinned(t) { 1 } else { 2 };
            threads.sort_by(|a, b| rank(a).cmp(&rank(b)).then(b.updated_at.as_str().cmp(a.updated_at.as_str())));
        }
        Section::Running => threads.sort_by(|a, b| b.updated_at.as_str().cmp(a.updated_at.as_str())),
        Section::Inactive => threads.sort_by(|a, b| shown_at(b).cmp(shown_at(a))),
        Section::Archived => {
            fn at(t: &ThreadShell) -> &str {
                t.archived_at.as_ref().map_or("", Timestamp::as_str)
            }
            threads.sort_by(|a, b| at(b).cmp(at(a)));
        }
    }
}

/// A project as the list heads it, without its threads.
fn project_header(model: &Model, p: &ProjectShell) -> SidebarProjectView {
    let id = p.id.as_str().to_owned();
    SidebarProjectView {
        icon: crate::project_icons::view(model, p, false),
        automatic_icon: crate::project_icons::view(model, p, true),
        expanded: !model.sidebar_layout.collapsed_projects.contains(&id),
        is_scratch: p.workspace_root.ends_with("/scratch"),
        pinned: matches!(p.pinned_at, Some(Some(_))),
        title: p.title.clone(),
        workspace_root: p.workspace_root.clone(),
        id,
        ..SidebarProjectView::default()
    }
}

/// Pinned projects first, then the latest active, then by title.
fn sort_projects(projects: &mut [SidebarProjectView], live: &[&ThreadShell]) {
    // When each project was last active, read once for the sort.
    let mut latest: FxHashMap<&str, &str> = FxHashMap::default();
    for t in live {
        let at = latest.entry(t.project_id.as_str()).or_default();
        *at = t.updated_at.as_str().max(*at);
    }
    let latest = |p: &SidebarProjectView| latest.get(p.id.as_str()).copied().unwrap_or_default();
    projects.sort_by(|a, b| b.pinned.cmp(&a.pinned).then(latest(b).cmp(latest(a))).then(a.title.cmp(&b.title)));
}

/// Threads under their projects (the layout before status groups).
fn by_project(model: &Model, labels: &Labels, all_live: &[&ThreadShell], q: &str, matches: &dyn Fn(&ThreadShell) -> bool, view: &mut SidebarView) {
    let mut snoozed: Vec<&ThreadShell> = all_live.iter().copied().filter(|t| snoozed_until(model, t).is_some()).collect();
    snoozed.sort_by_key(|t| snoozed_until(model, t).unwrap_or(i64::MAX));
    let live: Vec<&ThreadShell> = all_live.iter().copied().filter(|t| snoozed_until(model, t).is_none()).collect();
    fn activity(t: &ThreadShell) -> &str {
        t.updated_at.as_str()
    }
    let mut projects: Vec<SidebarProjectView> = model
        .projects
        .iter()
        .map(|p| {
            let id = p.id.as_str();
            let mut threads: Vec<&ThreadShell> = live.iter().copied().filter(|t| t.project_id == p.id && !is_settled(t) && matches(t)).collect();
            threads.sort_by(|a, b| is_pinned(b).cmp(&is_pinned(a)).then(activity(b).cmp(activity(a))));
            let total = threads.len();
            let limit = if model.show_all_projects.contains(id) || !q.is_empty() { usize::MAX } else { THREADS_PER_PROJECT };
            let shown: Vec<ThreadRowView> = threads.iter().take(limit).map(|t| row(model, labels, t)).collect();
            let mut earlier: Vec<&ThreadShell> = live.iter().copied().filter(|t| t.project_id == p.id && is_settled(t) && matches(t)).collect();
            // By the time each row shows (its last message from you).
            earlier.sort_by_key(|t| std::cmp::Reverse(shown_at(t)));
            let earlier_count = earlier.len() as u32;
            let earlier_limit = if model.show_all_projects.contains(id) || !q.is_empty() { 200 } else { EARLIER_PER_PROJECT };
            let earlier: Vec<ThreadRowView> = earlier.iter().take(earlier_limit).map(|t| row(model, labels, t)).collect();
            SidebarProjectView { hidden_count: total.saturating_sub(shown.len()) as u32, threads: shown, earlier, earlier_count, ..project_header(model, p) }
        })
        .filter(|p| (q.is_empty() && !model.unread_only) || !p.threads.is_empty() || !p.earlier.is_empty() || (!q.is_empty() && p.title.to_lowercase().contains(q)))
        .collect();
    sort_projects(&mut projects, &live);
    let mut settled: Vec<&ThreadShell> = live.iter().copied().filter(|t| is_settled(t) && matches(t)).collect();
    settled.sort_by_key(|t| std::cmp::Reverse(activity(t)));
    view.projects = projects;
    view.settled = settled.into_iter().take(50).map(|t| row(model, labels, t)).collect();
    view.snoozed = if model.unread_only { vec![] } else { snoozed.into_iter().map(|t| row(model, labels, t)).collect() };
}

/// Resolve both current and legacy server icon records.
pub fn project_icon(p: &ProjectShell) -> ProjectIconView {
    use domovoi_model::typed::ProjectIcon;
    let (text, color) = crate::present::project_identity(&p.title);
    let mut view = ProjectIconView { kind: "monogram".into(), text, color: color.into(), ..Default::default() };
    match p.project_icon.as_ref().and_then(Option::as_ref) {
        Some(ProjectIcon::Monogram(icon)) => { view.text = icon.text.clone(); view.color = icon.color.as_str().into(); }
        Some(ProjectIcon::Lucide(icon)) => {
            view.color = icon.color.as_str().into();
            if let Some(text) = icon.monogram_text.as_ref().or(icon.monogram.as_ref()) { view.text = text.clone(); }
            else { view.kind = "symbol".into(); view.symbol = icon.name.clone(); view.text.clear(); }
        }
        Some(ProjectIcon::Emoji(icon)) => { view.kind = "emoji".into(); view.text = icon.emoji.clone(); }
        _ => return view,
    }
    view.custom = true;
    view
}
