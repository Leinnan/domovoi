//! Settings: a page of the window's own, structured into two clear scopes:
//! **This PC** (device-local appearance and saved servers) and **Server**
//! (per-server configuration with an inline server switcher).
//!
//! Appearance is this app's own and kept in a file beside its store. The
//! rest is the server's, or the list of servers this device knows: the core
//! holds it (`SettingsView`, `ConnectionView`), each change is told to it at
//! once, and a section is asked for when it is opened (`LoadSettings`).

mod companion;
mod general;
mod providers;
mod server;
pub mod task;

use std::collections::HashMap;
use std::rc::Rc;

use domovoi_core::Event;
use domovoi_core::view::{ConnectionView, EnvironmentView, SettingsView};
use domovoi_desktop_host::prefs::{Appearance, Prefs};
use windows_reactor::{
    Border, Button, ButtonStyle, Callback, ComboBox, EncodedImage, FontWeight, Grid, GridLength, HorizontalAlignment, Image, Key, Orientation, ProgressRing,
    ScrollBarVisibility, ScrollViewer, StackPanel, Stretch, TextBlock, TextTrimming, TextWrapping, Thickness, VerticalAlignment, View,
};

use crate::companion::{self as mascot, Mood};
use crate::core_link::CoreLink;
use crate::theme;
use crate::views::sidebar::Icons;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Section {
    #[default]
    Appearance,
    Servers,
    General,
    Providers,
    Projects,
    Scheduled,
    Keybindings,
    Access,
    Archived,
    Maintenance,
    Companion,
}

impl Section {
    pub const CLIENT: [Self; 2] = [Self::Appearance, Self::Servers];

    pub const SERVER: [Self; 9] = [
        Self::General,
        Self::Providers,
        Self::Projects,
        Self::Scheduled,
        Self::Keybindings,
        Self::Access,
        Self::Archived,
        Self::Maintenance,
        Self::Companion,
    ];

    pub const ALL: [Self; 11] = [
        Self::Appearance,
        Self::Servers,
        Self::General,
        Self::Providers,
        Self::Projects,
        Self::Scheduled,
        Self::Keybindings,
        Self::Access,
        Self::Archived,
        Self::Maintenance,
        Self::Companion,
    ];

    fn tag(self) -> &'static str {
        match self {
            Self::Appearance => "appearance",
            Self::Servers => "servers",
            Self::General => "general",
            Self::Providers => "providers",
            Self::Projects => "projects",
            Self::Scheduled => "scheduled",
            Self::Keybindings => "keybindings",
            Self::Access => "access",
            Self::Archived => "archived",
            Self::Maintenance => "maintenance",
            Self::Companion => "companion",
        }
    }

    pub fn from_tag(tag: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.tag() == tag)
    }

    fn title(self) -> &'static str {
        match self {
            Self::Appearance => "Appearance",
            Self::Servers => "Servers",
            Self::General => "General",
            Self::Providers => "Providers",
            Self::Projects => "Projects",
            Self::Scheduled => "Scheduled Tasks",
            Self::Keybindings => "Keybindings",
            Self::Access => "Access & Remote",
            Self::Archived => "Archived",
            Self::Maintenance => "Maintenance",
            Self::Companion => "Companion",
        }
    }

    fn glyph(self) -> &'static str {
        match self {
            Self::Appearance => "\u{E790}",
            Self::Servers => "\u{E977}",
            Self::General => "\u{E713}",
            Self::Providers => "\u{E99A}",
            Self::Projects => "\u{E8B7}",
            Self::Scheduled => "\u{E823}",
            Self::Keybindings => "\u{E765}",
            Self::Access => "\u{E72E}",
            Self::Archived => "\u{E7B8}",
            Self::Maintenance => "\u{E90F}",
            Self::Companion => "\u{E76E}",
        }
    }

    /// Whether the section is scoped to a specific server.
    pub fn is_server_scoped(self) -> bool {
        !matches!(self, Self::Appearance | Self::Servers)
    }

    /// Whether the section reads/writes data stored on the server (requires an active connection).
    pub fn requires_connection(self) -> bool {
        self.is_server_scoped() && self != Self::Companion
    }

    /// What opening the section asks the core for.
    pub fn loads(self) -> Vec<Event> {
        let load = |section: &str| Event::LoadSettings { section: section.into() };
        match self {
            Self::Appearance | Self::Servers | Self::Companion => Vec::new(),
            Self::General => vec![load("general")],
            Self::Projects => vec![load("projects")],
            Self::Providers => vec![load("providers")],
            // A task's editor lists the projects too.
            Self::Scheduled => vec![load("scheduled"), load("projects")],
            Self::Keybindings => vec![load("keybindings")],
            Self::Access => vec![load("access"), Event::LoadRemoteAccess],
            Self::Archived => vec![load("archived")],
            Self::Maintenance => vec![load("storage"), load("diagnostics")],
        }
    }

    /// The sections shown in the left pane: server sections appear whenever at least one server is saved.
    pub fn shown(has_servers: bool) -> Vec<Self> {
        Self::ALL.into_iter().filter(|s| has_servers || !s.is_server_scoped()).collect()
    }
}

/// What the page keeps of its own.
#[derive(Default)]
pub struct State {
    pub section: Section,
    /// Text settings as typed, by key, until saved.
    pub drafts: HashMap<String, String>,
    /// The pairing link drawn as a QR code, with the link it is of.
    pub qr: Option<(String, EncodedImage)>,
    /// Last `(environment_id, section)` loaded while Settings was open, to auto-reload on server switches.
    pub loaded_for: Option<(String, Section)>,
}

impl State {
    /// Draws the pairing link's QR code when the link is new (the same image each render after).
    pub fn sync(&mut self, settings: &SettingsView) {
        let link = settings.remote.link.as_str();
        if link.is_empty() {
            self.qr = None;
        } else if self.qr.as_ref().is_none_or(|(of, _)| of != link) {
            self.qr = server::qr_svg(link).map(|svg| (link.to_owned(), EncodedImage::new(svg.into_bytes())));
        }
    }
}

/// What the page tells the window.
pub struct Hooks<'a> {
    /// Made once by the window: a section picked on the left (its tag).
    pub on_section: &'a Callback<Option<Rc<str>>>,
    pub on_appearance: Callback<Appearance>,
    pub on_text_size: Callback<f64>,
    /// A text setting typed: its key and text.
    pub on_draft: Callback<(String, Rc<str>)>,
    /// A choice from a row's menu, or a button that opens a dialog: `env-rename:<id>`, `icon:<project id>`, …
    pub on_menu: Callback<Key>,
    /// Text to put on the clipboard.
    pub on_copy: Callback<String>,
    /// A page to open in the browser.
    pub on_open: Callback<String>,
}

/// What the page shows.
pub struct Shown<'a> {
    pub connection: &'a ConnectionView,
    pub settings: &'a SettingsView,
    pub prefs: &'a Prefs,
    pub state: &'a State,
    pub icons: Icons<'a>,
}

pub(super) fn active_server(connection: &ConnectionView) -> Option<&EnvironmentView> {
    connection
        .environments
        .iter()
        .find(|s| s.id == connection.active_environment_id)
        .or_else(|| connection.environments.iter().find(|s| s.active))
        .or_else(|| connection.environments.first())
}

pub fn page(core: &CoreLink, shown: &Shown<'_>, hooks: Hooks<'_>) -> View {
    let connected = shown.connection.state == "connected";
    let server = active_server(shown.connection);
    let has_servers = server.is_some();
    let sections = Section::shown(has_servers);
    let section = if sections.contains(&shown.state.section) { shown.state.section } else { Section::Servers };

    let nav_button = |s: Section, badge: Option<&str>| -> View {
        let on_section = hooks.on_section.clone();
        let tag = Rc::<str>::from(s.tag());
        let selected = s == section;
        let mut cols: Vec<View> = vec![
            theme::glyph(s.glyph(), 15.0).opacity(if selected { 1.0 } else { 0.8 }).into(),
            TextBlock::new()
                .grid_column(1)
                .text(s.title())
                .font_weight(if selected { FontWeight::SEMI_BOLD } else { FontWeight::NORMAL })
                .text_trimming(TextTrimming::CharacterEllipsis)
                .vertical_alignment(VerticalAlignment::Center)
                .into(),
        ];
        if let Some(note) = badge {
            cols.push(caption(note).grid_column(2).vertical_alignment(VerticalAlignment::Center).into());
        }
        Button::new()
            .style(if selected { ButtonStyle::Accent } else { ButtonStyle::Subtle })
            .horizontal_alignment(HorizontalAlignment::Stretch)
            .horizontal_content_alignment(HorizontalAlignment::Stretch)
            .automation_name(s.title())
            .content(Grid::new().columns([GridLength::Auto, GridLength::STAR, GridLength::Auto]).column_spacing(10.0).children(cols))
            .on_click(move || on_section.call(Some(Rc::clone(&tag))))
            .into()
    };

    let mut left_items: Vec<View> = vec![
        TextBlock::new().text("THIS PC").font_size(theme::CAPTION).font_weight(FontWeight::SEMI_BOLD).opacity(0.6).margin(Thickness::new(8.0, 4.0, 0.0, 2.0)).into(),
    ];
    for s in Section::CLIENT {
        left_items.push(nav_button(s, None));
    }

    left_items.push(Border::new().height(1.0).background(theme::stroke()).margin(Thickness::xy(4.0, 8.0)).into());
    left_items.push(
        TextBlock::new().text("SERVER").font_size(theme::CAPTION).font_weight(FontWeight::SEMI_BOLD).opacity(0.6).margin(Thickness::new(8.0, 2.0, 0.0, 2.0)).into(),
    );

    if let Some(active) = server {
        let envs = &shown.connection.environments;
        let chosen = envs.iter().position(|e| e.id == active.id);
        let ids: Vec<String> = envs.iter().map(|e| e.id.clone()).collect();
        let on_menu = hooks.on_menu.clone();
        let switcher = ComboBox::new()
            .automation_name("Server to configure")
            .horizontal_alignment(HorizontalAlignment::Stretch)
            .items_source(envs.iter().map(|e| e.label.clone()))
            .selected_index(chosen)
            .on_selection_changed(move |index: Option<usize>| {
                if let Some(id) = index.filter(|i| Some(*i) != chosen).and_then(|i| ids.get(i)) {
                    on_menu.call(format!("env-select:{id}").as_str().into());
                }
            });
        left_items.push(switcher.into());
        for s in Section::SERVER {
            left_items.push(nav_button(s, (s == Section::Companion).then_some("Local")));
        }
    } else {
        let on_menu = hooks.on_menu.clone();
        left_items.push(caption("No servers paired yet.").margin(Thickness::new(8.0, 2.0, 8.0, 4.0)).into());
        left_items.push(
            Button::new()
                .horizontal_alignment(HorizontalAlignment::Stretch)
                .content(TextBlock::new().text("Add Server…"))
                .on_click(move || on_menu.call("env-add:".into()))
                .into(),
        );
    }

    let left_pane = Border::new()
        .padding(Thickness::xy(10.0, 14.0))
        .background(theme::card())
        .border_brush(theme::stroke())
        .border_thickness(Thickness::new(0.0, 0.0, 1.0, 0.0))
        .content(
            ScrollViewer::new()
                .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
                .horizontal_scroll_bar_visibility(ScrollBarVisibility::Disabled)
                .content(StackPanel::new().orientation(Orientation::Vertical).spacing(4.0).children(left_items)),
        );

    let body: Vec<View> = if section.requires_connection() && !connected {
        vec![connection_placeholder(core, shown.connection, server, section)]
    } else {
        match section {
            Section::Appearance => general::appearance(shown.prefs, &hooks),
            Section::Servers => general::servers(shown.connection, shown.icons.dark, &hooks),
            Section::General => general::general(core, shown, &hooks),
            Section::Companion => companion::companion(core, shown),
            Section::Projects => server::projects(shown, &hooks),
            Section::Providers => providers::providers(core, shown, &hooks),
            Section::Scheduled => providers::scheduled(core, shown, &hooks),
            Section::Keybindings => providers::keybindings(core, shown.settings),
            Section::Access => server::access(core, shown, &hooks),
            Section::Archived => server::archived(core, shown.settings, &hooks),
            Section::Maintenance => server::maintenance(core, shown.settings),
        }
    };

    let mut lines: Vec<View> = vec![detail_header(core, shown, section, server)];
    lines.extend(body);

    let right_pane = ScrollViewer::new()
        .grid_column(1)
        .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
        .horizontal_scroll_bar_visibility(ScrollBarVisibility::Disabled)
        .content(StackPanel::new().orientation(Orientation::Vertical).spacing(10.0).margin(Thickness::new(32.0, 20.0, 32.0, 32.0)).children(lines));

    Grid::new()
        .columns([GridLength::Pixel(248.0), GridLength::STAR])
        .children((left_pane, right_pane))
        .into()
}

/// Top context header in the detail pane showing section title, scope pill, and live server state.
fn detail_header(core: &CoreLink, shown: &Shown<'_>, section: Section, server: Option<&EnvironmentView>) -> View {
    let conn = shown.connection;
    let connected = conn.state == "connected";
    let (scope_pill, subtitle) = if !section.is_server_scoped() {
        ("This PC", "Saved on this PC only. Applies across all servers.".to_owned())
    } else if let Some(srv) = server {
        if section == Section::Companion {
            ("Saved on this PC", format!("Customizes how {} appears on this PC only.", srv.label))
        } else {
            ("Server setting", format!("Applies on {} across all connected devices.", srv.label))
        }
    } else {
        ("Server setting", "No server selected.".to_owned())
    };

    let mut title_row: Vec<View> = vec![
        TextBlock::new().text(section.title()).font_size(24.0).font_weight(FontWeight::SEMI_BOLD).vertical_alignment(VerticalAlignment::Center).into(),
        Border::new()
            .padding(Thickness::xy(8.0, 2.0))
            .corner_radius(10.0)
            .background(theme::code())
            .vertical_alignment(VerticalAlignment::Center)
            .content(TextBlock::new().text(scope_pill).font_size(theme::CAPTION).foreground(if section.requires_connection() { theme::accent() } else { theme::primary() }))
            .into(),
    ];

    let mut cols: Vec<View> = Vec::new();
    if section.is_server_scoped()
        && let Some(srv) = server
    {
        let mood = if connected { Mood::Idle } else { Mood::Offline };
        cols.push(
            Image::new()
                .source_svg(mascot::image(&srv.companion.look, mood, shown.icons.dark))
                .stretch(Stretch::Uniform)
                .width(32.0)
                .height(32.0)
                .vertical_alignment(VerticalAlignment::Center)
                .into(),
        );
    } else {
        cols.push(theme::glyph(section.glyph(), 22.0).opacity(0.8).vertical_alignment(VerticalAlignment::Center).into());
    }

    cols.push(
        StackPanel::new()
            .grid_column(1)
            .orientation(Orientation::Vertical)
            .spacing(2.0)
            .children(vec![
                StackPanel::new().orientation(Orientation::Horizontal).spacing(10.0).children(std::mem::take(&mut title_row)).into(),
                caption(&subtitle).into(),
            ])
            .into(),
    );

    if section.is_server_scoped() && server.is_some() {
        let status_view: View = if connected {
            let route = if conn.remote && !conn.path.is_empty() {
                format!("Connected · {} ({} ms)", conn.path, conn.rtt_ms)
            } else {
                "Connected".to_owned()
            };
            caption(&route).grid_column(2).vertical_alignment(VerticalAlignment::Center).into()
        } else if matches!(conn.state.as_str(), "connecting" | "reconnecting" | "pairing") {
            StackPanel::new()
                .grid_column(2)
                .orientation(Orientation::Horizontal)
                .spacing(6.0)
                .vertical_alignment(VerticalAlignment::Center)
                .children(vec![
                    ProgressRing::new().is_active(true).width(14.0).height(14.0).into(),
                    caption(if conn.state == "reconnecting" { "Reconnecting…" } else { "Connecting…" }).into(),
                ])
                .into()
        } else {
            let core = core.clone();
            Button::new()
                .grid_column(2)
                .style(ButtonStyle::Accent)
                .content(TextBlock::new().text("Reconnect"))
                .on_click(move || core.send(Event::Reconnect))
                .into()
        };
        cols.push(status_view);
    }

    Grid::new()
        .columns([GridLength::Auto, GridLength::STAR, GridLength::Auto])
        .column_spacing(12.0)
        .margin(Thickness::new(0.0, 0.0, 0.0, 6.0))
        .children(cols)
        .into()
}

fn connection_placeholder(core: &CoreLink, conn: &ConnectionView, server: Option<&EnvironmentView>, section: Section) -> View {
    let name = server.map_or("this server", |s| s.label.as_str());
    let connecting = matches!(conn.state.as_str(), "connecting" | "reconnecting" | "pairing");
    let mut lines: Vec<View> = vec![
        TextBlock::new()
            .text(if connecting { format!("Connecting to {name}…") } else { format!("{name} is offline") })
            .font_size(16.0)
            .font_weight(FontWeight::SEMI_BOLD)
            .into(),
        caption(&if connecting {
            format!("Loading {} from {name}.", section.title().to_lowercase())
        } else if let Some(err) = conn.error.as_deref().filter(|e| !e.is_empty()) {
            err.to_owned()
        } else {
            format!("{} is stored on {name}. Reconnect to view or change it.", section.title())
        })
        .into(),
    ];
    if connecting {
        lines.push(ProgressRing::new().is_active(true).width(24.0).height(24.0).horizontal_alignment(HorizontalAlignment::Left).into());
    } else {
        let core = core.clone();
        lines.push(
            Button::new()
                .style(ButtonStyle::Accent)
                .horizontal_alignment(HorizontalAlignment::Left)
                .content(TextBlock::new().text("Reconnect"))
                .on_click(move || core.send(Event::Reconnect))
                .into(),
        );
    }
    card(lines)
}

// ---- pieces the sections share ----

/// A group's heading.
fn heading(text: &str) -> View {
    TextBlock::new().text(text).font_weight(FontWeight::SEMI_BOLD).margin(Thickness::new(2.0, 10.0, 0.0, 0.0)).into()
}

fn caption(text: &str) -> TextBlock {
    TextBlock::new().text(text).font_size(theme::CAPTION).opacity(0.65).text_wrapping(TextWrapping::WrapWholeWords)
}

/// Lines in a card, as the other areas of the window have them.
fn card(lines: Vec<View>) -> View {
    Border::new()
        .padding(Thickness::xy(16.0, 12.0))
        .background(theme::card())
        .border_brush(theme::stroke())
        .border_thickness(Thickness::uniform(1.0))
        .corner_radius(theme::RADIUS)
        .content(StackPanel::new().orientation(Orientation::Vertical).spacing(12.0).children(lines))
        .into()
}

/// Nothing to list.
fn empty(text: &str) -> View {
    card(vec![caption(text).into()])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tag_names_its_section_and_only_its() {
        for section in Section::ALL {
            assert_eq!(Section::from_tag(section.tag()), Some(section));
        }
        assert_eq!(Section::from_tag("no-such-section"), None);
    }

    #[test]
    fn client_and_server_scopes_partition_all_sections() {
        assert_eq!(Section::shown(false), Section::CLIENT);
        assert_eq!(Section::shown(true), Section::ALL);
        assert!(Section::CLIENT.into_iter().all(|s| !s.is_server_scoped() && !s.requires_connection()));
        assert!(Section::SERVER.into_iter().all(Section::is_server_scoped));
        assert!(!Section::Companion.requires_connection());
        assert!(Section::Appearance.loads().is_empty());
        assert_eq!(Section::Maintenance.loads().len(), 2);
        assert!(
            Section::ALL.into_iter().filter(|s| s.requires_connection()).all(|s| !s.loads().is_empty()),
            "a server-stored section asks the server for its data"
        );
    }
}
