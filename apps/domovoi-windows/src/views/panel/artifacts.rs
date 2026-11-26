//! The Artifacts tab: what the thread made (pages, components, drawings,
//! diagrams), and the one opened: its versions, its source, and what can be
//! shown of it here. A page that runs opens in the browser.

use domovoi_core::Event;
use std::cell::RefCell;

use domovoi_core::view::{ActiveArtifactView, FilePreviewView};
use windows_reactor::{
    Border, Button, ButtonStyle, EncodedImage, FontWeight, Grid, GridLength, HorizontalAlignment, Image, Orientation, ScrollBarVisibility, ScrollViewer, StackPanel, Stretch, TextBlock,
    TextBox, TextTrimming, TextWrapping, Thickness, TooltipExt as _, VerticalAlignment, View,
};

use super::{Act, Shown, caption, card, does, does_icon, files, mono, nothing, tell, tell_icon};
use crate::core_link::CoreLink;
use crate::{theme, widgets};

/// What a kind of artifact is called, and its glyph.
fn kind(kind: &str) -> (&'static str, &'static str) {
    match kind {
        "html" => ("HTML page", "\u{E774}"),
        "svg" => ("SVG drawing", "\u{EB9F}"),
        "react" => ("React component", "\u{E943}"),
        "mermaid" => ("Mermaid diagram", "\u{E9D9}"),
        "markdown" => ("Markdown", "\u{E8A5}"),
        "hosted" => ("Hosted page", "\u{E774}"),
        _ => ("Code", "\u{E943}"),
    }
}

/// `OpenArtifactFile` for an artifact written to an HTML file of the workspace (the core reads only those).
fn open_file(thread_id: Option<&str>, path: Option<&str>) -> Option<Event> {
    let path = path.filter(|p| p.rsplit_once('.').is_some_and(|(_, ext)| ext.eq_ignore_ascii_case("html") || ext.eq_ignore_ascii_case("htm")))?;
    Some(Event::OpenArtifactFile { thread_id: thread_id?.to_owned(), path: path.to_owned() })
}

thread_local! {
    /// The drawing last shown, and the artifact and version it is of: the
    /// same bytes each render, so the toolkit decodes them once.
    static DRAWN: RefCell<Option<((String, u32), EncodedImage)>> = const { RefCell::new(None) };
}

/// An SVG artifact's source as a picture.
fn drawing(artifact: &ActiveArtifactView) -> EncodedImage {
    let key = (artifact.id.clone(), artifact.version);
    DRAWN.with(|drawn| {
        let mut drawn = drawn.borrow_mut();
        // A version still being written changes under the same number.
        if artifact.streaming || drawn.as_ref().is_none_or(|(of, _)| *of != key) {
            *drawn = Some((key, EncodedImage::new(artifact.source.clone().into_bytes())));
        }
        drawn.as_ref().map_or_else(|| EncodedImage::new(Vec::new()), |(_, image)| image.clone())
    })
}

fn streaming() -> View {
    caption("Streaming").foreground(theme::accent()).vertical_alignment(VerticalAlignment::Center).tooltip("The agent is still writing this artifact")
}

pub fn view(core: &CoreLink, shown: &Shown<'_>) -> View {
    let artifacts = &shown.panel.artifacts;
    if let Some(preview) = &shown.panel.files.artifact_preview {
        return file(core, preview, shown);
    }
    if let Some(active) = &artifacts.active {
        return opened(core, active, shown);
    }
    if artifacts.items.is_empty() {
        return nothing("\u{E734}", "No artifacts yet", "Pages, components, drawings and diagrams this thread makes are listed here.", None);
    }
    let mut lines: Vec<View> = vec![TextBlock::new().text("Thread artifacts").font_weight(FontWeight::SEMI_BOLD).into()];
    for item in &artifacts.items {
        let (name, glyph) = kind(&item.kind);
        let about = if item.version_count > 1 { format!("{name} · v{}", item.latest_version) } else { name.to_owned() };
        let mut cells: Vec<View> = vec![
            theme::glyph(glyph, 14.0).foreground(theme::accent()).into(),
            StackPanel::new()
                .grid_column(1)
                .orientation(Orientation::Vertical)
                .children(vec![
                    TextBlock::new().text(if item.title.is_empty() { name } else { item.title.as_str() }).text_trimming(TextTrimming::CharacterEllipsis).into(),
                    caption(&about).into(),
                ])
                .into(),
        ];
        if item.streaming {
            cells.push(Border::new().grid_column(2).content(streaming()).into());
        }
        let open = shown.thread_id.map(|thread_id| Event::OpenArtifact { thread_id: thread_id.to_owned(), artifact_id: item.id.clone(), version: None, dark: shown.dark });
        let core_open = core.clone();
        let mut row: Vec<View> = vec![
            Button::new()
                .style(ButtonStyle::Subtle)
                .horizontal_alignment(HorizontalAlignment::Stretch)
                .horizontal_content_alignment(HorizontalAlignment::Stretch)
                .automation_name(format!("Open {}", if item.title.is_empty() { name } else { item.title.as_str() }))
                .content(Grid::new().columns([GridLength::Auto, GridLength::STAR, GridLength::Auto]).column_spacing(10.0).children(cells))
                .on_click(move || {
                    if let Some(event) = &open {
                        core_open.send(event.clone());
                    }
                })
                .into(),
        ];
        if let Some(event) = open_file(shown.thread_id, item.path.as_deref()) {
            row.push(tell_icon(core, "\u{E8A5}", "View the workspace file", event).grid_column(1).tooltip("View the workspace file"));
        } else if let Some(url) = &item.hosted_url {
            row.push(does_icon(shown.on_act, "\u{E8A7}", "Open in the browser", Act::Open(url.clone())).grid_column(1).tooltip("Open in the browser"));
        }
        lines.push(Grid::new().columns([GridLength::STAR, GridLength::Auto]).children(row).into());
    }
    ScrollViewer::new()
        .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
        .horizontal_scroll_bar_visibility(ScrollBarVisibility::Disabled)
        .content(StackPanel::new().orientation(Orientation::Vertical).spacing(4.0).margin(Thickness::new(10.0, 10.0, 14.0, 10.0)).children(lines))
        .into()
}

/// The workspace file an artifact was written to, as text.
fn file(core: &CoreLink, preview: &FilePreviewView, shown: &Shown<'_>) -> View {
    let mut cells: Vec<View> = vec![
        tell_icon(core, "\u{E72B}", "Back to the artifacts", Event::CloseArtifactFile).tooltip("Back"),
        mono(&preview.path).grid_column(1).text_trimming(TextTrimming::CharacterEllipsis).vertical_alignment(VerticalAlignment::Center).into(),
    ];
    let mut actions: Vec<View> = Vec::new();
    if preview.error.is_none() {
        actions.push(tell_icon(core, "\u{E8BD}", "Mention the file in the message", Event::AppendToDraft { text: format!("@{} ", preview.path) }).tooltip("Mention in the message"));
        if !preview.loading {
            actions.push(does_icon(shown.on_act, "\u{E8C8}", "Copy the file's text", Act::Copy(preview.text.clone())).tooltip("Copy"));
        }
    }
    cells.push(StackPanel::new().grid_column(2).orientation(Orientation::Horizontal).children(actions).into());
    Grid::new()
        .rows([GridLength::Auto, GridLength::STAR])
        .children((
            Grid::new().columns([GridLength::Auto, GridLength::STAR, GridLength::Auto]).column_spacing(6.0).margin(Thickness::new(6.0, 4.0, 6.0, 4.0)).children(cells),
            Border::new().grid_row(1).background(theme::code()).content(files::body(core, preview, shown)),
        ))
        .into()
}

/// The opened artifact: its name, its versions and what can be done with it, what it shows, and a line to ask for a change.
fn opened(core: &CoreLink, artifact: &ActiveArtifactView, shown: &Shown<'_>) -> View {
    let (name, _) = kind(&artifact.kind);
    let code = artifact.mode == "code";
    let mut under: Vec<View> = vec![caption(&format!("{name} · v{}", artifact.version.max(1))).vertical_alignment(VerticalAlignment::Center).into()];
    if artifact.streaming {
        under.push(streaming());
    }
    let mode = |label: &str, to: &str, on: bool| tell(core, label, Event::SetArtifactViewMode { mode: to.into() }).style(if on { ButtonStyle::Accent } else { ButtonStyle::Default });
    let head = Grid::new().columns([GridLength::Auto, GridLength::STAR, GridLength::Auto]).column_spacing(6.0).margin(Thickness::new(6.0, 6.0, 6.0, 2.0)).children((
        tell_icon(core, "\u{E72B}", "Back to the artifacts", Event::CloseArtifact).tooltip("Back to the list"),
        StackPanel::new().grid_column(1).orientation(Orientation::Vertical).children(vec![
            TextBlock::new().text(if artifact.title.is_empty() { name } else { artifact.title.as_str() }).font_weight(FontWeight::SEMI_BOLD).text_trimming(TextTrimming::CharacterEllipsis).into(),
            StackPanel::new().orientation(Orientation::Horizontal).spacing(6.0).children(under).into(),
        ]),
        StackPanel::new().grid_column(2).orientation(Orientation::Horizontal).spacing(2.0).vertical_alignment(VerticalAlignment::Center).children(vec![
            mode("Preview", "preview", !code).into(),
            mode("Code", "code", code).into(),
            does_icon(shown.on_act, "\u{E8C8}", "Copy the source", Act::Copy(artifact.source.clone())).tooltip("Copy the source"),
        ]),
    ));

    let mut bar: Vec<View> = Vec::new();
    if artifact.versions.len() > 1 {
        for version in &artifact.versions {
            let on = version.version == artifact.version;
            let chip = tell(core, &format!("v{}", version.version), Event::SelectArtifactVersion { version: version.version, dark: shown.dark }).style(if on { ButtonStyle::Accent } else { ButtonStyle::Subtle });
            // What changed in this version, from the message that made it.
            bar.push(if version.summary.is_empty() { chip.into() } else { chip.tooltip(&version.summary) });
        }
    }
    let mut actions: Vec<View> = Vec::new();
    if let Some(event) = open_file(shown.thread_id, artifact.path.as_deref()) {
        actions.push(tell_icon(core, "\u{E8A5}", "View the workspace file", event).tooltip("View the workspace file"));
    }
    let page = artifact.preview_url.as_ref().or(artifact.hosted_url.as_ref());
    if let Some(url) = page {
        actions.push(does(shown.on_act, "Open in Browser", Act::Open(url.clone())).into());
    }
    actions.push(tell_icon(core, "\u{E72C}", "Refresh the preview", Event::RefreshArtifactPreview { dark: shown.dark }).tooltip("Refresh the preview"));
    let bar = Grid::new().grid_row(1).columns([GridLength::STAR, GridLength::Auto]).margin(Thickness::new(10.0, 0.0, 6.0, 4.0)).children((
        ScrollViewer::new()
            .horizontal_scroll_bar_visibility(ScrollBarVisibility::Hidden)
            .vertical_scroll_bar_visibility(ScrollBarVisibility::Disabled)
            .content(StackPanel::new().orientation(Orientation::Horizontal).spacing(2.0).children(bar)),
        StackPanel::new().grid_column(1).orientation(Orientation::Horizontal).spacing(2.0).children(actions),
    ));

    let source = || widgets::code_text(&artifact.spans, &artifact.source, shown.dark);
    let scrolled = |lines: Vec<View>, sideways: bool| -> View {
        ScrollViewer::new()
            .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
            .horizontal_scroll_bar_visibility(if sideways { ScrollBarVisibility::Auto } else { ScrollBarVisibility::Disabled })
            .content(StackPanel::new().orientation(Orientation::Vertical).spacing(8.0).margin(Thickness::new(10.0, 8.0, 14.0, 14.0)).children(lines))
            .into()
    };
    let body: View = if artifact.loading {
        nothing("\u{E895}", "Loading the artifact…", "Its contents are on their way from the server.", None)
    } else if let Some(error) = &artifact.error {
        nothing("\u{E783}", "The artifact could not be loaded", error, None)
    } else if code || artifact.kind == "code" {
        let mut lines = vec![source()];
        if artifact.truncated {
            lines.push(caption("The source is longer than what is shown here.").into());
        }
        scrolled(lines, true)
    } else {
        match artifact.kind.as_str() {
            "svg" => Image::new().source_svg(drawing(artifact)).stretch(Stretch::Uniform).margin(Thickness::uniform(16.0)).into(),
            "mermaid" => match artifact.preview_url.as_deref().and_then(|url| shown.media.image(url)) {
                Some(drawing) => scrolled(vec![Image::new().source_data(drawing).stretch(Stretch::Uniform).into()], false),
                None => scrolled(vec![caption("The diagram's source (its drawing is on its way, or the server has none):").into(), source()], true),
            },
            "markdown" => scrolled(vec![TextBlock::new().text(&artifact.source).text_wrapping(TextWrapping::WrapWholeWords).is_text_selection_enabled(true).into()], false),
            _ => {
                // A page that runs: it opens in the browser, and its source is here.
                let mut about: Vec<View> = vec![
                    TextBlock::new().text(format!("{name} preview")).font_weight(FontWeight::SEMI_BOLD).into(),
                    caption("A page that runs opens in your browser. Its source is below.").text_wrapping(TextWrapping::WrapWholeWords).into(),
                ];
                if let Some(url) = page {
                    about.push(does(shown.on_act, "Open the Preview in the Browser", Act::Open(url.clone())).style(ButtonStyle::Accent).horizontal_alignment(HorizontalAlignment::Left).into());
                }
                scrolled(vec![card(about), source()], true)
            }
        }
    };

    let on_ask = shown.on_act.clone();
    let ask = Grid::new().grid_row(3).columns([GridLength::STAR, GridLength::Auto]).column_spacing(4.0).margin(Thickness::uniform(8.0)).children((
        TextBox::new().automation_name("Ask for a change to this artifact").placeholder_text("Ask the agent to change this artifact").text(&shown.state.ask).on_text_changed(move |text| on_ask.call(Act::Ask(text))),
        does(shown.on_act, "Send", Act::SendAsk).grid_column(1).style(ButtonStyle::Accent).is_enabled(!shown.state.ask.trim().is_empty()),
    ));
    Grid::new().rows([GridLength::Auto, GridLength::Auto, GridLength::STAR, GridLength::Auto]).children((head, bar, Border::new().grid_row(2).background(theme::code()).content(body), ask)).into()
}
