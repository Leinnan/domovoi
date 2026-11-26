//! Settings the connected server holds: its projects, who may reach it (and
//! pairing another device), archived threads, and its upkeep.

use domovoi_core::Event;
use domovoi_core::view::SettingsView;
use windows_reactor::{
    Border, Button, ButtonStyle, FontWeight, Grid, GridLength, HorizontalAlignment, Image, InfoBar, InfoBarSeverity, Menu, MenuExt as _, MenuItem, Orientation,
    ProgressRing, StackPanel, Stretch, TextBlock, TextTrimming, TextWrapping, Thickness, ToggleSwitch, VerticalAlignment, View,
};

use super::{Hooks, Shown, caption, card, empty, heading};
use crate::core_link::CoreLink;
use crate::theme;

/// A button that tells the core `event`.
fn tell(core: &CoreLink, label: &str, event: Event) -> Button {
    let core = core.clone();
    Button::new().content(TextBlock::new().text(label)).on_click(move || core.send(event.clone()))
}

/// A name with a line under it, trimmed to one line each.
fn named(title: &str, under: &str) -> StackPanel {
    let mut lines: Vec<View> = vec![TextBlock::new().text(title).font_weight(FontWeight::SEMI_BOLD).text_trimming(TextTrimming::CharacterEllipsis).into()];
    if !under.is_empty() {
        lines.push(caption(under).text_wrapping(TextWrapping::NoWrap).text_trimming(TextTrimming::CharacterEllipsis).into());
    }
    StackPanel::new().orientation(Orientation::Vertical).vertical_alignment(VerticalAlignment::Center).children(lines)
}

pub fn projects(shown: &Shown<'_>, hooks: &Hooks<'_>) -> Vec<View> {
    let rows: Vec<View> = shown
        .settings
        .projects
        .iter()
        .map(|project| {
            let id = &project.id;
            let title = if project.is_scratch { "No project (scratch)" } else { project.title.as_str() };
            let threads = match project.thread_count {
                0 => "No threads".to_owned(),
                1 => "1 thread".to_owned(),
                n => format!("{n} threads"),
            };
            // An icon picked is on its way to the server.
            let threads = if project.icon.saving { format!("{threads} · saving the icon…") } else { threads };
            let icon = {
                let (on_menu, key) = (hooks.on_menu.clone(), format!("icon:{id}"));
                Button::new()
                    .style(ButtonStyle::Subtle)
                    .automation_name(format!("Change the icon of {title}"))
                    .is_enabled(!project.is_scratch)
                    .content(shown.icons.tile(&project.icon, 32.0))
                    .on_click(move || on_menu.call(key.as_str().into()))
            };
            let mut parts: Vec<View> = vec![
                icon.into(),
                named(title, &project.workspace_root).grid_column(1).into(),
                caption(&threads).grid_column(2).vertical_alignment(VerticalAlignment::Center).into(),
            ];
            if !project.is_scratch {
                let menu = Menu::new(
                    [
                        MenuItem::item(format!("icon:{id}"), "Change Icon…"),
                        MenuItem::item(format!("rename:{id}"), "Rename…"),
                        MenuItem::separator("separator"),
                        MenuItem::item(format!("remove:{id}"), "Remove Project…"),
                    ],
                    hooks.on_menu.clone(),
                );
                parts.push(Button::new().grid_column(3).style(ButtonStyle::Subtle).automation_name(format!("More for {title}")).content(theme::glyph("\u{E712}", 14.0)).menu(menu));
            }
            Grid::new().columns([GridLength::Auto, GridLength::STAR, GridLength::Auto, GridLength::Auto]).column_spacing(12.0).children(parts).into()
        })
        .collect();
    if rows.is_empty() { vec![empty("No projects yet. Add one from the new-thread page.")] } else { vec![card(rows)] }
}

/// The link's QR code as an SVG: dark squares on white, with the quiet edge readers need.
pub fn qr_svg(link: &str) -> Option<String> {
    let code = qrcode::QrCode::new(link.as_bytes()).ok()?;
    let (width, colors) = (code.width(), code.to_colors());
    const QUIET: usize = 4;
    let side = width + 2 * QUIET;
    let mut svg = format!(r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {side} {side}" shape-rendering="crispEdges"><rect width="{side}" height="{side}" fill="#ffffff"/><path fill="#000000" d=""##);
    for (index, _) in colors.iter().enumerate().filter(|(_, colour)| **colour == qrcode::Color::Dark) {
        let (x, y) = (index % width + QUIET, index / width + QUIET);
        svg.push_str(&format!("M{x} {y}h1v1h-1z"));
    }
    svg.push_str(r#""/></svg>"#);
    Some(svg)
}

pub fn access(core: &CoreLink, shown: &Shown<'_>, hooks: &Hooks<'_>) -> Vec<View> {
    let remote = &shown.settings.remote;
    let mut out: Vec<View> = vec![heading("Remote access")];
    if remote.available {
        let state = match remote.state.as_str() {
            "online" => "Online: reachable from anywhere",
            "lanOnly" => "On this network only",
            "starting" => "Starting…",
            "error" => "Not working",
            "off" => "Off",
            _ if remote.running => "On",
            _ => "Off",
        };
        let toggle = {
            let core = core.clone();
            let enabled = remote.enabled;
            ToggleSwitch::new()
                .min_width(0.0)
                .is_on(enabled)
                .on_content(TextBlock::new())
                .off_content(TextBlock::new())
                .automation_name("Reach this server from other devices")
                .is_enabled(!remote.busy)
                .on_toggled(move |on: bool| {
                    if on != enabled {
                        core.send(Event::SetRemoteAccess { enabled: on });
                    }
                })
        };
        let mut lines: Vec<View> = vec![
            Grid::new()
                .columns([GridLength::STAR, GridLength::Auto])
                .column_spacing(16.0)
                .children((
                    named("Reach this server from other devices", "Over an encrypted link (iroh), with no port to open. Each device is paired first.").vertical_alignment(VerticalAlignment::Center),
                    StackPanel::new().grid_column(1).orientation(Orientation::Horizontal).spacing(8.0).children(vec![ProgressRing::new().is_active(remote.busy).width(16.0).height(16.0).into(), toggle.into()]),
                ))
                .into(),
            fact("State", state),
        ];
        if !remote.endpoint_id.is_empty() {
            lines.push(fact("Endpoint", &remote.endpoint_id));
        }
        let relay = if remote.home_relay.is_empty() { remote.relays.join(", ") } else { remote.home_relay.clone() };
        if !relay.is_empty() {
            lines.push(fact("Relay", &relay));
        }
        if let Some(error) = &remote.error {
            lines.push(InfoBar::new().is_open(true).is_closable(false).severity(InfoBarSeverity::Error).message(error).into());
        }
        out.push(card(lines));

        out.push(heading("Pair a device"));
        let mut pair: Vec<View> = vec![caption("A link another device opens once to pair with this server: scan it with a phone, or paste it into Domovoi on another computer.").into()];
        if let Some((_, qr)) = shown.state.qr.as_ref().filter(|(of, _)| *of == remote.link) {
            let copy = |label: &str, text: &str| -> View {
                let (on_copy, text) = (hooks.on_copy.clone(), text.to_owned());
                Button::new().content(TextBlock::new().text(label)).on_click(move || on_copy.call(text.clone())).into()
            };
            let mut beside: Vec<View> = vec![fact("Code", &remote.code)];
            if !remote.link_expires_at.is_empty() {
                beside.push(fact("Expires", &remote.link_expires_at));
            }
            beside.push(StackPanel::new().orientation(Orientation::Horizontal).spacing(8.0).children(vec![copy("Copy Link", &remote.link), copy("Copy Code", &remote.code)]).into());
            pair.push(
                StackPanel::new()
                    .orientation(Orientation::Horizontal)
                    .spacing(20.0)
                    .children(vec![
                        Border::new().width(180.0).height(180.0).corner_radius(8.0).content(Image::new().source_svg(qr.clone()).stretch(Stretch::Uniform).width(180.0).height(180.0)).into(),
                        StackPanel::new().orientation(Orientation::Vertical).spacing(10.0).vertical_alignment(VerticalAlignment::Center).min_width(260.0).children(beside).into(),
                    ])
                    .into(),
            );
        }
        pair.push(
            tell(core, if remote.link.is_empty() { "Create Pairing Link" } else { "New Link" }, Event::CreatePairingLink)
                .horizontal_alignment(HorizontalAlignment::Left)
                .is_enabled(!remote.busy)
                .into(),
        );
        out.push(card(pair));
    } else {
        out.push(empty("This server has no remote access (a Domovoi server does)."));
    }

    out.push(heading("Paired devices and sessions"));
    let devices: Vec<View> = shown
        .settings
        .access
        .iter()
        .map(|entry| {
            let glyph = match entry.device_type.as_str() {
                "mobile" => "\u{E8EA}",
                "tablet" => "\u{E70A}",
                _ => "\u{E7F8}",
            };
            let mut marks: Vec<View> = Vec::new();
            if entry.current {
                marks.push(caption("This device").into());
            } else if entry.connected {
                marks.push(caption("Online").into());
            }
            if !entry.current {
                marks.push(tell(core, "Revoke", Event::RevokeClient { session_id: entry.id.clone() }).automation_name(format!("Revoke {}", entry.label)).into());
            }
            Grid::new()
                .columns([GridLength::Auto, GridLength::STAR, GridLength::Auto])
                .column_spacing(12.0)
                .children((
                    theme::glyph(glyph, 18.0).opacity(0.8),
                    named(&entry.label, &entry.detail).grid_column(1),
                    StackPanel::new().grid_column(2).orientation(Orientation::Horizontal).spacing(10.0).vertical_alignment(VerticalAlignment::Center).children(marks),
                ))
                .into()
        })
        .collect();
    out.push(if devices.is_empty() { empty("No paired devices.") } else { card(devices) });
    out
}

fn fact(label: &str, value: &str) -> View {
    Grid::new()
        .columns([GridLength::Pixel(120.0), GridLength::STAR])
        .children((caption(label), TextBlock::new().grid_column(1).text(value).text_wrapping(TextWrapping::Wrap).is_text_selection_enabled(true)))
        .into()
}

pub fn archived(core: &CoreLink, settings: &SettingsView, hooks: &Hooks<'_>) -> Vec<View> {
    let rows: Vec<View> = settings
        .archived
        .iter()
        .map(|thread| {
            let under = [thread.project_title.as_str(), thread.age.as_str()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ");
            let delete = {
                let (on_menu, key) = (hooks.on_menu.clone(), format!("thread-delete:{}", thread.id));
                Button::new().automation_name(format!("Delete {}", thread.title)).content(TextBlock::new().text("Delete…")).on_click(move || on_menu.call(key.as_str().into()))
            };
            Grid::new()
                .columns([GridLength::STAR, GridLength::Auto])
                .column_spacing(12.0)
                .children((
                    named(&thread.title, &under),
                    StackPanel::new().grid_column(1).orientation(Orientation::Horizontal).spacing(8.0).vertical_alignment(VerticalAlignment::Center).children(vec![
                        tell(core, "Unarchive", Event::ArchiveThread { thread_id: thread.id.clone(), archived: false }).automation_name(format!("Unarchive {}", thread.title)).into(),
                        delete.into(),
                    ]),
                ))
                .into()
        })
        .collect();
    if rows.is_empty() { vec![empty("No archived threads.")] } else { vec![card(rows)] }
}

pub fn maintenance(core: &CoreLink, settings: &SettingsView) -> Vec<View> {
    let storage = &settings.storage;
    let or_dash = |text: &str| if text.is_empty() { "—".to_owned() } else { text.to_owned() };
    let mut out: Vec<View> = vec![
        heading("Storage"),
        card(vec![
            fact("Database", &or_dash(&storage.database_size)),
            fact("Reclaimable", &or_dash(&storage.reclaimable)),
            fact("Last cleanup", &or_dash(&storage.last_cleanup)),
            StackPanel::new()
                .orientation(Orientation::Horizontal)
                .spacing(8.0)
                .children(vec![
                    tell(core, "Clean Up Now", Event::RunStorageCleanup).is_enabled(!storage.busy).into(),
                    tell(core, "Reclaim Database Space", Event::ReclaimDatabase).is_enabled(!storage.busy).into(),
                    ProgressRing::new().is_active(storage.busy).width(16.0).height(16.0).into(),
                ])
                .into(),
        ]),
        heading("Diagnostics"),
    ];
    let diagnostics = &settings.diagnostics;
    let mut lines: Vec<View> = vec![fact("CPU", &or_dash(&diagnostics.cpu)), fact("Memory", &or_dash(&diagnostics.memory))];
    for process in &diagnostics.processes {
        lines.push(
            Grid::new()
                .columns([GridLength::STAR, GridLength::Pixel(70.0), GridLength::Pixel(90.0)])
                .column_spacing(8.0)
                .margin(Thickness::new(16.0 * f64::from(process.depth), 0.0, 0.0, 0.0))
                .children((
                    TextBlock::new().text(format!("{}  {}", process.pid, process.command)).font_family(theme::MONO).font_size(theme::CAPTION).text_trimming(TextTrimming::CharacterEllipsis),
                    caption(&process.cpu).grid_column(1).horizontal_alignment(HorizontalAlignment::Right),
                    caption(&process.memory).grid_column(2).horizontal_alignment(HorizontalAlignment::Right),
                ))
                .into(),
        );
    }
    out.push(card(lines));
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_link_draws_as_squares_inside_a_quiet_edge() {
        let svg = super::qr_svg("domovoi://connect?ticket=abc").expect("a short link fits a QR code");
        assert!(svg.starts_with("<svg") && svg.ends_with("</svg>"));
        // The corner finder starts four squares in, past the quiet edge.
        assert!(svg.contains("M4 4h1v1h-1z"));
        assert!(!svg.contains("M0 0h1v1h-1z"));
    }
}
