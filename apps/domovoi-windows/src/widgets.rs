//! Pieces more than one view draws.

use domovoi_core::view::{CodeSpan, DiffFileView, LimitWindowView, ProjectIconView, ToastView};
use domovoi_desktop_host::palette;
use windows_reactor::{
    Border, Button, ButtonStyle, Callback, FontWeight, Grid, GridLength, HorizontalAlignment, Image, Orientation, ProgressBar, StackPanel, Stretch, TextBlock, TextWrapping,
    Thickness,
    TooltipExt as _, VerticalAlignment, View,
};

use crate::media::Media;
use crate::{icons, theme, time};

/// A project's icon, `size` on each side: its picture when it has one,
/// else its letters, emoji or symbol on a tile tinted with its colour (as
/// the other clients draw it).
pub fn project_icon_tile(icon: &ProjectIconView, size: f64, media: &Media, dark: bool) -> View {
    let tile = Border::new().width(size).height(size).corner_radius(size * 0.25).vertical_alignment(VerticalAlignment::Center);
    if let Some(picture) = media.project_icon(icon) {
        return tile.content(Image::new().source_data(picture).stretch(Stretch::UniformToFill).width(size).height(size)).into();
    }
    let rgb = palette::project_rgb(&icon.color, dark);
    let inner: View = match icon.kind.as_str() {
        "emoji" => TextBlock::new()
            .text(&icon.text)
            .font_size(size * 0.56)
            .horizontal_alignment(HorizontalAlignment::Center)
            .vertical_alignment(VerticalAlignment::Center)
            .into(),
        "symbol" => Image::new()
            .source_svg(icons::symbol(&icon.symbol, rgb))
            .stretch(Stretch::Uniform)
            .width(size * 0.58)
            .height(size * 0.58)
            .horizontal_alignment(HorizontalAlignment::Center)
            .vertical_alignment(VerticalAlignment::Center)
            .into(),
        _ => TextBlock::new()
            .text(&icon.text)
            .font_family(theme::MONO)
            .font_weight(FontWeight::BOLD)
            .font_size((size * 0.42).max(9.0))
            .foreground(theme::rgb(rgb))
            .horizontal_alignment(HorizontalAlignment::Center)
            .vertical_alignment(VerticalAlignment::Center)
            .into(),
    };
    tile.background(theme::tint(rgb, dark)).content(inner).into()
}

/// What the core has to say: an error, a success or a note, with the button
/// it offers ("Undo") and how many more wait behind it.
pub fn toast(toast: &ToastView, on_action: Callback<()>, on_dismiss: Callback<()>) -> View {
    let (glyph, brush) = match toast.kind.as_str() {
        "error" => ("\u{E783}", theme::critical()),
        "success" => ("\u{E73E}", theme::accent()),
        _ => ("\u{E946}", theme::accent()),
    };
    let mut cells: Vec<View> = vec![
        theme::glyph(glyph, 16.0).foreground(brush).into(),
        TextBlock::new().grid_column(1).text(&toast.message).text_wrapping(TextWrapping::WrapWholeWords).vertical_alignment(VerticalAlignment::Center).is_text_selection_enabled(true).into(),
    ];
    if toast.queued > 0 {
        cells.push(TextBlock::new().grid_column(2).text(format!("+{} more", toast.queued)).font_size(theme::CAPTION).opacity(0.65).vertical_alignment(VerticalAlignment::Center).into());
    }
    if !toast.action_label.is_empty() {
        cells.push(Button::new().grid_column(3).content(TextBlock::new().text(&toast.action_label)).on_click(move || on_action.call(())).into());
    }
    cells.push(Button::new().grid_column(4).style(ButtonStyle::Subtle).automation_name("Dismiss").content(theme::glyph("\u{E711}", 12.0)).on_click(move || on_dismiss.call(())).tooltip("Dismiss"));
    Border::new()
        .margin(Thickness::new(theme::GAP * 2.0, theme::GAP, theme::GAP * 2.0, 0.0))
        .padding(Thickness::new(12.0, 6.0, 6.0, 6.0))
        .background(theme::card())
        .border_brush(theme::stroke())
        .border_thickness(Thickness::uniform(1.0))
        .corner_radius(theme::RADIUS)
        .content(Grid::new().columns([GridLength::Auto, GridLength::STAR, GridLength::Auto, GridLength::Auto, GridLength::Auto]).column_spacing(10.0).children(cells))
        .into()
}

/// A file's change: its path and counts, then its lines, added ones and
/// removed ones in their colours. At most `cap` lines; the rest are counted.
pub fn diff_file(file: &DiffFileView, dark: bool, cap: usize) -> View {
    let (added, removed) = (palette::project_rgb("green", dark), palette::project_rgb("red", dark));
    let title = match (&file.old_path, file.change.as_str()) {
        (Some(old), "renamed") => format!("{old} → {}", file.path),
        (_, "added") => format!("{} (new)", file.path),
        (_, "deleted") => format!("{} (deleted)", file.path),
        _ => file.path.clone(),
    };
    let mono = |text: &str| TextBlock::new().text(text).font_family(theme::MONO).font_size(theme::CAPTION).text_wrapping(TextWrapping::NoWrap);
    let mut lines: Vec<View> = vec![
        Grid::new()
            .columns([GridLength::STAR, GridLength::Auto])
            .column_spacing(8.0)
            .children((
                mono(&title).font_weight(FontWeight::SEMI_BOLD).text_trimming(windows_reactor::TextTrimming::CharacterEllipsis),
                StackPanel::new().grid_column(1).orientation(Orientation::Horizontal).spacing(6.0).children(vec![
                    mono(&format!("+{}", file.additions)).foreground(theme::rgb(added)).into(),
                    mono(&format!("−{}", file.deletions)).foreground(theme::rgb(removed)).into(),
                ]),
            ))
            .into(),
    ];
    let all = file.hunks.iter().flat_map(|hunk| std::iter::once((hunk.header.as_str(), "note")).chain(hunk.lines.iter().map(|line| (line.text.as_str(), line.kind.as_str()))));
    let total = file.hunks.iter().map(|hunk| hunk.lines.len() + 1).sum::<usize>();
    for (text, kind) in all.take(cap) {
        let line = match kind {
            "add" => Border::new().background(theme::tint(added, dark)).content(mono(&format!("+ {text}")).foreground(theme::rgb(added))),
            "del" => Border::new().background(theme::tint(removed, dark)).content(mono(&format!("− {text}")).foreground(theme::rgb(removed))),
            "note" => Border::new().content(mono(text).opacity(0.55)),
            _ => Border::new().content(mono(&format!("  {text}")).opacity(0.85)),
        };
        lines.push(line.into());
    }
    if total > cap {
        lines.push(mono(&format!("… {} more lines", total - cap)).opacity(0.55).into());
    }
    Border::new()
        .padding(Thickness::uniform(8.0))
        .background(theme::code())
        .corner_radius(4.0)
        .content(StackPanel::new().orientation(Orientation::Vertical).children(lines))
        .into()
}

/// What a usage window says under its bar: when it refills (with the clock
/// time, when the provider gave one), whether it is used faster than time
/// passes, and how far through its period it is.
pub fn limit_details(window: &LimitWindowView) -> String {
    let resets = match window.resets.as_str() {
        "" => String::new(),
        "now" => "Resets now".to_owned(),
        resets => match time::clock_of(&window.resets_at) {
            Some(clock) => format!("Resets {resets} · {clock}"),
            None => format!("Resets {resets}"),
        },
    };
    let pace = match window.pace.as_str() {
        "ahead" => "Ahead of pace",
        "on" => "On pace",
        "under" => "Under pace",
        _ => "",
    };
    let gone = if window.elapsed_percent > 0 { format!("{}% of the period gone", window.elapsed_percent) } else { String::new() };
    [resets.as_str(), pace, gone.as_str()].into_iter().filter(|part| !part.is_empty()).collect::<Vec<_>>().join(" · ")
}

/// A provider account's usage windows: each one's name, how much of it is used as a bar, and `limit_details`.
pub fn limit_windows(windows: &[LimitWindowView], dark: bool) -> View {
    let rows = windows.iter().map(|window| {
        // Used faster than time passes and nearly gone: worth a warning's colour.
        let warn = window.high && window.pace == "ahead";
        let ink = theme::rgb(palette::project_rgb(if window.percent >= 90 { "red" } else { "amber" }, dark));
        let percent = TextBlock::new().grid_column(1).text(format!("{}%", window.percent)).font_family(theme::MONO).font_size(theme::CAPTION + 1.0);
        let mut lines: Vec<View> = vec![
            Grid::new()
                .columns([GridLength::STAR, GridLength::Auto])
                .children((TextBlock::new().text(&window.label).font_size(theme::CAPTION + 1.0), if window.high { percent.foreground(ink.clone()) } else { percent.opacity(0.7) }))
                .into(),
            ProgressBar::new().minimum(0.0).maximum(100.0).value(f64::from(window.percent.min(100))).into(),
        ];
        let details = limit_details(window);
        if !details.is_empty() {
            let line = TextBlock::new().text(details).font_size(theme::CAPTION);
            lines.push(if warn { line.foreground(ink).into() } else { line.opacity(0.65).into() });
        }
        StackPanel::new().orientation(Orientation::Vertical).spacing(3.0).children(lines).into()
    });
    StackPanel::new().orientation(Orientation::Vertical).spacing(10.0).children(rows.collect::<Vec<View>>()).into()
}

/// One of the line drawings (`icons`), `size` on each side, in the colour text has on the window.
pub fn line_icon(name: &str, size: f64, dark: bool) -> Image {
    Image::new().source_svg(icons::symbol(name, if dark { 0xE4E4E4 } else { 0x2B2B2B })).stretch(Stretch::Uniform).width(size).height(size)
}

/// The most lines of code drawn in colours; longer code is plain.
const COLOURED_LINES: usize = 300;

/// The colour a piece of highlighted code is drawn in (`None`: as the text is).
fn span_colour(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "keyword" => "violet",
        "string" | "added" => "green",
        "comment" => "gray",
        "number" => "orange",
        "type" => "teal",
        "function" => "blue",
        "removed" => "red",
        _ => return None,
    })
}

/// Highlighted code as lines of pieces, each piece within one line.
fn code_pieces(spans: &[CodeSpan]) -> Vec<Vec<(&str, &str)>> {
    let mut out: Vec<Vec<(&str, &str)>> = vec![Vec::new()];
    for span in spans {
        for (index, piece) in span.text.split('\n').enumerate() {
            if index > 0 {
                out.push(Vec::new());
            }
            if let Some(line) = out.last_mut().filter(|_| !piece.is_empty()) {
                line.push((piece, span.kind.as_str()));
            }
        }
    }
    // Code that ends in a newline has no empty last line.
    if out.len() > 1 && out.last().is_some_and(Vec::is_empty) {
        out.pop();
    }
    out
}

/// A text block gives no width to the space that ends its text, so pieces
/// set side by side would close up. A word joiner after it (no width, not a
/// space) keeps the space.
pub fn keep_last_space(text: &mut String) {
    if text.ends_with([' ', '\t', '\u{A0}']) {
        text.push('\u{2060}');
    }
}

fn code_piece(text: &str) -> TextBlock {
    let mut text = text.to_owned();
    keep_last_space(&mut text);
    TextBlock::new().text(text).font_family(theme::MONO).font_size(theme::CAPTION + 0.5)
}

/// Highlighted code line by line, each piece in its colour. `None` when it
/// is not highlighted or too long to draw piece by piece. A line does not
/// wrap: whoever places it lets it scroll sideways.
pub fn code_lines(spans: &[CodeSpan], dark: bool) -> Option<View> {
    let lines = code_pieces(spans);
    if spans.is_empty() || lines.len() > COLOURED_LINES {
        return None;
    }
    let rows = lines.iter().map(|line| {
        let pieces = line.iter().map(|(text, kind)| match span_colour(kind) {
            Some(key) => code_piece(text).foreground(theme::rgb(palette::project_rgb(key, dark))).into(),
            None => code_piece(text).into(),
        });
        // An empty line still takes a line's height.
        let pieces: Vec<View> = if line.is_empty() { vec![code_piece(" ").into()] } else { pieces.collect() };
        StackPanel::new().orientation(Orientation::Horizontal).children(pieces).into()
    });
    Some(StackPanel::new().orientation(Orientation::Vertical).children(rows.collect::<Vec<View>>()).into())
}

/// Code as it is shown where it scrolls both ways: in its colours, or (not
/// highlighted, or long) as plain text that can be selected.
pub fn code_text(spans: &[CodeSpan], text: &str, dark: bool) -> View {
    code_lines(spans, dark).unwrap_or_else(|| code_piece(text).text_wrapping(TextWrapping::NoWrap).is_text_selection_enabled(true).into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_says_when_it_refills_and_how_it_is_paced() {
        let window = LimitWindowView { resets: "in 3h".into(), pace: "ahead".into(), elapsed_percent: 40, ..LimitWindowView::default() };
        assert_eq!(limit_details(&window), "Resets in 3h · Ahead of pace · 40% of the period gone");
        assert_eq!(limit_details(&LimitWindowView { resets: "now".into(), ..LimitWindowView::default() }), "Resets now");
        assert_eq!(limit_details(&LimitWindowView::default()), "");
    }

    #[test]
    fn highlighted_code_is_cut_into_its_lines() {
        let span = |text: &str, kind: &str| CodeSpan { text: text.into(), kind: kind.into() };
        let spans = vec![span("fn", "keyword"), span(" main() {\n    ", "plain"), span("// hi", "comment"), span("\n\n}\n", "plain")];
        let got = code_pieces(&spans);
        assert_eq!(got, vec![vec![("fn", "keyword"), (" main() {", "plain")], vec![("    ", "plain"), ("// hi", "comment")], vec![], vec![("}", "plain")]]);
    }
}
