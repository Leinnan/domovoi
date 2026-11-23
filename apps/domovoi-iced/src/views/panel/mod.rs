//! Right side panel: Diff, Artifacts, Terminal, Files, and Git tabs.

pub mod artifacts;
pub mod diff;
pub mod files;
pub mod git;
pub mod terminal;

use domovoi_core::Event;
use iced::widget::{button, column, container, row, text};
use iced::{Alignment, Element, Length};

use crate::app::{App, Message};
use crate::ui::fonts;
use crate::ui::icons::Icon;
use crate::ui::tokens::*;
use crate::ui::widgets as w;

fn core(e: Event) -> Message {
    Message::Core(Box::new(e))
}

const TABS: [(&str, &str, Icon, &str); 5] = [
    ("diff", "Diff", Icon::Diff, "Mod+Shift+D"),
    ("artifacts", "Artifacts", Icon::Sparkles, "Mod+Shift+A"),
    ("terminal", "Terminal", Icon::Terminal, "Mod+Shift+T"),
    ("files", "Files", Icon::Folder, "Mod+Shift+E"),
    ("git", "Git", Icon::GitBranch, "Mod+Alt+G"),
];

pub fn view(app: &App) -> Element<'_, Message> {
    let panel = &app.vm.panel;
    let active_tab = if TABS.iter().any(|(id, _, _, _)| *id == panel.tab.as_str()) {
        panel.tab.as_str()
    } else {
        "diff"
    };

    let mut bar = row![].spacing(XXS).align_y(Alignment::Center);
    for (id, label, ic, shortcut) in TABS {
        let selected = active_tab == id;
        let tip = format!("{label} ({})", w::shortcut_label(shortcut));
        let btn = button(
            row![
                w::themed_icon(ic, ICON_SMALL, move |c| if selected { c.accent } else { c.muted }),
                text(label).size(TEXT_SMALL).font(if selected { fonts::SEMIBOLD } else { fonts::INTER }),
            ]
            .spacing(XS)
            .align_y(Alignment::Center),
        )
        .padding([TIGHT, S])
        .style(w::segment_style(selected))
        .on_press(core(Event::ShowPanel { tab: id.into() }));
        bar = bar.push(w::with_tooltip(btn, &tip));
    }

    let content: Element<'_, Message> = match active_tab {
        "artifacts" => artifacts::view(app),
        "terminal" => terminal::view(app),
        "files" => files::view(app),
        "git" => git::view(app),
        _ => diff::view(app),
    };

    column![
        container(bar).padding([TIGHT, S]).width(Length::Fill),
        w::divider(),
        container(content).width(Length::Fill).height(Length::Fill),
    ]
    .height(Length::Fill)
    .into()
}
