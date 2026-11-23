//! Native Iced Markdown block renderer for `domovoi_core::view::MarkdownBlock`
//! (used in assistant messages, plans, pull request descriptions, and Markdown artifacts).

use std::collections::HashMap;

use domovoi_core::view::{DiagramView, MarkdownBlock, MarkdownRun};
use iced::widget::{Column, Space, button, column, container, image, rich_text, row, span, text};
use iced::{Alignment, Background, Border, Element, Length, Padding, Theme};

use crate::ui::fonts;
use crate::ui::icons::Icon;
use crate::ui::theme::{alpha, colors};
use crate::ui::tokens::*;
use crate::ui::widgets as w;

/// Actions a rendered Markdown block can emit.
#[derive(Debug, Clone)]
pub enum MdAction {
    OpenUrl(String),
    OpenWorkspaceLink(String),
    CopyCode(String),
    OpenArtifact { artifact_id: String, code: bool },
    LoadDiagram { key: String, source: String },
}

pub fn render_blocks<'a, M: Clone + 'a>(
    blocks: &'a [MarkdownBlock],
    body_size: f32,
    dark: bool,
    diagrams: &'a [DiagramView],
    images: &'a HashMap<String, image::Handle>,
    map: impl Fn(MdAction) -> M + Copy + 'a,
) -> Element<'a, M> {
    let mut col = Column::new().spacing(S);
    let mut i = 0;
    while i < blocks.len() {
        let block = &blocks[i];
        if block.kind == "table_row" {
            let start = i;
            while i < blocks.len() && blocks[i].kind == "table_row" {
                i += 1;
            }
            col = col.push(render_table(&blocks[start..i], body_size, dark, map));
            continue;
        }
        col = col.push(render_block(block, body_size, dark, diagrams, images, map));
        i += 1;
    }
    col.width(Length::Fill).into()
}

fn render_block<'a, M: Clone + 'a>(
    block: &'a MarkdownBlock,
    body_size: f32,
    dark: bool,
    diagrams: &'a [DiagramView],
    images: &'a HashMap<String, image::Handle>,
    map: impl Fn(MdAction) -> M + Copy + 'a,
) -> Element<'a, M> {
    match block.kind.as_str() {
        "heading" => {
            let scale = match block.level {
                1 => 1.50,
                2 => 1.32,
                3 => 1.18,
                4 => 1.08,
                _ => 1.0,
            };
            let size = (body_size * scale).round();
            let heading_el: Element<'a, M> = if block.runs.is_empty() {
                text(block.text.clone()).size(size).font(fonts::SEMIBOLD).into()
            } else {
                render_runs(&block.runs, size, true, dark, map)
            };
            let mut c = Column::new().spacing(XS).push(heading_el);
            if block.level <= 2 {
                c = c.push(w::divider());
            }
            container(c).padding(Padding { top: XS, ..Padding::ZERO }).into()
        }
        "code" => render_code_block(block, body_size, dark, diagrams, images, map),
        "list_item" => {
            let indent = (block.level as f32) * INDENT;
            // A continuation is a further paragraph of the item above: no marker of its own, aligned with that item's text.
            let marker: Element<'a, M> = if block.continuation {
                Space::new().into()
            } else if let Some(checked) = block.task_checked {
                w::themed_icon(
                    if checked { Icon::CircleCheck } else { Icon::CircleDot },
                    ICON_SMALL,
                    move |c| if checked { c.success } else { c.muted },
                )
            } else if block.ordered {
                w::muted(format!("{}.", block.number.max(1)), body_size).font(fonts::MEDIUM).into()
            } else {
                w::muted("•", body_size).into()
            };
            let content = render_runs_or_text(&block.runs, &block.text, body_size, dark, map);
            let marker = container(marker).width(Length::Fixed((body_size * 1.4).round()));
            container(row![marker, container(content).width(Length::Fill)].spacing(XS).align_y(Alignment::Start))
                .padding(Padding { left: indent, ..Padding::ZERO })
                .into()
        }
        "quote" => {
            let content = render_runs_or_text(&block.runs, &block.text, body_size, dark, map);
            let depth = block.quote_depth.clamp(1, 4) as f32;
            container(
                row![
                    container(Space::new().width(3.0).height(Length::Fill)).style(|t: &Theme| container::Style {
                        background: Some(Background::Color(colors(t).border_strong)),
                        border: Border { radius: 2.0.into(), ..Border::default() },
                        ..Default::default()
                    }),
                    container(content).width(Length::Fill).padding([XS, S]),
                ]
                .spacing(S),
            )
            .padding(Padding { left: (depth - 1.0) * S, ..Padding::ZERO })
            .style(|t: &Theme| container::Style {
                background: Some(Background::Color(alpha(colors(t).surface, 0.5))),
                border: Border { radius: RADIUS_MEDIUM.into(), ..Border::default() },
                ..Default::default()
            })
            .into()
        }
        "rule" => container(w::divider()).padding([XS, 0.0]).into(),
        _ => render_runs_or_text(&block.runs, &block.text, body_size, dark, map),
    }
}

fn render_code_block<'a, M: Clone + 'a>(
    block: &'a MarkdownBlock,
    body_size: f32,
    dark: bool,
    diagrams: &'a [DiagramView],
    images: &'a HashMap<String, image::Handle>,
    map: impl Fn(MdAction) -> M + Copy + 'a,
) -> Element<'a, M> {
    if let Some(artifact_id) = block.artifact_id.as_ref() {
        let title = if block.artifact_title.is_empty() {
            artifact_kind_label(&block.artifact_kind).to_owned()
        } else {
            block.artifact_title.clone()
        };
        let subtitle = if !block.fence_closed {
            format!("Generating {}…", artifact_kind_label(&block.artifact_kind))
        } else if block.artifact_version > 1 {
            format!("{} · v{}", artifact_kind_label(&block.artifact_kind), block.artifact_version)
        } else {
            artifact_kind_label(&block.artifact_kind).to_owned()
        };
        let open_msg = map(MdAction::OpenArtifact { artifact_id: artifact_id.clone(), code: false });
        let code_msg = map(MdAction::OpenArtifact { artifact_id: artifact_id.clone(), code: true });
        return container(
            row![
                container(w::themed_icon(artifact_kind_icon(&block.artifact_kind), ICON, |c| c.accent))
                    .width(34.0)
                    .height(34.0)
                    .center_x(34.0)
                    .center_y(34.0)
                    .style(|t: &Theme| container::Style {
                        background: Some(Background::Color(colors(t).accent_soft)),
                        border: Border { radius: RADIUS_CHIP.into(), ..Border::default() },
                        ..Default::default()
                    }),
                column![
                    text(title).size(TEXT_UI).font(fonts::SEMIBOLD),
                    w::muted(subtitle, TEXT_SMALL),
                ]
                .spacing(XXS)
                .width(Length::Fill),
                w::small_button("Code", w::ghost_style, Some(code_msg)),
                w::small_button("Open", w::secondary_style, Some(open_msg)),
            ]
            .spacing(M)
            .align_y(Alignment::Center),
        )
        .padding(M)
        .width(Length::Fill)
        .style(w::card_style)
        .into();
    }

    // Mermaid diagram blocks: render server-drawn PNG if cached, otherwise offer/show source.
    if block.language.eq_ignore_ascii_case("mermaid") && block.fence_closed {
        let key = diagram_key(&block.text, dark);
        if let Some(diag) = diagrams.iter().find(|d| d.key == key) {
            if let Some(url) = diag.url.as_deref() {
                if let Some(handle) = images.get(url) {
                    return container(
                        column![
                            row![
                                w::muted("Mermaid diagram", TEXT_CAPTION).font(fonts::SEMIBOLD).width(Length::Fill),
                                w::small_button("Copy source", w::ghost_style, Some(map(MdAction::CopyCode(block.text.clone())))),
                            ]
                            .align_y(Alignment::Center),
                            container(image(handle.clone()).width(Length::Shrink)).center_x(Length::Fill).padding(S),
                        ]
                        .spacing(XS),
                    )
                    .padding(M)
                    .width(Length::Fill)
                    .style(w::card_style)
                    .into();
                }
            } else if diag.pending {
                return container(w::muted("Rendering Mermaid diagram…", TEXT_SMALL))
                    .padding(M)
                    .width(Length::Fill)
                    .style(w::card_style)
                    .into();
            }
        } else {
            let load_msg = map(MdAction::LoadDiagram { key, source: block.text.clone() });
            return container(
                column![
                    row![
                        w::muted("mermaid", TEXT_CAPTION).font(fonts::MONO_BOLD).width(Length::Fill),
                        w::small_button("Render diagram", w::secondary_style, Some(load_msg)),
                        w::small_button("Copy", w::ghost_style, Some(map(MdAction::CopyCode(block.text.clone())))),
                    ]
                    .spacing(XS)
                    .align_y(Alignment::Center),
                    w::code_spans_view(&block.spans, &block.text, dark, (body_size - 1.5).max(11.0)),
                ]
                .spacing(S),
            )
            .padding(M)
            .width(Length::Fill)
            .style(w::card_style)
            .into();
        }
    }

    let lang = if block.language.is_empty() { "code" } else { block.language.as_str() };
    let code_size = (body_size - 1.5).max(11.0);
    let header = row![
        w::muted(lang.to_owned(), TEXT_CAPTION).font(fonts::MONO_BOLD).width(Length::Fill),
        button(
            row![w::themed_icon(Icon::Copy, ICON_XS, |c| c.muted), w::muted("Copy", TEXT_CAPTION)]
                .spacing(XS)
                .align_y(Alignment::Center)
        )
        .padding([2.0, TIGHT])
        .style(w::ghost_style)
        .on_press(map(MdAction::CopyCode(block.text.clone()))),
    ]
    .align_y(Alignment::Center);

    let body = w::code_spans_view(&block.spans, &block.text, dark, code_size);
    container(column![header, w::divider(), container(body).padding([S, M])].spacing(0.0))
        .width(Length::Fill)
        .style(|t: &Theme| {
            let c = colors(t);
            container::Style {
                background: Some(Background::Color(c.sunken)),
                text_color: Some(c.text),
                border: Border { radius: RADIUS_CHIP.into(), width: 1.0, color: c.border },
                ..Default::default()
            }
        })
        .into()
}

fn render_table<'a, M: Clone + 'a>(
    rows: &'a [MarkdownBlock],
    body_size: f32,
    dark: bool,
    map: impl Fn(MdAction) -> M + Copy + 'a,
) -> Element<'a, M> {
    let mut col = Column::new().spacing(0.0);
    for (idx, r) in rows.iter().enumerate() {
        let mut line = row![].spacing(S);
        let cell_count = r.cell_runs.len().max(r.cells.len()).max(1);
        for c_idx in 0..cell_count {
            let cell_el: Element<'a, M> = if let Some(runs) = r.cell_runs.get(c_idx).filter(|v| !v.is_empty()) {
                render_runs(runs, body_size - 1.0, r.header, dark, map)
            } else {
                let txt = r.cells.get(c_idx).cloned().unwrap_or_default();
                text(txt)
                    .size(body_size - 1.0)
                    .font(if r.header { fonts::SEMIBOLD } else { fonts::INTER })
                    .into()
            };
            let align = match r.alignments.get(c_idx).map(String::as_str) {
                Some("right") => Alignment::End,
                Some("center") => Alignment::Center,
                _ => Alignment::Start,
            };
            line = line.push(container(cell_el).width(Length::FillPortion(1)).padding([TIGHT, S]).align_x(align));
        }
        let is_header = r.header;
        col = col.push(
            container(line).width(Length::Fill).style(move |t: &Theme| {
                let c = colors(t);
                let bg = if is_header {
                    Some(Background::Color(c.surface))
                } else if idx % 2 == 1 {
                    Some(Background::Color(alpha(c.surface, 0.45)))
                } else {
                    None
                };
                container::Style { background: bg, text_color: Some(c.text), ..Default::default() }
            }),
        );
        if idx + 1 < rows.len() {
            col = col.push(w::divider());
        }
    }
    container(col)
        .width(Length::Fill)
        .style(|t: &Theme| {
            let c = colors(t);
            container::Style {
                background: Some(Background::Color(c.raised)),
                border: Border { radius: RADIUS_CHIP.into(), width: 1.0, color: c.border },
                ..Default::default()
            }
        })
        .into()
}

fn render_runs_or_text<'a, M: Clone + 'a>(
    runs: &'a [MarkdownRun],
    fallback: &'a str,
    size: f32,
    dark: bool,
    map: impl Fn(MdAction) -> M + Copy + 'a,
) -> Element<'a, M> {
    if runs.is_empty() {
        text(fallback.to_owned()).size(size).into()
    } else {
        render_runs(runs, size, false, dark, map)
    }
}

fn render_runs<'a, M: Clone + 'a>(
    runs: &'a [MarkdownRun],
    size: f32,
    force_bold: bool,
    dark: bool,
    map: impl Fn(MdAction) -> M + Copy + 'a,
) -> Element<'a, M> {
    let h = if dark { &crate::ui::theme::DARK } else { &crate::ui::theme::LIGHT };
    let mut spans = Vec::with_capacity(runs.len());
    for r in runs {
        let action = if let Some(ws) = &r.workspace_link {
            Some(MdAction::OpenWorkspaceLink(ws.clone()))
        } else {
            r.link.as_ref().map(|u| MdAction::OpenUrl(u.clone()))
        };
        let mut s = span::<MdAction, _>(r.text.clone());
        if r.code {
            s = s.font(fonts::MONO).background(alpha(h.surface, 0.95)).color(h.accent);
        } else {
            s = s.font(fonts::styled(force_bold || r.bold, r.italic));
        }
        if r.strike {
            s = s.strikethrough(true);
        }
        if let Some(act) = action {
            s = s.color(h.accent).underline(true).link(act);
        }
        spans.push(s);
    }
    rich_text(spans).size(size).on_link_click(map).into()
}

pub fn diagram_key(source: &str, dark: bool) -> String {
    let hash = source.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |acc, b| (acc ^ u64::from(b)).wrapping_mul(0x0000_0100_0000_01b3));
    format!("{:016x}:{}", hash, if dark { "d" } else { "l" })
}

pub fn artifact_kind_label(kind: &str) -> &'static str {
    match kind {
        "react" => "Interactive React",
        "svg" => "SVG graphic",
        "mermaid" => "Mermaid diagram",
        "markdown" => "Document",
        "hosted" => "Claude artifact",
        "code" => "Code",
        _ => "Interactive HTML",
    }
}

pub fn artifact_kind_icon(kind: &str) -> Icon {
    match kind {
        "react" => Icon::Sparkles,
        "svg" => Icon::Image,
        "mermaid" => Icon::Waypoints,
        "markdown" => Icon::FileText,
        "hosted" => Icon::Globe,
        "code" => Icon::Code,
        _ => Icon::LayoutDashboard,
    }
}
