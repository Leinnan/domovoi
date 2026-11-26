//! A scheduled task's editor: what to send, when (every so often, daily or
//! weekly at a time, once, or on a webhook), where (a new thread each run or
//! one thread, the project's folder or a worktree) and with which agent. As
//! the other clients have it.

use std::rc::Rc;

use domovoi_core::Event;
use domovoi_core::app::ScheduledTaskEdit;
use domovoi_core::view::{ComposerView, ScheduledTaskView, ScheduledThreadChoiceView, SettingsView, ThreadPageView};
use jiff::{Timestamp, Zoned, civil, tz::TimeZone};
use windows_reactor::{
    Border, Button, ButtonStyle, Callback, ComboBox, FontWeight, Grid, GridLength, HorizontalAlignment, Orientation, ScrollBarVisibility, ScrollViewer, StackPanel,
    TextBlock, TextBox, TextTrimming, TextWrapping, Thickness, ToggleSwitch, VerticalAlignment, View,
};

use crate::theme;

const KINDS: [(&str, &str); 4] = [("interval", "Every so often"), ("fixed_time", "At a time of day"), ("once", "Once"), ("webhook", "On a webhook")];
const THREAD_MODES: [(&str, &str); 3] = [("new_thread", "A new thread each run"), ("continue", "The same thread"), ("fresh_session", "The same thread, a fresh session")];
const PERMISSIONS: [(&str, &str); 3] = [("approval-required", "Ask first"), ("auto-accept-edits", "Accept edits"), ("full-access", "Full access")];
const FOLDERS: [(&str, &str); 2] = [("root", "The project's folder"), ("worktree", "A new worktree")];
const INTERVALS: [(u32, &str); 9] = [(5, "5 minutes"), (15, "15 minutes"), (30, "30 minutes"), (60, "Hour"), (180, "3 hours"), (360, "6 hours"), (720, "12 hours"), (1440, "Day"), (10080, "Week")];
const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const ASKS: &str = "Runs stop at each tool the agent asks about until someone answers.";

/// The task as edited, and what the form needs beside it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TaskDraft {
    pub edit: ScheduledTaskEdit,
    /// The thread and project the task had when the editor opened: a task
    /// already bound in this project must keep a thread (the server keeps the
    /// one it has when none is sent).
    opened_thread_id: String,
    opened_project_id: String,
    /// The bound thread's permissions as changed here (the thread's own, changed on saving).
    thread_runtime_mode: Option<String>,
    /// For a task that runs once: the local date and time as typed, `YYYY-MM-DD HH:MM`.
    run_at_input: String,
    /// The threads to choose from are listed, narrowed by this search.
    picking_thread: bool,
    thread_query: String,
}

/// A change made in the form.
#[derive(Clone, Debug)]
pub enum TaskEdit {
    Name(Rc<str>),
    Prompt(Rc<str>),
    Kind(String),
    Interval(u32),
    Time(Rc<str>),
    /// A weekday (0 is Sunday) ticked or unticked.
    Day(u8),
    RunAt(Rc<str>),
    Project(String),
    ThreadMode(String),
    /// The threads listed, or the list put away.
    PickThread,
    ThreadQuery(Rc<str>),
    /// A thread chosen; empty: the first run starts one.
    Thread(String),
    Folder(String),
    BaseBranch(Rc<str>),
    /// `instance|model`; empty: the composer's.
    Model(String),
    Permissions(String),
    Plan,
    Enabled,
}

fn parse_hh_mm(text: &str) -> Option<(u8, u8)> {
    let (hours, minutes) = text.trim().split_once(':')?;
    let (hours, minutes) = (hours.parse::<u8>().ok()?, minutes.parse::<u8>().ok()?);
    (hours < 24 && minutes < 60).then_some((hours, minutes))
}

/// `2026-11-24 09:00` (local) from the server's RFC 3339 time.
fn local_input(rfc3339: &str) -> Option<String> {
    let at = rfc3339.parse::<Timestamp>().ok()?;
    Some(at.to_zoned(TimeZone::system()).strftime("%Y-%m-%d %H:%M").to_string())
}

/// RFC 3339 with this device's offset from `YYYY-MM-DD HH:MM` (local).
fn rfc3339_from_local(input: &str) -> Option<String> {
    let at = civil::DateTime::strptime("%Y-%m-%d %H:%M", input.trim()).ok()?;
    Some(at.to_zoned(TimeZone::system()).ok()?.strftime("%Y-%m-%dT%H:%M:%S%:z").to_string())
}

impl TaskDraft {
    fn wrap(edit: ScheduledTaskEdit) -> Self {
        // A task that has no time yet runs an hour from now, on the hour.
        let run_at_input = local_input(&edit.run_at).unwrap_or_else(|| {
            let now = Zoned::now().with_time_zone(TimeZone::system());
            now.checked_add(jiff::Span::new().hours(1)).unwrap_or(now).strftime("%Y-%m-%d %H:00").to_string()
        });
        Self { opened_thread_id: edit.thread_id.clone(), opened_project_id: edit.project_id.clone(), run_at_input, edit, ..Self::default() }
    }

    /// A new task in `project_id`, as the other clients start one.
    pub fn new_task(project_id: String) -> Self {
        Self::wrap(ScheduledTaskEdit {
            project_id,
            schedule_kind: "interval".into(),
            interval_minutes: 60,
            time_of_day: "09:00".into(),
            thread_mode: "new_thread".into(),
            workspace: "root".into(),
            runtime_mode: "full-access".into(),
            interaction_mode: "default".into(),
            enabled: true,
            ..ScheduledTaskEdit::default()
        })
    }

    /// A new task that goes on in `thread`, with its model and modes (the thread's "Schedule…").
    pub fn for_thread(thread: &ThreadPageView) -> Self {
        let mut draft = Self::new_task(thread.project_id.clone());
        draft.edit.thread_mode = "continue".into();
        draft.edit.thread_id = thread.id.clone();
        draft.edit.name = thread.title.clone();
        if !thread.model.is_empty() {
            draft.edit.model_instance = thread.model_instance_id.clone();
            draft.edit.model = thread.model.clone();
        }
        if !thread.runtime_mode.is_empty() {
            draft.edit.runtime_mode = thread.runtime_mode.clone();
        }
        draft.edit.interaction_mode = if thread.interaction_mode == "plan" { "plan" } else { "default" }.into();
        // Not bound yet: the thread may still be left to the first run.
        draft.opened_thread_id.clear();
        draft
    }

    pub fn from_task(task: &ScheduledTaskView) -> Self {
        Self::wrap(ScheduledTaskEdit {
            id: task.id.clone(),
            name: task.name.clone(),
            prompt: task.prompt.clone(),
            project_id: task.project_id.clone(),
            schedule_kind: if task.schedule_kind.is_empty() { "interval".into() } else { task.schedule_kind.clone() },
            interval_minutes: task.interval_minutes,
            time_of_day: if task.time_of_day.is_empty() { "09:00".into() } else { task.time_of_day.clone() },
            weekdays: task.weekdays.clone(),
            run_at: task.run_at.clone(),
            thread_mode: task.thread_mode.clone(),
            thread_id: task.thread_id.clone(),
            workspace: task.workspace.clone(),
            base_branch: task.base_branch.clone(),
            model_instance: task.model_instance.clone(),
            model: task.model.clone(),
            runtime_mode: task.runtime_mode.clone(),
            interaction_mode: task.interaction_mode.clone(),
            enabled: task.enabled,
        })
    }

    pub fn is_new(&self) -> bool {
        self.edit.id.is_empty()
    }

    /// A one-thread task may leave its thread to the first run, unless it is already bound in this project.
    fn first_run_starts_thread(&self) -> bool {
        self.opened_thread_id.is_empty() || self.edit.project_id != self.opened_project_id
    }

    /// Its runs go into a thread chosen here, and so use that thread's model, permissions and folder.
    fn bound(&self) -> bool {
        self.edit.thread_mode != "new_thread" && !self.edit.thread_id.is_empty()
    }

    /// Why the task cannot be saved yet; `None` when it can.
    fn incomplete(&self) -> Option<&'static str> {
        let edit = &self.edit;
        if edit.name.trim().is_empty() {
            Some("Name the task.")
        } else if edit.prompt.trim().is_empty() {
            Some("Write the prompt it sends.")
        } else if edit.project_id.is_empty() {
            Some("Choose a project.")
        } else if edit.thread_mode != "new_thread" && edit.thread_id.is_empty() && !self.first_run_starts_thread() {
            Some("Choose the thread its runs go into.")
        } else if edit.schedule_kind == "fixed_time" && parse_hh_mm(&edit.time_of_day).is_none() {
            Some("Enter the time as HH:MM.")
        } else if edit.schedule_kind == "once" && rfc3339_from_local(&self.run_at_input).is_none() {
            Some("Enter the date and time as YYYY-MM-DD HH:MM.")
        } else {
            None
        }
    }

    pub fn can_save(&self) -> bool {
        self.incomplete().is_none()
    }

    pub fn apply(&mut self, change: TaskEdit) {
        let bound = self.bound();
        let edit = &mut self.edit;
        match change {
            TaskEdit::Name(text) => edit.name = text.to_string(),
            TaskEdit::Prompt(text) => edit.prompt = text.to_string(),
            TaskEdit::Kind(kind) => edit.schedule_kind = kind,
            TaskEdit::Interval(minutes) => edit.interval_minutes = minutes,
            TaskEdit::Time(text) => edit.time_of_day = text.to_string(),
            TaskEdit::Day(day) => {
                if let Some(at) = edit.weekdays.iter().position(|d| *d == day) {
                    edit.weekdays.remove(at);
                } else {
                    edit.weekdays.push(day);
                    edit.weekdays.sort_unstable();
                }
            }
            TaskEdit::RunAt(text) => self.run_at_input = text.to_string(),
            TaskEdit::Project(project_id) => {
                if edit.project_id != project_id {
                    // A thread belongs to its project.
                    edit.thread_id.clear();
                    self.thread_runtime_mode = None;
                }
                edit.project_id = project_id;
            }
            TaskEdit::ThreadMode(mode) => edit.thread_mode = mode,
            TaskEdit::PickThread => self.picking_thread = !self.picking_thread,
            TaskEdit::ThreadQuery(text) => self.thread_query = text.to_string(),
            TaskEdit::Thread(thread_id) => {
                edit.thread_id = thread_id;
                self.thread_runtime_mode = None;
                self.picking_thread = false;
            }
            TaskEdit::Folder(folder) => edit.workspace = folder,
            TaskEdit::BaseBranch(text) => edit.base_branch = text.to_string(),
            TaskEdit::Model(key) => {
                let (instance, model) = key.split_once('|').unwrap_or_default();
                edit.model_instance = instance.to_owned();
                edit.model = model.to_owned();
            }
            // A bound task's permissions are its thread's.
            TaskEdit::Permissions(mode) if bound => self.thread_runtime_mode = Some(mode),
            TaskEdit::Permissions(mode) => edit.runtime_mode = mode,
            TaskEdit::Plan => edit.interaction_mode = if edit.interaction_mode == "plan" { "default" } else { "plan" }.into(),
            TaskEdit::Enabled => edit.enabled = !edit.enabled,
        }
    }

    /// What saving tells the core: the task, with its time as the server takes
    /// it, and before it the bound thread's permissions when they were changed.
    pub fn saved(mut self) -> Vec<Event> {
        if let Some((hours, minutes)) = parse_hh_mm(&self.edit.time_of_day) {
            self.edit.time_of_day = format!("{hours:02}:{minutes:02}");
        }
        self.edit.run_at = if self.edit.schedule_kind == "once" { rfc3339_from_local(&self.run_at_input).unwrap_or_default() } else { String::new() };
        let mut events = Vec::new();
        if let Some(mode) = self.thread_runtime_mode.take().filter(|_| self.bound()) {
            events.push(Event::SetThreadRuntimeMode { thread_id: self.edit.thread_id.clone(), mode });
        }
        events.push(Event::SaveScheduled { task: Box::new(self.edit) });
        events
    }
}

fn caption(text: &str) -> TextBlock {
    TextBlock::new().text(text).font_size(theme::CAPTION).opacity(0.65).text_wrapping(TextWrapping::WrapWholeWords)
}

fn section(title: &str) -> View {
    TextBlock::new().text(title).font_weight(FontWeight::SEMI_BOLD).margin(Thickness::new(0.0, 8.0, 0.0, 0.0)).into()
}

fn labelled(label: &str, control: View) -> View {
    Grid::new()
        .columns([GridLength::Pixel(110.0), GridLength::STAR])
        .column_spacing(8.0)
        .children((TextBlock::new().text(label).opacity(0.75).vertical_alignment(VerticalAlignment::Center), Border::new().grid_column(1).content(control)))
        .into()
}

/// The form. `settings` has the projects and the threads to choose from, `composer` the models.
pub fn form(draft: &TaskDraft, settings: &SettingsView, composer: &ComposerView, on_edit: &Callback<TaskEdit>) -> View {
    let edit = &draft.edit;
    let typed = |name: &str, hint: &str, value: &str, change: fn(Rc<str>) -> TaskEdit| -> TextBox {
        let on_edit = on_edit.clone();
        TextBox::new().automation_name(name).placeholder_text(hint).text(value).on_text_changed(move |text: Rc<str>| on_edit.call(change(text)))
    };
    // A list to choose one of: what each choice is called, and the change it makes.
    let pick = |name: &str, options: Vec<(String, TaskEdit)>, chosen: Option<usize>| -> View {
        let (on_edit, changes) = (on_edit.clone(), options.iter().map(|(_, change)| change.clone()).collect::<Vec<_>>());
        ComboBox::new()
            .automation_name(name)
            .horizontal_alignment(HorizontalAlignment::Stretch)
            .items_source(options.into_iter().map(|(label, _)| label))
            .selected_index(chosen)
            .on_selection_changed(move |index: Option<usize>| {
                // Set from the draft too: only a choice that differs is a change.
                if let Some(change) = index.filter(|i| Some(*i) != chosen).and_then(|i| changes.get(i)) {
                    on_edit.call(change.clone());
                }
            })
            .into()
    };
    let named = |options: &[(&str, &str)], current: &str, change: fn(String) -> TaskEdit| -> (Vec<(String, TaskEdit)>, Option<usize>) {
        (options.iter().map(|(value, label)| ((*label).to_owned(), change((*value).to_owned()))).collect(), options.iter().position(|(value, _)| *value == current))
    };
    let switch = |name: &str, on: bool, change: TaskEdit| -> View {
        let on_edit = on_edit.clone();
        ToggleSwitch::new()
            .min_width(0.0)
            .is_on(on)
            .on_content(TextBlock::new())
            .off_content(TextBlock::new())
            .automation_name(name)
            .on_toggled(move |to: bool| {
                if to != on {
                    on_edit.call(change.clone());
                }
            })
            .into()
    };

    let mut rows: Vec<View> = vec![
        typed("Task name", "Name", &edit.name, TaskEdit::Name).into(),
        typed("Prompt", "The prompt each run sends", &edit.prompt, TaskEdit::Prompt).accepts_return(true).text_wrapping(TextWrapping::Wrap).min_height(64.0).max_height(140.0).into(),
    ];
    if edit.schedule_kind == "webhook" {
        rows.push(caption("The prompt can use {{body.path}}, {{headers.name}} and {{query.name}} from each request.").into());
    }

    rows.push(section("When"));
    let (kinds, kind) = named(&KINDS, &edit.schedule_kind, TaskEdit::Kind);
    rows.push(labelled("Runs", pick("When it runs", kinds, kind)));
    match edit.schedule_kind.as_str() {
        "interval" => {
            let mut every: Vec<(String, TaskEdit)> = INTERVALS.iter().map(|(minutes, label)| ((*label).to_owned(), TaskEdit::Interval(*minutes))).collect();
            let mut chosen = INTERVALS.iter().position(|(minutes, _)| *minutes == edit.interval_minutes);
            // An interval set elsewhere that is not one of these.
            if chosen.is_none() {
                every.push((format!("{} minutes", edit.interval_minutes), TaskEdit::Interval(edit.interval_minutes)));
                chosen = Some(every.len() - 1);
            }
            rows.push(labelled("Every", pick("Interval", every, chosen)));
        }
        "fixed_time" => {
            rows.push(labelled("Time", typed("Time of day", "09:00", &edit.time_of_day, TaskEdit::Time).width(110.0).horizontal_alignment(HorizontalAlignment::Left).into()));
            let days = WEEKDAYS.iter().enumerate().map(|(day, name)| {
                let on_edit = on_edit.clone();
                Button::new()
                    .style(if edit.weekdays.contains(&(day as u8)) { ButtonStyle::Accent } else { ButtonStyle::Default })
                    .content(TextBlock::new().text(*name).font_size(theme::CAPTION))
                    .on_click(move || on_edit.call(TaskEdit::Day(day as u8)))
                    .into()
            });
            rows.push(labelled("Days", StackPanel::new().orientation(Orientation::Horizontal).spacing(2.0).children(days.collect::<Vec<View>>()).into()));
            rows.push(caption(if edit.weekdays.is_empty() { "Every day." } else { "On the days chosen." }).into());
        }
        "once" => rows.push(labelled("At", typed("Date and time", "YYYY-MM-DD HH:MM", &draft.run_at_input, TaskEdit::RunAt).width(200.0).horizontal_alignment(HorizontalAlignment::Left).into())),
        _ => rows.push(caption("Runs on each request to the task's address, which is shown once the task is saved.").into()),
    }
    if !settings.scheduled_time_zone.is_empty() {
        rows.push(caption(&format!("Times are in {}.", settings.scheduled_time_zone)).into());
    }

    rows.push(section("Where"));
    let projects: Vec<(String, TaskEdit)> = settings.projects.iter().map(|p| (p.title.clone(), TaskEdit::Project(p.id.clone()))).collect();
    rows.push(labelled("Project", pick("Project", projects, settings.projects.iter().position(|p| p.id == edit.project_id))));
    let (modes, mode) = named(&THREAD_MODES, &edit.thread_mode, TaskEdit::ThreadMode);
    rows.push(labelled("Thread", pick("Thread", modes, mode)));
    let threads: Vec<&ScheduledThreadChoiceView> = settings.scheduled_thread_choices.iter().filter(|t| t.project_id == edit.project_id).collect();
    let chosen = settings.scheduled_thread_choices.iter().find(|t| t.id == edit.thread_id);
    if edit.thread_mode != "new_thread" {
        let which = match (edit.thread_id.is_empty(), chosen) {
            (true, _) if draft.first_run_starts_thread() => "Started by the first run".to_owned(),
            (true, _) => "None chosen".to_owned(),
            (false, Some(thread)) => thread.title.clone(),
            (false, None) => "A thread not on this device".to_owned(),
        };
        let on_pick = on_edit.clone();
        rows.push(labelled(
            "Which",
            Grid::new()
                .columns([GridLength::STAR, GridLength::Auto])
                .column_spacing(8.0)
                .children((
                    TextBlock::new().text(which).text_trimming(TextTrimming::CharacterEllipsis).vertical_alignment(VerticalAlignment::Center),
                    Button::new().grid_column(1).automation_name("Choose the thread").content(TextBlock::new().text(if draft.picking_thread { "Done" } else { "Choose…" })).on_click(move || on_pick.call(TaskEdit::PickThread)),
                ))
                .into(),
        ));
        if draft.picking_thread {
            rows.push(thread_list(draft, &threads, on_edit));
        }
    }
    if !draft.bound() {
        let (folders, folder) = named(&FOLDERS, &edit.workspace, TaskEdit::Folder);
        rows.push(labelled("Folder", pick("Folder", folders, folder)));
        if edit.workspace == "worktree" {
            rows.push(labelled("Base branch", typed("Base branch", "The default branch", &edit.base_branch, TaskEdit::BaseBranch).into()));
        }
    }
    let scratch = settings.projects.iter().find(|p| p.id == edit.project_id).is_some_and(|p| p.is_scratch);
    rows.push(
        caption(match edit.thread_mode.as_str() {
            "continue" => "Each run is a new message in one thread, which remembers the earlier runs.",
            "fresh_session" => "Each run is a new message in one thread, answered by an agent that starts without the earlier conversation.",
            _ if scratch => "Each run starts its own thread, listed under No project.",
            _ => "Each run starts its own thread.",
        })
        .into(),
    );

    rows.push(section("Agent"));
    let permissions = if draft.bound() {
        let threads = chosen.map(|t| t.runtime_mode.as_str()).filter(|mode| !mode.is_empty()).unwrap_or("approval-required");
        draft.thread_runtime_mode.clone().unwrap_or_else(|| threads.to_owned())
    } else {
        edit.runtime_mode.clone()
    };
    let (allowed, allowance) = named(&PERMISSIONS, &permissions, TaskEdit::Permissions);
    if draft.bound() {
        let model = chosen.map(|t| t.model_label.as_str()).filter(|label| !label.is_empty()).unwrap_or("The thread's");
        rows.push(labelled("Model", TextBlock::new().text(model).into()));
        rows.push(labelled("Permissions", pick("Permissions", allowed, allowance)));
    } else {
        let key = if edit.model.is_empty() { String::new() } else { format!("{}|{}", edit.model_instance, edit.model) };
        let mut models: Vec<(String, String)> = vec![("The composer's model".to_owned(), String::new())];
        for model in &composer.favorite_models {
            let label = if model.detail.is_empty() { model.option.label.clone() } else { format!("{} · {}", model.detail, model.name) };
            models.push((format!("★ {label}"), format!("{}|{}", model.option.instance_id, model.option.model)));
        }
        for group in &composer.model_groups {
            models.extend(group.models.iter().map(|model| (format!("{} · {}", group.provider, model.name), format!("{}|{}", model.option.instance_id, model.option.model))));
        }
        // A model set elsewhere that this device does not list.
        if !key.is_empty() && !models.iter().any(|(_, each)| *each == key) {
            models.push((edit.model.clone(), key.clone()));
        }
        let at = models.iter().position(|(_, each)| *each == key);
        rows.push(labelled("Model", pick("Model", models.into_iter().map(|(label, key)| (label, TaskEdit::Model(key))).collect(), at)));
        rows.push(labelled("Permissions", pick("Permissions", allowed, allowance)));
        rows.push(labelled("Plan first", switch("Plan first", edit.interaction_mode == "plan", TaskEdit::Plan)));
    }
    rows.push(labelled("Enabled", switch("Enabled", edit.enabled, TaskEdit::Enabled)));
    if permissions == "approval-required" {
        let note = if draft.bound() { format!("{ASKS} The permissions are the thread's own: a change here changes the thread.") } else { ASKS.to_owned() };
        rows.push(TextBlock::new().text(note).font_size(theme::CAPTION).foreground(theme::critical()).text_wrapping(TextWrapping::WrapWholeWords).into());
    } else if draft.bound() {
        rows.push(caption("Runs use the thread's own model, permissions and folder: a change of permissions here changes the thread.").into());
    }
    if let Some(why) = draft.incomplete() {
        rows.push(TextBlock::new().text(why).font_weight(FontWeight::SEMI_BOLD).margin(Thickness::new(0.0, 6.0, 0.0, 0.0)).into());
    }
    ScrollViewer::new()
        .max_height(520.0)
        .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
        .horizontal_scroll_bar_visibility(ScrollBarVisibility::Disabled)
        .content(StackPanel::new().orientation(Orientation::Vertical).spacing(8.0).min_width(460.0).margin(Thickness::new(0.0, 0.0, 14.0, 0.0)).children(rows))
        .into()
}

/// The project's threads, narrowed by a search; for a task not bound yet, the first run may start one instead.
fn thread_list(draft: &TaskDraft, threads: &[&ScheduledThreadChoiceView], on_edit: &Callback<TaskEdit>) -> View {
    let query = draft.thread_query.to_lowercase();
    let choose = |title: &str, detail: &str, thread_id: &str| -> View {
        let (on_edit, id) = (on_edit.clone(), thread_id.to_owned());
        let mut lines: Vec<View> = vec![TextBlock::new().text(title).text_trimming(TextTrimming::CharacterEllipsis).into()];
        if !detail.is_empty() {
            lines.push(caption(detail).into());
        }
        let button = Button::new()
            .style(ButtonStyle::Subtle)
            .horizontal_alignment(HorizontalAlignment::Stretch)
            .horizontal_content_alignment(HorizontalAlignment::Left)
            .content(StackPanel::new().orientation(Orientation::Vertical).children(lines))
            .on_click(move || on_edit.call(TaskEdit::Thread(id.clone())));
        if thread_id == draft.edit.thread_id { Border::new().corner_radius(4.0).background(theme::selected()).content(button).into() } else { button.into() }
    };
    let mut list: Vec<View> = Vec::new();
    if draft.first_run_starts_thread() {
        list.push(choose("Started by the first run", "", ""));
    }
    let found: Vec<View> = threads.iter().filter(|t| query.is_empty() || t.title.to_lowercase().contains(&query)).take(50).map(|t| choose(&t.title, &t.detail, &t.id)).collect();
    if found.is_empty() {
        list.push(caption(if threads.is_empty() { "This project has no threads yet." } else { "No threads match." }).margin(Thickness::uniform(8.0)).into());
    }
    list.extend(found);
    let on_query = on_edit.clone();
    Border::new()
        .padding(Thickness::uniform(8.0))
        .corner_radius(6.0)
        .background(theme::code())
        .content(StackPanel::new().orientation(Orientation::Vertical).spacing(6.0).children(vec![
            TextBox::new().automation_name("Search the threads").placeholder_text("Search threads").text(&draft.thread_query).on_text_changed(move |text: Rc<str>| on_query.call(TaskEdit::ThreadQuery(text))).into(),
            ScrollViewer::new()
                .height(180.0)
                .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
                .horizontal_scroll_bar_visibility(ScrollBarVisibility::Disabled)
                .content(StackPanel::new().orientation(Orientation::Vertical).children(list))
                .into(),
        ]))
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn saved_task(draft: TaskDraft) -> (ScheduledTaskEdit, Option<(String, String)>) {
        let mut task = None;
        let mut change = None;
        for event in draft.saved() {
            match event {
                Event::SaveScheduled { task: saved } => task = Some(*saved),
                Event::SetThreadRuntimeMode { thread_id, mode } => change = Some((thread_id, mode)),
                _ => {}
            }
        }
        (task.unwrap_or_default(), change)
    }

    #[test]
    fn a_task_is_saved_once_it_is_whole_with_its_time_as_the_server_takes_it() {
        let mut draft = TaskDraft::new_task("p1".into());
        assert_eq!(draft.incomplete(), Some("Name the task."));
        draft.apply(TaskEdit::Name("Nightly".into()));
        draft.apply(TaskEdit::Prompt("Run the tests".into()));
        assert!(draft.can_save());
        draft.apply(TaskEdit::Kind("fixed_time".into()));
        draft.apply(TaskEdit::Time("25:00".into()));
        assert_eq!(draft.incomplete(), Some("Enter the time as HH:MM."));
        draft.apply(TaskEdit::Time("7:5".into()));
        let (task, change) = saved_task(draft.clone());
        assert_eq!((task.time_of_day.as_str(), task.run_at.as_str(), change), ("07:05", "", None));
        draft.apply(TaskEdit::Kind("once".into()));
        draft.apply(TaskEdit::RunAt("tomorrow".into()));
        assert!(!draft.can_save());
        draft.apply(TaskEdit::RunAt("2026-12-01 09:30".into()));
        let (task, _) = saved_task(draft);
        assert!(task.run_at.starts_with("2026-12-01T09:30:00"), "{}", task.run_at);
        assert!(task.run_at.parse::<Timestamp>().is_ok());
    }

    #[test]
    fn a_bound_task_keeps_a_thread_and_its_permissions_are_the_threads() {
        let mut draft = TaskDraft::wrap(ScheduledTaskEdit {
            id: "t1".into(),
            name: "n".into(),
            prompt: "p".into(),
            project_id: "p1".into(),
            schedule_kind: "interval".into(),
            interval_minutes: 60,
            thread_mode: "continue".into(),
            thread_id: "th1".into(),
            runtime_mode: "full-access".into(),
            ..ScheduledTaskEdit::default()
        });
        draft.apply(TaskEdit::Permissions("approval-required".into()));
        let (task, change) = saved_task(draft.clone());
        assert_eq!(task.runtime_mode, "full-access", "the task's own are left as they were");
        assert_eq!(change, Some(("th1".into(), "approval-required".into())));
        draft.apply(TaskEdit::Thread(String::new()));
        assert_eq!(draft.incomplete(), Some("Choose the thread its runs go into."));
        // In another project the first run may start the thread.
        draft.apply(TaskEdit::Project("p2".into()));
        assert!(draft.can_save());
        assert_eq!(saved_task(draft).1, None);
    }

    #[test]
    fn days_are_ticked_in_order_and_a_model_is_its_instance_and_name() {
        let mut draft = TaskDraft::new_task("p1".into());
        for day in [3, 1, 3, 5] {
            draft.apply(TaskEdit::Day(day));
        }
        assert_eq!(draft.edit.weekdays, [1, 5]);
        draft.apply(TaskEdit::Model("claudeAgent|claude-fable-5-2".into()));
        assert_eq!((draft.edit.model_instance.as_str(), draft.edit.model.as_str()), ("claudeAgent", "claude-fable-5-2"));
        draft.apply(TaskEdit::Model(String::new()));
        assert!(draft.edit.model.is_empty() && draft.edit.model_instance.is_empty());
        draft.apply(TaskEdit::Plan);
        assert_eq!(draft.edit.interaction_mode, "plan");
    }
}
