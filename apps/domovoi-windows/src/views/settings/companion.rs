//! Settings: a server's companion, the small face beside its name. Each
//! server has its own look, made from the server's id until it is changed
//! here; a change is told to the core at once.

use domovoi_core::Event;
use domovoi_core::companion::CompanionLook;
use domovoi_desktop_host::palette;
use windows_reactor::{
    Border, Button, ButtonStyle, ComboBox, Grid, GridLength, HorizontalAlignment, Image, Orientation, StackPanel, Stretch, TextBlock, Thickness, TooltipExt as _, View,
};

use super::general::{row, switch};
use super::{Shown, active_server, caption, card, heading};
use crate::companion::{self, Mood};
use crate::core_link::CoreLink;
use crate::theme;

const EYES: [(&str, &str); 4] = [("round", "Round"), ("oval", "Oval"), ("pill", "Tall"), ("sleepy", "Sleepy")];
const EARS: [(&str, &str); 3] = [("none", "None"), ("round", "Round"), ("pointy", "Pointy")];
const PUPILS: [(&str, &str); 3] = [("small", "Small"), ("medium", "Medium"), ("large", "Large")];

pub fn companion(core: &CoreLink, shown: &Shown<'_>) -> Vec<View> {
    let dark = shown.icons.dark;
    let server = active_server(shown.connection);
    // With no server saved yet, the look of the one being connected to is shown and cannot be changed.
    let look = server.map_or(&shown.connection.companion.look, |s| &s.companion.look);
    let name = server.map_or("this server", |s| s.label.as_str());
    let mut out: Vec<View> = vec![
        caption(&format!("Saved on this PC only — customizes how {name} looks here. It perks up while agents work, looks alert when one waits for you, and sleeps without a connection.")).into(),
    ];

    // How it looks at rest and in each mood.
    let face = |mood: Mood, size: f64, name: &str| -> View {
        StackPanel::new()
            .orientation(Orientation::Vertical)
            .spacing(4.0)
            .children(vec![
                Image::new().source_svg(companion::image(look, mood, dark)).stretch(Stretch::Uniform).width(size).height(size).into(),
                caption(name).horizontal_alignment(HorizontalAlignment::Center).into(),
            ])
            .into()
    };
    out.push(card(vec![
        StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(20.0)
            .horizontal_alignment(HorizontalAlignment::Center)
            .children(vec![
                face(Mood::Idle, 96.0, "At rest"),
                face(Mood::Working, 56.0, "Working"),
                face(Mood::Attention, 56.0, "Waiting for you"),
                face(Mood::Finished, 56.0, "Finished"),
                face(Mood::Failed, 56.0, "Failed"),
                face(Mood::Offline, 56.0, "Offline"),
            ])
            .into(),
    ]));
    let Some(server) = server else { return out };

    // A change: the look with one thing different.
    let set = |change: &dyn Fn(&mut CompanionLook)| -> Event {
        let mut next = look.clone();
        change(&mut next);
        Event::SetCompanion { id: server.id.clone(), look: next }
    };
    let dots: Vec<View> = palette::PROJECT_KEYS
        .iter()
        .enumerate()
        .map(|(index, key)| {
            let (core, event) = (core.clone(), set(&|next| next.body = (*key).to_owned()));
            let chosen = look.body == *key;
            Button::new()
                .grid_column((index % 9) as i32)
                .grid_row((index / 9) as i32)
                .style(ButtonStyle::Subtle)
                .automation_name(format!("Body colour {key}"))
                .content(
                    Border::new()
                        .width(22.0)
                        .height(22.0)
                        .corner_radius(11.0)
                        .background(theme::rgb(palette::project_rgb(key, dark)))
                        .border_brush(if chosen { theme::primary() } else { theme::stroke() })
                        .border_thickness(Thickness::uniform(if chosen { 3.0 } else { 1.0 })),
                )
                .on_click(move || core.send(event.clone()))
                .tooltip(*key)
        })
        .collect();
    let choice = |name: &str, options: &[(&str, &str)], current: &str, change: &dyn Fn(&mut CompanionLook, &str)| -> View {
        let chosen = options.iter().position(|(value, _)| *value == current);
        let events: Vec<Event> = options.iter().map(|(value, _)| set(&|next| change(next, value))).collect();
        let core = core.clone();
        ComboBox::new()
            .automation_name(name)
            .min_width(140.0)
            .items_source(options.iter().map(|(_, label)| (*label).to_owned()))
            .selected_index(chosen)
            .on_selection_changed(move |index: Option<usize>| {
                // Set from the look too: only a choice that differs is a change.
                if let Some(event) = index.filter(|i| Some(*i) != chosen).and_then(|i| events.get(i)) {
                    core.send(event.clone());
                }
            })
            .into()
    };
    let toggle = |name: &str, on: bool, change: &dyn Fn(&mut CompanionLook)| -> View {
        let (core, event) = (core.clone(), set(change));
        switch(on)
            .automation_name(name)
            .on_toggled(move |to: bool| {
                if to != on {
                    core.send(event.clone());
                }
            })
            .into()
    };
    out.push(heading("Look"));
    out.push(card(vec![
        row(
            "Body colour",
            "",
            Grid::new().columns(vec![GridLength::Auto; 9]).rows([GridLength::Auto, GridLength::Auto]).horizontal_alignment(HorizontalAlignment::Left).children(dots).into(),
        ),
        row("Eyes", "", choice("Eye shape", &EYES, &look.eye_shape, &|next, value| next.eye_shape = value.to_owned())),
        row("Pupils", "", choice("Pupil size", &PUPILS, &look.pupil_size, &|next, value| next.pupil_size = value.to_owned())),
        row("Ears", "", choice("Ears", &EARS, &look.ears, &|next, value| next.ears = value.to_owned())),
        row("Antenna", "", toggle("Antenna", look.antenna, &|next| next.antenna = !next.antenna)),
        row("Eyebrows", "", toggle("Eyebrows", look.eyebrows, &|next| next.eyebrows = !next.eyebrows)),
    ]));
    if server.companion.custom {
        let (core, id) = (core.clone(), server.id.clone());
        out.push(
            Button::new()
                .horizontal_alignment(HorizontalAlignment::Left)
                .content(TextBlock::new().text("Use the Server's Own Look"))
                .on_click(move || core.send(Event::ResetCompanion { id: id.clone() }))
                .tooltip("The look made from the server's id, as every device sees it at first"),
        );
    }
    out
}
