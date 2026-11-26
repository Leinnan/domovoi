//! Settings: this app's appearance, the connected server and its own
//! settings, and the servers this device knows.

use std::rc::Rc;

use domovoi_core::Event;
use domovoi_core::view::{ConnectionView, SettingFieldView};
use domovoi_desktop_host::prefs::{self, Appearance, Prefs};
use windows_reactor::{
    Button, ButtonStyle, ComboBox, FontWeight, Grid, GridLength, HorizontalAlignment, Image, Menu, MenuExt as _, MenuItem, Orientation, Slider, StackPanel,
    Stretch, TextBlock, TextBox, TextTrimming, TextWrapping, ToggleSwitch, VerticalAlignment, View,
};

use super::{Hooks, Shown, caption, card, heading};
use crate::companion::{self as mascot, Mood};
use crate::core_link::CoreLink;
use crate::theme;

/// A setting: its name and what it does on the left, its control on the right.
pub(super) fn row(label: &str, help: &str, control: View) -> View {
    let mut name: Vec<View> = vec![TextBlock::new().text(label).text_wrapping(TextWrapping::WrapWholeWords).into()];
    if !help.is_empty() {
        name.push(caption(help).into());
    }
    Grid::new()
        .columns([GridLength::STAR, GridLength::Auto])
        .column_spacing(16.0)
        .children((
            StackPanel::new().orientation(Orientation::Vertical).vertical_alignment(VerticalAlignment::Center).children(name),
            StackPanel::new().grid_column(1).orientation(Orientation::Horizontal).spacing(8.0).vertical_alignment(VerticalAlignment::Center).children(vec![control]),
        ))
        .into()
}

/// A switch with no On/Off words beside it (the control keeps room for them unless told otherwise).
pub(super) fn switch(on: bool) -> ToggleSwitch {
    ToggleSwitch::new().min_width(0.0).is_on(on).on_content(TextBlock::new()).off_content(TextBlock::new())
}

pub fn appearance(prefs: &Prefs, hooks: &Hooks<'_>) -> Vec<View> {
    let chosen = Appearance::ALL.iter().position(|a| *a == prefs.appearance).unwrap_or(0);
    let on_appearance = hooks.on_appearance.clone();
    let theme = ComboBox::new()
        .automation_name("Theme")
        .min_width(160.0)
        .items_source(["Use system setting", "Light", "Dark"].map(str::to_owned))
        .selected_index(chosen)
        .on_selection_changed(move |index: Option<usize>| {
            if let Some(appearance) = index.filter(|i| *i != chosen).and_then(|i| Appearance::ALL.get(i)) {
                on_appearance.call(*appearance);
            }
        });
    let size = f64::from(prefs.text_size);
    let on_text_size = hooks.on_text_size.clone();
    let text_size = StackPanel::new()
        .orientation(Orientation::Horizontal)
        .spacing(10.0)
        .children(vec![
            Slider::new()
                .automation_name("Transcript text size")
                .width(180.0)
                .minimum(f64::from(prefs::TEXT_MIN))
                .maximum(f64::from(prefs::TEXT_MAX))
                .step_frequency(1.0)
                .value(size)
                .on_value_changed(move |value: f64| {
                    if (value - size).abs() >= 0.5 {
                        on_text_size.call(value);
                    }
                })
                .into(),
            TextBlock::new().text(format!("{size:.0} pt")).min_width(40.0).vertical_alignment(VerticalAlignment::Center).into(),
        ])
        .into();
    vec![
        card(vec![
            row("Theme", "Light, dark, or as Windows is set.", theme.into()),
            row("Transcript text size", "How large a thread's messages are.", text_size),
        ]),
        card(vec![TextBlock::new().text("The quick brown fox asks the agent to jump over the lazy dog.").font_size(size).text_wrapping(TextWrapping::WrapWholeWords).into()]),
    ]
}

pub fn general(core: &CoreLink, shown: &Shown<'_>, hooks: &Hooks<'_>) -> Vec<View> {
    let mut out: Vec<View> = vec![heading("Connected server"), connected_server(core, shown.connection)];
    if shown.settings.groups.is_empty() {
        out.push(super::empty("The server's settings are loading."));
    }
    for group in &shown.settings.groups {
        out.push(heading(&group.title));
        out.push(card(group.fields.iter().map(|field| setting(core, field, shown, hooks)).collect()));
    }
    out
}

/// The server this window is connected to, and how.
fn connected_server(core: &CoreLink, connection: &ConnectionView) -> View {
    let fact = |label: &str, value: &str| -> View {
        Grid::new()
            .columns([GridLength::Pixel(120.0), GridLength::STAR])
            .children((
                caption(label),
                TextBlock::new().grid_column(1).text(value).text_wrapping(TextWrapping::Wrap).is_text_selection_enabled(true),
            ))
            .into()
    };
    let route = match (connection.remote, connection.path.as_str()) {
        (false, _) => "This network".to_owned(),
        (true, "direct") => format!("Remote, over a direct link ({} ms)", connection.rtt_ms),
        (true, "relay") => format!("Remote, over a relay ({} ms)", connection.rtt_ms),
        (true, _) => "Remote".to_owned(),
    };
    let button = |label: &str, event: Event| -> View {
        let core = core.clone();
        Button::new().content(TextBlock::new().text(label)).on_click(move || core.send(event.clone())).into()
    };
    let mut lines = vec![fact("Name", &connection.environment_label), fact("Address", &connection.server_url), fact("Route", &route)];
    if !connection.server_version.is_empty() {
        lines.push(fact("Version", &connection.server_version));
    }
    if !connection.platform.is_empty() {
        lines.push(fact("Platform", &connection.platform));
    }
    lines.push(
        StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(8.0)
            .children(vec![button("Resync", Event::Resync), button("Reconnect", Event::Reconnect), button("Disconnect", Event::Disconnect)])
            .into(),
    );
    card(lines)
}

/// One of the server's settings: a switch, a choice or a text.
fn setting(core: &CoreLink, field: &SettingFieldView, shown: &Shown<'_>, hooks: &Hooks<'_>) -> View {
    let control: View = match field.kind.as_str() {
        "bool" => {
            let (core, key) = (core.clone(), field.key.clone());
            switch(field.bool_value).automation_name(&field.label).on_toggled(move |on: bool| core.send(Event::SetSettingBool { key: key.clone(), value: on })).into()
        }
        "choice" => {
            let chosen = field.choices.iter().position(|c| c.value == field.text_value);
            let (core, key) = (core.clone(), field.key.clone());
            let values: Vec<String> = field.choices.iter().map(|c| c.value.clone()).collect();
            ComboBox::new()
                .automation_name(&field.label)
                .min_width(200.0)
                .items_source(field.choices.iter().map(|c| c.label.clone()))
                .selected_index(chosen)
                .on_selection_changed(move |index: Option<usize>| {
                    if let Some(value) = index.filter(|i| Some(*i) != chosen).and_then(|i| values.get(i)) {
                        core.send(Event::SetSettingText { key: key.clone(), value: value.clone() });
                    }
                })
                .into()
        }
        _ => {
            // The window's own copy while typing; Save tells the server.
            let typed = shown.state.drafts.get(&field.key).map_or(field.text_value.as_str(), String::as_str);
            let changed = typed != field.text_value;
            let (on_draft, key) = (hooks.on_draft.clone(), field.key.clone());
            let save = {
                let (core, key, value) = (core.clone(), field.key.clone(), typed.trim().to_owned());
                Button::new()
                    .style(if changed { ButtonStyle::Accent } else { ButtonStyle::Default })
                    .is_enabled(changed)
                    .automation_name(format!("Save {}", field.label))
                    .content(TextBlock::new().text("Save"))
                    .on_click(move || core.send(Event::SetSettingText { key: key.clone(), value: value.clone() }))
            };
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(8.0)
                .children(vec![
                    TextBox::new().automation_name(&field.label).width(260.0).text(typed).on_text_changed(move |text: Rc<str>| on_draft.call((key.clone(), text))).into(),
                    save.into(),
                ])
                .into()
        }
    };
    row(&field.label, &field.help, control)
}

/// The servers this device is paired with: switch, configure, rename, reorder, forget, add.
pub fn servers(connection: &ConnectionView, dark: bool, hooks: &Hooks<'_>) -> Vec<View> {
    let count = connection.environments.len();
    let mut rows: Vec<View> = connection
        .environments
        .iter()
        .enumerate()
        .map(|(index, server)| {
            let id = &server.id;
            let mut name: Vec<View> = vec![TextBlock::new().text(&server.label).font_weight(FontWeight::SEMI_BOLD).text_trimming(TextTrimming::CharacterEllipsis).into()];
            if !server.detail.is_empty() {
                name.push(caption(&server.detail).into());
            }
            let mut items = vec![
                MenuItem::item(format!("env-configure:{id}"), "Configure Settings…"),
                MenuItem::item(format!("env-rename:{id}"), "Rename on this PC…"),
            ];
            if index > 0 {
                items.push(MenuItem::item(format!("env-up:{id}"), "Move Up"));
            }
            if index + 1 < count {
                items.push(MenuItem::item(format!("env-down:{id}"), "Move Down"));
            }
            items.push(MenuItem::separator("separator"));
            items.push(MenuItem::item(format!("env-forget:{id}"), "Forget…"));
            let mut controls: Vec<View> = Vec::new();
            if server.active {
                controls.push(caption(if connection.state == "connected" { "Connected" } else { "Selected" }).vertical_alignment(VerticalAlignment::Center).into());
            } else {
                let (on_menu, key) = (hooks.on_menu.clone(), format!("env-select:{id}"));
                controls.push(Button::new().automation_name(format!("Switch to {}", server.label)).content(TextBlock::new().text("Switch")).on_click(move || on_menu.call(key.as_str().into())).into());
            }
            {
                let (on_menu, key) = (hooks.on_menu.clone(), format!("env-configure:{id}"));
                controls.push(Button::new().automation_name(format!("Configure {}", server.label)).content(TextBlock::new().text("Configure")).on_click(move || on_menu.call(key.as_str().into())).into());
            }
            let mood = if server.active && connection.state == "connected" { Mood::Idle } else { Mood::Offline };
            Grid::new()
                .columns([GridLength::Auto, GridLength::STAR, GridLength::Auto, GridLength::Auto])
                .column_spacing(12.0)
                .children((
                    Image::new().source_svg(mascot::image(&server.companion.look, mood, dark)).stretch(Stretch::Uniform).width(28.0).height(28.0),
                    StackPanel::new().grid_column(1).orientation(Orientation::Vertical).vertical_alignment(VerticalAlignment::Center).children(name),
                    StackPanel::new().grid_column(2).orientation(Orientation::Horizontal).spacing(8.0).vertical_alignment(VerticalAlignment::Center).children(controls),
                    Button::new()
                        .grid_column(3)
                        .style(ButtonStyle::Subtle)
                        .automation_name(format!("More for {}", server.label))
                        .content(theme::glyph("\u{E712}", 14.0))
                        .menu(Menu::new(items, hooks.on_menu.clone())),
                ))
                .into()
        })
        .collect();
    if rows.is_empty() {
        rows.push(caption("This PC is not paired with a server yet.").into());
    }
    let add = {
        let on_menu = hooks.on_menu.clone();
        Button::new().horizontal_alignment(HorizontalAlignment::Left).content(TextBlock::new().text("Add Server…")).on_click(move || on_menu.call("env-add:".into()))
    };
    vec![
        caption("Server names and order are saved on this PC. Choose Configure to edit settings stored on a specific server.").into(),
        card(rows),
        add.into(),
    ]
}
