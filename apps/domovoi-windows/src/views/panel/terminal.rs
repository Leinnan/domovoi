//! The Terminal tab: the thread's terminals and the screen of the one shown.
//!
//! The server runs the terminal and sends its screen as lines of coloured
//! pieces. Here the lines are drawn as text, and keys, typed characters, the
//! wheel and the size that fits go back as events.

use std::cell::Cell;

use domovoi_core::Event;
use domovoi_core::view::{TerminalLineView, TerminalScreenView, TerminalSpanView};
use domovoi_desktop_host::palette;
use windows_reactor::{
    Border, Button, ButtonStyle, FocusEventInfo, FontWeight, Grid, GridLength, HorizontalAlignment, InputModifiers, Orientation, PointerEventInfo, ScrollBarVisibility,
    ScrollViewer, StackPanel, TextBlock, TextWrapping, Thickness, TooltipExt as _, VerticalAlignment, View, VirtualKey,
};

use super::{Act, Hooks, Shown, caption, does_icon, nothing, tell, tell_icon};
use crate::core_link::CoreLink;
use crate::{theme, widgets};

/// A character's box. The width is a little over what the font takes, so a
/// full line never runs past the panel's edge.
const CELL_WIDTH: f64 = 7.3;
const CELL_HEIGHT: f64 = 16.0;
const FONT_SIZE: f64 = 12.0;
/// What the panel has around the screen: its tabs, the terminals' row, the screen's own edges.
const ABOVE: f64 = 96.0;
const AROUND: f64 = 24.0;

/// The rows and columns that fit a panel `width` by `height`.
pub fn grid(width: f64, height: f64) -> (u32, u32) {
    let rows = ((height - ABOVE - AROUND) / CELL_HEIGHT).floor().clamp(2.0, 300.0);
    let cols = ((width - AROUND) / CELL_WIDTH).floor().clamp(10.0, 500.0);
    (rows as u32, cols as u32)
}

/// The screen as text, without the spaces that end its lines and the empty lines that end it.
pub fn plain_text(screen: &TerminalScreenView) -> String {
    let mut rows: Vec<String> = screen.lines.iter().map(|line| line.spans.iter().map(|span| span.text.as_str()).collect::<String>().trim_end().to_owned()).collect();
    while rows.last().is_some_and(String::is_empty) {
        rows.pop();
    }
    rows.join("\n")
}

/// What a key does in the terminal, when it does more than type a character
/// (those come as typed, `typed`): the named keys, Ctrl and Alt with a
/// letter, copying and pasting.
pub fn key(key: VirtualKey, modifiers: InputModifiers) -> Option<Act> {
    let held = |modifier| modifiers.contains(modifier);
    let (ctrl, shift, alt) = (held(InputModifiers::CONTROL), held(InputModifiers::SHIFT), held(InputModifiers::ALT));
    const C: VirtualKey = VirtualKey(0x43);
    const V: VirtualKey = VirtualKey(0x56);
    match (key, ctrl, shift, alt) {
        (C, true, true, false) => return Some(Act::CopyScreen),
        (V, true, _, false) | (VirtualKey::INSERT, false, true, false) => return Some(Act::Paste),
        _ => {}
    }
    let named = match key {
        VirtualKey::ENTER => "enter",
        VirtualKey::TAB if shift => "backtab",
        VirtualKey::TAB => "tab",
        VirtualKey::BACK => "backspace",
        VirtualKey::ESCAPE => "escape",
        VirtualKey::UP => "up",
        VirtualKey::DOWN => "down",
        VirtualKey::LEFT => "left",
        VirtualKey::RIGHT => "right",
        VirtualKey::HOME => "home",
        VirtualKey::END => "end",
        VirtualKey::PAGE_UP => "pageup",
        VirtualKey::PAGE_DOWN => "pagedown",
        VirtualKey::DELETE => "delete",
        _ => "",
    };
    if !named.is_empty() {
        return Some(Act::Key { key: named, text: String::new(), ctrl, alt });
    }
    // Ctrl or Alt with a letter or digit. Both together is AltGr, which types a character.
    let character = char::from_u32(key.0).filter(|c| c.is_ascii_uppercase() || c.is_ascii_digit())?;
    (ctrl != alt).then(|| Act::Key { key: "char", text: if shift && alt { character.to_string() } else { character.to_ascii_lowercase().to_string() }, ctrl, alt })
}

/// A typed character for the terminal. One outside the basic plane comes as
/// two halves; the first waits in `half` for the second.
pub fn typed(unit: u16, modifiers: InputModifiers, half: &Cell<Option<u16>>) -> Option<Act> {
    let (ctrl, alt) = (modifiers.contains(InputModifiers::CONTROL), modifiers.contains(InputModifiers::ALT));
    if (0xD800..0xDC00).contains(&unit) {
        half.set(Some(unit));
        return None;
    }
    let units = match half.take() {
        Some(first) if (0xDC00..0xE000).contains(&unit) => vec![first, unit],
        _ => vec![unit],
    };
    let text = String::from_utf16(&units).ok()?;
    // Keys with a name and Ctrl or Alt with a letter were taken as keys (`key`).
    if text.chars().any(char::is_control) || ctrl != alt {
        return None;
    }
    Some(Act::Key { key: "char", text, ctrl: false, alt: false })
}

/// How a piece of a line is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Look {
    ink: u32,
    fill: Option<u32>,
    bold: bool,
    underline: bool,
}

/// The screen's own colours: its text and what is behind it.
fn base(dark: bool) -> (u32, u32) {
    if dark { (0xD8D8D8, 0x16181C) } else { (0x24292F, 0xF6F6F4) }
}

fn look(span: &TerminalSpanView, dark: bool) -> Look {
    let (text, behind) = base(dark);
    let (mut ink, mut fill) = (palette::xterm_rgb(span.fg), palette::xterm_rgb(span.bg));
    if span.inverse {
        (ink, fill) = (Some(fill.unwrap_or(behind)), Some(ink.unwrap_or(text)));
    }
    Look { ink: ink.unwrap_or(text), fill, bold: span.bold, underline: span.underline }
}

/// A line as pieces of one look each. With the cursor on it (its column, and
/// whether the terminal has the keyboard), the character there is drawn as
/// the cursor; past the line's end the cursor is a space of its own.
fn pieces(line: &TerminalLineView, cursor: Option<(u32, bool)>, dark: bool) -> Vec<(String, Look)> {
    let mut cells: Vec<(char, Look)> = line.spans.iter().flat_map(|span| span.text.chars().map(move |c| (c, look(span, dark)))).collect();
    if let Some((column, focused)) = cursor {
        let (text, behind) = base(dark);
        let column = column as usize;
        while cells.len() <= column {
            cells.push((' ', Look { ink: text, fill: None, bold: false, underline: false }));
        }
        let accent = palette::project_rgb("blue", dark);
        cells[column].1 = if focused { Look { ink: behind, fill: Some(accent), bold: false, underline: false } } else { Look { ink: text, fill: None, bold: false, underline: true } };
    }
    let mut out: Vec<(String, Look)> = Vec::new();
    for (c, look) in cells {
        // A space that does not break keeps its place in a line that is never wrapped.
        let c = if c == ' ' { '\u{A0}' } else { c };
        match out.last_mut() {
            Some((text, last)) if *last == look => text.push(c),
            _ => out.push((c.to_string(), look)),
        }
    }
    for (text, _) in &mut out {
        widgets::keep_last_space(text);
    }
    out
}

fn line(line: &TerminalLineView, cursor: Option<(u32, bool)>, dark: bool) -> View {
    let cells = pieces(line, cursor, dark).into_iter().map(|(text, look)| {
        let piece = TextBlock::new()
            .text(text)
            .font_family(theme::MONO)
            .font_size(FONT_SIZE)
            .font_weight(if look.bold { FontWeight::BOLD } else { FontWeight::NORMAL })
            .foreground(theme::rgb(look.ink))
            .text_wrapping(TextWrapping::NoWrap);
        match (look.fill, look.underline) {
            (None, false) => piece.into(),
            (fill, underline) => {
                let boxed = Border::new().content(piece);
                let boxed = if let Some(fill) = fill { boxed.background(theme::rgb(fill)) } else { boxed };
                if underline { boxed.border_brush(theme::rgb(look.ink)).border_thickness(Thickness::new(0.0, 0.0, 0.0, 1.0)).into() } else { boxed.into() }
            }
        }
    });
    StackPanel::new().orientation(Orientation::Horizontal).height(CELL_HEIGHT).children(cells.collect::<Vec<View>>()).into()
}

pub fn view(core: &CoreLink, shown: &Shown<'_>, hooks: Hooks<'_>) -> View {
    let terminal = &shown.panel.terminal;
    let mut tabs: Vec<View> = Vec::new();
    for tab in &terminal.tabs {
        let (core_select, core_close, id, id_close) = (core.clone(), core.clone(), tab.id.clone(), tab.id.clone());
        let running = palette::project_rgb(if tab.running { "green" } else { "gray" }, shown.dark);
        let cells: Vec<View> = vec![
            Border::new().width(6.0).height(6.0).corner_radius(3.0).background(theme::rgb(running)).vertical_alignment(VerticalAlignment::Center).margin(Thickness::new(8.0, 0.0, 0.0, 0.0)).into(),
            Button::new()
                .style(ButtonStyle::Subtle)
                .automation_name(format!("Terminal {}", tab.label))
                .content(TextBlock::new().text(&tab.label).font_size(theme::CAPTION + 1.0).font_weight(if tab.active { FontWeight::SEMI_BOLD } else { FontWeight::NORMAL }))
                .on_click(move || core_select.send(Event::SelectTerminal { terminal_id: id.clone() }))
                .into(),
            Button::new()
                .style(ButtonStyle::Subtle)
                .automation_name(format!("Close terminal {}", tab.label))
                .content(theme::glyph("\u{E711}", 9.0).opacity(0.7))
                .on_click(move || core_close.send(Event::CloseTerminal { terminal_id: id_close.clone() }))
                .tooltip("Close the terminal"),
        ];
        tabs.push(Border::new().corner_radius(4.0).background(if tab.active { theme::selected() } else { theme::clear() }).content(StackPanel::new().orientation(Orientation::Horizontal).children(cells)).into());
    }
    let has = terminal.active.is_some();
    let actions: Vec<View> = vec![
        does_icon(shown.on_act, "\u{E8C8}", "Copy the screen", Act::CopyScreen).is_enabled(has).tooltip("Copy the screen (Ctrl+Shift+C)"),
        does_icon(shown.on_act, "\u{E77F}", "Paste into the terminal", Act::Paste).is_enabled(has).tooltip("Paste (Ctrl+V)"),
        tell_icon(core, "\u{E75C}", "Clear the terminal", Event::ClearTerminal).is_enabled(has).tooltip("Clear the screen and its history"),
        tell_icon(core, "\u{E710}", "New terminal", Event::NewTerminal).is_enabled(shown.thread_id.is_some()).tooltip("New terminal"),
    ];
    let top = Grid::new().columns([GridLength::STAR, GridLength::Auto]).margin(Thickness::new(8.0, 4.0, 6.0, 4.0)).children((
        ScrollViewer::new()
            .horizontal_scroll_bar_visibility(ScrollBarVisibility::Hidden)
            .vertical_scroll_bar_visibility(ScrollBarVisibility::Disabled)
            .content(StackPanel::new().orientation(Orientation::Horizontal).spacing(4.0).children(tabs)),
        StackPanel::new().grid_column(1).orientation(Orientation::Horizontal).children(actions),
    ));
    let body: View = match &terminal.active {
        Some(active) => screen(core, active, shown, hooks),
        None => {
            let open = shown.thread_id.is_some();
            nothing(
                "\u{E756}",
                "No terminal",
                if open { "A terminal opens in the thread's workspace." } else { "Open a thread to use terminals." },
                open.then(|| tell(core, "New Terminal", Event::NewTerminal).style(ButtonStyle::Accent).horizontal_alignment(HorizontalAlignment::Center).into()),
            )
        }
    };
    Grid::new().rows([GridLength::Auto, GridLength::STAR]).children((top, Border::new().grid_row(1).content(body))).into()
}

/// The screen: its lines, and over them how far back it looks and that its process ended.
fn screen(core: &CoreLink, screen: &TerminalScreenView, shown: &Shown<'_>, hooks: Hooks<'_>) -> View {
    let focused = shown.state.terminal_focused;
    // Looking back into the history, the cursor is not on the screen.
    let live = screen.cursor_visible && screen.scrollback_offset == 0;
    let rows = screen.lines.iter().enumerate().map(|(row, text)| line(text, (live && row as u32 == screen.cursor_row).then_some((screen.cursor_col, focused)), shown.dark));
    let mut layers: Vec<View> = vec![StackPanel::new().orientation(Orientation::Vertical).children(rows.collect::<Vec<View>>()).into()];
    if screen.scrollback_offset > 0 {
        let back = screen.scrollback_offset;
        layers.push(
            tell(core, &format!("{back} of {} lines back · Jump to live", screen.scrollback_max.max(back)), Event::TerminalScroll { lines: -(back as i32) })
                .horizontal_alignment(HorizontalAlignment::Center)
                .vertical_alignment(VerticalAlignment::Bottom)
                .into(),
        );
    }
    if screen.exited {
        let said = if screen.status.is_empty() { "Process exited".to_owned() } else { format!("Process exited · {}", screen.status) };
        layers.push(caption(&said).horizontal_alignment(HorizontalAlignment::Right).vertical_alignment(VerticalAlignment::Bottom).into());
    }
    let (_, behind) = base(shown.dark);
    let (on_click, on_wheel, on_focus, on_blur) = (shown.on_act.clone(), shown.on_act.clone(), shown.on_act.clone(), shown.on_act.clone());
    Border::new()
        .element_ref(hooks.terminal)
        .is_tab_stop(true)
        .automation_name(format!("Terminal screen, {} rows by {} columns", screen.rows, screen.cols))
        .margin(Thickness::new(8.0, 0.0, 8.0, 8.0))
        .padding(Thickness::uniform(4.0))
        .corner_radius(4.0)
        .background(theme::rgb(behind))
        .border_brush(if focused { theme::accent() } else { theme::stroke() })
        .border_thickness(Thickness::uniform(1.0))
        .on_preview_key_down(hooks.on_keys)
        .on_character_received(hooks.on_chars)
        .on_pointer_pressed(move |_: PointerEventInfo| on_click.call(Act::Clicked))
        .on_pointer_wheel_changed(move |info: PointerEventInfo| on_wheel.call(Act::Wheel(info.wheel_delta / 120)))
        .on_got_focus(move |_: FocusEventInfo| on_focus.call(Act::Focused(true)))
        .on_lost_focus(move |_: FocusEventInfo| on_blur.call(Act::Focused(false)))
        .content(Grid::new().children(layers))
        .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn with(modifiers: &[InputModifiers]) -> InputModifiers {
        let mut all = InputModifiers::NONE;
        for modifier in modifiers {
            all |= *modifier;
        }
        all
    }

    fn sent(act: Option<Act>) -> Option<(&'static str, String, bool, bool)> {
        match act {
            Some(Act::Key { key, text, ctrl, alt }) => Some((key, text, ctrl, alt)),
            _ => None,
        }
    }

    #[test]
    fn named_keys_and_ctrl_letters_are_keys_and_a_plain_letter_is_typed() {
        let none = InputModifiers::NONE;
        let ctrl = with(&[InputModifiers::CONTROL]);
        assert_eq!(sent(key(VirtualKey::ENTER, none)), Some(("enter", String::new(), false, false)));
        assert_eq!(sent(key(VirtualKey::TAB, with(&[InputModifiers::SHIFT]))), Some(("backtab", String::new(), false, false)));
        assert_eq!(sent(key(VirtualKey(0x43), ctrl)), Some(("char", "c".into(), true, false)));
        assert_eq!(sent(key(VirtualKey(0x42), with(&[InputModifiers::ALT]))), Some(("char", "b".into(), false, true)));
        assert!(key(VirtualKey(0x41), none).is_none(), "a letter comes as typed");
        assert!(key(VirtualKey(0x51), with(&[InputModifiers::CONTROL, InputModifiers::ALT])).is_none(), "AltGr types a character");
        assert!(matches!(key(VirtualKey(0x43), with(&[InputModifiers::CONTROL, InputModifiers::SHIFT])), Some(Act::CopyScreen)));
        assert!(matches!(key(VirtualKey(0x56), ctrl), Some(Act::Paste)));
    }

    #[test]
    fn typed_characters_come_whole_and_what_a_key_took_is_not_typed_again() {
        let half = Cell::new(None);
        let none = InputModifiers::NONE;
        assert_eq!(sent(typed(u16::from(b'a'), none, &half)), Some(("char", "a".into(), false, false)));
        assert_eq!(sent(typed(0x105, with(&[InputModifiers::CONTROL, InputModifiers::ALT]), &half)), Some(("char", "ą".into(), false, false)));
        assert!(typed(0x0D, none, &half).is_none(), "Enter was a key");
        assert!(typed(0x03, with(&[InputModifiers::CONTROL]), &half).is_none(), "Ctrl+C was a key");
        // U+1F600 as its two halves.
        assert!(typed(0xD83D, none, &half).is_none());
        assert_eq!(sent(typed(0xDE00, none, &half)), Some(("char", "😀".into(), false, false)));
    }

    fn span(text: &str) -> TerminalSpanView {
        TerminalSpanView { text: text.into(), fg: -1, bg: -1, ..TerminalSpanView::default() }
    }

    #[test]
    fn pieces_of_one_look_join_and_the_cursor_is_a_piece_of_its_own() {
        let text = TerminalLineView { spans: vec![span("ab"), span("c d")] };
        let joined = pieces(&text, None, true);
        assert_eq!(joined.len(), 1);
        assert_eq!(joined[0].0, "abc\u{A0}d");
        let with_cursor = pieces(&text, Some((1, true)), true);
        assert_eq!(with_cursor.iter().map(|(text, _)| text.as_str()).collect::<Vec<_>>(), ["a", "b", "c\u{A0}d"]);
        assert!(with_cursor[1].1.fill.is_some());
        // Past the line's end the cursor still shows.
        let past = pieces(&text, Some((7, true)), true);
        assert_eq!(past.last().map(|(text, look)| (text.as_str(), look.fill.is_some())), Some(("\u{A0}\u{2060}", true)));
    }

    #[test]
    fn an_inverse_piece_swaps_its_colours_and_the_size_fits_the_panel() {
        let mut inverse = span("x");
        inverse.inverse = true;
        let (text, behind) = base(true);
        assert_eq!(look(&inverse, true), Look { ink: behind, fill: Some(text), bold: false, underline: false });
        let (rows, cols) = grid(440.0, 760.0);
        assert_eq!((rows, cols), (40, 56));
        assert_eq!(grid(10.0, 10.0), (2, 10));
    }

    #[test]
    fn the_screen_copies_without_the_space_that_ends_it() {
        let screen = TerminalScreenView { lines: vec![TerminalLineView { spans: vec![span("$ ls  ")] }, TerminalLineView { spans: vec![span("a  b")] }, TerminalLineView { spans: vec![span("   ")] }], ..TerminalScreenView::default() };
        assert_eq!(plain_text(&screen), "$ ls\na  b");
    }
}
