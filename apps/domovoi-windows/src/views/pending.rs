//! What waits in the open thread: an approval or a question from the agent,
//! a secret it asks for, and the messages queued behind the turn under way.
//!
//! What is ticked or typed here is the window's own ([`State`]) until it is
//! sent. A secret's value goes to the core once, when it is saved, and is
//! then dropped here.

use std::collections::HashMap;
use std::rc::Rc;

use domovoi_core::Event;
use domovoi_core::app::QuestionAnswer;
use domovoi_core::view::{QuestionView, RequestView, SecretRequestView, ThreadPageView};
use windows_reactor::{
    Border, Button, ButtonStyle, Callback, CheckBox, FontWeight, Grid, GridLength, HorizontalAlignment, Orientation, PasswordBox, StackPanel, TextBlock, TextBox,
    TextTrimming, TextWrapping, Thickness, TooltipExt as _, VerticalAlignment, View,
};

use crate::core_link::CoreLink;
use crate::{theme, widgets};

/// What is chosen and typed for one question.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct Answer {
    selected: Vec<String>,
    custom: String,
}

impl Answer {
    /// The answer as sent: the options chosen, then what was typed.
    fn values(&self) -> Vec<String> {
        let custom = self.custom.trim();
        self.selected.iter().cloned().chain((!custom.is_empty()).then(|| custom.to_owned())).collect()
    }
}

#[derive(Default)]
pub struct State {
    /// By request, then by question.
    answers: HashMap<String, HashMap<String, Answer>>,
    /// Secrets as typed, by their request's item.
    secrets: HashMap<String, String>,
    /// The queued message being changed: its run and its text.
    editing: Option<(String, String)>,
}

/// What the views here tell the window.
#[derive(Clone, Debug)]
pub enum Edit {
    /// An option of a question ticked or unticked (`multi`), or chosen.
    Toggle { request_id: String, question_id: String, value: String, multi: bool },
    /// A question's own answer, typed.
    Custom { request_id: String, question_id: String, text: Rc<str> },
    Submit { request_id: String },
    Secret { item_id: String, text: Rc<str> },
    SaveSecret { item_id: String },
    StartQueued { run_id: String, text: String },
    Queued(Rc<str>),
    CommitQueued,
    CancelQueued,
}

impl State {
    fn answer(&self, request_id: &str, question_id: &str) -> Option<&Answer> {
        self.answers.get(request_id)?.get(question_id)
    }

    /// Whether each required question of `request` has an answer.
    fn answered(&self, request: &RequestView) -> bool {
        request.questions.iter().all(|q| !q.required || self.answer(&request.id, &q.id).is_some_and(|a| !a.values().is_empty()))
    }

    /// Takes `edit`; the event for the core when it sends something. `page`
    /// is the open thread, which says whether what is sent is still wanted.
    pub fn apply(&mut self, edit: Edit, page: Option<&ThreadPageView>) -> Option<Event> {
        match edit {
            Edit::Toggle { request_id, question_id, value, multi } => {
                let answer = self.answers.entry(request_id).or_default().entry(question_id).or_default();
                match answer.selected.iter().position(|v| *v == value) {
                    Some(at) if multi => {
                        answer.selected.remove(at);
                    }
                    None if multi => answer.selected.push(value),
                    _ => answer.selected = vec![value],
                }
                None
            }
            Edit::Custom { request_id, question_id, text } => {
                self.answers.entry(request_id).or_default().entry(question_id).or_default().custom = text.to_string();
                None
            }
            Edit::Submit { request_id } => {
                let request = page?.pending_requests.iter().find(|r| r.id == request_id)?;
                if !self.answered(request) {
                    return None;
                }
                let answers = self.answers.remove(&request_id).unwrap_or_default();
                // In the order asked, whatever order they were answered in.
                let answers = request
                    .questions
                    .iter()
                    .filter_map(|q| answers.get(&q.id).map(|a| QuestionAnswer { question_id: q.id.clone(), values: a.values() }))
                    .filter(|a| !a.values.is_empty())
                    .collect();
                Some(Event::RespondUserInput { request_id, answers })
            }
            Edit::Secret { item_id, text } => {
                self.secrets.insert(item_id, text.to_string());
                None
            }
            Edit::SaveSecret { item_id } => {
                let waits = page?.secret_requests.iter().any(|r| r.item_id == item_id && !r.sending);
                let value = self.secrets.get(&item_id).filter(|v| !v.trim().is_empty())?.clone();
                if !waits {
                    return None;
                }
                self.secrets.remove(&item_id);
                Some(Event::AnswerSecret { item_id, value })
            }
            Edit::StartQueued { run_id, text } => {
                self.editing = Some((run_id, text));
                None
            }
            Edit::Queued(text) => {
                if let Some((_, typed)) = &mut self.editing {
                    *typed = text.to_string();
                }
                None
            }
            Edit::CommitQueued => self.editing.take().map(|(run_id, text)| Event::EditQueued { run_id, text }),
            Edit::CancelQueued => {
                self.editing = None;
                None
            }
        }
    }

    /// Forgets what was typed for requests the thread no longer has.
    pub fn prune(&mut self, page: Option<&ThreadPageView>) {
        let requests: Vec<&str> = page.map(|p| p.pending_requests.iter().map(|r| r.id.as_str()).collect()).unwrap_or_default();
        let secrets: Vec<&str> = page.map(|p| p.secret_requests.iter().map(|r| r.item_id.as_str()).collect()).unwrap_or_default();
        self.answers.retain(|id, _| requests.contains(&id.as_str()));
        self.secrets.retain(|id, _| secrets.contains(&id.as_str()));
        if let Some((run_id, _)) = &self.editing
            && !page.is_some_and(|p| p.queued.iter().any(|q| q.run_id == *run_id))
        {
            self.editing = None;
        }
    }
}

/// What the views here need besides what they show.
#[derive(Clone)]
pub struct Hooks {
    pub on_edit: Callback<Edit>,
    /// A page to open in the browser.
    pub on_open: Callback<String>,
    /// The transcript's text size.
    pub size: f64,
    pub dark: bool,
}

fn caption(text: &str) -> TextBlock {
    TextBlock::new().text(text).font_size(theme::CAPTION).opacity(0.65).text_wrapping(TextWrapping::WrapWholeWords)
}

fn text(s: &str, size: f64) -> TextBlock {
    TextBlock::new().text(s).font_size(size).text_wrapping(TextWrapping::WrapWholeWords).is_text_selection_enabled(true)
}

fn mono(s: &str) -> TextBlock {
    TextBlock::new().text(s).font_family(theme::MONO).font_size(theme::CAPTION).text_wrapping(TextWrapping::Wrap).is_text_selection_enabled(true)
}

/// A card with a glyph and a title, in the accent's outline: it wants an answer.
fn card(glyph: &str, title: &str, body: Vec<View>) -> View {
    let mut lines = vec![
        StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(8.0)
            .children(vec![
                theme::glyph(glyph, 14.0).foreground(theme::accent()).into(),
                TextBlock::new().text(title).font_weight(FontWeight::SEMI_BOLD).text_wrapping(TextWrapping::WrapWholeWords).into(),
            ])
            .into(),
    ];
    lines.extend(body);
    Border::new()
        .padding(Thickness::xy(14.0, 10.0))
        .background(theme::card())
        .border_brush(theme::accent())
        .border_thickness(Thickness::uniform(1.0))
        .corner_radius(theme::RADIUS)
        .content(StackPanel::new().orientation(Orientation::Vertical).spacing(8.0).children(lines))
        .into()
}

fn row(buttons: Vec<View>) -> View {
    StackPanel::new().orientation(Orientation::Horizontal).spacing(8.0).children(buttons).into()
}

/// An approval the turn waits on, or the agent's questions, with their answers.
pub fn request(core: &CoreLink, request: &RequestView, state: &State, hooks: &Hooks) -> View {
    let mut body: Vec<View> = Vec::new();
    if let Some(detail) = request.detail.as_deref().filter(|d| !d.is_empty()) {
        body.push(Border::new().padding(Thickness::uniform(8.0)).background(theme::code()).corner_radius(4.0).content(mono(detail)).into());
    }
    // What an edit would change, when the provider says.
    body.extend(request.files.iter().map(|file| widgets::diff_file(file, hooks.dark, 60)));
    if request.questions.is_empty() {
        let answer = |label: &str, decision: &'static str, style: ButtonStyle| -> View {
            let (core, id) = (core.clone(), request.id.clone());
            Button::new().style(style).content(TextBlock::new().text(label)).on_click(move || core.send(Event::Respond { request_id: id.clone(), decision: decision.to_owned() })).into()
        };
        let session = if request.session_label.is_empty() { "Allow for Session" } else { request.session_label.as_str() };
        body.push(row(vec![answer("Allow", "accept", ButtonStyle::Accent), answer(session, "acceptForSession", ButtonStyle::Default), answer("Decline", "decline", ButtonStyle::Default)]));
        return card("\u{E7BA}", &request.prompt, body);
    }
    for question in &request.questions {
        body.push(self::question(&request.id, question, state.answer(&request.id, &question.id), hooks));
    }
    let answered = state.answered(request);
    let submit = {
        let (on_edit, id) = (hooks.on_edit.clone(), request.id.clone());
        Button::new().style(ButtonStyle::Accent).is_enabled(answered).content(TextBlock::new().text("Submit Answer")).on_click(move || on_edit.call(Edit::Submit { request_id: id.clone() }))
    };
    let skip = {
        let (core, id) = (core.clone(), request.id.clone());
        Button::new().content(TextBlock::new().text("Skip")).on_click(move || core.send(Event::DismissUserInput { request_id: id.clone() })).tooltip("Leave it unanswered: the agent carries on")
    };
    body.push(row(vec![if answered { submit.into() } else { submit.tooltip("Answer the required questions first") }, skip]));
    card("\u{E897}", if request.prompt.is_empty() { "The agent has a question" } else { &request.prompt }, body)
}

/// One question: its options (one, or several when it takes several) and a box for another answer.
fn question(request_id: &str, question: &QuestionView, answer: Option<&Answer>, hooks: &Hooks) -> View {
    let mut lines: Vec<View> = Vec::new();
    if !question.header.is_empty() {
        lines.push(caption(&question.header).font_weight(FontWeight::SEMI_BOLD).into());
    }
    let asked = if question.required { format!("{}  (required)", question.question) } else { question.question.clone() };
    lines.push(text(&asked, hooks.size).into());
    for option in &question.options {
        let checked = answer.is_some_and(|a| a.selected.contains(&option.value));
        let edit = Edit::Toggle { request_id: request_id.to_owned(), question_id: question.id.clone(), value: option.value.clone(), multi: question.multi_select };
        let on_edit = hooks.on_edit.clone();
        let mut label: Vec<View> = vec![TextBlock::new().text(&option.label).text_wrapping(TextWrapping::WrapWholeWords).into()];
        if !option.description.is_empty() {
            label.push(caption(&option.description).into());
        }
        lines.push(
            CheckBox::new()
                .automation_name(&option.label)
                .is_checked(checked)
                .content(StackPanel::new().orientation(Orientation::Vertical).children(label))
                .on_click(move || on_edit.call(edit.clone()))
                .into(),
        );
    }
    if question.allow_custom_answer || question.options.is_empty() {
        let (on_edit, request_id, question_id) = (hooks.on_edit.clone(), request_id.to_owned(), question.id.clone());
        lines.push(
            TextBox::new()
                .automation_name(format!("Answer to: {}", question.question))
                .placeholder_text(if question.options.is_empty() { "Your answer" } else { "Another answer" })
                .text(answer.map_or("", |a| a.custom.as_str()))
                .on_text_changed(move |text: Rc<str>| on_edit.call(Edit::Custom { request_id: request_id.clone(), question_id: question_id.clone(), text }))
                .into(),
        );
    }
    StackPanel::new().orientation(Orientation::Vertical).spacing(4.0).children(lines).into()
}

/// A secret the agent asks for: a password box, Save and Decline.
pub fn secret(core: &CoreLink, asked: &SecretRequestView, state: &State, hooks: &Hooks) -> View {
    let typed = state.secrets.get(&asked.item_id).map_or("", String::as_str);
    let ready = !typed.trim().is_empty() && !asked.sending;
    let mut body: Vec<View> = Vec::new();
    if !asked.reason.is_empty() {
        body.push(text(&asked.reason, hooks.size).into());
    }
    if let Some(url) = asked.url.clone() {
        let on_open = hooks.on_open.clone();
        body.push(Button::new().horizontal_alignment(HorizontalAlignment::Left).content(TextBlock::new().text("Get One")).on_click(move || on_open.call(url.clone())).into());
    }
    let (on_edit, item_id) = (hooks.on_edit.clone(), asked.item_id.clone());
    body.push(
        PasswordBox::new()
            .automation_name(format!("Value of {}", asked.label))
            .placeholder_text(if asked.placeholder.is_empty() { "Secret value" } else { asked.placeholder.as_str() })
            .password(typed)
            .is_enabled(!asked.sending)
            .on_password_changed(move |text: Rc<str>| on_edit.call(Edit::Secret { item_id: item_id.clone(), text }))
            .into(),
    );
    if !asked.hint.is_empty() {
        body.push(caption(&asked.hint).into());
    }
    let save = {
        let (on_edit, item_id) = (hooks.on_edit.clone(), asked.item_id.clone());
        Button::new()
            .style(ButtonStyle::Accent)
            .is_enabled(ready)
            .content(TextBlock::new().text(if asked.sending { "Saving…" } else { "Save Secret" }))
            .on_click(move || on_edit.call(Edit::SaveSecret { item_id: item_id.clone() }))
    };
    let decline = {
        let (core, item_id) = (core.clone(), asked.item_id.clone());
        Button::new().is_enabled(!asked.sending).content(TextBlock::new().text("Decline")).on_click(move || core.send(Event::DeclineSecret { item_id: item_id.clone() }))
    };
    body.push(row(vec![save.into(), decline.into()]));
    card("\u{E8D7}", &format!("The agent asks for {}", asked.label), body)
}

/// The messages waiting behind the turn under way: each can go now, move, be changed or dropped.
pub fn queued(core: &CoreLink, page: &ThreadPageView, state: &State, hooks: &Hooks) -> View {
    let mut lines: Vec<View> = Vec::new();
    let title = TextBlock::new().text(format!("Queued ({})", page.queued.len())).font_weight(FontWeight::SEMI_BOLD).vertical_alignment(VerticalAlignment::Center);
    let mut head: Vec<View> = vec![title.into()];
    if page.queue_held {
        let core = core.clone();
        head.push(Button::new().grid_column(1).style(ButtonStyle::Accent).content(TextBlock::new().text("Resume Queue")).on_click(move || core.send(Event::ResumeQueue)).tooltip("A stop held the queue"));
    }
    lines.push(Grid::new().columns([GridLength::STAR, GridLength::Auto]).children(head).into());
    for message in &page.queued {
        if let Some((_, typed)) = state.editing.as_ref().filter(|(run_id, _)| *run_id == message.run_id) {
            let on_edit = hooks.on_edit.clone();
            let button = |label: &str, edit: Edit, style: ButtonStyle| -> View {
                let on_edit = hooks.on_edit.clone();
                Button::new().style(style).content(TextBlock::new().text(label)).on_click(move || on_edit.call(edit.clone())).into()
            };
            lines.push(
                Grid::new()
                    .columns([GridLength::STAR, GridLength::Auto])
                    .column_spacing(8.0)
                    .children((
                        TextBox::new().automation_name("Queued message").accepts_return(true).text_wrapping(TextWrapping::Wrap).max_height(160.0).text(typed).on_text_changed(move |text: Rc<str>| on_edit.call(Edit::Queued(text))),
                        StackPanel::new()
                            .grid_column(1)
                            .orientation(Orientation::Horizontal)
                            .spacing(6.0)
                            .vertical_alignment(VerticalAlignment::Bottom)
                            .children(vec![button("Save", Edit::CommitQueued, ButtonStyle::Accent), button("Cancel", Edit::CancelQueued, ButtonStyle::Default)]),
                    ))
                    .into(),
            );
            continue;
        }
        let run = &message.run_id;
        let act = |glyph: &str, name: &str, event: Event| -> View {
            let core = core.clone();
            Button::new().style(ButtonStyle::Subtle).automation_name(name).content(theme::glyph(glyph, 12.0)).on_click(move || core.send(event.clone())).tooltip(name)
        };
        let mut buttons: Vec<View> = Vec::new();
        if message.can_steer {
            let (core, run) = (core.clone(), run.clone());
            buttons.push(Button::new().content(TextBlock::new().text("Steer Now")).on_click(move || core.send(Event::SteerQueued { run_id: run.clone() })).tooltip("Send it into the turn under way"));
        }
        if !message.first {
            buttons.push(act("\u{E74A}", "Move up", Event::MoveQueued { run_id: run.clone(), delta: -1 }));
        }
        if !message.last {
            buttons.push(act("\u{E74B}", "Move down", Event::MoveQueued { run_id: run.clone(), delta: 1 }));
        }
        let edit = {
            let (on_edit, start) = (hooks.on_edit.clone(), Edit::StartQueued { run_id: run.clone(), text: message.text.clone() });
            Button::new().style(ButtonStyle::Subtle).automation_name("Edit").content(theme::glyph("\u{E70F}", 12.0)).on_click(move || on_edit.call(start.clone())).tooltip("Edit")
        };
        buttons.push(edit);
        buttons.push(act("\u{E711}", "Remove from the queue", Event::CancelQueued { run_id: run.clone() }));
        lines.push(
            Grid::new()
                .columns([GridLength::STAR, GridLength::Auto])
                .column_spacing(8.0)
                .children((
                    TextBlock::new().text(&message.text).max_lines(3).text_wrapping(TextWrapping::WrapWholeWords).text_trimming(TextTrimming::CharacterEllipsis).vertical_alignment(VerticalAlignment::Center),
                    StackPanel::new().grid_column(1).orientation(Orientation::Horizontal).spacing(2.0).children(buttons),
                ))
                .into(),
        );
    }
    Border::new()
        .padding(Thickness::xy(14.0, 10.0))
        .background(theme::card())
        .border_brush(theme::stroke())
        .border_thickness(Thickness::uniform(1.0))
        .corner_radius(theme::RADIUS)
        .content(StackPanel::new().orientation(Orientation::Vertical).spacing(6.0).children(lines))
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;
    use domovoi_core::view::{QueuedMessageView, QuestionOptionView};

    fn question(id: &str, required: bool) -> QuestionView {
        QuestionView { id: id.into(), required, options: vec![QuestionOptionView { value: "a".into(), ..QuestionOptionView::default() }], ..QuestionView::default() }
    }

    fn page() -> ThreadPageView {
        ThreadPageView {
            pending_requests: vec![RequestView { id: "r1".into(), questions: vec![question("q1", true), question("q2", false)], ..RequestView::default() }],
            secret_requests: vec![SecretRequestView { item_id: "s1".into(), ..SecretRequestView::default() }],
            queued: vec![QueuedMessageView { run_id: "run1".into(), text: "later".into(), ..QueuedMessageView::default() }],
            ..ThreadPageView::default()
        }
    }

    fn toggle(question: &str, value: &str, multi: bool) -> Edit {
        Edit::Toggle { request_id: "r1".into(), question_id: question.into(), value: value.into(), multi }
    }

    #[test]
    fn answers_go_once_each_required_question_has_one() {
        let (mut state, page) = (State::default(), page());
        let submit = || Edit::Submit { request_id: "r1".into() };
        state.apply(Edit::Custom { request_id: "r1".into(), question_id: "q2".into(), text: " also this ".into() }, Some(&page));
        assert!(state.apply(submit(), Some(&page)).is_none(), "the required question has no answer yet");
        state.apply(toggle("q1", "a", false), Some(&page));
        state.apply(toggle("q1", "b", false), Some(&page));
        let Some(Event::RespondUserInput { request_id, answers }) = state.apply(submit(), Some(&page)) else { panic!("it is answered") };
        assert_eq!(request_id, "r1");
        let sent: Vec<(&str, Vec<&str>)> = answers.iter().map(|a| (a.question_id.as_str(), a.values.iter().map(String::as_str).collect())).collect();
        assert_eq!(sent, [("q1", vec!["b"]), ("q2", vec!["also this"])], "one choice replaces another; answers go in the order asked");
        assert!(state.apply(submit(), Some(&page)).is_none(), "what was sent is not sent again");
    }

    #[test]
    fn a_question_that_takes_several_keeps_each_tick() {
        let (mut state, page) = (State::default(), page());
        for value in ["a", "b", "a", "c"] {
            state.apply(toggle("q1", value, true), Some(&page));
        }
        assert_eq!(state.answer("r1", "q1").map(Answer::values), Some(vec!["b".to_owned(), "c".to_owned()]), "ticking twice unticks");
    }

    #[test]
    fn a_secret_is_sent_once_and_then_forgotten() {
        let (mut state, mut page) = (State::default(), page());
        let save = || Edit::SaveSecret { item_id: "s1".into() };
        assert!(state.apply(save(), Some(&page)).is_none(), "nothing typed");
        state.apply(Edit::Secret { item_id: "s1".into(), text: "hunter2".into() }, Some(&page));
        page.secret_requests[0].sending = true;
        assert!(state.apply(save(), Some(&page)).is_none(), "an answer is already on its way");
        page.secret_requests[0].sending = false;
        assert!(matches!(state.apply(save(), Some(&page)), Some(Event::AnswerSecret { item_id, value }) if item_id == "s1" && value == "hunter2"));
        assert!(state.secrets.is_empty());
        assert!(state.apply(save(), Some(&page)).is_none());
    }

    #[test]
    fn a_queued_message_is_changed_or_left_and_forgotten_when_it_goes() {
        let (mut state, page) = (State::default(), page());
        state.apply(Edit::StartQueued { run_id: "run1".into(), text: "later".into() }, Some(&page));
        state.apply(Edit::Queued("sooner".into()), Some(&page));
        assert!(matches!(state.apply(Edit::CommitQueued, Some(&page)), Some(Event::EditQueued { run_id, text }) if run_id == "run1" && text == "sooner"));
        state.apply(Edit::StartQueued { run_id: "run1".into(), text: "later".into() }, Some(&page));
        state.apply(Edit::Secret { item_id: "s1".into(), text: "x".into() }, Some(&page));
        // The thread moved on: the message went, the secret was answered elsewhere.
        state.prune(Some(&ThreadPageView::default()));
        assert!(state.editing.is_none() && state.secrets.is_empty());
        assert!(state.apply(Edit::CommitQueued, None).is_none());
    }
}
