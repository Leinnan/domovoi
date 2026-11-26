//! The Files tab: the workspace's folders and files, a search over them, and
//! the file opened, which can be changed and saved.

use domovoi_core::Event;
use domovoi_core::view::{FileEntryView, FilePreviewView, ImageView, LfsView};
use domovoi_desktop_host::palette;
use windows_reactor::{
    Border, Button, ButtonStyle, FontWeight, Grid, GridLength, HorizontalAlignment, Image, Orientation, ScrollBarVisibility, ScrollViewer, StackPanel, Stretch,
    TextBlock, TextBox, TextTrimming, TextWrapping, Thickness, TooltipExt as _, VerticalAlignment, View,
};

use super::{Act, Shown, caption, does, does_icon, file_name, mono, tell, tell_icon};
use crate::core_link::CoreLink;
use crate::{theme, widgets};

/// The file types an artifact preview can be made of.
const PREVIEWED: [&str; 7] = ["html", "htm", "svg", "jsx", "tsx", "mmd", "mermaid"];

pub fn view(core: &CoreLink, shown: &Shown<'_>) -> View {
    let files = &shown.panel.files;
    let open = files.preview.as_ref().map(|p| p.path.as_str());
    let mut top: Vec<View> = Vec::new();
    if !files.root.is_empty() {
        top.push(
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(6.0)
                .children(vec![theme::glyph("\u{E838}", 13.0).foreground(theme::accent()).into(), TextBlock::new().text(file_name(&files.root)).font_weight(FontWeight::SEMI_BOLD).into()])
                .tooltip(&files.root),
        );
    }
    let on_act = shown.on_act.clone();
    top.push(TextBox::new().automation_name("Search files").placeholder_text("Search files").text(&shown.state.file_query).on_text_changed(move |text| on_act.call(Act::FileQuery(text))).into());

    let mut rows: Vec<View> = Vec::new();
    if !files.query.is_empty() {
        rows.extend(files.results.iter().map(|entry| row(core, entry, true, open)));
        if files.results.is_empty() && !files.loading {
            rows.push(caption(&format!("No files match “{}”", files.query)).margin(Thickness::uniform(8.0)).into());
        }
    } else if files.loading && files.entries.is_empty() {
        rows.push(caption("Loading the files…").margin(Thickness::uniform(8.0)).into());
    } else {
        rows.extend(files.entries.iter().map(|entry| row(core, entry, false, open)));
    }
    let tree = ScrollViewer::new()
        .grid_row(1)
        .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
        .horizontal_scroll_bar_visibility(ScrollBarVisibility::Disabled)
        .content(StackPanel::new().orientation(Orientation::Vertical).margin(Thickness::new(4.0, 0.0, 10.0, 6.0)).children(rows));
    let top = StackPanel::new().orientation(Orientation::Vertical).spacing(6.0).margin(Thickness::new(10.0, 8.0, 10.0, 6.0)).children(top);
    match &files.preview {
        None => Grid::new().rows([GridLength::Auto, GridLength::STAR]).children((top, tree)).into(),
        Some(preview) => Grid::new()
            .rows([GridLength::Auto, GridLength::Star(2.0), GridLength::Star(3.0)])
            .children((top, tree, Border::new().grid_row(2).border_brush(theme::stroke()).border_thickness(Thickness::new(0.0, 1.0, 0.0, 0.0)).content(opened(core, preview, shown))))
            .into(),
    }
}

/// A folder or a file of the tree; of the search, with its whole path.
fn row(core: &CoreLink, entry: &FileEntryView, found: bool, open: Option<&str>) -> View {
    let glyph = match (entry.directory, entry.expanded) {
        (true, true) => "\u{E838}",
        (true, false) => "\u{E8B7}",
        (false, _) => "\u{E8A5}",
    };
    let indent = if found { 0.0 } else { f64::from(entry.depth) * 14.0 };
    let dim = if entry.ignored { 0.5 } else { 1.0 };
    let mut cells: Vec<View> = vec![
        theme::glyph(glyph, 12.0).opacity(if entry.directory { 0.9 } else { 0.6 } * dim).margin(Thickness::new(indent, 0.0, 0.0, 0.0)).into(),
        TextBlock::new()
            .grid_column(1)
            .text(if found { &entry.path } else { &entry.name })
            .font_size(theme::CAPTION + 1.0)
            .opacity(dim)
            .text_trimming(TextTrimming::CharacterEllipsis)
            .vertical_alignment(VerticalAlignment::Center)
            .into(),
    ];
    if entry.ignored {
        cells.push(caption("ignored").grid_column(2).vertical_alignment(VerticalAlignment::Center).into());
    }
    let (core, path, directory) = (core.clone(), entry.path.clone(), entry.directory);
    let button = Button::new()
        .style(ButtonStyle::Subtle)
        .horizontal_alignment(HorizontalAlignment::Stretch)
        .horizontal_content_alignment(HorizontalAlignment::Stretch)
        .automation_name(&entry.path)
        .content(Grid::new().columns([GridLength::Auto, GridLength::STAR, GridLength::Auto]).column_spacing(6.0).children(cells))
        .on_click(move || core.send(if directory { Event::ToggleDirectory { path: path.clone() } } else { Event::OpenFile { path: path.clone() } }));
    if open == Some(entry.path.as_str()) { Border::new().corner_radius(4.0).background(theme::selected()).content(button).into() } else { button.into() }
}

/// The opened file: its path and what can be done with it, then what it holds.
fn opened(core: &CoreLink, preview: &FilePreviewView, shown: &Shown<'_>) -> View {
    let editing = shown.state.editing.as_ref().filter(|(path, _)| *path == preview.path);
    let ready = !preview.loading && preview.error.is_none();
    let extension = preview.path.rsplit('.').next().unwrap_or_default().to_ascii_lowercase();
    let mut actions: Vec<View> = Vec::new();
    if let Some(size) = &preview.size_label {
        actions.push(caption(size).vertical_alignment(VerticalAlignment::Center).into());
    }
    if editing.is_some() {
        actions.push(does(shown.on_act, "Save", Act::Save).style(ButtonStyle::Accent).into());
        actions.push(does(shown.on_act, "Cancel", Act::Edit(false)).into());
    } else {
        if ready && preview.image_url.is_none() && !preview.truncated && PREVIEWED.contains(&extension.as_str())
            && let Some(thread_id) = shown.thread_id
        {
            let event = Event::OpenArtifactWorkspacePath { thread_id: thread_id.to_owned(), path: preview.path.clone(), dark: shown.dark };
            actions.push(tell(core, "Preview", event).tooltip("Open it as an artifact"));
        }
        if ready && preview.editable {
            actions.push(does_icon(shown.on_act, "\u{E70F}", "Edit the file", Act::Edit(true)).tooltip("Edit"));
        }
        if preview.error.is_none() {
            actions.push(tell_icon(core, "\u{E8BD}", "Mention the file in the message", Event::AppendToDraft { text: format!("@{} ", preview.path) }).tooltip("Mention in the message"));
        }
        match &preview.image_url {
            Some(url) => {
                let picture = ImageView { name: file_name(&preview.path).to_owned(), url: url.clone() };
                actions.push(does_icon(shown.on_act, "\u{E7B3}", "View the picture full size", Act::View(picture)).tooltip("View full size"));
                actions.push(does_icon(shown.on_act, "\u{E71B}", "Copy the file's path", Act::Copy(preview.path.clone())).tooltip("Copy the path"));
            }
            None => actions.push(does_icon(shown.on_act, "\u{E8C8}", "Copy the file's text", Act::Copy(preview.text.clone())).tooltip("Copy")),
        }
    }
    let head = Grid::new().columns([GridLength::STAR, GridLength::Auto]).column_spacing(8.0).margin(Thickness::new(10.0, 4.0, 6.0, 4.0)).children((
        mono(&preview.path).text_trimming(TextTrimming::CharacterEllipsis).vertical_alignment(VerticalAlignment::Center).tooltip(&preview.path),
        StackPanel::new().grid_column(1).orientation(Orientation::Horizontal).spacing(2.0).children(actions),
    ));
    let body: View = match editing {
        Some((_, text)) => {
            let on_act = shown.on_act.clone();
            TextBox::new()
                .automation_name("The file's text")
                .accepts_return(true)
                .text_wrapping(TextWrapping::NoWrap)
                .text(text)
                .on_text_changed(move |text| on_act.call(Act::EditText(text)))
                .into()
        }
        None => body(core, preview, shown),
    };
    Grid::new().rows([GridLength::Auto, GridLength::STAR]).children((head, Border::new().grid_row(1).background(theme::code()).content(body))).into()
}

/// A file's contents under its Git LFS card, when it is kept there. Also what the Artifacts tab shows of a workspace file.
pub fn body(core: &CoreLink, preview: &FilePreviewView, shown: &Shown<'_>) -> View {
    let contents = contents(preview, shown);
    match &preview.lfs {
        Some(lfs) if !preview.loading && preview.error.is_none() => Grid::new().rows([GridLength::Auto, GridLength::STAR]).children((lfs_card(core, &preview.path, lfs, shown), Border::new().grid_row(1).content(contents))).into(),
        _ => contents,
    }
}

/// A file kept in Git LFS: whether the server has it, its size and id, and Fetch when it has not.
fn lfs_card(core: &CoreLink, path: &str, lfs: &LfsView, shown: &Shown<'_>) -> View {
    let colour = match lfs.state.as_str() {
        "cached" => "green",
        "missing" => "amber",
        _ => "red",
    };
    let mut cells: Vec<View> = vec![
        TextBlock::new().text("Git LFS").font_size(theme::CAPTION).font_weight(FontWeight::SEMI_BOLD).foreground(theme::accent()).vertical_alignment(VerticalAlignment::Center).into(),
        TextBlock::new().text(&lfs.state_label).font_size(theme::CAPTION + 1.0).foreground(theme::rgb(palette::project_rgb(colour, shown.dark))).vertical_alignment(VerticalAlignment::Center).into(),
        mono(&lfs.size_label).opacity(0.65).vertical_alignment(VerticalAlignment::Center).into(),
        // The short id; the whole SHA-256 in its tooltip, copied when pressed.
        does(shown.on_act, &lfs.oid_short, Act::Copy(lfs.oid.clone())).style(ButtonStyle::Subtle).tooltip(format!("Copy the object's id\n{}", lfs.oid)),
    ];
    if lfs.can_fetch || lfs.fetching {
        cells.push(tell(core, if lfs.fetching { "Fetching…" } else { "Fetch" }, Event::FetchLfsObject { path: path.to_owned() }).is_enabled(!lfs.fetching).into());
    }
    let mut lines: Vec<View> = vec![StackPanel::new().orientation(Orientation::Horizontal).spacing(8.0).children(cells).into()];
    if let Some(message) = &lfs.message {
        lines.push(caption(message).text_wrapping(TextWrapping::WrapWholeWords).into());
    }
    StackPanel::new().orientation(Orientation::Vertical).spacing(4.0).margin(Thickness::xy(10.0, 8.0)).children(lines).into()
}

fn contents(preview: &FilePreviewView, shown: &Shown<'_>) -> View {
    let said = |text: &str| caption(text).margin(Thickness::uniform(16.0)).horizontal_alignment(HorizontalAlignment::Center).vertical_alignment(VerticalAlignment::Center);
    if preview.loading {
        return said("Loading the file…").into();
    }
    if let Some(error) = &preview.error {
        return TextBlock::new().text(error).foreground(theme::critical()).text_wrapping(TextWrapping::WrapWholeWords).margin(Thickness::uniform(12.0)).into();
    }
    if let Some(url) = &preview.image_url {
        return match shown.media.image(url) {
            Some(image) => Image::new().source_data(image).stretch(Stretch::Uniform).margin(Thickness::uniform(12.0)).into(),
            None if shown.media.gave_up(url) => said("The picture could not be loaded.").into(),
            None => said("Loading the picture…").into(),
        };
    }
    let mut lines: Vec<View> = vec![widgets::code_text(&preview.spans, &preview.text, shown.dark)];
    if preview.truncated {
        lines.push(caption("The file is longer than what is shown here.").into());
    }
    ScrollViewer::new()
        .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
        .horizontal_scroll_bar_visibility(ScrollBarVisibility::Auto)
        .content(StackPanel::new().orientation(Orientation::Vertical).spacing(6.0).margin(Thickness::new(10.0, 8.0, 14.0, 14.0)).children(lines))
        .into()
}
