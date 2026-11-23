//! Top-anchored Command Palette (`Commands`, `Go to File`, `Search in Project`).

use domovoi_core::Event;
use domovoi_core::view::PaletteView;
use iced::widget::{Column, Space, button, column, container, row, text};
use iced::{Alignment, Element, Length, Padding};

use crate::app::{App, Message};
use crate::ui::fonts;
use crate::ui::icons::Icon;
use crate::ui::tokens::*;
use crate::ui::widgets as w;

fn core(e: Event) -> Message {
    Message::Core(Box::new(e))
}

pub fn view<'a>(_app: &'a App, pal: &'a PaletteView) -> Element<'a, Message> {
    let searches_files = pal.mode != "commands";
    let mode_title = match pal.mode.as_str() {
        "files" => "Go to File",
        "content" => "Search in Project",
        _ => "Commands",
    };

    let mut top = row![].spacing(S).align_y(Alignment::Center);
    if searches_files {
        top = top.push(
            button(
                row![w::themed_icon(Icon::ChevronRight, ICON_XS, |c| c.muted), text(mode_title).size(TEXT_SMALL).font(fonts::MEDIUM)]
                    .spacing(XS)
                    .align_y(Alignment::Center),
            )
            .padding([3.0, S])
            .style(w::secondary_style)
            .on_press(core(Event::SetPaletteMode { mode: "commands".into() })),
        );
    } else {
        top = top.push(w::themed_icon(Icon::Search, ICON, |c| c.muted));
    }

    let placeholder = if pal.placeholder.is_empty() { "Search commands, threads, files…" } else { pal.placeholder.as_str() };
    top = top.push(
        w::input(placeholder, &pal.query)
            .on_input(|q| core(Event::SetPaletteQuery { query: q }))
            .on_submit(core(Event::RunPaletteItem { id: String::new() }))
            .width(Length::Fill),
    );
    if pal.searching {
        top = top.push(w::muted("Searching…", TEXT_CAPTION));
    }

    let mut list = Column::new().spacing(XXS);
    if pal.items.is_empty() {
        let note = if pal.searching {
            "Searching…".into()
        } else if pal.query.is_empty() {
            format!("Type to search {mode_title}")
        } else {
            format!("No matches for “{}”", pal.query)
        };
        list = list.push(container(w::faint(note, TEXT_UI)).padding([M, S]));
    } else {
        for (idx, item) in pal.items.iter().take(40).enumerate() {
            let selected = idx == pal.highlighted as usize;
            let ic = match item.kind.as_str() {
                "thread" => Icon::MessageSquare,
                "project" => Icon::Folder,
                "file" => Icon::FileText,
                "match" => Icon::Search,
                _ => Icon::Command,
            };
            let mut r = row![
                w::themed_icon(ic, ICON_SMALL, move |c| if selected { c.accent } else { c.muted }),
                column![
                    text(&item.title)
                        .size(TEXT_UI)
                        .font(if item.kind == "match" { fonts::MONO } else { fonts::MEDIUM }),
                    if !item.subtitle.is_empty() {
                        w::muted(&item.subtitle, TEXT_CAPTION)
                    } else {
                        w::muted("", TEXT_CAPTION)
                    },
                ]
                .spacing(1.0)
                .width(Length::Fill),
            ]
            .spacing(S)
            .align_y(Alignment::Center);

            if !item.shortcut.is_empty() {
                r = r.push(w::kbd(&item.shortcut));
            }

            list = list.push(
                w::list_row(r, selected, !item.subtitle.is_empty())
                    .on_press(core(Event::RunPaletteItem { id: item.id.clone() })),
            );
        }
    }

    let hints = row![
        w::small_button("Commands", w::ghost_style, Some(core(Event::SetPaletteMode { mode: "commands".into() }))),
        w::small_button("Files", w::ghost_style, Some(core(Event::SetPaletteMode { mode: "files".into() }))),
        w::small_button("Content", w::ghost_style, Some(core(Event::SetPaletteMode { mode: "content".into() }))),
        Space::new().width(Length::Fill),
        w::kbd("↵"),
        w::faint("run", TEXT_CAPTION),
        w::kbd("Esc"),
        w::faint("close", TEXT_CAPTION),
    ]
    .spacing(XS)
    .align_y(Alignment::Center);

    container(
        column![
            top,
            w::divider(),
            w::scroll(list).height(Length::Shrink),
            w::divider(),
            container(hints).padding(Padding { top: XS, left: XS, right: XS, bottom: 0.0 }),
        ]
        .spacing(S),
    )
    .width(620)
    .max_height(500)
    .padding(M)
    .style(w::popover_style)
    .into()
}
