//! The message box under the thread: what is typed goes to the core as the
//! draft; Send (or Ctrl+Enter) sends it, Stop ends the turn under way.
//!
//! The box shows the window's own copy of the draft, not the core's: the
//! core's comes back a moment after each key, and setting a text box to it
//! would move the caret. The core's replaces it only when the core itself
//! changed the draft (`draft_revision`: sent, restored, a slash command).

use std::rc::Rc;

use domovoi_core::view::ComposerView;
use windows_reactor::{
    AcceleratorKey, AcceleratorModifiers, Border, Button, ButtonStyle, Callback, Grid, GridLength, KeyAccelerator, KeyAccelerators, Orientation, StackPanel,
    TextBlock, TextBox, TextWrapping, Thickness, VerticalAlignment, View,
};

use crate::theme;

pub struct Hooks {
    pub on_draft: Callback<Rc<str>>,
    pub on_send: Callback<()>,
    pub on_stop: Callback<()>,
}

/// `draft` is the window's copy of what is typed.
pub fn composer(composer: &ComposerView, draft: &str, hooks: Hooks) -> View {
    let can_send = composer.can_send && !composer.sending;
    let send = {
        let on_send = hooks.on_send.clone();
        Button::new()
            .style(ButtonStyle::Accent)
            .automation_name("Send")
            .is_enabled(can_send)
            .vertical_alignment(VerticalAlignment::Bottom)
            .content(theme::glyph("\u{E724}", 14.0))
            .key_accelerators(KeyAccelerators::new([KeyAccelerator::new(AcceleratorKey::Enter, AcceleratorModifiers::Control, move || on_send.call(()))]))
            .on_click(move || hooks.on_send.call(()))
    };
    let mut buttons: Vec<View> = Vec::new();
    if composer.can_stop {
        let on_stop = hooks.on_stop;
        buttons.push(Button::new().automation_name("Stop").vertical_alignment(VerticalAlignment::Bottom).content(theme::glyph("\u{E71A}", 14.0)).on_click(move || on_stop.call(())).into());
    }
    buttons.push(send.into());
    let placeholder = if composer.placeholder.is_empty() { "Message the agent (Ctrl+Enter sends)" } else { composer.placeholder.as_str() };
    let model = composer.selected_model.as_ref().map(|m| m.label.clone()).unwrap_or_default();
    let note = [composer.blocked_reason.as_str(), composer.send_hint.as_str()].into_iter().find(|s| !s.is_empty()).map_or(model, str::to_owned);
    Border::new()
        .margin(Thickness::new(theme::GAP * 2.0, 0.0, theme::GAP * 2.0, theme::GAP * 2.0))
        .padding(Thickness::uniform(8.0))
        .background(theme::card())
        .border_brush(theme::stroke())
        .border_thickness(Thickness::uniform(1.0))
        .corner_radius(theme::RADIUS)
        .content(
            Grid::new().rows([GridLength::STAR, GridLength::Auto]).children((
                Grid::new().columns([GridLength::STAR, GridLength::Auto]).column_spacing(8.0).children((
                    TextBox::new()
                        .automation_name("Message")
                        .placeholder_text(placeholder)
                        .accepts_return(true)
                        .text_wrapping(TextWrapping::Wrap)
                        .min_height(44.0)
                        .max_height(220.0)
                        .text(draft)
                        .on_text_changed(hooks.on_draft),
                    StackPanel::new().grid_column(1).orientation(Orientation::Horizontal).spacing(6.0).vertical_alignment(VerticalAlignment::Bottom).children(buttons),
                )),
                TextBlock::new().grid_row(1).text(note).text_wrapping(TextWrapping::WrapWholeWords).font_size(theme::CAPTION).opacity(0.6).margin(Thickness::new(4.0, 6.0, 0.0, 0.0)),
            )),
        )
        .into()
}
