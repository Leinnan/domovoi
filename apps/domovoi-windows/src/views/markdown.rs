//! Markdown as the core hands it over, block by block: text with its bold,
//! italic and links, code in its colours, lists, quotes, tables, and Mermaid
//! diagrams the server drew.
//!
//! Reactor's rich text knows bold, italic and links only. Inline code keeps
//! its backticks, struck text is struck with combining strokes, and a link
//! into the workspace is plain text until the files panel can open it.

use domovoi_core::view::{MarkdownBlock, MarkdownRun};
use windows_reactor::{
    Border, Button, ButtonStyle, FontWeight, Grid, GridLength, HorizontalAlignment, Image, Orientation, RichText, RichTextBlock, RichTextHyperlink, RichTextInline,
    RichTextRun, ScrollBarVisibility, ScrollViewer, StackPanel, Stretch, TextBlock, TextWrapping, Thickness, TooltipExt as _, VerticalAlignment, View,
};

use crate::views::transcript::{Act, Draw, diagram_key};
use crate::{theme, widgets};

/// Whether the rich text can follow `link` (it opens web pages only).
fn opens(link: &str) -> bool {
    (link.starts_with("https://") || link.starts_with("http://")) && !link.contains(char::is_whitespace)
}

/// `text` struck through, where no run can be: a stroke over each character.
fn struck(text: &str) -> String {
    text.chars().flat_map(|c| [c, '\u{0336}']).collect()
}

/// Runs as the rich text's inlines: `(text, bold, italic, link)` each.
fn inlines(runs: &[MarkdownRun], fallback: &str, force_bold: bool) -> Vec<(String, bool, bool, Option<String>)> {
    if runs.is_empty() {
        return vec![(fallback.to_owned(), force_bold, false, None)];
    }
    runs.iter()
        .map(|run| {
            let text = if run.code {
                format!("`{}`", run.text)
            } else if run.strike {
                struck(&run.text)
            } else {
                run.text.clone()
            };
            (text, force_bold || run.bold, run.italic, run.link.clone().filter(|link| opens(link)))
        })
        .collect()
}

/// A paragraph of runs.
fn rich(runs: &[MarkdownRun], fallback: &str, force_bold: bool, size: f64) -> RichTextBlock {
    let inlines = inlines(runs, fallback, force_bold).into_iter().map(|(text, is_bold, is_italic, link)| match link {
        Some(uri) => RichTextInline::Hyperlink(RichTextHyperlink { text, uri }),
        None => RichTextInline::Run(RichTextRun { text, is_bold, is_italic }),
    });
    RichTextBlock::new().paragraphs(RichText::single_paragraph(inlines)).font_size(size).text_wrapping(TextWrapping::WrapWholeWords).is_text_selection_enabled(true)
}

fn mono(text: &str) -> TextBlock {
    TextBlock::new().text(text).font_family(theme::MONO).font_size(theme::CAPTION + 0.5)
}

/// A code block: its language and a copy button, then the code.
fn code(block: &MarkdownBlock, draw: &Draw<'_>) -> View {
    // A line of pieces cannot wrap: a long one scrolls sideways.
    let body: View = match widgets::code_lines(&block.spans, draw.dark) {
        Some(lines) => ScrollViewer::new().horizontal_scroll_bar_visibility(ScrollBarVisibility::Auto).vertical_scroll_bar_visibility(ScrollBarVisibility::Disabled).content(lines).into(),
        None => mono(&block.text).text_wrapping(TextWrapping::Wrap).is_text_selection_enabled(true).into(),
    };
    let copy = {
        let (on_act, text) = (draw.on_act.clone(), block.text.clone());
        Button::new().grid_column(1).style(ButtonStyle::Subtle).automation_name("Copy the code").content(theme::glyph("\u{E8C8}", 12.0)).on_click(move || on_act.call(Act::Copy(text.clone()))).tooltip("Copy")
    };
    Border::new()
        .padding(Thickness::new(10.0, 4.0, 4.0, 10.0))
        .background(theme::code())
        .corner_radius(6.0)
        .content(Grid::new().columns([GridLength::STAR, GridLength::Auto]).rows([GridLength::Auto, GridLength::Auto]).children((
            TextBlock::new().text(&block.language).font_size(theme::CAPTION).opacity(0.55).vertical_alignment(VerticalAlignment::Center),
            copy,
            Border::new().grid_row(1).grid_column_span(2).content(body),
        )))
        .into()
}

/// A finished Mermaid block: the server's drawing once it has come, else its source.
fn diagram(block: &MarkdownBlock, draw: &Draw<'_>) -> View {
    let key = diagram_key(&block.text, draw.dark);
    let drawn = draw.diagrams.iter().find(|d| d.key == key);
    if let Some(picture) = drawn.and_then(|d| d.url.as_deref()).and_then(|url| draw.media.image(url)) {
        return Border::new()
            .horizontal_alignment(HorizontalAlignment::Left)
            .corner_radius(6.0)
            .content(Image::new().source_data(picture).stretch(Stretch::Uniform).max_width(720.0).max_height(520.0))
            .into();
    }
    let note = match drawn {
        Some(d) if d.failed => "The server could not draw this diagram.",
        Some(d) if d.offline => "The diagram is drawn once the server is back.",
        _ => "Drawing the diagram…",
    };
    StackPanel::new().orientation(Orientation::Vertical).spacing(4.0).children(vec![TextBlock::new().text(note).font_size(theme::CAPTION).opacity(0.6).into(), code(block, draw)]).into()
}

/// The rows of a table, cell under cell.
fn table(rows: &[MarkdownBlock], size: f64) -> View {
    let columns = rows.iter().map(|row| row.cells.len().max(row.cell_runs.len())).max().unwrap_or(0);
    let mut cells: Vec<View> = Vec::new();
    for (at, row) in rows.iter().enumerate() {
        for column in 0..columns {
            let text = row.cells.get(column).map_or("", String::as_str);
            let runs = row.cell_runs.get(column).map_or(&[][..], Vec::as_slice);
            cells.push(
                Border::new()
                    .grid_row(at as i32)
                    .grid_column(column as i32)
                    .padding(Thickness::xy(10.0, 5.0))
                    .border_brush(theme::stroke())
                    .border_thickness(Thickness::new(0.0, 0.0, 0.0, if row.header { 2.0 } else { 1.0 }))
                    .content(rich(runs, text, row.header, size).max_width(420.0).horizontal_alignment(match row.alignments.get(column).map(String::as_str) {
                        // The cell is as wide as its column: its text sits where the column is aligned.
                        Some("right") => HorizontalAlignment::Right,
                        Some("center") => HorizontalAlignment::Center,
                        _ => HorizontalAlignment::Left,
                    }))
                    .into(),
            );
        }
    }
    // A table wider than the page scrolls sideways.
    ScrollViewer::new()
        .horizontal_scroll_bar_visibility(ScrollBarVisibility::Auto)
        .vertical_scroll_bar_visibility(ScrollBarVisibility::Disabled)
        .content(Grid::new().horizontal_alignment(HorizontalAlignment::Left).columns(vec![GridLength::Auto; columns]).rows(vec![GridLength::Auto; rows.len()]).children(cells))
        .into()
}

/// Something made to look at (a page, a drawing): what it is, by name.
fn artifact(block: &MarkdownBlock) -> View {
    let kind = match block.artifact_kind.as_str() {
        "react" => "Interactive React",
        "svg" => "SVG graphic",
        "mermaid" => "Mermaid diagram",
        "markdown" => "Document",
        "hosted" => "Claude artifact",
        "code" => "Code",
        _ => "Interactive HTML",
    };
    let title = if block.artifact_title.is_empty() { kind.to_owned() } else { format!("{} · {kind}", block.artifact_title) };
    Border::new()
        .padding(Thickness::xy(12.0, 8.0))
        .background(theme::card())
        .border_brush(theme::stroke())
        .border_thickness(Thickness::uniform(1.0))
        .corner_radius(theme::RADIUS)
        .content(StackPanel::new().orientation(Orientation::Horizontal).spacing(8.0).children(vec![
            theme::glyph("\u{E7C3}", 14.0).opacity(0.8).into(),
            TextBlock::new().text(title).font_weight(FontWeight::SEMI_BOLD).into(),
            TextBlock::new().text(format!("version {}", block.artifact_version.max(1))).font_size(theme::CAPTION).opacity(0.6).vertical_alignment(VerticalAlignment::Center).into(),
        ]))
        .into()
}

/// The blocks, drawn top to bottom.
pub fn blocks(blocks: &[MarkdownBlock], draw: &Draw<'_>) -> Vec<View> {
    let size = draw.size;
    let mut out: Vec<View> = Vec::new();
    let mut at = 0;
    while at < blocks.len() {
        let block = &blocks[at];
        at += 1;
        if block.artifact_id.is_some() {
            out.push(artifact(block));
            continue;
        }
        // A quote's bar, as deep as the quote is.
        let quoted = |view: View| -> View {
            (0..block.quote_depth).fold(view, |inner, _| Border::new().padding(Thickness::new(10.0, 2.0, 0.0, 2.0)).border_brush(theme::stroke()).border_thickness(Thickness::new(3.0, 0.0, 0.0, 0.0)).content(inner).into())
        };
        let view: View = match block.kind.as_str() {
            "heading" => {
                let bigger = match block.level {
                    1 => 8.0,
                    2 => 4.0,
                    _ => 1.0,
                };
                rich(&block.runs, &block.text, true, size + bigger).margin(Thickness::new(0.0, 4.0, 0.0, 0.0)).into()
            }
            "code" if block.fence_closed && block.language.eq_ignore_ascii_case("mermaid") => diagram(block, draw),
            "code" => code(block, draw),
            "list_item" => {
                let marker = match block.task_checked {
                    _ if block.continuation => String::new(),
                    Some(true) => "☑".to_owned(),
                    Some(false) => "☐".to_owned(),
                    None if block.ordered => format!("{}.", block.number),
                    None => "•".to_owned(),
                };
                Grid::new()
                    .columns([GridLength::Pixel(26.0), GridLength::STAR])
                    .margin(Thickness::new(18.0 * f64::from(block.level.saturating_sub(1)), 0.0, 0.0, 0.0))
                    .children((
                        TextBlock::new().text(marker).font_size(size).opacity(0.75).horizontal_alignment(HorizontalAlignment::Right).margin(Thickness::new(0.0, 0.0, 8.0, 0.0)),
                        rich(&block.runs, &block.text, false, size).grid_column(1),
                    ))
                    .into()
            }
            "quote" if block.quote_depth == 0 => {
                Border::new().padding(Thickness::new(10.0, 2.0, 0.0, 2.0)).border_brush(theme::stroke()).border_thickness(Thickness::new(3.0, 0.0, 0.0, 0.0)).content(rich(&block.runs, &block.text, false, size).opacity(0.8)).into()
            }
            "quote" => rich(&block.runs, &block.text, false, size).opacity(0.8).into(),
            "rule" => Border::new().height(1.0).background(theme::stroke()).margin(Thickness::xy(0.0, 6.0)).into(),
            "table_row" => {
                // The rows that follow belong to the same table.
                let start = at - 1;
                while at < blocks.len() && blocks[at].kind == "table_row" && blocks[at].artifact_id.is_none() {
                    at += 1;
                }
                table(&blocks[start..at], size)
            }
            _ => rich(&block.runs, &block.text, false, size).into(),
        };
        out.push(quoted(view));
        // The rich text follows web links only: a file of the workspace named in the block is a button under it.
        let files = workspace_files(block);
        if !files.is_empty() {
            let chips = files.into_iter().map(|path| {
                let (on_act, open) = (draw.on_act.clone(), path.to_owned());
                Button::new()
                    .style(ButtonStyle::Subtle)
                    .automation_name(format!("Open {path}"))
                    .content(StackPanel::new().orientation(Orientation::Horizontal).spacing(4.0).children(vec![
                        theme::glyph("\u{E8A5}", 11.0).opacity(0.7).into(),
                        TextBlock::new().text(path).font_family(theme::MONO).font_size(theme::CAPTION).into(),
                    ]))
                    .on_click(move || on_act.call(Act::OpenFile(open.clone())))
                    .tooltip("Open in the Files tab")
            });
            out.push(StackPanel::new().orientation(Orientation::Horizontal).spacing(2.0).children(chips.collect::<Vec<View>>()).into());
        }
    }
    out
}

/// The workspace files a block's text links to, each once, in order.
fn workspace_files(block: &MarkdownBlock) -> Vec<&str> {
    let mut files: Vec<&str> = Vec::new();
    for run in block.runs.iter().chain(block.cell_runs.iter().flatten()) {
        if let Some(path) = run.workspace_link.as_deref().filter(|path| !files.contains(path)) {
            files.push(path);
        }
    }
    files
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(text: &str) -> MarkdownRun {
        MarkdownRun { text: text.into(), ..MarkdownRun::default() }
    }

    #[test]
    fn runs_keep_what_the_rich_text_can_show_and_mark_the_rest() {
        let runs = vec![
            MarkdownRun { bold: true, ..run("bold") },
            MarkdownRun { code: true, ..run("x = 1") },
            MarkdownRun { strike: true, ..run("no") },
            MarkdownRun { link: Some("https://example.com/a".into()), ..run("a page") },
            MarkdownRun { link: Some("file:///C:/secrets".into()), ..run("a file") },
            MarkdownRun { workspace_link: Some("src/main.rs".into()), ..run("src/main.rs") },
        ];
        let got = inlines(&runs, "", false);
        assert_eq!(got[0], ("bold".to_owned(), true, false, None));
        assert_eq!(got[1].0, "`x = 1`");
        assert_eq!(got[2].0, "n\u{0336}o\u{0336}");
        assert_eq!(got[3].3.as_deref(), Some("https://example.com/a"));
        assert_eq!(got[4].3, None, "only web pages are followed");
        assert_eq!(got[5], ("src/main.rs".to_owned(), false, false, None));
        assert_eq!(inlines(&[], "plain", true), [("plain".to_owned(), true, false, None)]);
        let block = MarkdownBlock { runs: vec![runs[5].clone(), runs[0].clone(), runs[5].clone()], ..MarkdownBlock::default() };
        assert_eq!(workspace_files(&block), ["src/main.rs"], "a file linked twice is one button");
    }
}
