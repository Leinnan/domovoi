//! The command palette: a search over commands, threads and projects, the
//! workspace's files, or what the files hold. The core finds and runs; here
//! the search is typed and a row is picked.

use std::rc::Rc;

use domovoi_core::Event;
use domovoi_core::view::PaletteView;
use windows_reactor::{
    Border, Button, ButtonStyle, Callback, Color, ElementRef, FontWeight, Grid, GridLength, HorizontalAlignment, KeyEventInfo, Orientation, PointerEventInfo,
    RoutedCallback, ScrollBarVisibility, ScrollViewer, StackPanel, TextBlock, TextBox, TextTrimming, Thickness, VerticalAlignment, View, VirtualKey,
};

use crate::core_link::CoreLink;
use crate::theme;

/// The most rows drawn.
const ROWS: usize = 40;

/// What the palette needs of the window.
pub struct Hooks<'a> {
    /// The search as typed (the window's own copy), and its box.
    pub query: &'a str,
    pub field: &'a ElementRef<TextBox>,
    pub on_query: Callback<Rc<str>>,
    /// The arrows, Enter and Escape in the search.
    pub on_keys: RoutedCallback<KeyEventInfo>,
}

/// What a key in the search tells the core, if anything: the arrows move the
/// mark, Enter runs the marked row, Escape shuts the palette.
pub fn key(key: VirtualKey) -> Option<Event> {
    Some(match key {
        VirtualKey::DOWN => Event::MovePalette { delta: 1 },
        VirtualKey::UP => Event::MovePalette { delta: -1 },
        // No row named: the core runs the one it has marked.
        VirtualKey::ENTER => Event::RunPaletteItem { id: String::new() },
        VirtualKey::ESCAPE => Event::ClosePalette,
        _ => return None,
    })
}

/// A shortcut as this keyboard has it. The core writes them with the Mac's
/// signs, in the Mac's order.
fn keys(shortcut: &str) -> String {
    let (mut ctrl, mut alt, mut shift, mut key) = (false, false, false, String::new());
    for c in shortcut.chars() {
        match c {
            '⌘' | '⌃' => ctrl = true,
            '⌥' => alt = true,
            '⇧' => shift = true,
            c => key.push(c),
        }
    }
    [ctrl.then_some("Ctrl"), alt.then_some("Alt"), shift.then_some("Shift"), (!key.is_empty()).then_some(key.as_str())].into_iter().flatten().collect::<Vec<_>>().join("+")
}

fn mode_title(mode: &str) -> &'static str {
    match mode {
        "files" => "Go to File",
        "content" => "Search in Project",
        _ => "Commands",
    }
}

fn caption(text: &str) -> TextBlock {
    TextBlock::new().text(text).font_size(theme::CAPTION).opacity(0.65)
}

/// The palette over the whole window: a shade that shuts it when pressed, and its card.
pub fn palette(core: &CoreLink, palette: &PaletteView, hooks: Hooks<'_>) -> View {
    let title = mode_title(&palette.mode);
    let placeholder = if palette.placeholder.is_empty() { "Search commands, threads and files" } else { palette.placeholder.as_str() };
    let mut top: Vec<View> = vec![
        theme::glyph("\u{E721}", 14.0).opacity(0.6).into(),
        Border::new()
            .grid_column(1)
            .on_preview_key_down(hooks.on_keys)
            .content(TextBox::new().element_ref(hooks.field).automation_name("Search the palette").placeholder_text(placeholder).text(hooks.query).on_text_changed(hooks.on_query))
            .into(),
    ];
    if palette.searching {
        top.push(caption("Searching…").grid_column(2).vertical_alignment(VerticalAlignment::Center).into());
    }

    let mut rows: Vec<View> = Vec::new();
    if palette.items.is_empty() {
        let said = if palette.searching {
            "Searching…".to_owned()
        } else if palette.query.is_empty() {
            format!("Type to search: {title}")
        } else {
            format!("Nothing matches “{}”", palette.query)
        };
        rows.push(caption(&said).margin(Thickness::xy(8.0, 12.0)).into());
    }
    for (index, item) in palette.items.iter().take(ROWS).enumerate() {
        let marked = index == palette.highlighted as usize;
        let glyph = match item.kind.as_str() {
            "thread" => "\u{E8BD}",
            "project" => "\u{E8B7}",
            "file" => "\u{E8A5}",
            "match" => "\u{E721}",
            _ => "\u{E756}",
        };
        let name = TextBlock::new().text(&item.title).text_trimming(TextTrimming::CharacterEllipsis);
        let mut about: Vec<View> = vec![if item.kind == "match" { name.font_family(theme::MONO).font_size(theme::CAPTION + 0.5).into() } else { name.into() }];
        if !item.subtitle.is_empty() {
            about.push(caption(&item.subtitle).text_trimming(TextTrimming::CharacterEllipsis).into());
        }
        let mut cells: Vec<View> = vec![
            theme::glyph(glyph, 13.0).opacity(if marked { 1.0 } else { 0.6 }).into(),
            StackPanel::new().grid_column(1).orientation(Orientation::Vertical).children(about).into(),
        ];
        if !item.shortcut.is_empty() {
            cells.push(caption(&keys(&item.shortcut)).grid_column(2).vertical_alignment(VerticalAlignment::Center).into());
        }
        let (core, id) = (core.clone(), item.id.clone());
        let button = Button::new()
            .style(ButtonStyle::Subtle)
            .horizontal_alignment(HorizontalAlignment::Stretch)
            .horizontal_content_alignment(HorizontalAlignment::Stretch)
            .automation_name(&item.title)
            .content(Grid::new().columns([GridLength::Auto, GridLength::STAR, GridLength::Auto]).column_spacing(10.0).children(cells))
            .on_click(move || core.send(Event::RunPaletteItem { id: id.clone() }));
        rows.push(Border::new().corner_radius(4.0).background(if marked { theme::selected() } else { theme::clear() }).content(button).into());
    }

    // Any mode the palette does not know by name searches the commands.
    let current = if matches!(palette.mode.as_str(), "files" | "content") { palette.mode.as_str() } else { "commands" };
    let mode = |label: &str, to: &str| -> View {
        let (core, to, on) = (core.clone(), to.to_owned(), to == current);
        Button::new()
            .style(if on { ButtonStyle::Default } else { ButtonStyle::Subtle })
            .content(TextBlock::new().text(label).font_size(theme::CAPTION).font_weight(if on { FontWeight::SEMI_BOLD } else { FontWeight::NORMAL }))
            .on_click(move || core.send(Event::SetPaletteMode { mode: to.clone() }))
            .into()
    };
    let foot = Grid::new().grid_row(2).columns([GridLength::STAR, GridLength::Auto]).children((
        StackPanel::new().orientation(Orientation::Horizontal).spacing(2.0).children(vec![mode("Commands", "commands"), mode("Files", "files"), mode("Content", "content")]),
        caption("↑↓ move  ·  Enter runs  ·  Esc closes").grid_column(1).vertical_alignment(VerticalAlignment::Center),
    ));

    let card = Border::new()
        .width(620.0)
        .max_height(500.0)
        .horizontal_alignment(HorizontalAlignment::Center)
        .vertical_alignment(VerticalAlignment::Top)
        .margin(Thickness::new(16.0, 72.0, 16.0, 16.0))
        .padding(Thickness::uniform(12.0))
        .corner_radius(theme::RADIUS)
        .background(theme::solid())
        .border_brush(theme::stroke())
        .border_thickness(Thickness::uniform(1.0))
        .content(Grid::new().rows([GridLength::Auto, GridLength::STAR, GridLength::Auto]).row_spacing(8.0).children((
            Grid::new().columns([GridLength::Auto, GridLength::STAR, GridLength::Auto]).column_spacing(8.0).children(top),
            ScrollViewer::new()
                .grid_row(1)
                .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
                .horizontal_scroll_bar_visibility(ScrollBarVisibility::Disabled)
                .content(StackPanel::new().orientation(Orientation::Vertical).margin(Thickness::new(0.0, 0.0, 10.0, 0.0)).children(rows)),
            foot,
        )));
    let shut = core.clone();
    let shade = Border::new().background(Color::argb(0x66, 0x10, 0x10, 0x10)).on_pointer_pressed(move |_: PointerEventInfo| shut.send(Event::ClosePalette));
    Grid::new().children((shade, card)).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_arrows_move_enter_runs_and_escape_shuts() {
        assert!(matches!(key(VirtualKey::DOWN), Some(Event::MovePalette { delta: 1 })));
        assert!(matches!(key(VirtualKey::UP), Some(Event::MovePalette { delta: -1 })));
        assert!(matches!(key(VirtualKey::ENTER), Some(Event::RunPaletteItem { id }) if id.is_empty()));
        assert!(matches!(key(VirtualKey::ESCAPE), Some(Event::ClosePalette)));
        assert!(key(VirtualKey::TAB).is_none());
        assert_eq!(keys("⇧⌘N"), "Ctrl+Shift+N");
        assert_eq!(keys("⌥⌘G"), "Ctrl+Alt+G");
        assert_eq!(keys("Ctrl+K"), "Ctrl+K");
    }
}
