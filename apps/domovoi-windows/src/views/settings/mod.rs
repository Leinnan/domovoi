//! Settings: a page of the window's own, with its sections listed on the
//! left (as Windows' Settings has them) and the title bar's back button to
//! leave it.
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
use domovoi_core::view::{ConnectionView, SettingsView};
use domovoi_desktop_host::prefs::{Appearance, Prefs};
use windows_reactor::{
    Border, Callback, EncodedImage, FontWeight, Icon, Key, KeyedView, NavigationView, NavigationViewBackButtonVisible, NavigationViewItem,
    NavigationViewPaneDisplayMode, Orientation, ScrollBarVisibility, ScrollViewer, StackPanel, TextBlock, TextWrapping, Thickness, View, keyed,
};

use crate::core_link::CoreLink;
use crate::theme;
use crate::views::sidebar::Icons;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Section {
    #[default]
    Appearance,
    General,
    Servers,
    Companion,
    Projects,
    Providers,
    Scheduled,
    Keybindings,
    Access,
    Archived,
    Maintenance,
}

impl Section {
    pub const ALL: [Self; 11] = [
        Self::Appearance,
        Self::General,
        Self::Servers,
        Self::Companion,
        Self::Projects,
        Self::Providers,
        Self::Scheduled,
        Self::Keybindings,
        Self::Access,
        Self::Archived,
        Self::Maintenance,
    ];

    fn tag(self) -> &'static str {
        match self {
            Self::Appearance => "appearance",
            Self::General => "general",
            Self::Servers => "servers",
            Self::Companion => "companion",
            Self::Projects => "projects",
            Self::Providers => "providers",
            Self::Scheduled => "scheduled",
            Self::Keybindings => "keybindings",
            Self::Access => "access",
            Self::Archived => "archived",
            Self::Maintenance => "maintenance",
        }
    }

    pub fn from_tag(tag: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|s| s.tag() == tag)
    }

    fn title(self) -> &'static str {
        match self {
            Self::Appearance => "Appearance",
            Self::General => "General",
            Self::Servers => "Servers",
            Self::Companion => "Companion",
            Self::Projects => "Projects",
            Self::Providers => "Providers",
            Self::Scheduled => "Scheduled Tasks",
            Self::Keybindings => "Keybindings",
            Self::Access => "Access & Remote",
            Self::Archived => "Archived",
            Self::Maintenance => "Maintenance",
        }
    }

    fn glyph(self) -> &'static str {
        match self {
            Self::Appearance => "\u{E790}",
            Self::General => "\u{E713}",
            Self::Servers => "\u{E977}",
            Self::Companion => "\u{E76E}",
            Self::Projects => "\u{E8B7}",
            Self::Providers => "\u{E99A}",
            Self::Scheduled => "\u{E823}",
            Self::Keybindings => "\u{E765}",
            Self::Access => "\u{E72E}",
            Self::Archived => "\u{E7B8}",
            Self::Maintenance => "\u{E90F}",
        }
    }

    /// Whether the section shows what the connected server holds (hidden while there is none).
    fn of_the_server(self) -> bool {
        !matches!(self, Self::Appearance | Self::Servers | Self::Companion)
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

    /// The sections there are to show: the server's only while connected to one.
    pub fn shown(connected: bool) -> Vec<Self> {
        Self::ALL.into_iter().filter(|s| connected || !s.of_the_server()).collect()
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
    /// The server whose companion is shown (`None`: the connected one).
    pub companion_server: Option<String>,
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

pub fn page(core: &CoreLink, shown: &Shown<'_>, hooks: Hooks<'_>) -> View {
    let connected = shown.connection.state == "connected";
    let sections = Section::shown(connected);
    // A section of a server that is gone: the first one there is.
    let section = if sections.contains(&shown.state.section) { shown.state.section } else { Section::Appearance };
    let items: Vec<KeyedView> = sections
        .iter()
        .map(|s| keyed(s.tag(), NavigationViewItem::new().tag(s.tag()).icon(Icon::font(s.glyph())).content(s.title()).automation_name(s.title()).selects_on_invoked(true).is_selected(*s == section)))
        .collect();
    let body: Vec<View> = match section {
        Section::Appearance => general::appearance(shown.prefs, &hooks),
        Section::General => general::general(core, shown, &hooks),
        Section::Servers => general::servers(shown.connection, &hooks),
        Section::Companion => companion::companion(core, shown, &hooks),
        Section::Projects => server::projects(shown, &hooks),
        Section::Providers => providers::providers(core, shown, &hooks),
        Section::Scheduled => providers::scheduled(core, shown, &hooks),
        Section::Keybindings => providers::keybindings(core, shown.settings),
        Section::Access => server::access(core, shown, &hooks),
        Section::Archived => server::archived(core, shown.settings, &hooks),
        Section::Maintenance => server::maintenance(core, shown.settings),
    };
    let mut lines: Vec<View> = vec![TextBlock::new().text(section.title()).font_size(26.0).font_weight(FontWeight::SEMI_BOLD).margin(Thickness::new(0.0, 0.0, 0.0, 6.0)).into()];
    lines.extend(body);
    NavigationView::new()
        .pane_display_mode(NavigationViewPaneDisplayMode::Left)
        .is_pane_toggle_button_visible(false)
        .is_back_button_visible(NavigationViewBackButtonVisible::Collapsed)
        .is_settings_visible(false)
        .always_show_header(false)
        .open_pane_length(240.0)
        .is_pane_open(true)
        .on_selected_tag_changed(hooks.on_section.clone())
        .keyed_menu_items(items)
        .content(
            ScrollViewer::new()
                .vertical_scroll_bar_visibility(ScrollBarVisibility::Auto)
                .horizontal_scroll_bar_visibility(ScrollBarVisibility::Disabled)
                .content(StackPanel::new().orientation(Orientation::Vertical).spacing(10.0).margin(Thickness::new(32.0, 20.0, 32.0, 32.0)).children(lines)),
        )
        .into()
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
    fn without_a_server_only_this_apps_own_sections_show() {
        assert_eq!(Section::shown(false), [Section::Appearance, Section::Servers, Section::Companion]);
        assert_eq!(Section::shown(true), Section::ALL);
        assert!(Section::Appearance.loads().is_empty());
        assert_eq!(Section::Maintenance.loads().len(), 2);
        assert!(Section::ALL.into_iter().filter(|s| s.of_the_server()).all(|s| !s.loads().is_empty()), "a section of the server asks for it");
    }
}
