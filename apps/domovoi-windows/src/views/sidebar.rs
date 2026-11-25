//! The thread list: by status (Inbox, Running, Inactive, Archived) or under
//! each project, as the core groups it, with the server's name on top.

use domovoi_core::Event;
use domovoi_core::view::{ConnectionView, SidebarView, ThreadRowView};
use windows_reactor::{
    Border, Button, ButtonStyle, FontWeight, Grid, GridLength, HorizontalAlignment, Orientation, ScrollBarVisibility, ScrollViewer, StackPanel, TextBlock,
    TextTrimming, Thickness, VerticalAlignment, View,
};

use crate::core_link::CoreLink;
use crate::theme;

pub fn sidebar(core: &CoreLink, connection: &ConnectionView, sidebar: &SidebarView) -> View {
    let mut parts: Vec<View> = vec![header(core, connection)];
    if sidebar.grouping == "project" {
        for project in &sidebar.projects {
            let toggle = {
                let (core, id) = (core.clone(), project.id.clone());
                move || core.send(Event::ToggleProject { project_id: id.clone() })
            };
            parts.push(heading(&project.title, None, project.expanded, toggle));
            if project.expanded {
                parts.extend(project.threads.iter().map(|row| thread_row(core, row, sidebar.selected_thread_id.as_deref())));
            }
        }
    } else {
        for section in &sidebar.sections {
            let toggle = {
                let (core, kind) = (core.clone(), section.kind.clone());
                move || core.send(Event::ToggleSection { section: kind.clone() })
            };
            parts.push(heading(&section.title, Some(section.count), section.expanded, toggle));
            parts.extend(section.rows.iter().map(|row| thread_row(core, row, sidebar.selected_thread_id.as_deref())));
        }
    }
    if parts.len() == 1 {
        parts.push(TextBlock::new().text("No threads yet").opacity(0.6).margin(Thickness::uniform(12.0)).into());
    }
    ScrollViewer::new()
        .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
        .horizontal_scroll_bar_visibility(ScrollBarVisibility::Disabled)
        .content(StackPanel::new().orientation(Orientation::Vertical).spacing(1.0).margin(Thickness::uniform(6.0)).children(parts))
        .into()
}

/// The server's name and state, and the button for a new thread.
fn header(core: &CoreLink, connection: &ConnectionView) -> View {
    let state = match connection.state.as_str() {
        "connected" if connection.remote => format!("Connected over {}", if connection.path == "direct" { "a direct link" } else { "a relay" }),
        "connected" => "Connected".to_owned(),
        "reconnecting" if connection.reconnect_in_seconds > 0 => format!("Reconnecting in {} s", connection.reconnect_in_seconds),
        other => capitalized(other),
    };
    let new_thread = {
        let core = core.clone();
        Button::new()
            .style(ButtonStyle::Subtle)
            .automation_name("New thread")
            .content(theme::glyph("\u{E710}", 14.0))
            .on_click(move || core.send(Event::NewThread { project_id: None }))
            .grid_column(1)
    };
    let label = if connection.environment_label.is_empty() { "Domovoi" } else { connection.environment_label.as_str() };
    Grid::new()
        .columns([GridLength::STAR, GridLength::Auto])
        .margin(Thickness::new(6.0, 4.0, 0.0, 10.0))
        .children((
            StackPanel::new().orientation(Orientation::Vertical).children(vec![
                TextBlock::new().text(label).font_weight(FontWeight::SEMI_BOLD).text_trimming(TextTrimming::CharacterEllipsis).into(),
                TextBlock::new().text(state).font_size(theme::CAPTION).opacity(0.7).into(),
            ]),
            new_thread,
        ))
        .into()
}

/// A section's or project's title; pressed, it folds or unfolds.
fn heading(title: &str, count: Option<u32>, expanded: bool, toggle: impl Fn() + 'static) -> View {
    let chevron = if expanded { "\u{E70D}" } else { "\u{E76C}" };
    let text = match count {
        Some(n) => format!("{title}  {n}"),
        None => title.to_owned(),
    };
    Button::new()
        .style(ButtonStyle::Subtle)
        .horizontal_alignment(HorizontalAlignment::Stretch)
        .horizontal_content_alignment(HorizontalAlignment::Left)
        .margin(Thickness::new(0.0, 6.0, 0.0, 0.0))
        .automation_name(title)
        .content(StackPanel::new().orientation(Orientation::Horizontal).spacing(6.0).children(vec![
            theme::glyph(chevron, 10.0).opacity(0.7).into(),
            TextBlock::new().text(text).font_size(theme::CAPTION).font_weight(FontWeight::SEMI_BOLD).opacity(0.8).into(),
        ]))
        .on_click(toggle)
        .into()
}

fn thread_row(core: &CoreLink, row: &ThreadRowView, selected: Option<&str>) -> View {
    let open = {
        let (core, id) = (core.clone(), row.id.clone());
        move || core.send(Event::SelectThread { thread_id: id.clone() })
    };
    let marker = match row.status.as_str() {
        "working" => Some(("\u{E916}", theme::accent())),
        "approval" => Some(("\u{E7BA}", theme::accent())),
        "failed" => Some(("\u{E783}", theme::critical())),
        _ if row.unread => Some(("\u{E91F}", theme::accent())),
        _ => None,
    };
    let right = if row.status_label.is_empty() { row.age.clone() } else { row.status_label.clone() };
    let subtitle = if row.project_title.is_empty() { row.model_label.clone() } else { format!("{} · {}", row.project_title, row.model_label) };
    let body = Grid::new()
        .columns([GridLength::Pixel(18.0), GridLength::STAR, GridLength::Auto])
        .column_spacing(6.0)
        .children((
            match marker {
                Some((code, brush)) => Border::new().vertical_alignment(VerticalAlignment::Top).margin(Thickness::new(0.0, 3.0, 0.0, 0.0)).content(theme::glyph(code, 10.0).foreground(brush)),
                None => Border::new(),
            },
            StackPanel::new().grid_column(1).orientation(Orientation::Vertical).children(vec![
                TextBlock::new()
                    .text(&row.title)
                    .font_weight(if row.unread { FontWeight::SEMI_BOLD } else { FontWeight::NORMAL })
                    .text_trimming(TextTrimming::CharacterEllipsis)
                    .into(),
                TextBlock::new().text(subtitle).font_size(theme::CAPTION).opacity(0.65).text_trimming(TextTrimming::CharacterEllipsis).into(),
            ]),
            TextBlock::new().grid_column(2).text(right).font_size(theme::CAPTION).opacity(0.65).vertical_alignment(VerticalAlignment::Top),
        ));
    let button = Button::new()
        .style(ButtonStyle::Subtle)
        .horizontal_alignment(HorizontalAlignment::Stretch)
        .horizontal_content_alignment(HorizontalAlignment::Stretch)
        .automation_name(&row.title)
        .content(body)
        .on_click(open);
    let button = if selected == Some(row.id.as_str()) { button.background(theme::selected()) } else { button };
    button.into()
}

fn capitalized(word: &str) -> String {
    let mut chars = word.chars();
    chars.next().map(|c| c.to_uppercase().chain(chars).collect()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    #[test]
    fn states_read_as_words() {
        assert_eq!(super::capitalized("disconnected"), "Disconnected");
        assert_eq!(super::capitalized(""), "");
    }
}
