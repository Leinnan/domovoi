//! Pairing: the server on this computer when the core offers it (one press,
//! no code), then a `domovoi://` link (from `domovoi-server pair`, its QR
//! code or the server's tray menu), or a server URL and one-time code on
//! this network.

use domovoi_core::view::{ConnectionView, LocalServerView};
use windows_reactor::{
    Border, Button, ButtonStyle, Callback, FontWeight, Grid, GridLength, HorizontalAlignment, InfoBar, InfoBarSeverity, Orientation, ProgressRing, ScrollBarVisibility,
    ScrollViewer, StackPanel, TextBlock, TextBox, TextWrapping, Thickness, VerticalAlignment, View,
};

use crate::theme;

/// What the pairing form tells the window.
pub struct Hooks {
    pub on_connect_local: Callback<()>,
    pub on_link: Callback<std::rc::Rc<str>>,
    pub on_url: Callback<std::rc::Rc<str>>,
    pub on_code: Callback<std::rc::Rc<str>>,
    pub on_pair_link: Callback<()>,
    pub on_pair_code: Callback<()>,
}

/// The form, centred on the window, under the card for the server on this
/// computer when there is one. `draft` is what was typed so far.
pub fn pairing(connection: &ConnectionView, draft: &Draft, hooks: Hooks) -> View {
    let busy = matches!(connection.state.as_str(), "pairing" | "connecting") || connection.attempting;
    let mut cards: Vec<View> = Vec::new();
    if let Some(local) = &connection.local_server {
        cards.push(local_server(local, busy, hooks.on_connect_local));
    }
    let heading = |text: &str| TextBlock::new().text(text).font_size(theme::BODY).font_weight(FontWeight::SEMI_BOLD).margin(Thickness::new(0.0, 12.0, 0.0, 4.0));
    let mut lines: Vec<View> = vec![
        TextBlock::new().text("Connect to a Domovoi server").font_size(theme::TITLE).font_weight(FontWeight::SEMI_BOLD).into(),
        TextBlock::new()
            .text("Run `domovoi-server pair` on the machine your agents work on, then paste the link it prints. On the same network a server address and its code work too.")
            .text_wrapping(TextWrapping::WrapWholeWords)
            .opacity(0.75)
            .into(),
        heading("Pairing link").into(),
        TextBox::new().automation_name("Pairing link").placeholder_text("domovoi://connect?ticket=…").text(&draft.link).on_text_changed(hooks.on_link).into(),
        Button::new()
            .style(ButtonStyle::Accent)
            .is_enabled(!busy && draft.link.trim().starts_with("domovoi://"))
            .content(TextBlock::new().text("Connect"))
            .on_click(move || hooks.on_pair_link.call(()))
            .into(),
        heading("Or a server on this network").into(),
        TextBox::new().automation_name("Server address").placeholder_text("http://192.168.1.20:13773").text(&draft.url).on_text_changed(hooks.on_url).into(),
        TextBox::new().automation_name("Pairing code").placeholder_text("Pairing code").text(&draft.code).on_text_changed(hooks.on_code).into(),
        Button::new()
            .is_enabled(!busy && !draft.url.trim().is_empty())
            .content(TextBlock::new().text("Connect with code"))
            .on_click(move || hooks.on_pair_code.call(()))
            .into(),
    ];
    if busy {
        lines.push(
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(8.0)
                .children(vec![ProgressRing::new().is_active(true).width(16.0).height(16.0).into(), TextBlock::new().text("Connecting…").into()])
                .into(),
        );
    }
    if let Some(error) = connection.error.as_ref().filter(|e| !e.is_empty()) {
        let message = if connection.hint.is_empty() { error.clone() } else { format!("{error}\n{}", connection.hint) };
        lines.push(InfoBar::new().is_open(true).is_closable(false).severity(InfoBarSeverity::Error).title("Couldn't connect").message(message).into());
    }
    cards.push(card(Thickness::uniform(24.0), lines));
    // Both cards and an error are taller than the smallest window: they scroll.
    ScrollViewer::new()
        .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
        .horizontal_scroll_bar_visibility(ScrollBarVisibility::Disabled)
        .content(
            StackPanel::new()
                .orientation(Orientation::Vertical)
                .spacing(theme::GAP)
                .horizontal_alignment(HorizontalAlignment::Center)
                .vertical_alignment(VerticalAlignment::Center)
                .max_width(WIDTH)
                .margin(Thickness::uniform(theme::GAP))
                .children(cards),
        )
        .into()
}

/// The cards' width.
const WIDTH: f64 = 520.0;

/// The server on this computer, which the app signs in to with no pairing
/// code: what and where it is, and the button that connects.
fn local_server(local: &LocalServerView, busy: bool, on_connect: Callback<()>) -> View {
    let lines: Vec<View> = vec![
        TextBlock::new().text("Server on this computer").font_size(theme::BODY).font_weight(FontWeight::SEMI_BOLD).into(),
        Grid::new()
            .columns([GridLength::STAR, GridLength::Auto])
            .column_spacing(12.0)
            .children((
                StackPanel::new().orientation(Orientation::Vertical).spacing(2.0).vertical_alignment(VerticalAlignment::Center).children(vec![
                    TextBlock::new().text(format!("{} · {}", local.label, local.url)).text_wrapping(TextWrapping::Wrap).into(),
                    TextBlock::new().text("No pairing code needed.").font_size(theme::CAPTION).opacity(0.65).into(),
                ]),
                Button::new()
                    .grid_column(1)
                    .style(ButtonStyle::Accent)
                    .automation_name("Connect to the server on this computer")
                    .is_enabled(!busy)
                    .vertical_alignment(VerticalAlignment::Center)
                    .content(TextBlock::new().text("Connect"))
                    .on_click(move || on_connect.call(())),
            ))
            .into(),
    ];
    card(Thickness::new(24.0, 16.0, 24.0, 16.0), lines)
}

/// A card as wide as the column, with `lines` one under the other.
fn card(padding: Thickness, lines: Vec<View>) -> View {
    Border::new()
        .padding(padding)
        .background(theme::card())
        .border_brush(theme::stroke())
        .border_thickness(Thickness::uniform(1.0))
        .corner_radius(theme::RADIUS)
        .content(StackPanel::new().orientation(Orientation::Vertical).spacing(8.0).children(lines))
        .into()
}

/// What the pairing form holds while it is filled in.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Draft {
    pub link: String,
    pub url: String,
    pub code: String,
}
