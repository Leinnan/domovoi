//! The scheduled task editor: what to send, when (every N minutes, daily or
//! weekly at a time, once, or on a webhook), where (a new thread each run or
//! one thread, the project folder or a worktree) and with which agent.
//! Mirrors Apple's `ScheduledTaskEditor`.

use domovoi_core::app::ScheduledTaskEdit;
use domovoi_core::view::{ScheduledTaskView, ScheduledThreadChoiceView, ThreadPageView};
use iced::widget::{Column, checkbox, column, container, row, text};
use iced::{Alignment, Element, Length};
use jiff::{Timestamp, Zoned, civil, tz::TimeZone};

use crate::app::{App, Message, Overlay, SettingsSection};
use crate::ui::fonts;
use crate::ui::icons::Icon;
use crate::ui::tokens::*;
use crate::ui::widgets as w;

/// The editor's state: the task as edited plus what the form needs on the side.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ScheduledDraft {
    pub edit: ScheduledTaskEdit,
    /// The thread and project the task had when the editor opened: a task
    /// already bound in this project must pick a thread (the server keeps
    /// the one it has when none is sent).
    opened_thread_id: String,
    opened_project_id: String,
    /// The bound thread's permissions as changed here (`SetThreadRuntimeMode` on save).
    pub thread_runtime_mode: Option<String>,
    /// `once`: local date and time as typed, `YYYY-MM-DD HH:MM`.
    run_at_input: String,
    /// The thread list is open, filtered by this query.
    picking_thread: bool,
    thread_query: String,
}

pub const THREAD_MODES: [(&str, &str); 3] = [("new_thread", "New thread each run"), ("continue", "Same thread"), ("fresh_session", "Same thread, fresh session")];
const PERMISSIONS: [(&str, &str); 3] = [("approval-required", "Ask first"), ("auto-accept-edits", "Accept edits"), ("full-access", "Full access")];
const INTERVALS: [(u32, &str); 9] = [(5, "5 minutes"), (15, "15 minutes"), (30, "30 minutes"), (60, "hour"), (180, "3 hours"), (360, "6 hours"), (720, "12 hours"), (1440, "day"), (10080, "week")];
const WEEKDAYS: [&str; 7] = ["S", "M", "T", "W", "T", "F", "S"];
const ASKS_WARNING: &str = "Runs stop at each tool the agent asks about until someone answers.";

impl ScheduledDraft {
    fn wrap(edit: ScheduledTaskEdit) -> Self {
        let run_at_input = local_input(&edit.run_at).unwrap_or_else(|| {
            let soon = Zoned::now().with_time_zone(TimeZone::system()).checked_add(jiff::Span::new().hours(1)).unwrap_or_else(|_| Zoned::now());
            soon.strftime("%Y-%m-%d %H:00").to_string()
        });
        Self { opened_thread_id: edit.thread_id.clone(), opened_project_id: edit.project_id.clone(), run_at_input, edit, ..Self::default() }
    }

    /// A new task in `project_id`, with Apple's defaults.
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

    /// A new task that continues `thread` with its model and modes (the thread's "Schedule…").
    pub fn for_thread(thread: &ThreadPageView) -> Self {
        let mut d = Self::new_task(thread.project_id.clone());
        d.edit.thread_mode = "continue".into();
        d.edit.thread_id = thread.id.clone();
        d.edit.name = thread.title.clone();
        if !thread.model.is_empty() {
            d.edit.model_instance = thread.model_instance_id.clone();
            d.edit.model = thread.model.clone();
        }
        if !thread.runtime_mode.is_empty() {
            d.edit.runtime_mode = thread.runtime_mode.clone();
        }
        d.edit.interaction_mode = if thread.interaction_mode == "plan" { "plan" } else { "default" }.into();
        // Not bound yet: the editor may leave the thread to the first run.
        d.opened_thread_id.clear();
        d
    }

    pub fn from_task(t: &ScheduledTaskView) -> Self {
        Self::wrap(ScheduledTaskEdit {
            id: t.id.clone(),
            name: t.name.clone(),
            prompt: t.prompt.clone(),
            project_id: t.project_id.clone(),
            schedule_kind: if t.schedule_kind.is_empty() { "interval".into() } else { t.schedule_kind.clone() },
            interval_minutes: t.interval_minutes,
            time_of_day: if t.time_of_day.is_empty() { "09:00".into() } else { t.time_of_day.clone() },
            weekdays: t.weekdays.clone(),
            run_at: t.run_at.clone(),
            thread_mode: t.thread_mode.clone(),
            thread_id: t.thread_id.clone(),
            workspace: t.workspace.clone(),
            base_branch: t.base_branch.clone(),
            model_instance: t.model_instance.clone(),
            model: t.model.clone(),
            runtime_mode: t.runtime_mode.clone(),
            interaction_mode: t.interaction_mode.clone(),
            enabled: t.enabled,
        })
    }

    /// A one-thread task may leave its thread to the first run, unless it is
    /// already bound in this project.
    fn first_run_starts_thread(&self) -> bool {
        self.opened_thread_id.is_empty() || self.edit.project_id != self.opened_project_id
    }

    fn bound_to_chosen_thread(&self) -> bool {
        self.edit.thread_mode != "new_thread" && !self.edit.thread_id.is_empty()
    }

    fn time_valid(&self) -> bool {
        parse_hh_mm(&self.edit.time_of_day).is_some()
    }

    /// Why the task can't be saved yet; `None` when it can.
    fn incomplete(&self) -> Option<&'static str> {
        let e = &self.edit;
        if e.name.trim().is_empty() {
            Some("Name the task.")
        } else if e.prompt.trim().is_empty() {
            Some("Write the prompt it sends.")
        } else if e.project_id.is_empty() {
            Some("Choose a project.")
        } else if e.thread_mode != "new_thread" && e.thread_id.is_empty() && !self.first_run_starts_thread() {
            Some("Choose the thread its runs go into.")
        } else if e.schedule_kind == "fixed_time" && !self.time_valid() {
            Some("Enter the time as HH:MM.")
        } else if e.schedule_kind == "once" && rfc3339_from_local(&self.run_at_input).is_none() {
            Some("Enter the date and time as YYYY-MM-DD HH:MM.")
        } else {
            None
        }
    }

    /// The task to save: `run_at` from the typed local time, `HH:MM` normalized.
    pub fn finished(mut self) -> (ScheduledTaskEdit, Option<(String, String)>) {
        if let Some((h, m)) = parse_hh_mm(&self.edit.time_of_day) {
            self.edit.time_of_day = format!("{h:02}:{m:02}");
        }
        self.edit.run_at = if self.edit.schedule_kind == "once" { rfc3339_from_local(&self.run_at_input).unwrap_or_default() } else { String::new() };
        let mode_change = if self.bound_to_chosen_thread() { self.thread_runtime_mode.clone().map(|m| (self.edit.thread_id.clone(), m)) } else { None };
        (self.edit, mode_change)
    }

    pub fn can_save(&self) -> bool {
        self.incomplete().is_none()
    }
}

fn parse_hh_mm(s: &str) -> Option<(u8, u8)> {
    let (h, m) = s.trim().split_once(':')?;
    let (h, m) = (h.parse::<u8>().ok()?, m.parse::<u8>().ok()?);
    (h < 24 && m < 60).then_some((h, m))
}

/// `2026-11-24 09:00` (local) from the server's RFC 3339 time.
fn local_input(rfc3339: &str) -> Option<String> {
    let ts = rfc3339.parse::<Timestamp>().ok()?;
    Some(ts.to_zoned(TimeZone::system()).strftime("%Y-%m-%d %H:%M").to_string())
}

/// RFC 3339 with this device's offset from `YYYY-MM-DD HH:MM` (local).
fn rfc3339_from_local(input: &str) -> Option<String> {
    let dt = civil::DateTime::strptime("%Y-%m-%d %H:%M", input.trim()).ok()?;
    let zoned = dt.to_zoned(TimeZone::system()).ok()?;
    Some(zoned.strftime("%Y-%m-%dT%H:%M:%S%:z").to_string())
}

/// One of a pick list's options: shown by `label`, chosen by `key`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Choice {
    key: String,
    label: String,
}

impl std::fmt::Display for Choice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.label)
    }
}

fn update(d: &ScheduledDraft, f: impl FnOnce(&mut ScheduledDraft)) -> Message {
    let mut next = d.clone();
    f(&mut next);
    Message::UpdateOverlay(Box::new(Overlay::ScheduledTaskEditor(Box::new(next))))
}

fn field<'a>(label: &'a str, control: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    row![w::muted(label, TEXT_SMALL).width(Length::Fixed(96.0)), control.into()].spacing(S).align_y(Alignment::Center).into()
}

fn pick<'a>(options: Vec<Choice>, current: &str, placeholder: &'a str, on_pick: impl Fn(Choice) -> Message + 'a) -> Element<'a, Message> {
    let selected = options.iter().find(|c| c.key == current).cloned();
    iced::widget::pick_list(options, selected, on_pick).placeholder(placeholder).text_size(TEXT_UI).padding(INPUT_PAD).width(Length::Fill).into()
}

fn choices<K: ToString + Copy>(items: &[(K, &str)]) -> Vec<Choice> {
    items.iter().map(|(k, l)| Choice { key: k.to_string(), label: (*l).to_owned() }).collect()
}

pub fn view<'a>(app: &'a App, d: &'a ScheduledDraft) -> Element<'a, Message> {
    let e = &d.edit;
    let s = &app.vm.settings;
    let mut col = Column::new().spacing(M).push(w::dialog_header(
        Some((Icon::CalendarClock, |c| c.accent)),
        if e.id.is_empty() { "New Scheduled Task" } else { "Edit Scheduled Task" },
        Some(Message::OpenSettings(SettingsSection::Scheduled)),
    ));

    // What
    let mut what = Column::new().spacing(S);
    what = what.push(w::input("Name", &e.name).on_input(move |v| update(d, |n| n.edit.name = v)));
    what = what.push(w::input("Prompt", &e.prompt).on_input(move |v| update(d, |n| n.edit.prompt = v)));
    if e.schedule_kind == "webhook" {
        what = what.push(w::faint("The prompt can use {{body.path}}, {{headers.name}} and {{query.name}} from each request.", TEXT_CAPTION));
    }
    col = col.push(what);

    // When
    let mut when = Column::new().spacing(S).push(w::heading("When"));
    when = when.push(w::segmented(
        &[("interval", None, "Every"), ("fixed_time", None, "At a time"), ("once", None, "Once"), ("webhook", None, "Webhook")],
        e.schedule_kind.as_str(),
        move |k| update(d, |n| n.edit.schedule_kind = k.to_owned()),
    ));
    match e.schedule_kind.as_str() {
        "interval" => {
            let mut opts = choices(&INTERVALS);
            if !INTERVALS.iter().any(|(m, _)| *m == e.interval_minutes) {
                opts.push(Choice { key: e.interval_minutes.to_string(), label: format!("{} minutes", e.interval_minutes) });
            }
            when = when.push(field("Every", pick(opts, &e.interval_minutes.to_string(), "Interval", move |c| update(d, |n| n.edit.interval_minutes = c.key.parse().unwrap_or(60)))));
        }
        "fixed_time" => {
            when = when.push(field("Time", w::input("09:00", &e.time_of_day).on_input(move |v| update(d, |n| n.edit.time_of_day = v)).width(Length::Fixed(96.0))));
            let mut days = row![].spacing(XXS);
            for (i, name) in WEEKDAYS.iter().enumerate() {
                let day = i as u8;
                let on = e.weekdays.contains(&day);
                days = days.push(w::small_button(*name, if on { w::primary_style } else { w::secondary_style }, Some(update(d, move |n| {
                    if on {
                        n.edit.weekdays.retain(|x| *x != day);
                    } else {
                        n.edit.weekdays.push(day);
                        n.edit.weekdays.sort_unstable();
                    }
                }))));
            }
            when = when.push(field("Days", days));
            when = when.push(w::faint(if e.weekdays.is_empty() { "Every day." } else { "On the days chosen." }, TEXT_CAPTION));
        }
        "once" => {
            when = when.push(field("At", w::input("YYYY-MM-DD HH:MM", &d.run_at_input).on_input(move |v| update(d, |n| n.run_at_input = v)).width(Length::Fixed(180.0))));
        }
        _ => {
            when = when.push(w::faint("Runs on each request to the task's URL, shown once it is saved.", TEXT_CAPTION));
        }
    }
    if !s.scheduled_time_zone.is_empty() {
        when = when.push(w::faint(format!("Times are in {}.", s.scheduled_time_zone), TEXT_CAPTION));
    }
    col = col.push(when);

    // Where
    let mut where_ = Column::new().spacing(S).push(w::heading("Where"));
    let projects: Vec<Choice> = s.projects.iter().map(|p| Choice { key: p.id.clone(), label: p.title.clone() }).collect();
    where_ = where_.push(field("Project", pick(projects, &e.project_id, "Choose a project", move |c| update(d, |n| {
        if n.edit.project_id != c.key {
            // A thread belongs to its project.
            n.edit.thread_id.clear();
            n.thread_runtime_mode = None;
        }
        n.edit.project_id = c.key;
    }))));
    where_ = where_.push(field("Thread", pick(choices(&THREAD_MODES), &e.thread_mode, "Thread", move |c| update(d, |n| n.edit.thread_mode = c.key))));
    let threads: Vec<&ScheduledThreadChoiceView> = s.scheduled_thread_choices.iter().filter(|t| t.project_id == e.project_id).collect();
    let chosen = s.scheduled_thread_choices.iter().find(|t| t.id == e.thread_id);
    if e.thread_mode != "new_thread" {
        let label = if e.thread_id.is_empty() {
            if d.first_run_starts_thread() { "Started by the first run".to_owned() } else { "Choose…".to_owned() }
        } else {
            chosen.map_or("A thread not on this device".to_owned(), |t| t.title.clone())
        };
        where_ = where_.push(field(
            "Which",
            row![
                text(label).size(TEXT_UI).font(fonts::MEDIUM).width(Length::Fill),
                w::small_button(if d.picking_thread { "Done" } else { "Choose…" }, w::secondary_style, Some(update(d, |n| n.picking_thread = !n.picking_thread))),
            ]
            .spacing(S)
            .align_y(Alignment::Center),
        ));
        if d.picking_thread {
            where_ = where_.push(thread_picker(d, &threads));
        }
    }
    if !d.bound_to_chosen_thread() {
        where_ = where_.push(field("Folder", w::segmented(&[("root", None, "Project folder"), ("worktree", None, "New worktree")], e.workspace.as_str(), move |k| update(d, |n| n.edit.workspace = k.to_owned()))));
        if e.workspace == "worktree" {
            where_ = where_.push(field("Base branch", w::input("Default branch", &e.base_branch).on_input(move |v| update(d, |n| n.edit.base_branch = v))));
        }
    }
    let scratch = s.projects.iter().find(|p| p.id == e.project_id).is_some_and(|p| p.is_scratch);
    where_ = where_.push(w::faint(
        match e.thread_mode.as_str() {
            "continue" => "Each run is a new message in one thread, which remembers the earlier runs.",
            "fresh_session" => "Each run is a new message in one thread, answered by an agent that starts without the earlier conversation.",
            _ if scratch => "Each run starts its own thread, listed under No project.",
            _ => "Each run starts its own thread.",
        },
        TEXT_CAPTION,
    ));
    col = col.push(where_);

    // Agent
    let mut agent = Column::new().spacing(S).push(w::heading("Agent"));
    let permissions = if d.bound_to_chosen_thread() {
        d.thread_runtime_mode.clone().or_else(|| chosen.map(|t| if t.runtime_mode.is_empty() { "approval-required".to_owned() } else { t.runtime_mode.clone() })).unwrap_or_else(|| "approval-required".into())
    } else {
        e.runtime_mode.clone()
    };
    if d.bound_to_chosen_thread() {
        let model = chosen.map(|t| t.model_label.as_str()).filter(|l| !l.is_empty()).unwrap_or("The thread's");
        agent = agent.push(field("Model", text(model).size(TEXT_UI)));
        agent = agent.push(field("Permissions", pick(choices(&PERMISSIONS), &permissions, "Permissions", move |c| update(d, |n| n.thread_runtime_mode = Some(c.key)))));
    } else {
        let comp = &app.vm.composer;
        let mut models = vec![Choice { key: String::new(), label: "The composer's model".into() }];
        for m in &comp.favorite_models {
            let label = if m.detail.is_empty() { m.option.label.clone() } else { format!("{} · {}", m.detail, m.name) };
            models.push(Choice { key: format!("{}|{}", m.option.instance_id, m.option.model), label: format!("★ {label}") });
        }
        for g in &comp.model_groups {
            for m in &g.models {
                models.push(Choice { key: format!("{}|{}", m.option.instance_id, m.option.model), label: format!("{} · {}", g.provider, m.name) });
            }
        }
        let key = if e.model.is_empty() { String::new() } else { format!("{}|{}", e.model_instance, e.model) };
        if !key.is_empty() && !models.iter().any(|c| c.key == key) {
            models.push(Choice { key: key.clone(), label: e.model.clone() });
        }
        agent = agent.push(field("Model", pick(models, &key, "Model", move |c| update(d, |n| {
            let (inst, model) = c.key.split_once('|').unwrap_or(("", ""));
            n.edit.model_instance = inst.to_owned();
            n.edit.model = model.to_owned();
        }))));
        agent = agent.push(field("Permissions", pick(choices(&PERMISSIONS), &permissions, "Permissions", move |c| update(d, |n| n.edit.runtime_mode = c.key))));
        agent = agent.push(checkbox(e.interaction_mode == "plan").label("Plan first").text_size(TEXT_UI).on_toggle(move |on| update(d, |n| n.edit.interaction_mode = if on { "plan" } else { "default" }.into())));
    }
    agent = agent.push(checkbox(e.enabled).label("Enabled").text_size(TEXT_UI).on_toggle(move |on| update(d, |n| n.edit.enabled = on)));
    if permissions == "approval-required" {
        let note = if d.bound_to_chosen_thread() { format!("{ASKS_WARNING} The permissions are the thread's own: a change here changes the thread.") } else { ASKS_WARNING.to_owned() };
        agent = agent.push(w::colored(note, TEXT_CAPTION, |c| c.warning));
    } else if d.bound_to_chosen_thread() {
        agent = agent.push(w::faint("Runs use the thread's own model, permissions and folder: a change of permissions here changes the thread.", TEXT_CAPTION));
    }
    col = col.push(agent);

    let mut actions = row![].spacing(S).align_y(Alignment::Center);
    if let Some(why) = d.incomplete() {
        actions = actions.push(w::faint(why, TEXT_CAPTION));
    }
    actions = actions.push(w::dialog_actions(vec![
        w::label_button("Cancel", w::secondary_style, Some(Message::OpenSettings(SettingsSection::Scheduled))).into(),
        w::label_button(if e.id.is_empty() { "Create" } else { "Save" }, w::primary_style, d.can_save().then_some(Message::SubmitOverlay)).into(),
    ]));
    col = col.push(actions);

    w::dialog(w::scroll(col).height(Length::Shrink), 560.0).into()
}

/// The project's threads, searched by title; for a task not bound yet, the
/// first run may start one instead.
fn thread_picker<'a>(d: &'a ScheduledDraft, threads: &[&'a ScheduledThreadChoiceView]) -> Element<'a, Message> {
    let q = d.thread_query.to_lowercase();
    let mut list = Column::new().spacing(XXS);
    if d.first_run_starts_thread() {
        list = list.push(w::list_row(w::muted("Started by the first run", TEXT_UI), d.edit.thread_id.is_empty(), false).on_press(update(d, |n| {
            n.edit.thread_id.clear();
            n.picking_thread = false;
        })));
    }
    let mut shown = 0;
    for t in threads.iter().filter(|t| q.is_empty() || t.title.to_lowercase().contains(&q)).take(50) {
        shown += 1;
        let id = t.id.clone();
        list = list.push(
            w::list_row(column![text(&t.title).size(TEXT_UI).font(fonts::MEDIUM), w::faint(&t.detail, TEXT_CAPTION)].spacing(XXS), t.id == d.edit.thread_id, true).on_press(update(d, move |n| {
                n.edit.thread_id = id;
                n.thread_runtime_mode = None;
                n.picking_thread = false;
            })),
        );
    }
    if shown == 0 {
        list = list.push(w::faint(if threads.is_empty() { "This project has no threads yet." } else { "No threads match." }, TEXT_SMALL));
    }
    container(column![w::input("Search threads…", &d.thread_query).on_input(move |v| update(d, |n| n.thread_query = v)), w::scroll(list).height(Length::Fixed(200.0))].spacing(S))
        .padding(M)
        .style(w::inset_sunken)
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_and_finishes_a_draft() {
        let mut d = ScheduledDraft::new_task("p1".into());
        assert_eq!(d.incomplete(), Some("Name the task."));
        d.edit.name = "Nightly".into();
        d.edit.prompt = "Run the tests".into();
        assert!(d.can_save());

        d.edit.schedule_kind = "fixed_time".into();
        d.edit.time_of_day = "7:5".into();
        assert!(d.can_save());
        d.edit.time_of_day = "25:00".into();
        assert_eq!(d.incomplete(), Some("Enter the time as HH:MM."));
        d.edit.time_of_day = "7:05".into();
        let (task, change) = d.clone().finished();
        assert_eq!((task.time_of_day.as_str(), task.run_at.as_str(), change), ("07:05", "", None));

        d.edit.schedule_kind = "once".into();
        d.run_at_input = "tomorrow".into();
        assert!(!d.can_save());
        d.run_at_input = "2026-12-01 09:30".into();
        let (task, _) = d.finished();
        assert!(task.run_at.starts_with("2026-12-01T09:30:00"), "{}", task.run_at);
        assert!(task.run_at.parse::<Timestamp>().is_ok());
    }

    #[test]
    fn a_bound_task_must_keep_a_thread_and_reports_permission_changes() {
        let mut d = ScheduledDraft::wrap(ScheduledTaskEdit {
            id: "t1".into(),
            name: "n".into(),
            prompt: "p".into(),
            project_id: "p1".into(),
            schedule_kind: "interval".into(),
            interval_minutes: 60,
            thread_mode: "continue".into(),
            thread_id: "th1".into(),
            ..ScheduledTaskEdit::default()
        });
        d.edit.thread_id.clear();
        assert_eq!(d.incomplete(), Some("Choose the thread its runs go into."));
        d.edit.thread_id = "th2".into();
        d.thread_runtime_mode = Some("full-access".into());
        let (_, change) = d.finished();
        assert_eq!(change, Some(("th2".to_owned(), "full-access".to_owned())));
    }
}
