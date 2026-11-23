//! An Antigravity instance's settings: its sign-in method and credentials.
//! Mirrors Apple's `ProviderInstanceEditor`.

use domovoi_core::app::ProviderInstanceEdit;
use domovoi_core::view::{ChoiceView, ProviderView};
use iced::widget::{Column, checkbox, row};
use iced::{Alignment, Element, Length};

use crate::app::{Message, Overlay, SettingsSection};
use crate::ui::icons::Icon;
use crate::ui::tokens::*;
use crate::ui::widgets as w;

/// A provider instance being edited, or added (`instance_id` empty).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ProviderDraft {
    pub edit: ProviderInstanceEdit,
    methods: Vec<ChoiceView>,
    built_in: bool,
    has_key: bool,
}

impl ProviderDraft {
    /// The editor of `p`'s settings as they are (the key is never shown).
    pub fn of(p: &ProviderView) -> Self {
        let c = &p.config;
        Self {
            edit: ProviderInstanceEdit {
                instance_id: p.instance_id.clone(),
                driver: p.driver.clone(),
                display_name: c.display_name.clone(),
                enabled: p.enabled,
                auth_method: c.auth_method.clone(),
                api_key: String::new(),
                gcp_project: c.gcp_project.clone(),
                gcp_location: c.gcp_location.clone(),
                binary_path: c.binary_path.clone(),
            },
            methods: c.auth_methods.clone(),
            built_in: c.built_in,
            has_key: c.has_api_key,
        }
    }

    /// A new Antigravity instance: an API key or Vertex (the Google sign-in is the built-in one's).
    pub fn new_antigravity() -> Self {
        let choice = |value: &str, label: &str| ChoiceView { value: value.into(), label: label.into() };
        Self {
            edit: ProviderInstanceEdit { driver: "antigravity".into(), enabled: true, auth_method: "gemini-api-key".into(), ..ProviderInstanceEdit::default() },
            methods: vec![choice("gemini-api-key", "Gemini API key"), choice("agent-platform", "Agent Platform (Vertex AI)")],
            built_in: false,
            has_key: false,
        }
    }

    fn is_new(&self) -> bool {
        self.edit.instance_id.is_empty()
    }

    fn uses_key(&self) -> bool {
        matches!(self.edit.auth_method.as_str(), "gemini-api-key" | "agent-platform")
    }

    fn uses_project(&self) -> bool {
        matches!(self.edit.auth_method.as_str(), "oauth-business" | "agent-platform")
    }

    pub fn can_save(&self) -> bool {
        !(self.is_new() && self.edit.auth_method == "gemini-api-key" && self.edit.api_key.is_empty())
    }
}

fn update(d: &ProviderDraft, f: impl FnOnce(&mut ProviderDraft)) -> Message {
    let mut next = d.clone();
    f(&mut next);
    Message::UpdateOverlay(Box::new(Overlay::ProviderInstanceEditor(Box::new(next))))
}

fn field<'a>(label: &'a str, control: impl Into<Element<'a, Message>>) -> Element<'a, Message> {
    row![w::muted(label, TEXT_SMALL).width(Length::Fixed(110.0)), control.into()].spacing(S).align_y(Alignment::Center).into()
}

pub fn view(d: &ProviderDraft) -> Element<'_, Message> {
    let e = &d.edit;
    let back = Message::OpenSettings(SettingsSection::Providers);
    let mut col = Column::new().spacing(M).push(w::dialog_header(
        Some((Icon::Cpu, |c| c.accent)),
        if d.is_new() { "New Provider Account" } else { "Configure Provider" },
        Some(back.clone()),
    ));
    if !d.built_in {
        col = col.push(field("Name", w::input("Antigravity (work)", &e.display_name).on_input(move |v| update(d, |n| n.edit.display_name = v))));
    }
    if d.methods.len() > 1 {
        let opts: Vec<(&str, Option<Icon>, &str)> = d.methods.iter().map(|m| (m.value.as_str(), None, m.label.as_str())).collect();
        col = col.push(field("Sign-in method", w::segmented(&opts, e.auth_method.as_str(), move |v| update(d, |n| n.edit.auth_method = v.to_owned()))));
    }
    if d.uses_key() {
        let hint = if d.has_key {
            "Saved (type to replace)"
        } else if e.auth_method == "agent-platform" {
            "Optional"
        } else {
            "Required"
        };
        col = col.push(field("API key", w::input(hint, &e.api_key).secure(true).on_input(move |v| update(d, |n| n.edit.api_key = v))));
    }
    if d.uses_project() {
        col = col
            .push(field("GCP project", w::input("my-project-id", &e.gcp_project).on_input(move |v| update(d, |n| n.edit.gcp_project = v))))
            .push(field("GCP location", w::input("us-central1", &e.gcp_location).on_input(move |v| update(d, |n| n.edit.gcp_location = v))));
    }
    col = col.push(field("Binary path", w::input("Automatic", &e.binary_path).on_input(move |v| update(d, |n| n.edit.binary_path = v))));
    col = col.push(checkbox(e.enabled).label("Enabled").text_size(TEXT_UI).on_toggle(move |on| update(d, |n| n.edit.enabled = on)));

    let actions = w::dialog_actions(vec![
        w::label_button("Cancel", w::secondary_style, Some(back)).into(),
        w::label_button(if d.is_new() { "Add" } else { "Save" }, w::primary_style, d.can_save().then_some(Message::SubmitOverlay)).into(),
    ]);
    w::dialog(col.push(actions), 520.0).into()
}
