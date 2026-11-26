//! The window: the title bar, then the pairing form, or the thread list on
//! the left and the open thread with its composer on the right, or Settings.
//! A link from outside the app puts its question over all of it
//! (`views::link_dialog`).
//!
//! It owns the core link and the store. The host's reports wake the UI
//! thread (`ui_thread`), which sends `Message::CoreWoke`; `absorb` takes all
//! of them, applies the deltas and acts on the rest.

use std::cell::Cell;
use std::collections::HashSet;
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant};

use domovoi_core::Event;
use domovoi_desktop_host::HostEvent;
use domovoi_desktop_host::attach;
use domovoi_desktop_host::prefs::{Appearance, Prefs, WindowFrame};
use windows_reactor::{
    Border, Button, ButtonStyle, Callback, CharacterEventInfo, Color, ColorScheme, Component, ComponentContext, ComponentTimer, ContentDialogExt as _, ContentDialogResult, DroppedData,
    ElementObservation, ElementRef, Grid, GridLength, HorizontalAlignment, Image, Key, KeyEventInfo, RoutedCallback, ScrollPosition, ScrollViewer, Stretch, TextBlock, TextBox,
    Thickness, TitleBar, VerticalAlignment, View, ViewContext, VirtualKey, WindowBackdrop, WindowConstraints, WindowPlacement, WindowSize, WindowTheme, WindowVisuals,
};

use crate::core_link::{CoreLink, IDENTITY, now_ms};
use crate::launch::Launch;
use crate::media::Media;
use crate::overlays::{self, Overlay};
use crate::store::Store;
use crate::views::transcript::{self, Act};
use crate::views::{composer, link_dialog, new_thread, pairing, palette, panel, pending, pull_requests, settings, sidebar, thread, usage};
use crate::shortcuts::{self, Command};
use crate::{companion, hwnd, menus, theme, ui_thread, widgets};

const SIZE: (f64, f64) = (1280.0, 820.0);
const MIN_SIZE: (f64, f64) = (720.0, 480.0);

/// The title bar's height, for what is sized from the window's.
const TITLE_BAR: f64 = 48.0;

/// Back at the window after this long, the network may be another one.
const FOCUS_NETWORK_CHECK: Duration = Duration::from_secs(60);

pub struct MainWindow {
    core: Option<CoreLink>,
    /// Why the core could not start (shown instead of anything else).
    failed: Option<String>,
    store: Store,
    /// The server's images shown here (project icons).
    media: Media,
    /// The window is dark (colours that differ by theme follow it).
    dark: bool,
    /// Made once: a new observer each render would subscribe again.
    on_scheme: Callback<ColorScheme>,
    pairing: pairing::Draft,
    /// The composer's text as typed here, and the core's revision it started from.
    draft: String,
    draft_revision: u32,
    /// The core replaced the draft: the caret goes to its end once it is drawn.
    caret_to_end: bool,
    caret_timer: Option<ComponentTimer>,
    /// The arrows are going through earlier messages (so they keep doing it on one of several lines).
    recalling: bool,
    /// The row marked in the composer's list of commands or skills.
    mark: usize,
    /// The models are open above the composer; their search as typed.
    models_open: bool,
    model_query: String,
    /// The last `/model` the core asked the models to open for.
    model_requests: u32,
    /// What the composer's keys do now: said when drawing, read when a key comes.
    composer_keys: Rc<Cell<composer::Keys>>,
    on_composer_keys: RoutedCallback<KeyEventInfo>,
    /// What is ticked and typed in the cards that wait for an answer.
    pending: pending::State,
    /// What the open thread's page keeps: what is opened, how much is shown, whether it follows the end.
    transcript: transcript::State,
    /// The thread's rows, to keep the page at their end.
    scroll: ElementRef<ScrollViewer>,
    /// Dropping it would stop the reports of where the page is.
    _scrolled: ElementObservation,
    /// The page goes to its end once it is drawn, and again while it settles.
    scroll_due: bool,
    scroll_again: u8,
    scroll_timer: Option<ComponentTimer>,
    /// How many rows the thread had, to tell when more came.
    rows_seen: usize,
    /// The diagrams asked for: each one's key, under the key the core asked with.
    diagram_loads: HashSet<(String, String)>,
    /// Settings is shown, not the threads.
    in_settings: bool,
    settings: settings::State,
    /// This app's own preferences (appearance), kept beside its store.
    prefs: Prefs,
    /// The new-thread page's search, and the tile its arrows are on.
    picker: new_thread::Picker,
    /// What the dialog asks now.
    overlay: Overlay,
    /// The window's size (how many project tiles fit side by side, and how tall their list is).
    size: WindowSize,
    /// The route last seen, to tell when the new-thread page opens.
    route_seen: Rc<str>,
    /// The project search asked for the keyboard; given once the page is drawn.
    focus_search: bool,
    focus_timer: Option<ComponentTimer>,
    search_field: ElementRef<TextBox>,
    composer_field: ElementRef<TextBox>,
    // Made once: a new one each render would subscribe or attach again.
    on_size: Callback<WindowSize>,
    on_placement: Callback<WindowPlacement>,
    on_dialog_closed: Callback<ContentDialogResult>,
    on_picker_keys: RoutedCallback<KeyEventInfo>,
    /// The window's shortcuts.
    on_keys: RoutedCallback<KeyEventInfo>,
    on_section: Callback<Option<Rc<str>>>,
    /// The reconnect countdown already scheduled.
    scheduled_retry: u64,
    /// The thread the host was last told is on screen.
    focused_thread: Option<String>,
    /// This is the active window (no notification for the thread it shows).
    focused: bool,
    last_focus: Instant,
    /// Tells when the window gains and loses the focus and closes.
    watch: Option<hwnd::Watch>,
    /// Where the window opens: where it was last time.
    opens_at: Option<WindowPlacement>,
    /// The thread search as typed.
    search: String,
    /// What the panel beside the thread keeps: what is typed and folded in it.
    panel: panel::State,
    /// What is typed and chosen on the Pull Requests page.
    pulls: pull_requests::State,
    /// The terminal's screen, to give it the keyboard.
    terminal: ElementRef<Border>,
    /// The terminal has the keyboard, so most keys are its own. Read where the window's shortcuts are decided.
    terminal_keys: Rc<Cell<bool>>,
    on_terminal_keys: RoutedCallback<KeyEventInfo>,
    on_terminal_chars: RoutedCallback<CharacterEventInfo>,
    /// The palette's search as typed, what it searched when that was taken from the core, and its box.
    palette_query: String,
    palette_mode: Option<String>,
    palette_field: ElementRef<TextBox>,
    on_palette_keys: RoutedCallback<KeyEventInfo>,
    /// The palette opened: its search gets the keyboard once it is drawn.
    focus_palette: bool,
    /// How the companion takes a turn that just ended, when that passes, and the timer that draws it again then.
    reaction: companion::Reaction,
    reaction_until: Option<i64>,
    reaction_timer: Option<ComponentTimer>,
    /// The terminal on screen, and that it gets the keyboard once it is drawn.
    terminal_shown: Option<String>,
    focus_terminal: bool,
    /// The projects whose earlier sessions are shown.
    earlier: HashSet<String>,
    smoke: bool,
}

pub enum Message {
    CoreWoke,
    /// The native window, once there is one.
    Handle(isize),
    /// The window became the active one, or stopped being it.
    Focused(bool),
    Placed(WindowPlacement),
    Command(Command),
    /// The thread search's text.
    Search(Rc<str>),
    ToastAction,
    /// Forget the server that refuses this device.
    ForgetServer,
    Scheme(ColorScheme),
    PairLink(Rc<str>),
    PairUrl(Rc<str>),
    PairCode(Rc<str>),
    ConnectLink,
    ConnectCode,
    /// Sign in to the server on this computer.
    ConnectLocal,
    /// The answer to the link request with this `id`.
    AnswerLink { id: u32, accept: bool },
    Draft(Rc<str>),
    /// A key in the composer that does more than type.
    ComposerKey(composer::KeyAction),
    CaretToEnd,
    /// Opens or shuts the models.
    Models,
    ModelQuery(Rc<str>),
    /// Pick pictures for the message; then the ones picked, or dropped.
    Attach,
    Picked(Vec<PathBuf>),
    Dropped(DroppedData),
    /// A card that waits for an answer: a tick, what was typed, its sending.
    Pending(pending::Edit),
    /// Something done on the thread's page.
    Transcript(Act),
    /// The page came to its end, or left it.
    AtEnd(bool),
    ScrollToEnd,
    Resized(WindowSize),
    /// The project search's text.
    Query(Rc<str>),
    /// An arrow, Enter or Escape in the project search.
    PickerKey(VirtualKey),
    FocusSearch,
    /// A choice from the Add menu or a project's menu.
    Menu(Key),
    OverlayEdit(overlays::Edit),
    DialogClosed(ContentDialogResult),
    OpenSettings,
    CloseSettings,
    /// A section picked in Settings (its tag).
    Section(Option<Rc<str>>),
    Appearance(Appearance),
    TextSize(f64),
    /// A text setting typed: its key and text.
    SettingDraft((String, Rc<str>)),
    /// Text for the clipboard.
    Copy(String),
    /// A page for the browser.
    OpenUrl(String),
    DismissToast,
    /// Something done in the panel beside the thread.
    Panel(panel::Act),
    /// Something done on the Pull Requests page.
    Pulls(pull_requests::Act),
    /// The companion's moment of gladness or dismay is over.
    ReactionOver,
    /// The palette's search, a key in it, and the keyboard for it.
    PaletteQuery(Rc<str>),
    PaletteKey(VirtualKey),
    FocusPalette,
}

impl MainWindow {
    fn send(&self, event: Event) {
        if let Some(core) = &self.core {
            core.send(event);
        }
    }

    /// Takes what the host reported since the last wake-up.
    fn absorb(&mut self) {
        let Some(core) = self.core.clone() else { return };
        let (mut sidebar_changed, mut settings_changed, mut thread_changed, mut panel_changed, mut palette_changed) = (false, false, false, false, false);
        for event in core.take() {
            match event {
                HostEvent::Delta(delta) => {
                    let changed = self.store.apply(*delta);
                    sidebar_changed |= changed.sidebar;
                    settings_changed |= changed.settings;
                    panel_changed |= changed.panel;
                    palette_changed |= changed.palette;
                    if changed.composer && self.store.composer.draft_revision != self.draft_revision {
                        self.draft_revision = self.store.composer.draft_revision;
                        self.draft = self.store.composer.draft.clone();
                        // Set from here, a text box's caret is at its start.
                        self.caret_to_end = !self.draft.is_empty();
                    }
                    thread_changed |= changed.thread || changed.diagrams;
                    if changed.thread {
                        self.pending.prune(self.store.thread.as_ref().map(|t| &*t.page));
                    }
                }
                HostEvent::OpenThreadFromNotification { environment_id, thread_id } => core.send(Event::OpenNotification { environment_id, thread_id }),
                HostEvent::RespondFromNotification { environment_id, thread_id, request_id, decision } => {
                    core.send(Event::RespondFromNotification { environment_id, thread_id, request_id, decision });
                }
                HostEvent::ImageLoaded { url, bytes } => self.media.loaded(url, bytes),
                HostEvent::ImageFailed { url } => self.media.failed(url),
                // The terminals other servers have open: this app lists only the open thread's.
                HostEvent::TerminalDetails { .. } => {}
            }
        }
        if thread_changed {
            self.follow_thread(&core);
        }
        if panel_changed {
            self.panel.sync(&self.store.panel);
            // The page is narrower or wider with the panel: one that was at its end stays there.
            self.scroll_due |= self.transcript.following;
            // A terminal that left the screen cannot say it lost the keyboard.
            let on_screen = self.store.panel.terminal.active.as_ref().filter(|_| self.store.panel.open && panel::tab(&self.store.panel.tab) == "terminal").map(|t| t.id.clone());
            if on_screen.is_none() {
                self.panel.terminal_focused = false;
                self.terminal_keys.set(false);
            } else if on_screen != self.terminal_shown {
                // A terminal just shown is typed into next.
                self.focus_terminal = true;
            }
            self.terminal_shown = on_screen;
            self.fit_terminal();
        }
        if palette_changed {
            let mode = self.store.palette.as_ref().map(|p| p.mode.clone());
            if mode != self.palette_mode {
                // Opened, shut, or searching something else: the search is what the core has.
                self.palette_query = self.store.palette.as_ref().map(|p| p.query.clone()).unwrap_or_default();
                self.focus_palette = mode.is_some();
                self.palette_mode = mode;
            }
        }
        if sidebar_changed || settings_changed || thread_changed || panel_changed {
            let pictures = self.thread_pictures();
            self.media.refresh(&core, &self.store.sidebar, &self.store.settings, &pictures);
        }
        if settings_changed {
            self.settings.sync(&self.store.settings);
            // A text setting the server now has as typed is no longer a draft.
            let fields = self.store.settings.groups.iter().flat_map(|g| &g.fields);
            let saved: Vec<&str> = fields.filter(|f| self.settings.drafts.get(&f.key) == Some(&f.text_value)).map(|f| f.key.as_str()).collect();
            self.settings.drafts.retain(|key, _| !saved.contains(&key.as_str()));
        }
        self.overlay.seed(&self.store.agent_sessions);
        if self.store.route != self.route_seen {
            self.route_seen = Rc::clone(&self.store.route);
            // A new thread with no project yet: typing finds one.
            self.focus_search = &*self.route_seen == "new-thread" && self.store.sidebar.selected_project_id.is_none();
        }
        let connection = Rc::clone(&self.store.connection);
        core.set_server(&connection.server_url, connection.remote);
        // A turn that just ended shows on the companion's face for a moment.
        if let Some(until) = self.reaction.observe(&connection.active_environment_id, &connection.activity, now_ms()) {
            self.reaction_until = Some(until);
        }
        // The core counts down; the shell keeps the time.
        if connection.state == "reconnecting" && connection.reconnect_in_seconds > 0 && !connection.offline && self.scheduled_retry != connection.retry_gen {
            self.scheduled_retry = connection.retry_gen;
            core.send_after(Duration::from_secs(u64::from(connection.reconnect_in_seconds)), Event::RetryTimer { retry_gen: connection.retry_gen });
        }
        // Pictures read for the message: to the core, or why one could not be.
        for read in core.take_attachments() {
            match read {
                Ok(picture) => core.send(Event::AddAttachment { name: picture.name, mime_type: picture.mime_type, size_bytes: picture.size_bytes, data_url: picture.data_url }),
                // Nothing to say: the clipboard held no picture.
                Err(why) if why.is_empty() => {}
                Err(why) => core.send(Event::ShowNotice { kind: "error".into(), message: why }),
            }
        }
        // `/model` opens the models.
        if self.store.composer.model_picker_requested != self.model_requests {
            self.model_requests = self.store.composer.model_picker_requested;
            self.models_open = true;
        }
        let shown = self.store.thread.as_ref().map(|t| t.page.id.clone());
        if shown != self.focused_thread {
            self.focused_thread.clone_from(&shown);
            core.set_focus(self.focused, shown);
        }
        if self.smoke {
            match connection.state.as_str() {
                "connected" => {
                    println!("smoke: connected to {} ({})", connection.environment_label, connection.server_version);
                    std::process::exit(0);
                }
                "failed" => {
                    eprintln!("smoke: the connection failed: {}", connection.error.as_deref().unwrap_or("no reason given"));
                    std::process::exit(1);
                }
                _ => {}
            }
        }
    }

    /// An arrow, Enter or Escape in the project search.
    fn picker_key(&mut self, key: VirtualKey) {
        let groups = new_thread::groups(&self.store.sidebar, &self.picker.query);
        let last = new_thread::shown(&groups).saturating_sub(1);
        match key {
            VirtualKey::DOWN => self.picker.marked = (self.picker.marked + 1).min(last),
            VirtualKey::UP => self.picker.marked = self.picker.marked.saturating_sub(1),
            VirtualKey::ENTER => {
                if let Some(project) = new_thread::marked(&groups, self.picker.marked) {
                    self.send(Event::NewThread { project_id: Some(project.id.clone()) });
                    self.picker = new_thread::Picker::default();
                    _ = self.composer_field.request_focus();
                }
            }
            _ => self.picker = new_thread::Picker::default(),
        }
    }

    /// After the open thread changed: another thread starts its page over, more
    /// rows are followed (or marked unseen), the find's match is in sight, and
    /// finished diagrams are asked for.
    fn follow_thread(&mut self, core: &CoreLink) {
        let Some(thread) = &self.store.thread else {
            self.rows_seen = 0;
            return;
        };
        if self.focused_thread.as_deref() != Some(thread.page.id.as_str()) {
            self.transcript.reset();
            self.rows_seen = 0;
        }
        let rows = thread.rows.len();
        if self.transcript.following {
            self.scroll_due = true;
        } else if rows > self.rows_seen {
            self.transcript.unseen = true;
        }
        self.rows_seen = rows;
        // The find's match may be above what is drawn.
        let entries = transcript::entries(&thread.rows);
        if let Some(id) = thread.page.find.as_ref().and_then(|f| f.active_item_id.as_deref()) {
            let holds = |entry: &transcript::Entry<'_>| match entry {
                transcript::Entry::Row(row) => row.id == id,
                transcript::Entry::Group(steps) => steps.iter().any(|s| s.id == id),
            };
            if let Some(at) = entries.iter().position(holds) {
                self.transcript.shown = self.transcript.shown.max(entries.len() - at);
            }
        }
        // Mermaid blocks that are finished: once each, and again when the core says (a new connection).
        let (_, shown) = transcript::window(&entries, self.transcript.shown);
        let rows = shown.iter().flat_map(|entry| match entry {
            transcript::Entry::Row(row) => std::slice::from_ref(*row).iter().collect::<Vec<_>>(),
            transcript::Entry::Group(steps) => steps.iter().map(|s| &**s).collect(),
        });
        for (key, source) in transcript::diagrams_in(rows, self.dark) {
            let drawn = self.store.diagrams.iter().find(|d| d.key == key);
            let asked_under = match drawn {
                None => String::new(),
                Some(diagram) => match &diagram.load_key {
                    Some(load_key) => load_key.clone(),
                    None => continue,
                },
            };
            if self.diagram_loads.insert((key.clone(), asked_under)) {
                core.send(Event::LoadDiagram { key, source: source.to_owned(), dark: self.dark });
            }
        }
    }

    /// The links of the pictures the open thread's page shows: its rows', its diagrams', the viewer's.
    fn thread_pictures(&self) -> Vec<String> {
        let mut urls: Vec<String> = Vec::new();
        if let Some(thread) = &self.store.thread {
            let entries = transcript::entries(&thread.rows);
            let (_, shown) = transcript::window(&entries, self.transcript.shown);
            for entry in shown {
                let rows: Vec<&domovoi_core::view::ItemView> = match entry {
                    transcript::Entry::Row(row) => vec![*row],
                    transcript::Entry::Group(steps) => steps.iter().map(|s| &**s).collect(),
                };
                for row in rows {
                    urls.extend(row.images.iter().chain(&row.group_images).map(|image| image.url.clone()));
                }
            }
            urls.extend(self.store.diagrams.iter().filter_map(|d| d.url.clone()));
        }
        if let Some((pictures, _)) = &self.transcript.viewer {
            urls.extend(pictures.iter().map(|image| image.url.clone()));
        }
        // What the panel shows: a picture file, and a diagram artifact's drawing.
        let panel = &self.store.panel;
        urls.extend([&panel.files.preview, &panel.files.artifact_preview].into_iter().flatten().filter_map(|preview| preview.image_url.clone()));
        urls.extend(panel.artifacts.active.iter().filter(|a| a.kind == "mermaid").filter_map(|a| a.preview_url.clone()));
        urls
    }

    /// The panel's width: what the preferences say, within half the window.
    fn panel_width(&self) -> f64 {
        f64::from(self.prefs.panel_width).min((self.size.width / 2.0).max(f64::from(domovoi_desktop_host::prefs::PANEL_MIN)))
    }

    /// Asks for the terminal to be the size the panel has room for.
    fn fit_terminal(&mut self) {
        if let Some(core) = self.core.clone() {
            let (width, height) = (self.panel_width(), self.size.height - TITLE_BAR - theme::GAP);
            let panel = Rc::clone(&self.store.panel);
            self.panel.fit(&core, &panel, width, height);
        }
    }

    /// Something done in the panel beside the thread.
    fn panel(&mut self, act: panel::Act) {
        let Some(core) = self.core.clone() else { return };
        let shown = Rc::clone(&self.store.panel);
        match self.panel.apply(act, &core, &shown) {
            panel::After::Nothing => {}
            panel::After::Copy(text) => self.copy(text),
            panel::After::Open(url) => self.open_url(&url),
            panel::After::View(picture) => {
                self.transcript.viewer = Some((vec![picture], 0));
                self.refresh_pictures();
            }
            panel::After::FocusTerminal => _ = self.terminal.request_focus(),
            panel::After::Paste => match arboard::Clipboard::new().and_then(|mut clipboard| clipboard.get_text()) {
                Ok(text) if !text.is_empty() => core.send(Event::TerminalPaste { text }),
                Ok(_) => {}
                Err(error) => tracing::debug!("the clipboard has no text to paste: {error}"),
            },
        }
        self.terminal_keys.set(self.panel.terminal_focused);
    }

    /// Something done on the thread's page.
    fn transcript(&mut self, act: Act) {
        match act {
            Act::Toggle(id) => self.transcript.toggle(id),
            Act::Copy(text) => self.copy(text),
            Act::Open(url) => self.open_url(&url),
            Act::View(pictures, at) => {
                self.transcript.viewer = Some((pictures, at));
                self.refresh_pictures();
            }
            Act::Viewer(delta) => self.transcript.viewer(delta),
            Act::SavePlan(item_id) => self.overlay = Overlay::SavePlan { item_id, path: String::new() },
            Act::Revert(checkpoint_id, label) => self.overlay = Overlay::ConfirmRevert { checkpoint_id, label },
            Act::Earlier => {
                self.transcript.shown += transcript::WINDOW;
                self.refresh_pictures();
            }
            Act::Progress => {
                self.transcript.progress_open = !self.transcript.progress_open;
                // The page is shorter with the plan open: one that was at its end stays there.
                self.scroll_due |= self.transcript.following;
            }
            Act::Find(text) => {
                if *text != *self.transcript.find {
                    self.transcript.find = text.to_string();
                    self.send(Event::FindInThread { query: self.transcript.find.clone() });
                }
            }
            Act::Latest => {
                self.transcript.following = true;
                self.transcript.unseen = false;
                self.scroll_due = true;
            }
            Act::OpenFile(path) => {
                self.send(Event::ShowPanel { tab: "files".into() });
                self.send(Event::OpenFile { path });
            }
            Act::Preview(path) => {
                if let Some(thread_id) = self.store.thread.as_ref().map(|t| t.page.id.clone()) {
                    self.send(Event::ShowPanel { tab: "artifacts".into() });
                    self.send(Event::OpenArtifactFile { thread_id, path });
                }
            }
            Act::Diff(source) => {
                self.send(Event::SelectDiffSource { source });
                self.send(Event::ShowPanel { tab: "diff".into() });
            }
            Act::Reveal(id) => {
                let Some(thread) = &self.store.thread else { return };
                let entries = transcript::entries(&thread.rows);
                let holds = |entry: &transcript::Entry<'_>| match entry {
                    transcript::Entry::Row(row) => row.id == id,
                    transcript::Entry::Group(steps) => steps.iter().any(|s| s.id == id),
                };
                if let Some(at) = entries.iter().position(holds) {
                    // Within what is drawn, and out of its group's fold.
                    self.transcript.shown = self.transcript.shown.max(entries.len() - at);
                    if let transcript::Entry::Group(steps) = &entries[at] {
                        let key = format!("group:{}", steps[0].group);
                        if !self.transcript.open(&key, false) {
                            self.transcript.toggle(key);
                        }
                    }
                }
            }
        }
    }

    /// Asks for the pictures the page shows now (more of it was opened).
    fn refresh_pictures(&mut self) {
        if let Some(core) = self.core.clone() {
            let pictures = self.thread_pictures();
            self.media.refresh(&core, &self.store.sidebar, &self.store.settings, &pictures);
        }
    }

    /// Opens a web page in the browser. Only a web page: nothing else the server names is handed to the shell.
    fn open_url(&self, url: &str) {
        if url.starts_with("https://") || url.starts_with("http://") {
            if let Err(error) = open::that(url) {
                tracing::warn!("the page could not be opened: {error}");
            }
        }
    }

    /// A key in the composer that does more than type.
    fn composer_key(&mut self, action: composer::KeyAction) {
        let composer = Rc::clone(&self.store.composer);
        let mention = composer.mention.as_ref().filter(|m| !m.suggestions.is_empty());
        let commands = if composer.skills.is_empty() { &composer.slash_commands } else { &composer.skills };
        match action {
            composer::KeyAction::Enter => {
                if let Some(mention) = mention {
                    if let Some(file) = mention.suggestions.get(mention.highlighted as usize).or_else(|| mention.suggestions.first()) {
                        self.send(Event::PickMention { path: file.path.clone() });
                    }
                } else if let Some(command) = commands.get(self.mark).or_else(|| commands.first()) {
                    let name = command.name.clone();
                    self.mark = 0;
                    self.send(if composer.skills.is_empty() { Event::PickSlashCommand { name } } else { Event::PickSkill { name } });
                } else if composer.can_send && !composer.sending {
                    self.recalling = false;
                    self.models_open = false;
                    self.send(Event::Send);
                }
            }
            composer::KeyAction::Move(delta) => {
                // The core keeps the mark in the list of files; this window the one in the commands.
                if mention.is_some() {
                    self.send(Event::MoveMention { delta });
                } else {
                    self.mark = composer::moved(self.mark, delta, commands.len());
                }
            }
            composer::KeyAction::Recall(delta) => {
                self.recalling = true;
                self.send(Event::RecallPrompt { delta });
            }
            composer::KeyAction::TogglePlan => self.send(Event::SetInteractionMode { mode: if composer.interaction_mode == "plan" { "default" } else { "plan" }.into() }),
            composer::KeyAction::Dismiss => self.send(Event::DismissMention),
            composer::KeyAction::Paste => {
                // Text pastes into the box as ever; a picture on the clipboard is attached.
                if let Some(core) = &self.core {
                    core.attach(|| {
                        let mut clipboard = arboard::Clipboard::new().map_err(|_| String::new())?;
                        let picture = clipboard.get_image().map_err(|_| String::new())?;
                        attach::from_rgba("pasted-image.png", picture.width, picture.height, &picture.bytes)
                    });
                }
            }
        }
    }

    /// Reads the files at `paths` as pictures for the message.
    fn attach(&self, paths: impl IntoIterator<Item = PathBuf>) {
        if let Some(core) = &self.core {
            for path in paths {
                core.attach(move || attach::from_file(&path));
            }
        }
    }

    /// Puts `text` on the clipboard.
    fn copy(&self, text: String) {
        if let Err(error) = arboard::Clipboard::new().and_then(|mut clipboard| clipboard.set_text(text)) {
            tracing::warn!("nothing could be put on the clipboard: {error}");
        }
    }

    /// A keyboard shortcut.
    fn command(&mut self, command: Command) {
        match command {
            Command::NewThread => {
                self.in_settings = false;
                self.send(Event::NewThread { project_id: None });
            }
            Command::ScratchThread => {
                self.in_settings = false;
                self.send(Event::OpenScratch);
            }
            Command::Settings => self.open_settings(),
            Command::ToggleSidebar => {
                self.prefs.show_sidebar = !self.prefs.show_sidebar;
                self.prefs.save(IDENTITY);
            }
            Command::Stop => {
                if self.store.composer.can_stop {
                    self.send(Event::Interrupt);
                }
            }
            Command::Find => {
                if self.store.thread.is_some() && !self.in_settings {
                    self.send(Event::FindInThread { query: self.transcript.find.clone() });
                }
            }
            Command::FindNext => self.send(Event::FindNext { forward: true }),
            Command::FindPrevious => self.send(Event::FindNext { forward: false }),
            Command::Palette => self.send(if self.store.palette.is_some() { Event::ClosePalette } else { Event::OpenPalette }),
            Command::PaletteFiles | Command::PaletteContent => {
                if self.store.palette.is_none() {
                    self.send(Event::OpenPalette);
                }
                self.send(Event::SetPaletteMode { mode: if command == Command::PaletteFiles { "files" } else { "content" }.into() });
            }
            Command::TogglePanel => self.send(Event::TogglePanel),
            Command::Panel(tab) => {
                self.in_settings = false;
                self.send(Event::ShowPanel { tab: tab.into() });
            }
            Command::Back => {
                // What is on top goes first. The palette shuts itself: Escape is a key of its search.
                if self.overlay != Overlay::None || self.store.palette.is_some() {
                } else if self.transcript.viewer.is_some() {
                    self.transcript.viewer = None;
                } else if self.in_settings {
                    self.in_settings = false;
                } else if self.store.thread.as_ref().is_some_and(|t| t.page.find.is_some()) {
                    self.send(Event::CloseFind);
                }
            }
        }
    }

    fn open_settings(&mut self) {
        if !self.in_settings {
            self.in_settings = true;
            // The first time, the server's own settings; after, where it was left.
            let connected = self.store.connection.state == "connected";
            let section = if self.settings.section == settings::Section::Appearance && connected { settings::Section::General } else { self.settings.section };
            self.open_section(section);
        }
    }

    /// Shows `section` of Settings and asks the core for what it shows.
    fn open_section(&mut self, section: settings::Section) {
        self.settings.section = section;
        for event in section.loads() {
            self.send(event);
        }
    }

    /// A choice from a menu, or a button that opens a dialog: the Add menu (`add:…`), a project's
    /// (`rename:<id>`, …), a server's (`env-forget:<id>`, …), an archived thread's.
    fn menu(&mut self, key: &str) {
        let (what, id) = key.split_once(':').unwrap_or((key, ""));
        if (what, id) == ("perm", "full-access") {
            self.overlay = Overlay::ConfirmFullAccess;
            return;
        }
        // Most choices only tell the core something.
        if let Some(event) = menus::event(what, id, now_ms()) {
            self.send(event);
            return;
        }
        let title = || self.store.sidebar.projects.iter().find(|p| p.id == id).map(|p| p.title.clone()).unwrap_or_default();
        let thread_title = || {
            let listed = sidebar::find_row(&self.store.sidebar, id).or_else(|| self.store.settings.archived.iter().find(|t| t.id == id));
            listed.map(|t| t.title.clone()).unwrap_or_default()
        };
        let server = || self.store.connection.environments.iter().find(|e| e.id == id).map(|e| e.label.clone()).unwrap_or_default();
        match (what, id) {
            ("icon", id) => {
                if let Some(project) = self.store.sidebar.projects.iter().find(|p| p.id == id) {
                    let icon = &project.icon;
                    let kind = if !icon.custom {
                        "auto"
                    } else if icon.file_path.is_some() {
                        "image"
                    } else {
                        icon.kind.as_str()
                    };
                    let or = |text: &str, default: &str| if text.is_empty() { default.to_owned() } else { text.to_owned() };
                    self.overlay = Overlay::IconPicker {
                        project_id: id.to_owned(),
                        kind: kind.to_owned(),
                        text: if icon.custom { icon.text.clone() } else { String::new() },
                        symbol: or(&icon.symbol, "folder-code"),
                        color: or(&icon.color, "gray"),
                        query: String::new(),
                    };
                    if kind == "image" {
                        self.send(Event::SearchProjectIconFiles { project_id: id.to_owned(), query: String::new() });
                    }
                }
            }
            ("provider-add", _) => self.overlay = Overlay::ProviderEditor(Box::new(overlays::ProviderDraft::new_antigravity())),
            ("provider-edit", id) => {
                if let Some(provider) = self.store.settings.providers.iter().find(|p| p.instance_id == id) {
                    self.overlay = Overlay::ProviderEditor(Box::new(overlays::ProviderDraft::of(provider)));
                }
            }
            ("provider-remove", id) => {
                let name = self.store.settings.providers.iter().find(|p| p.instance_id == id).map_or_else(|| id.to_owned(), |p| if p.name.is_empty() { id.to_owned() } else { p.name.clone() });
                self.overlay = Overlay::RemoveProvider { instance_id: id.to_owned(), name };
            }
            ("task-delete" | "task-rotate", id) => {
                let name = self.store.settings.scheduled_tasks.iter().find(|t| t.id == id).map(|t| t.name.clone()).unwrap_or_default();
                self.overlay = if what == "task-delete" { Overlay::DeleteTask { id: id.to_owned(), name } } else { Overlay::RotateToken { id: id.to_owned(), name } };
            }
            ("task-new", _) => {
                // In the project of the thread on screen, else the one chosen for a new thread.
                let project = self.store.thread.as_ref().map(|t| t.page.project_id.clone()).or_else(|| self.store.sidebar.selected_project_id.clone());
                self.overlay = Overlay::TaskEditor(Box::new(settings::task::TaskDraft::new_task(project.unwrap_or_default())));
            }
            ("task-edit", id) => {
                if let Some(task) = self.store.settings.scheduled_tasks.iter().find(|t| t.id == id) {
                    self.overlay = Overlay::TaskEditor(Box::new(settings::task::TaskDraft::from_task(task)));
                }
            }
            ("t-schedule", id) => {
                // The editor lists the projects and the threads a task can go into.
                for event in settings::Section::Scheduled.loads() {
                    self.send(event);
                }
                let draft = match self.store.thread.as_ref().filter(|t| t.page.id == id) {
                    Some(open) => settings::task::TaskDraft::for_thread(&open.page),
                    // A thread not on screen: its project is known, its model is the composer's.
                    None => {
                        let mut draft = settings::task::TaskDraft::new_task(sidebar::find_row(&self.store.sidebar, id).map(|row| row.project_id.clone()).unwrap_or_default());
                        draft.apply(settings::task::TaskEdit::ThreadMode("continue".into()));
                        draft.apply(settings::task::TaskEdit::Thread(id.to_owned()));
                        draft.apply(settings::task::TaskEdit::Name(thread_title().into()));
                        draft
                    }
                };
                self.overlay = Overlay::TaskEditor(Box::new(draft));
            }
            ("companion-server", id) => self.settings.companion_server = Some(id.to_owned()),
            ("go", "palette") => self.command(Command::Palette),
            ("go", "files") => self.command(Command::PaletteFiles),
            ("go", "content") => self.command(Command::PaletteContent),
            ("thread-open", id) => {
                self.send(Event::SelectThread { thread_id: id.to_owned() });
                self.in_settings = false;
            }
            ("t-rename", id) => self.overlay = Overlay::RenameThread { thread_id: id.to_owned(), title: thread_title() },
            ("t-copy", id) => self.copy(id.to_owned()),
            ("earlier", id) => {
                if !self.earlier.remove(id) {
                    self.earlier.insert(id.to_owned());
                }
            }
            ("next-waiting", _) => {
                if let Some(thread_id) = sidebar::next_waiting(&self.store.sidebar).map(str::to_owned) {
                    self.send(Event::SelectThread { thread_id });
                }
            }
            ("settings", "servers") => {
                self.in_settings = true;
                self.open_section(settings::Section::Servers);
            }
            ("env-rename", id) => self.overlay = Overlay::RenameServer { id: id.to_owned(), label: server() },
            ("env-forget", id) => self.overlay = Overlay::ForgetServer { id: id.to_owned(), label: server() },
            ("env-add", _) => self.overlay = Overlay::AddServer { link: String::new(), url: String::new(), code: String::new() },
            ("thread-delete", id) => self.overlay = Overlay::DeleteThread { thread_id: id.to_owned(), title: thread_title() },
            ("add", "browse") => {
                self.overlay = Overlay::FolderBrowser { path: String::new() };
                self.send(Event::BrowseFolders { path: String::new() });
            }
            ("add", "path") => self.overlay = Overlay::AddByPath { path: String::new() },
            ("add", "create") => self.overlay = Overlay::CreateProject { name: String::new() },
            ("add", "import") => {
                self.overlay = Overlay::ImportSessions { picked: Default::default(), seeded: false };
                self.send(Event::ScanAgentSessions);
            }
            ("add", "scratch") => self.send(Event::OpenScratch),
            ("rename", id) => self.overlay = Overlay::RenameProject { project_id: id.to_owned(), title: title() },
            ("remove", id) => self.overlay = Overlay::RemoveProject { project_id: id.to_owned(), title: title() },
            _ => {}
        }
    }
}

impl MainWindow {
    /// A picture over the whole window: the one at `at` of `pictures`, with the way to its neighbours and out.
    fn viewer(&self, pictures: &[domovoi_core::view::ImageView], at: usize, on_act: &Callback<Act>) -> View {
        let Some(picture) = pictures.get(at) else { return Border::new().into() };
        let button = |glyph: &str, name: &str, act: Act| {
            let on_act = on_act.clone();
            Button::new().style(ButtonStyle::Default).automation_name(name).content(theme::glyph(glyph, 14.0)).on_click(move || on_act.call(act.clone()))
        };
        let shown: View = match self.media.image(&picture.url) {
            Some(image) => Image::new().source_data(image).stretch(Stretch::Uniform).margin(Thickness::new(64.0, 56.0, 64.0, 56.0)).into(),
            None => TextBlock::new().text("Loading the picture…").horizontal_alignment(HorizontalAlignment::Center).vertical_alignment(VerticalAlignment::Center).into(),
        };
        let mut layers: Vec<View> = vec![
            shown,
            TextBlock::new()
                .text(if pictures.len() > 1 { format!("{}  ·  {} of {}", picture.name, at + 1, pictures.len()) } else { picture.name.clone() })
                .horizontal_alignment(HorizontalAlignment::Center)
                .vertical_alignment(VerticalAlignment::Bottom)
                .margin(Thickness::uniform(20.0))
                .into(),
            button("\u{E711}", "Close the picture", Act::Viewer(None)).horizontal_alignment(HorizontalAlignment::Right).vertical_alignment(VerticalAlignment::Top).margin(Thickness::uniform(14.0)).into(),
        ];
        if at > 0 {
            layers.push(button("\u{E76B}", "Previous picture", Act::Viewer(Some(-1))).horizontal_alignment(HorizontalAlignment::Left).vertical_alignment(VerticalAlignment::Center).margin(Thickness::uniform(14.0)).into());
        }
        if at + 1 < pictures.len() {
            layers.push(button("\u{E76C}", "Next picture", Act::Viewer(Some(1))).horizontal_alignment(HorizontalAlignment::Right).vertical_alignment(VerticalAlignment::Center).margin(Thickness::uniform(14.0)).into());
        }
        // Over the title bar too, on a shade that keeps the window's own look out of the picture.
        Border::new().grid_row_span(2).background(Color::argb(0xE6, 0x10, 0x10, 0x10)).content(Grid::new().children(layers)).into()
    }
}

impl Component for MainWindow {
    type Input = Launch;
    type Message = Message;

    fn create(launch: &Launch, context: &ComponentContext<Self>) -> Self {
        if !context.run_window(|handle| Message::Handle(handle.as_raw() as isize)) {
            tracing::warn!("the window has no handle to give: its focus and closing go unnoticed");
        }
        let prefs = Prefs::load(IDENTITY);
        // Where the thread's page is: told only when it comes to its end or leaves it.
        let scroll: ElementRef<ScrollViewer> = ElementRef::new();
        let scrolled = {
            let (sender, at_end) = (context.sender(), Cell::new(true));
            scroll.observe_view(move |at: ScrollPosition| {
                let end = at.vertical_offset + at.viewport_height >= at.extent_height - 48.0;
                if end != at_end.replace(end) {
                    let _ = sender.send(Message::AtEnd(end));
                }
            })
        };
        let composer_keys: Rc<Cell<composer::Keys>> = Rc::default();
        let terminal_keys: Rc<Cell<bool>> = Rc::default();
        let opens_at = prefs.window.map(|w| WindowPlacement { x: w.x, y: w.y, width: w.width, height: w.height, maximized: w.maximized });
        let sender = context.sender();
        if !ui_thread::install(move || {
            let _ = sender.send(Message::CoreWoke);
        }) {
            tracing::error!("the UI thread's wake window could not be made: the window will not update");
        }
        let (core, failed) = match CoreLink::start() {
            Ok(core) => (Some(core), None),
            Err(e) => (None, Some(format!("Domovoi could not start its runtime: {e}"))),
        };
        if let Some(core) = &core {
            core.send(Event::Tick { now_ms: crate::core_link::now_ms() });
            let (iana, utc_offset_secs) = domovoi_desktop_host::local_timezone();
            core.send(Event::SetTimeZone { iana, utc_offset_secs });
            core.send(Event::Launch { surface: "desktop".into(), device_label: domovoi_desktop_host::device_label(), device_os: domovoi_desktop_host::device_os().into() });
            // The link came from outside the app: the core asks before it connects.
            if let Some(link) = &launch.link {
                core.send(Event::OpenLink { link: link.clone() });
            } else if let Some(server) = &launch.server {
                core.send(Event::Connect { server_url: server.clone(), pairing_code: launch.pairing.clone().unwrap_or_default() });
            }
        }
        MainWindow {
            core,
            failed,
            store: Store::default(),
            media: Media::default(),
            dark: false,
            on_scheme: context.callback(Message::Scheme),
            pairing: pairing::Draft::default(),
            draft: String::new(),
            draft_revision: 0,
            caret_to_end: false,
            caret_timer: None,
            recalling: false,
            mark: 0,
            models_open: false,
            model_query: String::new(),
            model_requests: 0,
            on_composer_keys: {
                let (sender, keys) = (context.sender(), Rc::clone(&composer_keys));
                RoutedCallback::new(move |info: KeyEventInfo| match composer::key_action(info.key, info.modifiers, keys.get()) {
                    Some((action, handled)) => {
                        let _ = sender.send(Message::ComposerKey(action));
                        handled
                    }
                    None => false,
                })
            },
            composer_keys,
            pending: pending::State::default(),
            transcript: transcript::State::default(),
            scroll,
            _scrolled: scrolled,
            scroll_due: false,
            scroll_again: 0,
            scroll_timer: None,
            rows_seen: 0,
            diagram_loads: HashSet::new(),
            picker: new_thread::Picker::default(),
            overlay: Overlay::None,
            size: WindowSize { width: SIZE.0, height: SIZE.1 },
            in_settings: false,
            settings: settings::State::default(),
            prefs,
            route_seen: Rc::from(""),
            focus_search: false,
            focus_timer: None,
            search_field: ElementRef::new(),
            composer_field: ElementRef::new(),
            on_size: context.callback(Message::Resized),
            on_placement: context.callback(Message::Placed),
            on_dialog_closed: context.callback(Message::DialogClosed),
            on_section: context.callback(Message::Section),
            on_keys: {
                let (sender, terminal) = (context.sender(), Rc::clone(&terminal_keys));
                RoutedCallback::new(move |info: KeyEventInfo| match shortcuts::command(info.key, info.modifiers) {
                    // The terminal has the keyboard: the key is the shell's.
                    Some(command) if terminal.get() && !command.over_terminal() => false,
                    Some(command) => {
                        let _ = sender.send(Message::Command(command));
                        command.handled()
                    }
                    None => false,
                })
            },
            on_picker_keys: {
                let sender = context.sender();
                RoutedCallback::new(move |info: KeyEventInfo| match info.key {
                    VirtualKey::UP | VirtualKey::DOWN | VirtualKey::ENTER | VirtualKey::ESCAPE => {
                        let _ = sender.send(Message::PickerKey(info.key));
                        true
                    }
                    _ => false,
                })
            },
            scheduled_retry: 0,
            focused_thread: None,
            // Until the window says otherwise: a notification for the thread on screen would be noise.
            focused: true,
            last_focus: Instant::now(),
            watch: None,
            opens_at,
            search: String::new(),
            earlier: HashSet::new(),
            smoke: launch.smoke,
            panel: panel::State::default(),
            pulls: pull_requests::State::default(),
            terminal: ElementRef::new(),
            on_terminal_keys: {
                let sender = context.sender();
                RoutedCallback::new(move |info: KeyEventInfo| match panel::terminal::key(info.key, info.modifiers) {
                    Some(act) => {
                        let _ = sender.send(Message::Panel(act));
                        true
                    }
                    None => false,
                })
            },
            on_terminal_chars: {
                let (sender, half) = (context.sender(), Cell::new(None));
                RoutedCallback::new(move |info: CharacterEventInfo| {
                    if let Some(act) = panel::terminal::typed(info.character, info.modifiers, &half) {
                        let _ = sender.send(Message::Panel(act));
                    }
                    true
                })
            },
            terminal_keys,
            palette_query: String::new(),
            palette_mode: None,
            palette_field: ElementRef::new(),
            on_palette_keys: {
                let sender = context.sender();
                RoutedCallback::new(move |info: KeyEventInfo| {
                    let used = palette::key(info.key).is_some();
                    if used {
                        let _ = sender.send(Message::PaletteKey(info.key));
                    }
                    used
                })
            },
            focus_palette: false,
            reaction: companion::Reaction::default(),
            reaction_until: None,
            reaction_timer: None,
            terminal_shown: None,
            focus_terminal: false,
        }
    }

    fn update(&mut self, message: Message, context: &ComponentContext<Self>) {
        match message {
            Message::CoreWoke => {
                self.absorb();
                if std::mem::take(&mut self.scroll_due) {
                    // Once the rows are drawn, and twice more while pictures and long text settle.
                    self.scroll_again = 2;
                    self.scroll_timer = Some(context.set_local_timeout(Duration::from_millis(30), || Message::ScrollToEnd));
                }
                if std::mem::take(&mut self.caret_to_end) {
                    self.caret_timer = Some(context.set_local_timeout(Duration::from_millis(40), || Message::CaretToEnd));
                }
                if std::mem::take(&mut self.focus_search) {
                    // After the page is drawn: the box does not exist before.
                    self.focus_timer = Some(context.set_local_timeout(Duration::from_millis(60), || Message::FocusSearch));
                }
                if let Some(until) = self.reaction_until.take() {
                    let lasts = Duration::from_millis((until - now_ms()).max(0) as u64 + 30);
                    self.reaction_timer = Some(context.set_local_timeout(lasts, || Message::ReactionOver));
                }
                if std::mem::take(&mut self.focus_terminal) {
                    self.focus_timer = Some(context.set_local_timeout(Duration::from_millis(80), || Message::Panel(panel::Act::Clicked)));
                }
                if std::mem::take(&mut self.focus_palette) {
                    self.focus_timer = Some(context.set_local_timeout(Duration::from_millis(60), || Message::FocusPalette));
                }
            }
            Message::Handle(window) => {
                let (sender, core) = (context.sender(), self.core.clone());
                self.watch = Some(hwnd::watch(window, move |signal| match signal {
                    hwnd::Signal::Active(active) => {
                        let _ = sender.send(Message::Focused(active));
                    }
                    hwnd::Signal::Closing => {
                        if let Some(core) = &core {
                            core.shutdown();
                        }
                    }
                }));
            }
            Message::Focused(focused) => {
                if focused != self.focused {
                    tracing::debug!("the window {} the focus", if focused { "has" } else { "lost" });
                    self.focused = focused;
                    self.send(Event::AppFocus { focused });
                    if let Some(core) = &self.core {
                        core.set_focus(focused, self.focused_thread.clone());
                    }
                }
                if focused {
                    // Away for a while: the laptop may have moved to another network.
                    if self.last_focus.elapsed() >= FOCUS_NETWORK_CHECK {
                        self.send(Event::NetworkChanged);
                    }
                    self.last_focus = Instant::now();
                }
            }
            Message::Placed(at) => {
                let frame = WindowFrame { x: at.x, y: at.y, width: at.width, height: at.height, maximized: at.maximized };
                if self.prefs.window != Some(frame) {
                    self.prefs.window = Some(frame);
                    self.prefs.save(IDENTITY);
                }
            }
            Message::Command(command) => self.command(command),
            Message::Search(text) => {
                if *text != *self.search {
                    self.search = text.to_string();
                    self.send(Event::SetSearch { query: self.search.clone() });
                }
            }
            Message::ToastAction => self.send(Event::ToastAction),
            Message::ForgetServer => self.send(Event::ForgetEnvironment { id: self.store.connection.active_environment_id.clone() }),
            Message::Scheme(scheme) => self.dark = scheme == ColorScheme::Dark,
            Message::PairLink(text) => self.pairing.link = text.to_string(),
            Message::PairUrl(text) => self.pairing.url = text.to_string(),
            Message::PairCode(text) => self.pairing.code = text.to_string(),
            Message::ConnectLink => self.send(Event::ConnectRemote { link: self.pairing.link.trim().to_owned() }),
            Message::ConnectCode => self.send(Event::Connect { server_url: self.pairing.url.trim().to_owned(), pairing_code: self.pairing.code.trim().to_owned() }),
            Message::ConnectLocal => self.send(Event::ConnectLocalServer),
            Message::AnswerLink { id, accept } => self.send(Event::AnswerLinkRequest { id, accept }),
            Message::CaretToEnd => {
                self.caret_timer = None;
                let end = self.draft.encode_utf16().count();
                _ = self.composer_field.request_select(end as _, 0, |_| {});
            }
            Message::ComposerKey(action) => self.composer_key(action),
            Message::Models => {
                self.models_open = !self.models_open;
                if !self.models_open && !self.model_query.is_empty() {
                    self.model_query.clear();
                    self.send(Event::SetModelQuery { query: String::new() });
                }
            }
            Message::ModelQuery(text) => {
                if *text != *self.model_query {
                    self.model_query = text.to_string();
                    self.send(Event::SetModelQuery { query: self.model_query.clone() });
                }
            }
            Message::Attach => {
                let pictures = windows_pickers::OpenFilePicker::new().title("Attach Pictures").filter_extensions("Pictures", ["png", "jpg", "jpeg", "gif", "webp"]);
                if !pictures.request_multiple(context, |picked| Message::Picked(picked.unwrap_or_default())) {
                    tracing::warn!("the file dialog could not be opened");
                }
            }
            Message::Picked(paths) => self.attach(paths),
            Message::Dropped(dropped) => {
                if let DroppedData::StorageItems(items) = dropped {
                    self.attach(items.into_iter().map(|item| PathBuf::from(item.path)));
                }
            }
            Message::Transcript(act) => {
                self.transcript(act);
                if std::mem::take(&mut self.scroll_due) {
                    self.scroll_again = 1;
                    self.scroll_timer = Some(context.set_local_timeout(Duration::from_millis(30), || Message::ScrollToEnd));
                }
            }
            Message::AtEnd(end) => {
                self.transcript.following = end;
                if end {
                    self.transcript.unseen = false;
                }
            }
            Message::ScrollToEnd => {
                // Past the end: the viewer stops at it.
                _ = self.scroll.request_scroll_to(None, Some(1.0e7), false, |_| {});
                self.scroll_timer = if self.scroll_again > 0 {
                    self.scroll_again -= 1;
                    Some(context.set_local_timeout(Duration::from_millis(140), || Message::ScrollToEnd))
                } else {
                    None
                };
            }
            Message::Pending(edit) => {
                let page = self.store.thread.as_ref().map(|t| Rc::clone(&t.page));
                if let Some(event) = self.pending.apply(edit, page.as_deref()) {
                    self.send(event);
                }
            }
            Message::Draft(text) => {
                if *text != *self.draft {
                    self.draft = text.to_string();
                    // Typing ends the going through earlier messages, and starts a list's mark over.
                    self.recalling = false;
                    self.mark = 0;
                    self.send(Event::SetDraft { text: self.draft.clone() });
                }
            }
            Message::Resized(size) => {
                self.size = size;
                self.fit_terminal();
            }
            Message::Query(text) => {
                if *text != *self.picker.query {
                    self.picker = new_thread::Picker { query: text.to_string(), marked: 0 };
                }
            }
            Message::PickerKey(key) => self.picker_key(key),
            Message::FocusSearch => {
                self.focus_timer = None;
                _ = self.search_field.request_focus();
            }
            Message::Menu(key) => {
                if let Some(key) = key.as_str() {
                    self.menu(key);
                }
            }
            Message::OverlayEdit(edit) => {
                if let (Overlay::IconPicker { project_id, .. }, overlays::Edit::File(path)) = (&self.overlay, &edit) {
                    // A picture picked is the icon at once, and the picker is done.
                    self.send(Event::SetProjectIconFile { project_id: project_id.clone(), path: path.clone() });
                    self.send(Event::CloseProjectIconFiles { project_id: project_id.clone() });
                    self.overlay = Overlay::None;
                    return;
                }
                self.overlay.edit(edit);
                if let Overlay::IconPicker { project_id, kind, query, .. } = &self.overlay
                    && kind == "image"
                {
                    self.send(Event::SearchProjectIconFiles { project_id: project_id.clone(), query: query.clone() });
                }
            }
            Message::DialogClosed(result) => {
                let overlay = std::mem::take(&mut self.overlay);
                if let Overlay::IconPicker { project_id, .. } = &overlay {
                    self.send(Event::CloseProjectIconFiles { project_id: project_id.clone() });
                }
                if result == ContentDialogResult::Primary {
                    match overlay {
                        // A task is the task, and its thread's permissions when they were changed.
                        Overlay::TaskEditor(draft) if draft.can_save() => {
                            for event in draft.saved() {
                                self.send(event);
                            }
                        }
                        overlay => {
                            if let Some(event) = overlay.confirmed(&self.store.folder_browser) {
                                self.send(event);
                            }
                        }
                    }
                }
            }
            Message::OpenSettings => self.open_settings(),
            Message::CloseSettings => self.in_settings = false,
            Message::Section(tag) => {
                if let Some(section) = tag.as_deref().and_then(settings::Section::from_tag).filter(|s| *s != self.settings.section) {
                    self.open_section(section);
                }
            }
            Message::Appearance(appearance) => {
                self.prefs.appearance = appearance;
                self.prefs.save(IDENTITY);
            }
            Message::TextSize(size) => {
                self.prefs.text_size = size as f32;
                self.prefs.save(IDENTITY);
            }
            Message::SettingDraft((key, text)) => {
                self.settings.drafts.insert(key, text.to_string());
            }
            Message::Copy(text) => self.copy(text),
            Message::OpenUrl(url) => self.open_url(&url),
            Message::DismissToast => self.send(Event::DismissToast),
            Message::Panel(act) => self.panel(act),
            Message::Pulls(act) => {
                let (event, page) = self.pulls.apply(act);
                if let Some(event) = event {
                    self.send(event);
                }
                if let Some(url) = page {
                    self.open_url(&url);
                }
            }
            // Drawn again: the face is as the server's work has it.
            Message::ReactionOver => self.reaction_timer = None,
            Message::PaletteQuery(text) => {
                if *text != *self.palette_query {
                    self.palette_query = text.to_string();
                    self.send(Event::SetPaletteQuery { query: self.palette_query.clone() });
                }
            }
            Message::PaletteKey(key) => {
                if let Some(event) = palette::key(key) {
                    self.send(event);
                }
            }
            Message::FocusPalette => {
                self.focus_timer = None;
                _ = self.palette_field.request_focus();
            }
        }
    }

    fn view(&self, _launch: &Launch, context: &mut ViewContext<'_, Self>) -> View {
        let title = self.store.thread.as_ref().map_or_else(|| "Domovoi".to_owned(), |t| format!("{} - Domovoi", t.page.title));
        context.window_title(title);
        let visuals = WindowVisuals::new()
            .backdrop(WindowBackdrop::Mica)
            .theme(match self.prefs.appearance {
                Appearance::System => WindowTheme::System,
                Appearance::Light => WindowTheme::Light,
                Appearance::Dark => WindowTheme::Dark,
            })
            .constraints(WindowConstraints { min_width: Some(MIN_SIZE.0), min_height: Some(MIN_SIZE.1), ..WindowConstraints::default() });
        context.window_visuals(match self.opens_at {
            Some(at) => visuals.initial_placement(at),
            None => visuals.client_size(SIZE.0, SIZE.1),
        });
        context.on_window_placement(self.on_placement.clone());
        context.on_color_scheme(self.on_scheme.clone());
        context.on_window_size(self.on_size.clone());
        let icons = sidebar::Icons { media: &self.media, dark: self.dark };
        let on_act = context.callback(Message::Transcript);
        {
            let composer = &self.store.composer;
            let mention = composer.mention.as_ref().is_some_and(|m| !m.suggestions.is_empty());
            self.composer_keys.set(composer::Keys {
                list: mention || !composer.skills.is_empty() || !composer.slash_commands.is_empty(),
                mention,
                recall: self.store.thread.is_some() && (self.recalling || !self.draft.contains('\n')),
                plan: composer.shows_interaction_mode || composer.interaction_mode == "plan",
            });
        }
        let subtitle = match self.store.connection.state.as_str() {
            "connected" => self.store.connection.environment_label.clone(),
            "" => String::new(),
            other => other.to_owned(),
        };
        let in_settings = self.in_settings && self.core.is_some() && self.failed.is_none();
        let title_bar = TitleBar::new()
            .title("Domovoi")
            .subtitle(if in_settings { "Settings".to_owned() } else { subtitle })
            .is_back_button_visible(in_settings)
            .on_back_requested(context.callback(|()| Message::CloseSettings));
        let body: View = match (&self.core, &self.failed) {
            (_, Some(failed)) => TextBlock::new().text(failed).margin(Thickness::uniform(24.0)).into(),
            (None, None) => Border::new().into(),
            (Some(core), None) if in_settings => settings::page(
                core,
                &settings::Shown {
                    connection: &self.store.connection,
                    settings: &self.store.settings,
                    prefs: &self.prefs,
                    state: &self.settings,
                    icons,
                },
                settings::Hooks {
                    on_section: &self.on_section,
                    on_appearance: context.callback(Message::Appearance),
                    on_text_size: context.callback(Message::TextSize),
                    on_draft: context.callback(Message::SettingDraft),
                    on_menu: context.callback(Message::Menu),
                    on_copy: context.callback(Message::Copy),
                    on_open: context.callback(Message::OpenUrl),
                },
            ),
            (Some(_), None) if self.store.needs_pairing() => pairing::pairing(
                &self.store.connection,
                &self.pairing,
                pairing::Hooks {
                    on_connect_local: context.callback(|()| Message::ConnectLocal),
                    on_link: context.callback(Message::PairLink),
                    on_url: context.callback(Message::PairUrl),
                    on_code: context.callback(Message::PairCode),
                    on_pair_link: context.callback(|()| Message::ConnectLink),
                    on_pair_code: context.callback(|()| Message::ConnectCode),
                    on_forget: context.callback(|()| Message::ForgetServer),
                },
            ),
            (Some(core), None) => {
                // The pages beside the threads take the thread's place, and need no composer or panel.
                let beside_threads = matches!(&*self.store.route, "pull-requests" | "usage");
                let draw = transcript::Draw { size: f64::from(self.prefs.text_size), dark: self.dark, media: &self.media, diagrams: &self.store.diagrams, state: &self.transcript, on_act: &on_act };
                let main = match &self.store.thread {
                    _ if &*self.store.route == "pull-requests" => pull_requests::page(core, &self.store.pull_requests, &self.pulls, &draw, &context.callback(Message::Pulls)),
                    _ if &*self.store.route == "usage" => usage::page(core, &self.store.usage, self.dark, &context.callback(Message::OpenUrl)),
                    Some(open) => thread::thread(
                        core,
                        open,
                        &self.store.sidebar,
                        icons,
                        &self.pending,
                        &draw,
                        thread::Hooks {
                            pending: &pending::Hooks { on_edit: context.callback(Message::Pending), on_open: context.callback(Message::OpenUrl), size: f64::from(self.prefs.text_size), dark: self.dark },
                            on_menu: context.callback(Message::Menu),
                            scroll: &self.scroll,
                        },
                    ),
                    None if &*self.store.route == "new-thread" => new_thread::new_thread(
                        core,
                        &self.store.sidebar,
                        &self.store.composer,
                        &self.picker,
                        icons,
                        new_thread::Hooks {
                            on_query: context.callback(Message::Query),
                            on_keys: self.on_picker_keys.clone(),
                            on_menu: context.callback(Message::Menu),
                            search: &self.search_field,
                            columns: new_thread::columns(self.size.width),
                            list_height: new_thread::list_height(self.size.height),
                        },
                    ),
                    None => thread::empty(),
                };
                let mut right: Vec<View> = Vec::new();
                if let Some(toast) = &self.store.toast {
                    right.push(widgets::toast(toast, context.callback(|()| Message::ToastAction), context.callback(|()| Message::DismissToast)));
                }
                let toasts = right.len() as i32;
                right.push(Border::new().grid_row(toasts).content(main).into());
                let composer: View = if beside_threads {
                    Border::new().into()
                } else {
                    composer::composer(
                            core,
                            &self.store.composer,
                            &composer::Local { draft: &self.draft, models_open: self.models_open, model_query: &self.model_query, mark: self.mark },
                            composer::Hooks {
                                field: &self.composer_field,
                                on_draft: context.callback(Message::Draft),
                                on_keys: self.on_composer_keys.clone(),
                                on_menu: context.callback(Message::Menu),
                                on_model_query: context.callback(Message::ModelQuery),
                                on_models: context.callback(|()| Message::Models),
                                on_attach: context.callback(|()| Message::Attach),
                                on_drop: context.callback(Message::Dropped),
                            },
                        )
                };
                right.push(Border::new().grid_row(toasts + 1).content(composer).into());
                let rows: Vec<GridLength> = (0..toasts).map(|_| GridLength::Auto).chain([GridLength::STAR, GridLength::Auto]).collect();
                // Ctrl+B folds the list away.
                let list: View = if self.prefs.show_sidebar {
                    sidebar::sidebar(
                        core,
                        &self.store.connection,
                        &self.store.sidebar,
                        icons,
                        &sidebar::Local {
                            search: &self.search,
                            earlier: &self.earlier,
                            now_ms: now_ms(),
                            open: self.store.thread.as_ref().map(|t| &*t.page),
                            mood: companion::mood_for(self.store.connection.state == "connected", &self.store.connection.activity, self.reaction.mood(now_ms())),
                        },
                        sidebar::Hooks {
                            on_settings: context.callback(|()| Message::OpenSettings),
                            on_search: context.callback(Message::Search),
                            on_menu: context.callback(Message::Menu),
                        },
                    )
                } else {
                    Border::new().into()
                };
                let (list_width, left_gap) = if self.prefs.show_sidebar { (theme::SIDEBAR, 0.0) } else { (0.0, theme::GAP) };
                let card = |column: i32, left: f64, content: View| -> View {
                    Border::new()
                        .grid_column(column)
                        .margin(Thickness::new(left, 0.0, theme::GAP, theme::GAP))
                        .background(theme::card())
                        .border_brush(theme::stroke())
                        .border_thickness(Thickness::uniform(1.0))
                        .corner_radius(theme::RADIUS)
                        .content(content)
                        .into()
                };
                let mut columns: Vec<View> = vec![Border::new().content(list).into(), card(1, left_gap, Grid::new().rows(rows).children(right).into())];
                // The panel beside the thread, when the core has it open.
                let beside = self.store.panel.open && !beside_threads;
                if beside {
                    let on_panel = context.callback(Message::Panel);
                    let shown = panel::Shown {
                        panel: &self.store.panel,
                        thread_id: self.store.thread.as_ref().map(|t| t.page.id.as_str()),
                        state: &self.panel,
                        media: &self.media,
                        dark: self.dark,
                        on_act: &on_panel,
                    };
                    let hooks = panel::Hooks { terminal: &self.terminal, on_keys: self.on_terminal_keys.clone(), on_chars: self.on_terminal_chars.clone() };
                    columns.push(card(2, 0.0, panel::panel(core, &shown, hooks)));
                }
                Grid::new()
                    .columns([GridLength::Pixel(list_width), GridLength::STAR, GridLength::Pixel(if beside { self.panel_width() + theme::GAP } else { 0.0 })])
                    .children(columns)
                    .into()
            }
        };
        let dialog = match &self.core {
            Some(core) => overlays::dialog(
                &self.overlay,
                core,
                &overlays::Shown {
                    folders: &self.store.folder_browser,
                    sessions: &self.store.agent_sessions,
                    settings: &self.store.settings,
                    sidebar: &self.store.sidebar,
                    composer: &self.store.composer,
                    icons,
                },
                overlays::Hooks { on_closed: &self.on_dialog_closed, on_edit: context.callback(Message::OverlayEdit) },
            ),
            None => windows_reactor::ContentDialog::new().is_open(false),
        };
        let mut layers: Vec<View> = vec![title_bar.into(), Border::new().grid_row(1).on_preview_key_down(self.on_keys.clone()).content(body).into()];
        if let Some((pictures, at)) = &self.transcript.viewer {
            layers.push(self.viewer(pictures, *at, &on_act));
        }
        if let (Some(core), Some(open)) = (&self.core, &self.store.palette) {
            let hooks = palette::Hooks { query: &self.palette_query, field: &self.palette_field, on_query: context.callback(Message::PaletteQuery), on_keys: self.on_palette_keys.clone() };
            // Over the title bar too; the window's shortcuts work in it (Ctrl+K shuts it).
            layers.push(Border::new().grid_row_span(2).on_preview_key_down(self.on_keys.clone()).content(palette::palette(core, open, hooks)).into());
        }
        // Last, so it draws over the rest and takes the presses meant for it.
        if let Some(request) = &self.store.connection.link_request {
            layers.push(link_dialog::link_dialog(request, context.callback(|(id, accept)| Message::AnswerLink { id, accept })));
        }
        Grid::new().rows([GridLength::Auto, GridLength::STAR]).children(layers).content_dialog(dialog)
    }
}
