//! The question a `domovoi://` link from outside the app raises before
//! anything connects: what it asks, what identifies the server, then Cancel
//! and the accepting button. It lies over whatever the window shows, on a
//! scrim that dims it.
//!
//! Only the two buttons answer it. A press beside the panel lands on the
//! scrim, which has no handler and keeps it from what is underneath, so a
//! link that arrives mid-click is neither accepted nor dropped.

use domovoi_core::view::LinkRequestView;
use windows_reactor::{
    Border, Button, ButtonStyle, Callback, FontWeight, Grid, GridLength, HorizontalAlignment, Orientation, StackPanel, TextBlock, TextWrapping, Thickness,
    VerticalAlignment, View,
};

use crate::theme;

/// The panel's width.
const WIDTH: f64 = 460.0;
/// The label column's width, wide enough for "Name in the link".
const LABEL: f64 = 120.0;

/// The dialog and its scrim, to lay over the window's rows. `on_answer`
/// takes the request's `id` and whether the user accepted.
pub fn link_dialog(request: &LinkRequestView, on_answer: Callback<(u32, bool)>) -> View {
    let mut facts: Vec<View> =
        vec![fact("Server key", TextBlock::new().text(format!("{}…", request.endpoint)).font_family(theme::MONO).is_text_selection_enabled(true))];
    if let Some((label, name)) = name_line(request) {
        facts.push(fact(label, TextBlock::new().text(name)));
    }
    let id = request.id;
    let (on_cancel, on_confirm) = (on_answer.clone(), on_answer);
    let lines: Vec<View> = vec![
        TextBlock::new().text(&request.title).font_size(theme::TITLE).font_weight(FontWeight::SEMI_BOLD).text_wrapping(TextWrapping::WrapWholeWords).into(),
        TextBlock::new().text(&request.message).text_wrapping(TextWrapping::WrapWholeWords).opacity(0.75).into(),
        Border::new()
            .padding(Thickness::new(12.0, 8.0, 12.0, 8.0))
            .background(theme::code())
            .corner_radius(theme::RADIUS)
            .content(StackPanel::new().orientation(Orientation::Vertical).spacing(6.0).children(facts))
            .into(),
        StackPanel::new()
            .orientation(Orientation::Horizontal)
            .spacing(8.0)
            .horizontal_alignment(HorizontalAlignment::Right)
            .margin(Thickness::new(0.0, 4.0, 0.0, 0.0))
            .children(vec![
                Button::new().content(TextBlock::new().text("Cancel")).on_click(move || on_cancel.call((id, false))).into(),
                Button::new().style(ButtonStyle::Accent).content(TextBlock::new().text(&request.confirm_label)).on_click(move || on_confirm.call((id, true))).into(),
            ])
            .into(),
    ];
    let panel = Border::new()
        .horizontal_alignment(HorizontalAlignment::Center)
        .vertical_alignment(VerticalAlignment::Center)
        .width(WIDTH)
        .padding(Thickness::uniform(24.0))
        .background(theme::panel())
        .border_brush(theme::stroke())
        .border_thickness(Thickness::uniform(1.0))
        .corner_radius(theme::RADIUS)
        .content(StackPanel::new().orientation(Orientation::Vertical).spacing(12.0).children(lines));
    // The title bar's row and the body's: the scrim covers both.
    Border::new().grid_row_span(2).background(theme::scrim()).content(panel).into()
}

/// The name under the key, and what to call it. The saved name is this
/// app's own. The link's is whatever its author wrote, so it shows only for
/// a server not saved here, and says whose it is.
fn name_line(request: &LinkRequestView) -> Option<(&'static str, &str)> {
    if !request.known_label.is_empty() {
        Some(("Saved here as", request.known_label.as_str()))
    } else if !request.link_name.is_empty() {
        Some(("Name in the link", request.link_name.as_str()))
    } else {
        None
    }
}

/// One thing that identifies the server: what it is, then its value.
fn fact(label: &str, value: TextBlock) -> View {
    Grid::new()
        .columns([GridLength::Pixel(LABEL), GridLength::STAR])
        .column_spacing(12.0)
        .children((
            TextBlock::new().text(label).font_size(theme::CAPTION).opacity(0.65).vertical_alignment(VerticalAlignment::Center),
            value.grid_column(1).font_size(theme::CAPTION).text_wrapping(TextWrapping::Wrap).vertical_alignment(VerticalAlignment::Center),
        ))
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(known_label: &str, link_name: &str) -> LinkRequestView {
        LinkRequestView { known_label: known_label.into(), link_name: link_name.into(), ..LinkRequestView::default() }
    }

    #[test]
    fn a_saved_server_shows_its_saved_name_not_the_links() {
        assert_eq!(name_line(&request("Studio", "Your bank")), Some(("Saved here as", "Studio")));
    }

    #[test]
    fn a_server_not_saved_shows_the_links_name_as_the_links() {
        assert_eq!(name_line(&request("", "Studio")), Some(("Name in the link", "Studio")));
        assert_eq!(name_line(&request("", "")), None);
    }
}
