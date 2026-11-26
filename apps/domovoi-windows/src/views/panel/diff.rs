//! The Diff tab: which change is shown, then each file's lines, folded or open.

use domovoi_core::Event;
use domovoi_core::view::DiffFileView;
use domovoi_desktop_host::palette;
use windows_reactor::{
    Border, Button, ButtonStyle, Grid, GridLength, HorizontalAlignment, Orientation, PointerEventInfo, ScrollBarVisibility, ScrollViewer, StackPanel, TextBlock,
    TextTrimming, TextWrapping, Thickness, TooltipExt as _, VerticalAlignment, View,
};

use super::{Act, Shown, caption, mono, nothing, tell_icon};
use crate::core_link::CoreLink;
use crate::theme;

/// The most lines of one file drawn; the rest are counted.
const LINES: usize = 400;

pub fn view(core: &CoreLink, shown: &Shown<'_>) -> View {
    let diff = &shown.panel.diff;
    let mut sources: Vec<View> = Vec::new();
    for source in &diff.sources {
        let (core, value) = (core.clone(), source.value.clone());
        sources.push(
            Button::new()
                .style(if source.value == diff.selected_source { ButtonStyle::Accent } else { ButtonStyle::Default })
                .content(TextBlock::new().text(&source.label).font_size(theme::CAPTION))
                .on_click(move || core.send(Event::SelectDiffSource { source: value.clone() }))
                .into(),
        );
    }
    let top = Grid::new().columns([GridLength::STAR, GridLength::Auto]).margin(Thickness::new(10.0, 8.0, 6.0, 8.0)).children((
        ScrollViewer::new()
            .horizontal_scroll_bar_visibility(ScrollBarVisibility::Hidden)
            .vertical_scroll_bar_visibility(ScrollBarVisibility::Disabled)
            .content(StackPanel::new().orientation(Orientation::Horizontal).spacing(4.0).children(sources)),
        tell_icon(core, "\u{E72C}", "Refresh the diff", Event::SelectDiffSource { source: diff.selected_source.clone() }).grid_column(1).tooltip("Refresh"),
    ));

    let body: View = if diff.loading {
        nothing("\u{E895}", "Loading the diff…", "The changes are on their way from the server.", None)
    } else if let Some(error) = &diff.error {
        nothing("\u{E783}", "No diff to show", error, None)
    } else if diff.files.is_empty() {
        nothing("\u{E73E}", "No changes", "Nothing differs here.", None)
    } else {
        let (added, removed) = (palette::project_rgb("green", shown.dark), palette::project_rgb("red", shown.dark));
        let files = diff.files.len();
        let mut lines: Vec<View> = vec![
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(8.0)
                .children(vec![
                    caption(&format!("{files} file{}", if files == 1 { "" } else { "s" })).into(),
                    mono(&format!("+{}", diff.files.iter().map(|f| f.additions).sum::<u32>())).foreground(theme::rgb(added)).into(),
                    mono(&format!("−{}", diff.files.iter().map(|f| f.deletions).sum::<u32>())).foreground(theme::rgb(removed)).into(),
                    caption("· a line's number quotes the line in the message").into(),
                ])
                .into(),
        ];
        lines.extend(diff.files.iter().map(|file| file_card(core, file, shown)));
        if diff.truncated {
            lines.push(caption("The diff is longer than what is shown here.").into());
        }
        ScrollViewer::new()
            .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
            .horizontal_scroll_bar_visibility(ScrollBarVisibility::Disabled)
            .content(StackPanel::new().orientation(Orientation::Vertical).spacing(8.0).margin(Thickness::new(10.0, 0.0, 14.0, 10.0)).children(lines))
            .into()
    };
    Grid::new().rows([GridLength::Auto, GridLength::STAR]).children((top, Border::new().grid_row(1).content(body))).into()
}

/// One file: its path and counts on a button that folds it, then its lines.
fn file_card(core: &CoreLink, file: &DiffFileView, shown: &Shown<'_>) -> View {
    let open = !shown.state.folded.contains(&file.path);
    let (added, removed) = (palette::project_rgb("green", shown.dark), palette::project_rgb("red", shown.dark));
    let title = match &file.old_path {
        Some(old) => format!("{old} → {}", file.path),
        None => file.path.clone(),
    };
    let mut cells: Vec<View> = vec![
        theme::glyph(if open { "\u{E70D}" } else { "\u{E76C}" }, 9.0).opacity(0.6).into(),
        mono(&title).grid_column(1).text_trimming(TextTrimming::CharacterEllipsis).vertical_alignment(VerticalAlignment::Center).into(),
    ];
    if file.change != "modified" {
        cells.push(caption(&file.change).grid_column(2).vertical_alignment(VerticalAlignment::Center).into());
    }
    cells.push(mono(&format!("+{}", file.additions)).grid_column(3).foreground(theme::rgb(added)).vertical_alignment(VerticalAlignment::Center).into());
    cells.push(mono(&format!("−{}", file.deletions)).grid_column(4).foreground(theme::rgb(removed)).vertical_alignment(VerticalAlignment::Center).into());
    let (on_act, path) = (shown.on_act.clone(), file.path.clone());
    let head = Button::new()
        .style(ButtonStyle::Subtle)
        .horizontal_alignment(HorizontalAlignment::Stretch)
        .horizontal_content_alignment(HorizontalAlignment::Stretch)
        .automation_name(format!("{} {title}", if open { "Fold" } else { "Unfold" }))
        .content(Grid::new().columns([GridLength::Auto, GridLength::STAR, GridLength::Auto, GridLength::Auto, GridLength::Auto]).column_spacing(8.0).children(cells))
        .on_click(move || on_act.call(Act::Fold(path.clone())));
    let mut parts: Vec<View> = vec![head.into()];
    if open {
        parts.push(lines(core, file, shown.dark));
    }
    Border::new().background(theme::code()).corner_radius(6.0).content(StackPanel::new().orientation(Orientation::Vertical).children(parts)).into()
}

/// A file's lines with their numbers before and after the change. A long line scrolls sideways.
fn lines(core: &CoreLink, file: &DiffFileView, dark: bool) -> View {
    let (added, removed) = (palette::project_rgb("green", dark), palette::project_rgb("red", dark));
    let number = |n: Option<u32>| n.map_or_else(|| "    ".to_owned(), |n| format!("{n:>4}"));
    let mut rows: Vec<View> = Vec::new();
    let mut drawn = 0;
    let total: usize = file.hunks.iter().map(|hunk| hunk.lines.len()).sum();
    'hunks: for hunk in &file.hunks {
        if !hunk.header.is_empty() {
            rows.push(mono(&hunk.header).opacity(0.55).margin(Thickness::new(8.0, 4.0, 8.0, 2.0)).into());
        }
        for line in &hunk.lines {
            if drawn == LINES {
                rows.push(caption(&format!("… {} more lines", total - drawn)).margin(Thickness::xy(8.0, 4.0)).into());
                break 'hunks;
            }
            drawn += 1;
            let (sign, colour) = match line.kind.as_str() {
                "add" => ("+", Some(added)),
                "del" => ("−", Some(removed)),
                _ => (" ", None),
            };
            let text = mono(&format!("{sign} {}", line.text)).grid_column(1).text_wrapping(TextWrapping::NoWrap);
            // The numbers quote the line into the message, as a place and what is there.
            let quote = format!("`{}:{}`: `{}`", file.path, line.new_line.or(line.old_line).unwrap_or(0), line.text);
            let core = core.clone();
            let numbers = Border::new()
                .on_pointer_pressed(move |_: PointerEventInfo| core.send(Event::AppendToDraft { text: quote.clone() }))
                .content(mono(&format!("{} {}", number(line.old_line), number(line.new_line))).opacity(0.45));
            let row = Grid::new().columns([GridLength::Auto, GridLength::STAR]).column_spacing(8.0).children((
                numbers,
                match (colour, line.kind.as_str()) {
                    (Some(colour), _) => text.foreground(theme::rgb(colour)),
                    (None, "note") => text.opacity(0.55),
                    (None, _) => text.opacity(0.9),
                },
            ));
            let row = Border::new().padding(Thickness::xy(8.0, 0.0)).content(row);
            rows.push(match colour {
                Some(colour) => row.background(theme::tint(colour, dark)).into(),
                None => row.into(),
            });
        }
    }
    ScrollViewer::new()
        .horizontal_scroll_bar_visibility(ScrollBarVisibility::Auto)
        .vertical_scroll_bar_visibility(ScrollBarVisibility::Disabled)
        .content(StackPanel::new().orientation(Orientation::Vertical).margin(Thickness::new(0.0, 0.0, 0.0, 8.0)).children(rows))
        .into()
}
