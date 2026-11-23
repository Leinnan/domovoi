//! Artifacts side panel: native Iced previews for SVG, Mermaid, Markdown, and
//! syntax-highlighted Code, plus one-click browser launch for interactive HTML/React
//! previews, version selector, quick-iterate prompt bar, and the workspace file an
//! HTML artifact was written to (shown with the Files panel's preview renderer).

use domovoi_core::Event;
use domovoi_core::view::{ActiveArtifactView, FilePreviewView};
use iced::widget::{Column, Space, button, column, container, image, row, svg, text};
use iced::{Alignment, Element, Length};

use crate::app::{App, Message};
use crate::ui::fonts;
use crate::ui::icons::Icon;
use crate::ui::tokens::*;
use crate::ui::widgets as w;
use crate::views::markdown;
use crate::views::panel::files;

fn core(e: Event) -> Message {
    Message::Core(Box::new(e))
}

pub fn view(app: &App) -> Element<'_, Message> {
    let arts = &app.vm.panel.artifacts;

    if let Some(preview) = &app.vm.panel.files.artifact_preview {
        return artifact_file_view(app, preview);
    }
    if let Some(active) = &arts.active {
        return active_artifact_view(app, active);
    }

    if arts.items.is_empty() {
        return w::empty_state(
            Icon::Sparkles,
            "No artifacts yet",
            "HTML dashboards, React components, SVG graphics, and Mermaid diagrams from this thread appear here.",
            None,
        );
    }

    let tid = app.vm.thread.as_ref().map(|t| t.id.clone()).unwrap_or_default();
    let mut col = Column::new().spacing(S).padding(M).push(w::heading("Thread artifacts"));
    for item in &arts.items {
        let title = if item.title.is_empty() {
            markdown::artifact_kind_label(&item.kind).to_owned()
        } else {
            item.title.clone()
        };
        let sub = format!(
            "{}{}",
            markdown::artifact_kind_label(&item.kind),
            if item.version_count > 1 { format!(" · v{}", item.latest_version) } else { String::new() }
        );
        let mut line = row![
            w::themed_icon(markdown::artifact_kind_icon(&item.kind), ICON, |c| c.accent),
            column![text(title).size(TEXT_UI).font(fonts::MEDIUM), w::muted(sub, TEXT_CAPTION)]
                .spacing(XXS)
                .width(Length::Fill),
        ]
        .spacing(S)
        .align_y(Alignment::Center);
        if item.streaming {
            line = line.push(streaming_badge());
        }
        line = line.push(w::themed_icon(Icon::ChevronRight, ICON_SMALL, |c| c.faint));
        let mut entry = row![
            w::list_row(line, false, true).on_press(core(Event::OpenArtifact {
                thread_id: tid.clone(),
                artifact_id: item.id.clone(),
                version: None,
                dark: app.is_dark(),
            })),
        ]
        .spacing(XXS)
        .align_y(Alignment::Center);
        if let Some(open) = open_file(&tid, item.path.as_deref()) {
            entry = entry.push(w::icon_button(Icon::FileText, "View workspace file", Some(open)));
        }
        col = col.push(entry);
    }

    w::scroll(col).height(Length::Fill).into()
}

/// `OpenArtifactFile` for an artifact written to an HTML file in the workspace (the core reads only those).
fn open_file(thread_id: &str, path: Option<&str>) -> Option<Message> {
    let path = path.filter(|p| p.rsplit_once('.').is_some_and(|(_, ext)| ext.eq_ignore_ascii_case("html") || ext.eq_ignore_ascii_case("htm")))?;
    (!thread_id.is_empty()).then(|| core(Event::OpenArtifactFile { thread_id: thread_id.to_owned(), path: path.to_owned() }))
}

fn streaming_badge<'a>() -> Element<'a, Message> {
    w::with_tooltip(
        row![w::themed_icon(Icon::Loader, ICON_XS, |c| c.info), w::badge_capsule("Streaming", Some(|c| c.info))]
            .spacing(XXS)
            .align_y(Alignment::Center),
        "The agent is still writing this artifact",
    )
}

/// The workspace file an artifact references, read as text.
fn artifact_file_view<'a>(app: &'a App, preview: &'a FilePreviewView) -> Element<'a, Message> {
    let mut hdr = row![
        w::icon_button(Icon::ArrowLeft, "Back", Some(core(Event::CloseArtifactFile))),
        text(&preview.path).size(TEXT_SMALL).font(fonts::MONO).width(Length::Fill),
    ]
    .spacing(XS)
    .align_y(Alignment::Center);
    if preview.error.is_none() {
        hdr = hdr.push(w::icon_button(
            Icon::MessageSquare,
            "Mention in composer",
            Some(core(Event::AppendToDraft { text: format!("@{} ", preview.path) })),
        ));
        if !preview.loading {
            hdr = hdr.push(w::icon_button(Icon::Copy, "Copy contents", Some(Message::CopyText(preview.text.clone()))));
        }
    }
    files::preview_frame(hdr, files::preview_body(app, preview)).height(Length::Fill).into()
}

fn active_artifact_view<'a>(app: &'a App, art: &'a ActiveArtifactView) -> Element<'a, Message> {
    let is_code = art.mode == "code";
    let mut subtitle = row![w::muted(format!("{} · v{}", markdown::artifact_kind_label(&art.kind), art.version.max(1)), TEXT_CAPTION)]
        .spacing(XS)
        .align_y(Alignment::Center);
    if art.streaming {
        subtitle = subtitle.push(streaming_badge());
    }
    let header = row![
        w::icon_button(Icon::ArrowLeft, "Back to list", Some(core(Event::CloseArtifact))),
        column![
            text(if art.title.is_empty() { markdown::artifact_kind_label(&art.kind) } else { art.title.as_str() })
                .size(TEXT_UI)
                .font(fonts::SEMIBOLD),
            subtitle,
        ]
        .spacing(1.0)
        .width(Length::Fill),
        w::segmented(
            &[(false, Some(Icon::Eye), "Preview"), (true, Some(Icon::Code), "Code")],
            is_code,
            |code| core(Event::SetArtifactViewMode { mode: if code { "code" } else { "preview" }.into() }),
        ),
        w::icon_button(Icon::Copy, "Copy source", Some(Message::CopyText(art.source.clone()))),
    ]
    .spacing(XS)
    .align_y(Alignment::Center);

    let mut subbar = row![].spacing(XS).align_y(Alignment::Center);
    if art.versions.len() > 1 {
        for v in &art.versions {
            let active = v.version == art.version;
            subbar = subbar.push(
                button(text(format!("v{}", v.version)).size(TEXT_CAPTION))
                    .padding([2.0, TIGHT])
                    .style(w::chip_style(active))
                    .on_press(core(Event::SelectArtifactVersion { version: v.version, dark: app.is_dark() })),
            );
        }
    }
    subbar = subbar.push(Space::new().width(Length::Fill));
    let tid = app.vm.thread.as_ref().map(|t| t.id.as_str()).unwrap_or_default();
    if let Some(open) = open_file(tid, art.path.as_deref()) {
        subbar = subbar.push(w::icon_button(Icon::FileText, "View workspace file", Some(open)));
    }
    if let Some(url) = art.preview_url.as_ref().or(art.hosted_url.as_ref()) {
        subbar = subbar.push(w::icon_label_button(
            Icon::ExternalLink,
            "Open in Browser",
            w::Kind::Secondary,
            Some(Message::OpenUrl(url.clone())),
        ));
    }
    subbar = subbar.push(w::icon_button(
        Icon::RefreshCw,
        "Refresh preview",
        Some(core(Event::RefreshArtifactPreview { dark: app.is_dark() })),
    ));

    let body: Element<'a, Message> = if art.loading {
        w::empty_state(Icon::Loader, "Loading artifact…", "Fetching artifact contents.", None)
    } else if let Some(err) = &art.error {
        w::empty_state(Icon::CircleAlert, "Could not load artifact", err, None)
    } else if is_code || art.kind == "code" {
        w::scroll_both(container(w::code_spans_view(&art.spans, &art.source, app.is_dark(), TEXT_CODE)).padding(M))
            .height(Length::Fill)
            .into()
    } else {
        native_preview(app, art)
    };

    let iterate_bar = container(
        row![
            w::input("Ask the agent to edit this artifact…", &app.artifact_quick_edit)
                .on_input(Message::ArtifactQuickEditChanged)
                .on_submit(Message::SubmitArtifactQuickEdit)
                .width(Length::Fill),
            w::small_button(
                "Send",
                w::primary_style,
                (!app.artifact_quick_edit.trim().is_empty()).then_some(Message::SubmitArtifactQuickEdit),
            ),
        ]
        .spacing(XS)
        .align_y(Alignment::Center),
    )
    .padding(S)
    .style(w::surface);

    column![
        container(header).padding([S, M]).style(w::surface),
        container(subbar).padding([XS, M]).style(w::surface),
        w::divider(),
        container(body).width(Length::Fill).height(Length::Fill).style(w::page),
        w::divider(),
        iterate_bar,
    ]
    .height(Length::Fill)
    .into()
}

fn native_preview<'a>(app: &'a App, art: &'a ActiveArtifactView) -> Element<'a, Message> {
    match art.kind.as_str() {
        "svg" => {
            let handle = svg::Handle::from_memory(art.source.as_bytes().to_vec());
            container(svg(handle).width(Length::Fill).height(Length::Fill))
                .padding(L)
                .center(Length::Fill)
                .into()
        }
        "mermaid" => {
            if let Some(url) = art.preview_url.as_deref()
                && let Some(handle) = app.images.get(url) {
                    return w::scroll(container(image(handle.clone()).width(Length::Shrink)).center_x(Length::Fill).padding(L))
                        .height(Length::Fill)
                        .into();
                }
            w::scroll(
                column![
                    w::muted("Mermaid source (server diagram preview loading or unavailable):", TEXT_SMALL),
                    w::code_spans_view(&art.spans, &art.source, app.is_dark(), TEXT_CODE),
                ]
                .spacing(S)
                .padding(M),
            )
            .height(Length::Fill)
            .into()
        }
        "markdown" => w::scroll(container(text(&art.source).size(app.prefs.text_size)).padding(L))
            .height(Length::Fill)
            .into(),
        _ => {
            // HTML / React / Hosted interactive web artifacts: show native summary + Open in Browser + code preview.
            let open_url = art.preview_url.as_ref().or(art.hosted_url.as_ref()).cloned();
            let mut top = Column::new().spacing(S).push(
                row![
                    w::themed_icon(markdown::artifact_kind_icon(&art.kind), ICON_LARGE, |c| c.accent),
                    column![
                        text(format!("{} preview", markdown::artifact_kind_label(&art.kind))).size(TEXT_UI).font(fonts::SEMIBOLD),
                        w::muted("Interactive HTML/React artifacts open cleanly in your system browser; source is shown below.", TEXT_SMALL),
                    ]
                    .spacing(XXS)
                    .width(Length::Fill),
                ]
                .spacing(M)
                .align_y(Alignment::Center),
            );
            if let Some(url) = open_url {
                top = top.push(w::icon_label_button(
                    Icon::ExternalLink,
                    "Open Interactive Preview in Browser",
                    w::Kind::Primary,
                    Some(Message::OpenUrl(url)),
                ));
            }
            w::scroll(
                column![
                    w::card(top).width(Length::Fill),
                    container(w::code_spans_view(&art.spans, &art.source, app.is_dark(), TEXT_CODE))
                        .padding(M)
                        .width(Length::Fill)
                        .style(w::sunken),
                ]
                .spacing(M)
                .padding(M),
            )
            .height(Length::Fill)
            .into()
        }
    }
}
