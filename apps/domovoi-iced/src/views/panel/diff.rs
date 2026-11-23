//! Diff pane: source selector, file summary, collapsible file cards, and diff hunks.

use domovoi_core::Event;
use domovoi_core::view::DiffFileView;
use iced::widget::{Column, Space, button, column, container, row, text};
use iced::{Alignment, Background, Element, Length, Theme};

use crate::app::{App, Message};
use crate::ui::fonts;
use crate::ui::icons::Icon;
use crate::ui::theme::{alpha, colors};
use crate::ui::tokens::*;
use crate::ui::widgets as w;

fn core(e: Event) -> Message {
    Message::Core(Box::new(e))
}

pub fn view(app: &App) -> Element<'_, Message> {
    let diff = &app.vm.panel.diff;

    let mut source_row = row![].spacing(XS).align_y(Alignment::Center);
    for src in &diff.sources {
        let active = src.value == diff.selected_source;
        source_row = source_row.push(
            button(text(&src.label).size(TEXT_SMALL))
                .padding([4.0, S])
                .style(w::chip_style(active))
                .on_press(core(Event::SelectDiffSource { source: src.value.clone() })),
        );
    }
    source_row = source_row.push(Space::new().width(Length::Fill)).push(w::icon_button(
        Icon::RefreshCw,
        "Refresh diff",
        Some(core(Event::SelectDiffSource { source: diff.selected_source.clone() })),
    ));

    let body: Element<'_, Message> = if diff.loading {
        w::empty_state(Icon::Loader, "Loading diff…", "Fetching changes from the server.", None)
    } else if let Some(err) = &diff.error {
        w::empty_state(Icon::CircleAlert, "No diff available", err, None)
    } else if diff.files.is_empty() {
        w::empty_state(Icon::CircleCheck, "No changes", "Working tree is clean.", None)
    } else {
        let total_add: u32 = diff.files.iter().map(|f| f.additions).sum();
        let total_del: u32 = diff.files.iter().map(|f| f.deletions).sum();
        let mut col = Column::new().spacing(M).padding(M).push(
            row![
                w::muted(
                    format!("{} file{}", diff.files.len(), if diff.files.len() == 1 { "" } else { "s" }),
                    TEXT_SMALL,
                ),
                w::colored(format!("+{total_add}"), TEXT_SMALL, |c| c.success),
                w::colored(format!("−{total_del}"), TEXT_SMALL, |c| c.danger),
            ]
            .spacing(S),
        );
        for file in &diff.files {
            let expanded = !app.collapsed_diffs.contains(&file.path);
            col = col.push(diff_file_card(file, expanded));
        }
        if diff.truncated {
            col = col.push(w::colored("The diff was truncated.", TEXT_SMALL, |c| c.warning));
        }
        w::scroll(col).height(Length::Fill).into()
    };

    column![container(source_row).padding([S, M]), w::divider(), body].height(Length::Fill).into()
}

fn diff_file_card(file: &DiffFileView, expanded: bool) -> Element<'_, Message> {
    let title = match &file.old_path {
        Some(old) => format!("{old} → {}", file.path),
        None => file.path.clone(),
    };
    let chev = if expanded { Icon::ChevronDown } else { Icon::ChevronRight };
    let header = button(
        row![
            w::themed_icon(chev, ICON_XS, |c| c.muted),
            text(title).size(TEXT_SMALL).font(fonts::MONO).width(Length::Fill),
            if file.change != "modified" {
                w::badge_capsule(&file.change, None)
            } else {
                Space::new().into()
            },
            w::colored(format!("+{}", file.additions), TEXT_CAPTION, |c| c.success),
            w::colored(format!("−{}", file.deletions), TEXT_CAPTION, |c| c.danger),
        ]
        .spacing(S)
        .align_y(Alignment::Center),
    )
    .width(Length::Fill)
    .padding([S, M])
    .style(w::ghost_style)
    .on_press(Message::ToggleDiffFile(file.path.clone()));

    let mut col = Column::new().spacing(0.0).push(header);
    if expanded {
        col = col.push(w::divider()).push(diff_file_body(file, false));
    }
    container(col).width(Length::Fill).style(w::card_style).into()
}

/// Reusable diff body (used in Diff pane, inline tool rows, and approval cards).
pub fn diff_file_body<'a>(file: &'a DiffFileView, compact: bool) -> Element<'a, Message> {
    let mut col = Column::new().spacing(0.0);
    let max_lines = if compact { 60 } else { 2000 };
    let mut rendered = 0usize;

    for hunk in &file.hunks {
        if !hunk.header.is_empty() {
            col = col.push(
                container(w::colored(&hunk.header, TEXT_CAPTION, |c| c.info).font(fonts::MONO))
                    .padding([XS, S])
                    .width(Length::Fill)
                    .style(w::surface),
            );
        }
        for line in &hunk.lines {
            if rendered >= max_lines {
                col = col.push(container(w::faint("… more lines in Diff panel", TEXT_CAPTION)).padding([XS, S]));
                break;
            }
            rendered += 1;
            let (prefix, kind) = match line.kind.as_str() {
                "add" => ("+", 1u8),
                "del" => ("-", 2u8),
                "note" => (" ", 3u8),
                _ => (" ", 0u8),
            };
            let old_no = line.old_line.map(|n| format!("{n:>4}")).unwrap_or_else(|| "    ".into());
            let new_no = line.new_line.map(|n| format!("{n:>4}")).unwrap_or_else(|| "    ".into());
            let quote = format!("`{}:{}`: `{}`", file.path, line.new_line.or(line.old_line).unwrap_or(0), line.text);
            let line_row = button(
                row![
                    w::faint(format!("{old_no} {new_no}"), TEXT_CAPTION).font(fonts::MONO),
                    text(format!("{prefix}{}", line.text))
                        .size(TEXT_CODE)
                        .font(fonts::MONO)
                        .style(move |t: &Theme| {
                            let c = colors(t);
                            let color = match kind {
                                1 => c.success,
                                2 => c.danger,
                                3 => c.muted,
                                _ => c.text,
                            };
                            text::Style { color: Some(color) }
                        }),
                ]
                .spacing(S),
            )
            .padding([1.0, S])
            .width(Length::Fill)
            .style(move |t: &Theme, _status| {
                let c = colors(t);
                let bg = match kind {
                    1 => Some(Background::Color(alpha(c.success, if c.dark { 0.14 } else { 0.11 }))),
                    2 => Some(Background::Color(alpha(c.danger, if c.dark { 0.14 } else { 0.11 }))),
                    _ => None,
                };
                button::Style { background: bg, text_color: c.text, ..Default::default() }
            })
            .on_press(core(Event::AppendToDraft { text: quote }));
            col = col.push(line_row);
        }
        if rendered >= max_lines {
            break;
        }
    }

    container(col).width(Length::Fill).style(w::sunken).into()
}
