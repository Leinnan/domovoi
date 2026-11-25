//! Workspace Files pane: workspace root, file search, collapsible directory tree,
//! syntax-highlighted preview, image preview, open-in-Artifacts shortcut, and inline
//! file editor. The preview renderer is shared with the Artifacts panel's file view.

use domovoi_core::Event;
use domovoi_core::view::{FileEntryView, FilePreviewView, ImageView, LfsView};
use iced::widget::{Column, Space, button, column, container, image, keyed_column, row, text, text_editor};
use iced::{Alignment, Element, Length, Padding};

use crate::app::{App, ImageAction, Message};
use crate::ui::fonts;
use crate::ui::icons::Icon;
use crate::ui::tokens::*;
use crate::ui::widgets as w;

fn core(e: Event) -> Message {
    Message::Core(Box::new(e))
}

pub fn view(app: &App) -> Element<'_, Message> {
    let files = &app.vm.panel.files;

    let search = container(
        w::input("Search files…", &app.file_query_input).on_input(Message::FileQueryChanged),
    )
    .padding(S);

    let mut main = Column::new();
    if !files.root.is_empty() {
        let name = files.root.trim_end_matches(['/', '\\']).rsplit(['/', '\\']).next().filter(|n| !n.is_empty()).unwrap_or(&files.root);
        main = main.push(
            container(w::with_tooltip(
                row![
                    w::themed_icon(Icon::FolderOpen, ICON_SMALL, |c| c.accent),
                    text(name).size(TEXT_SMALL).font(fonts::SEMIBOLD),
                ]
                .spacing(XS)
                .align_y(Alignment::Center),
                &files.root,
            ))
            .padding(Padding { top: S, right: S, bottom: 0.0, left: M }),
        );
    }

    let mut tree = Column::new().spacing(XXS).padding([0.0, XS]);
    if !files.query.is_empty() {
        for entry in &files.results {
            tree = tree.push(file_row(entry, true, files.preview.as_ref().map(|p| p.path.as_str())));
        }
        if files.results.is_empty() && !files.loading {
            tree = tree.push(container(w::faint(format!("No files match “{}”", files.query), TEXT_SMALL)).padding(S));
        }
    } else if files.loading && files.entries.is_empty() {
        tree = tree.push(container(w::muted("Loading files…", TEXT_SMALL)).padding(S));
    } else {
        for entry in &files.entries {
            tree = tree.push(file_row(entry, false, files.preview.as_ref().map(|p| p.path.as_str())));
        }
    }

    main = main
        .push(search)
        .push(w::scroll(tree).height(if files.preview.is_some() { Length::FillPortion(2) } else { Length::Fill }));

    if let Some(preview) = &files.preview {
        let is_editing = app.file_edit.as_ref().is_some_and(|e| e.path == preview.path);
        let ext = preview.path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
        let can_artifact = !preview.loading
            && preview.error.is_none()
            && preview.image_url.is_none()
            && !preview.truncated
            && matches!(ext.as_str(), "html" | "htm" | "svg" | "jsx" | "tsx" | "mmd" | "mermaid");

        let mut hdr = row![
            text(&preview.path).size(TEXT_SMALL).font(fonts::MONO).width(Length::Fill),
        ]
        .spacing(XS)
        .align_y(Alignment::Center);
        if let Some(size) = &preview.size_label {
            hdr = hdr.push(w::faint(size, TEXT_CAPTION));
        }

        if is_editing {
            hdr = hdr
                .push(w::small_button("Save", w::primary_style, Some(Message::SaveFileEdit)))
                .push(w::small_button("Cancel", w::ghost_style, Some(Message::CancelFileEdit)));
        } else {
            if can_artifact
                && let Some(tid) = app.vm.thread.as_ref().map(|t| t.id.clone()) {
                    hdr = hdr.push(w::small_button(
                        "Preview",
                        w::secondary_style,
                        Some(core(Event::OpenArtifactWorkspacePath {
                            thread_id: tid,
                            path: preview.path.clone(),
                            dark: app.is_dark(),
                        })),
                    ));
                }
            if preview.editable && !preview.loading && preview.error.is_none() {
                hdr = hdr.push(w::icon_button(Icon::Pencil, "Edit file", Some(Message::StartFileEdit)));
            }
            if preview.error.is_none() {
                hdr = hdr.push(w::icon_button(
                    Icon::MessageSquare,
                    "Mention in composer",
                    Some(core(Event::AppendToDraft { text: format!("@{} ", preview.path) })),
                ));
            }
            if let Some(url) = &preview.image_url {
                let shown = file_image(preview, url);
                let loaded = app.images.contains_key(url);
                let act = |action: ImageAction| loaded.then(|| Message::Image { action, image: shown.clone() });
                hdr = hdr
                    .push(w::icon_button(Icon::Eye, "View full size", loaded.then(|| Message::OpenImages { images: vec![shown.clone()], index: 0 })))
                    .push(w::icon_button(Icon::Copy, "Copy image", act(ImageAction::Copy)))
                    .push(w::icon_button(Icon::Download, "Save image as…", act(ImageAction::Save)))
                    .push(w::icon_button(Icon::ExternalLink, "Open in default app", act(ImageAction::OpenExternally)))
                    .push(w::icon_button(Icon::Link, "Copy path", Some(Message::CopyText(preview.path.clone()))));
            } else {
                hdr = hdr.push(w::icon_button(Icon::Copy, "Copy", Some(Message::CopyText(preview.text.clone()))));
            }
        }

        let preview_body: Element<'_, Message> = if is_editing {
            text_editor(&app.file_editor)
                .on_action(Message::FileEditAction)
                .font(fonts::MONO)
                .size(TEXT_CODE)
                .height(Length::Fill)
                .style(w::editor_style)
                .into()
        } else {
            preview_body(app, preview)
        };

        main = main.push(w::divider()).push(preview_frame(hdr, preview_body).height(Length::FillPortion(3)));
    }

    main.height(Length::Fill).into()
}

/// A file preview's header bar over its body, as the Files and Artifacts panels show it.
pub fn preview_frame<'a>(header: impl Into<Element<'a, Message>>, body: Element<'a, Message>) -> Column<'a, Message> {
    column![
        container(header).padding([TIGHT, S]).style(w::surface),
        w::divider(),
        container(body).width(Length::Fill).height(Length::Fill).style(w::sunken),
    ]
}

/// A file's contents: loading, its error, the image, or its (highlighted)
/// text; under its Git LFS card when it is kept in LFS.
pub fn preview_body<'a>(app: &'a App, preview: &'a FilePreviewView) -> Element<'a, Message> {
    let body = contents(app, preview);
    match &preview.lfs {
        Some(lfs) if !preview.loading && preview.error.is_none() => column![lfs_card(&preview.path, lfs), w::divider(), body].into(),
        _ => body,
    }
}

/// A Git LFS file's object: its state, size and id, and Fetch when the
/// server doesn't have it.
fn lfs_card<'a>(path: &'a str, lfs: &'a LfsView) -> Element<'a, Message> {
    let accent: w::Tone = |c| c.accent;
    let tone: w::Tone = match lfs.state.as_str() {
        "cached" => |c| c.success,
        "missing" => |c| c.warning,
        _ => |c| c.danger,
    };
    let mut top = row![
        w::badge_capsule("Git LFS", Some(accent)),
        w::colored(&lfs.state_label, TEXT_SMALL, tone),
        w::faint(&lfs.size_label, TEXT_CAPTION).font(fonts::MONO),
        // The short id; the whole SHA-256 on hover, copied on click.
        w::with_tooltip(
            button(w::faint(&lfs.oid_short, TEXT_CAPTION).font(fonts::MONO)).padding([1.0, XS]).style(w::ghost_style).on_press(Message::CopyText(lfs.oid.clone())),
            &format!("Copy object id\n{}", lfs.oid),
        ),
        Space::new().width(Length::Fill),
    ]
    .spacing(XS)
    .align_y(Alignment::Center);
    if lfs.can_fetch || lfs.fetching {
        let label = if lfs.fetching { "Fetching…" } else { "Fetch" };
        let fetch = (!lfs.fetching).then(|| core(Event::FetchLfsObject { path: path.to_owned() }));
        top = top.push(w::icon_label_button(Icon::Download, label, w::Kind::Secondary, fetch));
    }
    let mut card = column![top].spacing(TIGHT);
    if let Some(message) = &lfs.message {
        card = card.push(w::muted(message, TEXT_SMALL));
    }
    container(card).padding([S, M]).width(Length::Fill).style(w::surface).into()
}

fn contents<'a>(app: &'a App, preview: &'a FilePreviewView) -> Element<'a, Message> {
    if preview.loading {
        container(w::muted("Loading file…", TEXT_SMALL)).center(Length::Fill).into()
    } else if let Some(err) = &preview.error {
        container(w::colored(err.clone(), TEXT_SMALL, |c| c.danger)).padding(M).into()
    } else if let Some(url) = &preview.image_url {
        if let Some(handle) = app.images.get(url) {
            // Wheel to zoom, drag to pan; keyed by file so another one starts unzoomed.
            keyed_column([(path_key(&preview.path), image::viewer(handle.clone()).min_scale(1.0).max_scale(8.0).scale_step(0.15).width(Length::Fill).height(Length::Fill).into())])
                .padding(M)
                .height(Length::Fill)
                .into()
        } else if app.failed_images.contains(url) {
            container(
                column![
                    w::muted("Couldn’t load the image.", TEXT_SMALL),
                    w::small_button("Retry", w::secondary_style, Some(Message::RetryImage(url.clone()))),
                ]
                .spacing(S)
                .align_x(Alignment::Center),
            )
            .center(Length::Fill)
            .into()
        } else {
            container(w::muted("Loading image…", TEXT_SMALL)).center(Length::Fill).into()
        }
    } else {
        let mut body_col = Column::new().spacing(XS).padding(M);
        body_col = body_col.push(w::code_spans_view(&preview.spans, &preview.text, app.is_dark(), TEXT_CODE));
        if preview.truncated {
            body_col = body_col.push(w::colored("File preview was truncated.", TEXT_CAPTION, |c| c.warning));
        }
        w::scroll_both(body_col).height(Length::Fill).into()
    }
}

fn file_row<'a>(entry: &'a FileEntryView, show_path: bool, selected_path: Option<&'a str>) -> Element<'a, Message> {
    let selected = selected_path == Some(entry.path.as_str());
    let indent = if show_path { 0.0 } else { (entry.depth as f32) * INDENT };
    let ic = if entry.directory {
        if entry.expanded { Icon::FolderOpen } else { Icon::Folder }
    } else {
        Icon::FileText
    };
    let msg = if entry.directory {
        core(Event::ToggleDirectory { path: entry.path.clone() })
    } else {
        core(Event::OpenFile { path: entry.path.clone() })
    };
    let mut r = row![
        Space::new().width(indent),
        w::themed_icon(ic, ICON_SMALL, move |c| if entry.directory { c.accent } else if entry.ignored { c.faint } else { c.muted }),
        text(if show_path { &entry.path } else { &entry.name })
            .size(TEXT_SMALL)
            .font(if entry.directory { fonts::MEDIUM } else { fonts::INTER })
            .width(Length::Fill),
    ]
    .spacing(XS)
    .align_y(Alignment::Center);

    if entry.ignored {
        r = r.push(w::faint("ignored", TEXT_CAPTION));
    }

    container(w::list_row(r, selected, false).on_press(msg))
        .padding(Padding::ZERO)
        .into()
}

/// A previewed image file, as the viewer and the image actions take it.
fn file_image(preview: &FilePreviewView, url: &str) -> ImageView {
    let name = preview.path.rsplit(['/', '\\']).next().filter(|n| !n.is_empty()).unwrap_or(&preview.path);
    ImageView { name: name.to_owned(), url: url.to_owned() }
}

/// A stable key for a file's path (keyed widgets want a plain value).
fn path_key(path: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    path.hash(&mut h);
    h.finish()
}
