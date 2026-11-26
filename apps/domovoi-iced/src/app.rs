//! Application state, messages, subscriptions, and update loop.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use domovoi_core::app::QuestionAnswer;
use domovoi_core::companion::CompanionLook;
use domovoi_core::view::{ImageView, ViewModel};
use domovoi_core::Event;
use iced::keyboard::{self, Key, key};
use iced::widget::{image, text_editor};
use iced::{Subscription, Task, Theme, event};

use crate::host::{self, HostHandle, HostMsg};
use crate::ui::prefs::Prefs;
use crate::ui::theme::{self, Appearance};
use crate::ui::tokens;
use crate::views::markdown::MdAction;

const FOCUS_NETWORK_CHECK: Duration = Duration::from_secs(60);
/// A gap this long between 5 s ticks means the machine slept.
const SLEEP_GAP_MS: i64 = 60_000;
/// Loaded images kept before those nothing shows are let go.
pub const IMAGE_CACHE_LIMIT: usize = 128;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConnectMode {
    #[default]
    Local,
    Remote,
}

#[derive(Debug, Clone, Default)]
pub struct ConnectForm {
    pub mode: ConnectMode,
    pub server_url: String,
    pub pairing_code: String,
    pub remote_link: String,
    pub adding_server: bool,
}

impl ConnectForm {
    fn initial() -> Self {
        Self {
            mode: ConnectMode::Local,
            server_url: "http://127.0.0.1:13773".into(),
            pairing_code: String::new(),
            remote_link: String::new(),
            adding_server: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResizeSide {
    Sidebar,
    Panel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComposerPopover {
    Model,
    Effort,
    Permissions,
    Stash,
    Limits,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SettingsSection {
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

impl SettingsSection {
    pub fn is_server_scoped(self) -> bool {
        !matches!(self, SettingsSection::Appearance | SettingsSection::Servers)
    }

    pub fn requires_connection(self) -> bool {
        self.is_server_scoped() && self != SettingsSection::Companion
    }

    pub fn title(self) -> &'static str {
        match self {
            SettingsSection::Appearance => "Appearance",
            SettingsSection::Servers => "Servers",
            SettingsSection::General => "General",
            SettingsSection::Providers => "Providers",
            SettingsSection::Projects => "Projects",
            SettingsSection::Scheduled => "Scheduled Tasks",
            SettingsSection::Keybindings => "Keybindings",
            SettingsSection::Access => "Access & Remote",
            SettingsSection::Archived => "Archived Threads",
            SettingsSection::Maintenance => "Maintenance",
            SettingsSection::Companion => "Companion",
        }
    }

    pub fn core_name(self) -> Option<&'static str> {
        match self {
            SettingsSection::General => Some("general"),
            SettingsSection::Providers => Some("providers"),
            SettingsSection::Keybindings => Some("keybindings"),
            SettingsSection::Scheduled => Some("scheduled"),
            SettingsSection::Archived => Some("archived"),
            SettingsSection::Projects => Some("projects"),
            SettingsSection::Maintenance => Some("storage"),
            SettingsSection::Access => Some("access"),
            SettingsSection::Appearance | SettingsSection::Servers | SettingsSection::Companion => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Overlay {
    Settings(SettingsSection),
    ServerMenu,
    SidebarAddMenu,
    ThreadMenu(String),
    ProjectMenu(String),
    RenameThread { thread_id: String, value: String },
    RenameProject { project_id: String, value: String },
    RenameEnvironment { env_id: String, value: String },
    ConfirmDeleteThread { thread_id: String, title: String },
    ConfirmRemoveProject { project_id: String, title: String },
    ConfirmForgetServer { env_id: String, label: String },
    ConfirmFullAccess,
    SnoozeThread { thread_id: String, title: String },
    NewProject { name: String, path: String },
    FolderBrowser,
    ImportSessions { selected: Vec<String> },
    ProjectIconPicker { project_id: String, kind: String, text: String, symbol: String, color: String, file_query: String },
    ScheduledTaskEditor(Box<crate::views::scheduled_editor::ScheduledDraft>),
    ProviderInstanceEditor(Box<crate::views::provider_editor::ProviderDraft>),
    SavePlanDialog { item_id: String, path: String },
    ContextPopover,
    /// An entry's images full-window, showing `images[index]`.
    ImageViewer { images: Vec<ImageView>, index: usize },
    /// What can be done with `images[index]` (a thumbnail's right-click).
    ImageMenu { images: Vec<ImageView>, index: usize },
}

/// Keys the image viewer answers while it is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewerKey {
    Previous,
    Next,
    First,
    Last,
    Copy,
    Save,
}

/// What to do with an image's bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageAction {
    Copy,
    Save,
    OpenExternally,
}

/// A secret typed into a secret request's card: shows in no debug output,
/// and is dropped once sent or once its card is gone.
#[derive(Clone, Default, PartialEq)]
pub struct SecretText(pub String);

impl std::fmt::Debug for SecretText {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SecretText(..)")
    }
}

#[derive(Debug, Clone, Default)]
pub struct QuestionDraft {
    pub selected: Vec<String>,
    pub custom: String,
}

impl QuestionDraft {
    /// The picked options, then the typed answer (when there is one).
    pub fn values(&self) -> Vec<String> {
        let mut values = self.selected.clone();
        if !self.custom.trim().is_empty() {
            values.push(self.custom.trim().to_owned());
        }
        values
    }
}

#[derive(Debug, Clone)]
pub struct FileEditDraft {
    pub path: String,
    pub content_rev: u64,
}

#[derive(Debug, Clone, Default)]
pub struct WorktreeDraft {
    pub open: bool,
    pub base: String,
    pub branch: String,
}

#[derive(Debug, Clone, Default)]
pub struct PrReviewDraft {
    pub verdict: String,
    pub body: String,
    pub comment: String,
    pub merge_method: String,
}

#[derive(Default)]
pub struct TerminalServerSummary {
    pub terminals: Vec<domovoi_core::view::TerminalDetailView>,
    pub stale: bool,
    pub loaded: bool,
    busy: bool,
    next_poll_ms: i64,
    failures: u32,
}

pub struct TerminalSplitGrid {
    pub panes: iced::widget::pane_grid::State<bool>,
    split: iced::widget::pane_grid::Split,
    stacked: bool,
}

impl TerminalSplitGrid {
    fn new(stacked: bool) -> Self {
        use iced::widget::pane_grid::{Axis, State};
        let (mut panes, first) = State::new(false);
        let axis = if stacked { Axis::Horizontal } else { Axis::Vertical };
        let (_, split) = panes.split(axis, first, true).expect("the first pane can be split");
        Self { panes, split, stacked }
    }
}

pub struct App {
    pub host: Option<HostHandle>,
    pub vm: ViewModel,
    pub images: HashMap<String, image::Handle>,
    /// Image URLs whose fetch failed: shown with Retry, not asked for again by themselves.
    pub failed_images: HashSet<String>,
    /// Project icon thumbnails asked for, as `revision:path` (once each, like Apple's `.task(id:)`).
    requested_thumbnails: HashSet<String>,
    pub prefs: Prefs,
    pub system_dark: bool,
    pub focused: bool,
    last_focus: Instant,
    pub window_size: iced::Size,
    pub drag: Option<ResizeSide>,
    pub tick_phase: f32,
    /// Wall clock (Unix ms) as of the last `Clock`/`Tick`, for elapsed times.
    pub clock_ms: i64,
    /// Wall clock at the last `Tick`, to notice a sleep.
    last_tick_ms: i64,
    /// What the core was last told about the network (`None`: nothing yet),
    /// and the address the default route leaves from.
    online: Option<bool>,
    route_from: Option<std::net::IpAddr>,
    pub background_expanded: bool,
    pub companion_reaction: crate::views::companion::Reaction,
    pub transcript: crate::scroll::Transcript,
    pub companion_poked: bool,
    pub pending_deep_link: Option<String>,
    scheduled_retry_gen: u64,

    // Connect form
    pub connect: ConnectForm,

    // Sidebar local state
    pub search_input: String,
    pub earlier_shown: HashSet<String>,

    // Thread local state
    pub expanded_groups: HashSet<String>,
    pub expanded_items: HashSet<String>,
    pub find_input: String,
    pub question_drafts: HashMap<String, HashMap<String, QuestionDraft>>,
    /// What is typed into each secret request's card, by its item id.
    pub secret_drafts: HashMap<String, SecretText>,
    pub editing_queued: Option<(String, String)>,
    /// The thread whose plan and checklist are open above the composer.
    pub progress_open: Option<String>,

    // Composer local state
    pub draft: text_editor::Content,
    last_draft_rev: u32,
    /// Highlighted row of the composer's `/command` / `$skill` list (shell-side; the core has none).
    pub command_highlight: usize,
    last_model_picker_req: u32,
    pub composer_popover: Option<ComposerPopover>,
    pub attachment_error: Option<String>,

    // Side panel local state
    pub terminal_focused: bool,
    pub terminal_grid: TerminalSplitGrid,
    pub terminal_rename: Option<(String, String)>,
    pub all_terminal_servers: bool,
    pub terminal_servers: HashMap<String, TerminalServerSummary>,
    pub collapsed_diffs: HashSet<String>,
    pub git_commit_msg: String,
    pub git_branch_name: String,
    pub worktree_draft: WorktreeDraft,
    pub file_query_input: String,
    pub file_edit: Option<FileEditDraft>,
    pub file_editor: text_editor::Content,
    pub artifact_quick_edit: String,

    // Pull Requests local state
    pub pr_review: PrReviewDraft,

    // Settings local state
    pub provider_redirect_inputs: HashMap<String, String>,
    pub settings_server_picker_open: bool,
    pub pairing_qr: Option<(String, iced::widget::qr_code::Data)>,
    pub setting_text_drafts: HashMap<String, String>,

    // Overlays
    pub overlay: Option<Overlay>,
    pub app_icon_handle: image::Handle,
}

#[derive(Debug, Clone)]
pub enum Message {
    Host(HostMsg),
    Core(Box<Event>),
    // System & window
    SystemTheme(iced::theme::Mode),
    Tick,
    /// One-second clock while a turn runs (elapsed time, pulsing mark).
    Clock,
    RetryFired(u64),
    /// A `domovoi://` link from the OS or a later launch. The core asks
    /// before it connects (`ConnectionView.link_request`).
    DeepLink(String),
    /// The answer to the link dialog; `id` is the request's.
    AnswerLink { id: u32, accept: bool },
    /// Back from a sleep: the core reconnects.
    Woke,
    /// Where the default route leaves from (`None`: no route, offline).
    NetworkProbed(Option<std::net::IpAddr>),
    WindowFocused(bool),
    WindowResized(iced::Size),
    CloseRequested,
    Shortcut(&'static str),
    Escape,
    FileDropped(PathBuf),
    // Layout & prefs
    Appearance(Appearance),
    TextSize(f32),
    ToggleSidebar,
    ResizeStart(ResizeSide),
    ResizeMove(f32),
    ResizeEnd,
    // Connect form
    ConnectModeChanged(ConnectMode),
    ConnectUrlChanged(String),
    ConnectCodeChanged(String),
    ConnectLinkChanged(String),
    ConnectSubmit,
    /// Signs in to the server on this computer (`ConnectionView.local_server`).
    ConnectLocal,
    ShowAddServer(bool),
    // Sidebar
    SearchChanged(String),
    ToggleEarlier(String),
    // Thread & Markdown
    ToggleGroup(String),
    ToggleItem(String),
    Markdown(MdAction),
    FindQueryChanged(String),
    QuestionToggleOption { request_id: String, question_id: String, value: String, multi: bool },
    QuestionCustomChanged { request_id: String, question_id: String, text: String },
    QuestionSubmit { request_id: String },
    SecretChanged { item_id: String, value: SecretText },
    /// Sends the typed secret (`AnswerSecret`) and forgets it here.
    SecretSubmit { item_id: String },
    /// Opens or closes the plan and checklist above the composer.
    ToggleProgress,
    /// Carries out the plan from the progress strip (and closes it).
    ImplementPlan(String),
    /// A checkpoint's turn in the Diff panel.
    ViewDiff(String),
    /// A workspace file an edit wrote, in the Artifacts panel.
    PreviewArtifactFile(String),
    /// Scrolls the transcript to the row an artifact came from.
    ShowInThread(String),
    StartEditQueued { run_id: String, text: String },
    EditQueuedChanged(String),
    CommitEditQueued,
    CancelEditQueued,
    // Composer
    DraftAction(text_editor::Action),
    /// Plain Enter in the composer: picks the open list's entry, else sends.
    ComposerEnter,
    /// Up/Down through the open `/command` or `$skill` list.
    MoveCommand(i32),
    /// Earlier messages of the thread, like a shell's history.
    RecallPrompt(i32),
    /// Up/Down outside text editors (the command palette).
    ArrowKey(i32),
    ToggleBackgroundTasks,
    /// The transcript's viewport changed (scrolled, or its content resized).
    TranscriptScrolled(iced::widget::scrollable::Viewport),
    /// Back to the newest output.
    JumpToLatest,
    /// Scroll a transcript row into view (opening its step group first).
    RevealItem(String),
    RevealNow(String),
    /// Closes any overlay and shows a page (`usage`, `pull-requests`).
    ShowRoute(&'static str),
    /// A scheduled task that continues the open thread (its \"Schedule…\").
    ScheduleThread,
    ComposerPopover(Option<ComposerPopover>),
    SetPermissionsRequested(String),
    PickAttachment,
    AttachmentLoaded(Result<(String, String, u64, String), String>),
    PasteClipboardImage,
    ClearAttachmentError,
    // Side panel: Diff / Terminal / Files / Git / Artifacts
    ToggleDiffFile(String),
    TerminalFocus(bool),
    FocusTerminalPane(String),
    ResizeTerminalDivider(f32),
    ToggleAllTerminalServers,
    EditTerminalName(String, String),
    TerminalNameChanged(String),
    SaveTerminalName,
    CancelTerminalName,
    TerminalKey { key: String, text: String, ctrl: bool, alt: bool },
    TerminalResize { terminal_id: String, rows: u32, cols: u32 },
    TerminalScroll(i32),
    TerminalPasteClipboard,
    TerminalCopyScreen,
    FileQueryChanged(String),
    StartFileEdit,
    FileEditAction(text_editor::Action),
    SaveFileEdit,
    CancelFileEdit,
    GitCommitMsgChanged(String),
    GitBranchNameChanged(String),
    ToggleWorktreeForm,
    WorktreeBaseChanged(String),
    WorktreeBranchChanged(String),
    SubmitCreateWorktree,
    ArtifactQuickEditChanged(String),
    SubmitArtifactQuickEdit,
    // Pull requests
    PrReviewVerdictChanged(String),
    PrReviewBodyChanged(String),
    PrCommentChanged(String),
    PrMergeMethodChanged(String),
    SubmitPrReview(String),
    SubmitPrComment(String),
    // Settings & Companion
    OpenSettings(SettingsSection),
    ToggleSettingsServerPicker,
    SelectSettingsServer(String),
    ConfigureServer(String),
    ProviderRedirectChanged { instance_id: String, url: String },
    SettingTextChanged { key: String, value: String },
    CompanionLookChanged { env_id: String, look: CompanionLook },
    PokeCompanion,
    ClearCompanionPoke,
    // Overlays & forms
    Overlay(Option<Overlay>),
    UpdateOverlay(Box<Overlay>),
    SubmitOverlay,
    ImportTogglePath(String),
    PickLocalProjectFolder,
    LocalProjectFolderPicked(Option<String>),
    // Images
    /// Opens the viewer on `images[index]`.
    OpenImages { images: Vec<ImageView>, index: usize },
    /// Shows `images[index]` in the open viewer.
    ShowImage(usize),
    ViewerKey(ViewerKey),
    RetryImage(String),
    Image { action: ImageAction, image: ImageView },
    /// An image action finished: a notice to show, or nothing (cancelled).
    ImageActionDone(Result<Option<String>, String>),
    // Utilities
    CopyText(String),
    OpenUrl(String),
    /// An interactive artifact's preview (`webview` feature: in the app).
    OpenPreview { url: String, title: String },
}

impl App {
    pub fn new() -> (Self, Task<Message>) {
        let pending_deep_link = crate::links::from_args();
        let app = Self {
            host: None,
            vm: ViewModel::default(),
            images: HashMap::new(),
            failed_images: HashSet::new(),
            requested_thumbnails: HashSet::new(),
            prefs: Prefs::load(),
            system_dark: true,
            focused: true,
            last_focus: Instant::now(),
            window_size: iced::Size::new(1440.0, 900.0),
            drag: None,
            tick_phase: 0.0,
            clock_ms: now_ms(),
            last_tick_ms: now_ms(),
            online: None,
            route_from: None,
            background_expanded: false,
            companion_reaction: Default::default(),
            transcript: crate::scroll::Transcript::new(),
            companion_poked: false,
            pending_deep_link,
            scheduled_retry_gen: 0,
            connect: ConnectForm::initial(),
            search_input: String::new(),
            earlier_shown: HashSet::new(),
            expanded_groups: HashSet::new(),
            expanded_items: HashSet::new(),
            find_input: String::new(),
            question_drafts: HashMap::new(),
            secret_drafts: HashMap::new(),
            editing_queued: None,
            progress_open: None,
            draft: text_editor::Content::new(),
            last_draft_rev: 0,
            command_highlight: 0,
            last_model_picker_req: 0,
            composer_popover: None,
            attachment_error: None,
            terminal_focused: false,
            terminal_grid: TerminalSplitGrid::new(false),
            terminal_rename: None,
            all_terminal_servers: false,
            terminal_servers: HashMap::new(),
            collapsed_diffs: HashSet::new(),
            git_commit_msg: String::new(),
            git_branch_name: String::new(),
            worktree_draft: WorktreeDraft::default(),
            file_query_input: String::new(),
            file_edit: None,
            file_editor: text_editor::Content::new(),
            artifact_quick_edit: String::new(),
            pr_review: PrReviewDraft { verdict: "comment".into(), merge_method: "squash".into(), ..Default::default() },
            provider_redirect_inputs: HashMap::new(),
            settings_server_picker_open: false,
            pairing_qr: None,
            setting_text_drafts: HashMap::new(),
            overlay: None,
            app_icon_handle: image::Handle::from_bytes(crate::WINDOW_ICON_PNG),
        };
        (app, iced::system::theme().map(Message::SystemTheme))
    }

    pub fn is_dark(&self) -> bool {
        self.prefs.appearance.is_dark(self.system_dark)
    }

    pub fn theme(&self) -> Theme {
        theme::theme(self.is_dark())
    }

    pub fn title(&self) -> String {
        let env = &self.vm.connection.environment_label;
        match (&self.vm.thread, env.is_empty()) {
            (Some(t), false) if !t.title.is_empty() => format!("{} — {} — Domovoi", t.title, env),
            (Some(t), true) if !t.title.is_empty() => format!("{} — Domovoi", t.title),
            (_, false) => format!("{env} — Domovoi"),
            _ => "Domovoi".into(),
        }
    }

    pub fn show_main(&self) -> bool {
        let c = &self.vm.connection;
        match c.state.as_str() {
            "connected" | "reconnecting" => true,
            "connecting" | "disconnected" => !c.active_environment_id.is_empty() && !c.needs_pairing,
            _ => false,
        }
    }

    /// Every required question of `req` has an answer in its draft.
    pub fn questions_answered(&self, req: &domovoi_core::view::RequestView) -> bool {
        let drafts = self.question_drafts.get(&req.id);
        req.questions.iter().all(|q| !q.required || drafts.and_then(|d| d.get(&q.id)).is_some_and(|d| !d.values().is_empty()))
    }

    pub fn send_core(&self, event: Event) {
        if let Some(host) = &self.host {
            host.send(event);
        }
    }

    fn load_settings_section(&self, section: SettingsSection) {
        if let Some(name) = section.core_name() {
            self.send_core(Event::LoadSettings { section: name.into() });
            if section == SettingsSection::Access {
                self.send_core(Event::LoadRemoteAccess);
            } else if section == SettingsSection::Maintenance {
                self.send_core(Event::LoadSettings { section: "diagnostics".into() });
            }
        }
    }

    pub fn subscription(&self) -> Subscription<Message> {
        let mut subs = vec![
            host::subscription().map(Message::Host),
            crate::links::subscription().map(Message::DeepLink),
            iced::time::every(Duration::from_secs(5)).map(|_| Message::Tick),
            iced::system::theme_changes().map(Message::SystemTheme),
            iced::event::listen().filter_map(map_global_event),
        ];
        if self.vm.thread.as_ref().is_some_and(|t| t.working || !t.background_tasks.is_empty()) {
            subs.push(iced::time::every(Duration::from_secs(1)).map(|_| Message::Clock));
        }
        if self.drag.is_some() {
            subs.push(iced::event::listen().filter_map(|e| match e {
                event::Event::Mouse(iced::mouse::Event::CursorMoved { position }) => Some(Message::ResizeMove(position.x)),
                event::Event::Mouse(iced::mouse::Event::ButtonReleased(iced::mouse::Button::Left)) => Some(Message::ResizeEnd),
                _ => None,
            }));
        }
        Subscription::batch(subs)
    }

    fn poll_terminal_servers(&mut self) {
        let Some(host) = &self.host else { return };
        self.terminal_servers.retain(|id, _| self.vm.connection.environments.iter().any(|environment| environment.id == *id));
        if !self.all_terminal_servers {
            return;
        }
        let mut available = 4usize.saturating_sub(self.terminal_servers.values().filter(|server| server.busy).count());
        for environment in &self.vm.connection.environments {
            if available == 0 { break; }
            let server = self.terminal_servers.entry(environment.id.clone()).or_default();
            if !server.busy && self.clock_ms >= server.next_poll_ms && host.probe_terminals(environment.id.clone()) {
                server.busy = true;
                available -= 1;
            }
        }
    }

    pub fn update(&mut self, msg: Message) -> Task<Message> {
        match msg {
            Message::Host(host_msg) => return self.handle_host(host_msg),
            Message::Core(ev) => {
                self.send_core(*ev);
            }
            Message::SystemTheme(mode) => {
                self.system_dark = mode != iced::theme::Mode::Light;
            }
            Message::Clock => {
                self.clock_ms = now_ms();
            }
            Message::ScheduleThread => {
                if let Some(t) = &self.vm.thread {
                    let draft = crate::views::scheduled_editor::ScheduledDraft::for_thread(t);
                    self.send_core(Event::LoadSettings { section: "scheduled".into() });
                    self.overlay = Some(Overlay::ScheduledTaskEditor(Box::new(draft)));
                }
            }
            Message::ShowRoute(route) => {
                self.overlay = None;
                self.send_core(Event::ShowRoute { route: route.into() });
            }
            Message::TranscriptScrolled(viewport) => return self.transcript.scrolled(viewport),
            Message::JumpToLatest => return self.transcript.jump_to_latest(),
            Message::RevealItem(id) => {
                // A row inside a collapsed step group is laid out only once the group opens.
                if let Some(group) = self.vm.thread.as_ref().and_then(|t| t.items.iter().find(|i| i.id == id)).map(|i| i.group.clone()).filter(|g| !g.is_empty())
                    && self.expanded_groups.insert(group)
                {
                    return Task::perform(async { tokio::time::sleep(Duration::from_millis(32)).await }, move |()| Message::RevealNow(id));
                }
                return self.transcript.reveal(&id);
            }
            Message::RevealNow(id) => return self.transcript.reveal(&id),
            Message::ToggleBackgroundTasks => {
                self.background_expanded = !self.background_expanded;
            }
            Message::Tick => {
                let now = now_ms();
                let slept = now - self.last_tick_ms > SLEEP_GAP_MS;
                self.last_tick_ms = now;
                self.clock_ms = now;
                self.tick_phase = (self.tick_phase + 1.0) % 1000.0;
                self.send_core(Event::Tick { now_ms: now });
                self.poll_terminal_servers();
                let probe = Task::perform(probe_route(), Message::NetworkProbed);
                if slept {
                    // The socket died with the sleep; drop it now instead of
                    // waiting out a ping, and reconnect once it is closed.
                    self.send_core(Event::Suspend);
                    return Task::batch([probe, Task::perform(tokio::time::sleep(Duration::from_millis(500)), |()| Message::Woke)]);
                }
                return probe;
            }
            Message::Woke => self.send_core(Event::Resume),
            Message::DeepLink(link) => {
                match &self.host {
                    Some(host) => host.send(Event::OpenLink { link }),
                    None => self.pending_deep_link = Some(link),
                }
                // The link was opened elsewhere: show where it went.
                return iced::window::oldest().then(|id| id.map_or_else(Task::none, |id| Task::batch([iced::window::minimize(id, false), iced::window::gain_focus(id)])));
            }
            Message::AnswerLink { id, accept } => self.send_core(Event::AnswerLinkRequest { id, accept }),
            Message::NetworkProbed(from) => {
                // A server on this machine is there with no network at all
                // (a remote one is reached through a local port too, so not those).
                let c = &self.vm.connection;
                let online = from.is_some() || (!c.remote && is_loopback_url(&c.server_url));
                if self.online != Some(online) {
                    self.online = Some(online);
                    self.send_core(Event::NetworkStatus { online });
                } else if from.is_some() && self.route_from.is_some() && from != self.route_from {
                    // Another network (Wi-Fi to Ethernet, a VPN).
                    self.send_core(Event::NetworkChanged);
                }
                self.route_from = from;
            }
            Message::RetryFired(gen_id) => {
                if self.vm.connection.retry_gen == gen_id {
                    self.send_core(Event::RetryTimer { retry_gen: gen_id });
                }
            }
            Message::WindowFocused(focused) => {
                self.focused = focused;
                self.send_core(Event::AppFocus { focused });
                if let Some(h) = &self.host {
                    h.set_focus_context(focused, self.vm.thread.as_ref().map(|t| t.id.clone()));
                }
                if focused {
                    if self.last_focus.elapsed() >= FOCUS_NETWORK_CHECK {
                        self.send_core(Event::NetworkChanged);
                    }
                    self.last_focus = Instant::now();
                }
            }
            Message::WindowResized(size) => {
                self.window_size = size;
            }
            Message::CloseRequested => {
                if let Some(h) = &self.host {
                    h.shutdown();
                }
                return iced::exit();
            }
            Message::Shortcut(name) => return self.handle_shortcut(name),
            Message::Escape => {
                // The link dialog lies over everything else, so Escape answers it first.
                if let Some(request) = &self.vm.connection.link_request {
                    self.send_core(Event::AnswerLinkRequest { id: request.id, accept: false });
                } else if self.vm.palette.is_some() {
                    if self.vm.palette.as_ref().is_some_and(|p| p.mode != "commands") {
                        self.send_core(Event::SetPaletteMode { mode: "commands".into() });
                    } else {
                        self.send_core(Event::ClosePalette);
                    }
                } else if self.overlay.is_some() {
                    self.overlay = None;
                } else if self.composer_popover.is_some() {
                    self.composer_popover = None;
                } else if self.vm.composer.mention.is_some() {
                    self.send_core(Event::DismissMention);
                } else if self.vm.thread.as_ref().and_then(|t| t.find.as_ref()).is_some() {
                    self.send_core(Event::CloseFind);
                }
            }
            Message::FileDropped(path) => {
                if self.show_main() {
                    return Task::perform(load_image_attachment(path), Message::AttachmentLoaded);
                }
            }
            Message::Appearance(a) => {
                self.prefs.appearance = a;
                self.prefs.save();
            }
            Message::TextSize(sz) => {
                self.prefs.text_size = sz.clamp(tokens::TEXT_MIN, tokens::TEXT_MAX);
                self.prefs.save();
            }
            Message::ToggleSidebar => {
                self.prefs.show_sidebar = !self.prefs.show_sidebar;
                self.prefs.save();
            }
            Message::ResizeStart(side) => {
                self.drag = Some(side);
            }
            Message::ResizeMove(x) => match self.drag {
                Some(ResizeSide::Sidebar) => {
                    self.prefs.sidebar_width = x.clamp(tokens::SIDEBAR_MIN, tokens::SIDEBAR_MAX);
                }
                Some(ResizeSide::Panel) => {
                    let w = (self.window_size.width - x).clamp(tokens::PANEL_MIN, tokens::PANEL_MAX);
                    self.prefs.panel_width = w;
                }
                None => {}
            },
            Message::ResizeEnd => {
                if self.drag.take().is_some() {
                    self.prefs.save();
                }
            }
            // Connect form
            Message::ConnectModeChanged(mode) => self.connect.mode = mode,
            Message::ConnectUrlChanged(s) => self.connect.server_url = s,
            Message::ConnectCodeChanged(s) => self.connect.pairing_code = s,
            Message::ConnectLinkChanged(s) => self.connect.remote_link = s,
            Message::ConnectSubmit => match self.connect.mode {
                ConnectMode::Local => {
                    if !self.connect.server_url.trim().is_empty() {
                        self.send_core(Event::Connect {
                            server_url: self.connect.server_url.trim().to_owned(),
                            pairing_code: self.connect.pairing_code.trim().to_owned(),
                        });
                    }
                }
                ConnectMode::Remote => {
                    if !self.connect.remote_link.trim().is_empty() {
                        self.send_core(Event::ConnectRemote { link: self.connect.remote_link.trim().to_owned() });
                    }
                }
            },
            Message::ConnectLocal => self.send_core(Event::ConnectLocalServer),
            Message::ShowAddServer(show) => {
                self.connect.adding_server = show;
                self.overlay = None;
            }
            // Sidebar
            Message::SearchChanged(q) => {
                self.search_input = q.clone();
                self.send_core(Event::SetSearch { query: q });
            }
            Message::ToggleEarlier(project_id) => {
                if !self.earlier_shown.remove(&project_id) {
                    self.earlier_shown.insert(project_id.clone());
                    self.send_core(Event::ShowAllThreads { project_id });
                }
            }
            // Thread & Markdown
            Message::ToggleGroup(id) => {
                if !self.expanded_groups.remove(&id) {
                    self.expanded_groups.insert(id);
                }
            }
            Message::ToggleItem(id) => {
                if !self.expanded_items.remove(&id) {
                    self.expanded_items.insert(id);
                }
            }
            Message::Markdown(act) => match act {
                MdAction::OpenUrl(url) => {
                    let _ = open::that(&url);
                }
                MdAction::OpenWorkspaceLink(path) => {
                    self.send_core(Event::ShowPanel { tab: "files".into() });
                    self.send_core(Event::OpenFile { path });
                }
                MdAction::CopyCode(code) => copy_to_clipboard(&code),
                MdAction::OpenArtifact { artifact_id, code } => {
                    if let Some(tid) = self.vm.thread.as_ref().map(|t| t.id.clone()) {
                        self.send_core(Event::OpenArtifact { thread_id: tid, artifact_id, version: None, dark: self.is_dark() });
                        if code {
                            self.send_core(Event::SetArtifactViewMode { mode: "code".into() });
                        }
                    }
                }
                MdAction::LoadDiagram { key, source } => {
                    self.send_core(Event::LoadDiagram { key, source, dark: self.is_dark() });
                }
            },
            Message::FindQueryChanged(q) => {
                self.find_input = q.clone();
                self.send_core(Event::FindInThread { query: q });
            }
            Message::QuestionToggleOption { request_id, question_id, value, multi } => {
                let entry = self.question_drafts.entry(request_id).or_default().entry(question_id).or_default();
                if multi {
                    if let Some(pos) = entry.selected.iter().position(|v| v == &value) {
                        entry.selected.remove(pos);
                    } else {
                        entry.selected.push(value);
                    }
                } else {
                    entry.selected = vec![value];
                }
            }
            Message::QuestionCustomChanged { request_id, question_id, text } => {
                let entry = self.question_drafts.entry(request_id).or_default().entry(question_id).or_default();
                entry.custom = text;
            }
            Message::QuestionSubmit { request_id } => {
                // Enter in an answer field goes through here too.
                let request = self.vm.thread.as_ref().and_then(|t| t.pending_requests.iter().find(|r| r.id == request_id));
                if request.is_some_and(|r| !self.questions_answered(r)) {
                    return Task::none();
                }
                let drafts = self.question_drafts.remove(&request_id).unwrap_or_default();
                let answers = drafts
                    .into_iter()
                    .map(|(question_id, d)| QuestionAnswer { values: d.values(), question_id })
                    .filter(|a| !a.values.is_empty())
                    .collect();
                self.send_core(Event::RespondUserInput { request_id, answers });
            }
            Message::SecretChanged { item_id, value } => {
                self.secret_drafts.insert(item_id, value);
            }
            Message::SecretSubmit { item_id } => {
                let waiting = self.vm.thread.as_ref().and_then(|t| t.secret_requests.iter().find(|r| r.item_id == item_id)).is_some_and(|r| !r.sending);
                if !waiting {
                    return Task::none();
                }
                if let Some(SecretText(value)) = self.secret_drafts.remove(&item_id).filter(|v| !v.0.trim().is_empty()) {
                    self.send_core(Event::AnswerSecret { item_id, value });
                }
            }
            Message::ToggleProgress => {
                let id = self.vm.thread.as_ref().map(|t| t.id.clone());
                self.progress_open = if self.progress_open == id { None } else { id };
            }
            Message::ImplementPlan(plan_id) => {
                self.progress_open = None;
                self.send_core(Event::ImplementPlan { plan_id });
            }
            Message::ViewDiff(source) => {
                self.send_core(Event::SelectDiffSource { source });
                self.send_core(Event::ShowPanel { tab: "diff".into() });
            }
            Message::PreviewArtifactFile(path) => {
                if let Some(thread_id) = self.vm.thread.as_ref().map(|t| t.id.clone()) {
                    self.send_core(Event::ShowPanel { tab: "artifacts".into() });
                    self.send_core(Event::OpenArtifactFile { thread_id, path });
                }
            }
            Message::ShowInThread(item_id) => {
                // Some ids name a part of a row (`row:block`).
                let row = item_id.split(':').next().unwrap_or_default().to_owned();
                if !row.is_empty() {
                    return self.update(Message::RevealItem(row));
                }
            }
            Message::StartEditQueued { run_id, text } => {
                self.editing_queued = Some((run_id, text));
            }
            Message::EditQueuedChanged(text) => {
                if let Some((_, ref mut cur)) = self.editing_queued {
                    *cur = text;
                }
            }
            Message::CommitEditQueued => {
                if let Some((run_id, text)) = self.editing_queued.take() {
                    self.send_core(Event::EditQueued { run_id, text });
                }
            }
            Message::CancelEditQueued => {
                self.editing_queued = None;
            }
            // Composer
            Message::DraftAction(action) => {
                self.draft.perform(action);
                let text = self.draft.text();
                if text != self.vm.composer.draft {
                    self.command_highlight = 0;
                    self.send_core(Event::SetDraft { text });
                }
            }
            Message::ComposerEnter => {
                let comp = &self.vm.composer;
                if let Some(m) = comp.mention.as_ref().filter(|m| !m.suggestions.is_empty()) {
                    let hit = m.suggestions.get(m.highlighted as usize).unwrap_or(&m.suggestions[0]);
                    self.send_core(Event::PickMention { path: hit.path.clone() });
                } else if let Some(item) = comp.skills.get(self.command_highlight).or_else(|| comp.skills.first()) {
                    let name = item.name.clone();
                    self.command_highlight = 0;
                    self.send_core(Event::PickSkill { name });
                } else if let Some(item) = comp.slash_commands.get(self.command_highlight).or_else(|| comp.slash_commands.first()) {
                    let name = item.name.clone();
                    self.command_highlight = 0;
                    self.send_core(Event::PickSlashCommand { name });
                } else if comp.can_send {
                    self.send_core(Event::Send);
                    return self.transcript.jump_to_latest();
                }
            }
            Message::MoveCommand(delta) => {
                let comp = &self.vm.composer;
                let len = if comp.skills.is_empty() { comp.slash_commands.len() } else { comp.skills.len() }.min(crate::views::composer::LIST_LIMIT);
                if len > 0 {
                    self.command_highlight = (self.command_highlight as i64 + i64::from(delta)).rem_euclid(len as i64) as usize;
                }
            }
            Message::RecallPrompt(delta) => {
                self.send_core(Event::RecallPrompt { delta });
            }
            Message::ArrowKey(delta) => {
                if self.vm.palette.is_some() {
                    self.send_core(Event::MovePalette { delta });
                }
            }
            Message::ComposerPopover(pop) => {
                self.composer_popover = if self.composer_popover == pop { None } else { pop };
            }
            Message::SetPermissionsRequested(mode) => {
                self.composer_popover = None;
                if mode == "full-access" {
                    self.overlay = Some(Overlay::ConfirmFullAccess);
                } else {
                    self.send_core(Event::SetRuntimeMode { mode });
                }
            }
            Message::PickAttachment => {
                return Task::perform(
                    async {
                        let picked = rfd::AsyncFileDialog::new()
                            .add_filter("Images", &["png", "jpg", "jpeg", "gif", "webp"])
                            .pick_file()
                            .await;
                        match picked {
                            Some(handle) => load_image_attachment(handle.path().to_path_buf()).await,
                            None => Err(String::new()),
                        }
                    },
                    Message::AttachmentLoaded,
                );
            }
            Message::AttachmentLoaded(Ok((name, mime_type, size_bytes, data_url))) => {
                self.attachment_error = None;
                self.send_core(Event::AddAttachment { name, mime_type, size_bytes, data_url });
            }
            Message::AttachmentLoaded(Err(err)) => {
                if !err.is_empty() {
                    self.attachment_error = Some(err);
                }
            }
            Message::PasteClipboardImage => {
                // Text pastes natively into the composer's text_editor; this only
                // picks up bitmap clipboard contents (screenshots, copied images).
                if self.show_main() {
                    return Task::perform(
                        async { tokio::task::spawn_blocking(clipboard_image_attachment).await.unwrap_or_else(|_| Err(String::new())) },
                        Message::AttachmentLoaded,
                    );
                }
            }
            Message::ClearAttachmentError => {
                self.attachment_error = None;
            }
            // Side panel
            Message::ToggleDiffFile(path) => {
                if !self.collapsed_diffs.remove(&path) {
                    self.collapsed_diffs.insert(path);
                }
            }
            Message::TerminalFocus(f) => {
                self.terminal_focused = f;
            }
            Message::FocusTerminalPane(terminal_id) => {
                self.terminal_focused = true;
                self.send_core(Event::SelectTerminal { terminal_id });
            }
            Message::EditTerminalName(id, label) => self.terminal_rename = Some((id, label)),
            Message::TerminalNameChanged(label) => {
                if let Some((_, draft)) = &mut self.terminal_rename { *draft = label; }
            }
            Message::SaveTerminalName => {
                if let Some((id, label)) = self.terminal_rename.take()
                    && self.vm.panel.terminal.active.as_ref().is_some_and(|screen| screen.id == id)
                {
                    self.send_core(Event::RenameTerminal { label });
                }
            }
            Message::CancelTerminalName => self.terminal_rename = None,
            Message::ToggleAllTerminalServers => {
                self.all_terminal_servers = !self.all_terminal_servers;
                self.poll_terminal_servers();
            }
            Message::ResizeTerminalDivider(ratio) => {
                let ratio = ratio.clamp(0.15, 0.85);
                self.terminal_grid.panes.resize(self.terminal_grid.split, ratio);
                self.send_core(Event::ResizeTerminalSplit { percent: (ratio * 100.0).round() as u32 });
            }
            Message::TerminalKey { key, text, ctrl, alt } => {
                self.send_core(Event::TerminalKey { key, text, ctrl, alt });
            }
            Message::TerminalResize { terminal_id, rows, cols } => {
                self.send_core(Event::TerminalPaneResize { terminal_id, rows, cols });
            }
            Message::TerminalScroll(lines) => {
                self.send_core(Event::TerminalScroll { lines });
            }
            Message::TerminalPasteClipboard => {
                if let Ok(mut cb) = arboard::Clipboard::new()
                    && let Ok(text) = cb.get_text() {
                        self.send_core(Event::TerminalPaste { text });
                    }
            }
            Message::TerminalCopyScreen => {
                if let Some(screen) = &self.vm.panel.terminal.active {
                    let plain = crate::views::panel::terminal::plain_text(screen);
                    copy_to_clipboard(&plain);
                }
            }
            Message::FileQueryChanged(q) => {
                self.file_query_input = q.clone();
                self.send_core(Event::SetFileQuery { query: q });
            }
            Message::StartFileEdit => {
                if let Some(preview) = &self.vm.panel.files.preview
                    && preview.editable && preview.error.is_none() {
                        self.file_editor = text_editor::Content::with_text(&preview.text);
                        self.file_edit = Some(FileEditDraft { path: preview.path.clone(), content_rev: 1 });
                    }
            }
            Message::FileEditAction(action) => {
                self.file_editor.perform(action);
                if let Some(draft) = &mut self.file_edit {
                    draft.content_rev += 1;
                }
            }
            Message::SaveFileEdit => {
                if let Some(draft) = self.file_edit.take() {
                    self.send_core(Event::SaveFile { path: draft.path, text: self.file_editor.text() });
                }
            }
            Message::CancelFileEdit => {
                self.file_edit = None;
            }
            Message::GitCommitMsgChanged(s) => self.git_commit_msg = s,
            Message::GitBranchNameChanged(s) => self.git_branch_name = s,
            Message::ToggleWorktreeForm => {
                self.worktree_draft.open = !self.worktree_draft.open;
                if self.worktree_draft.base.is_empty() {
                    self.worktree_draft.base = self.vm.panel.git.branch.clone().unwrap_or_else(|| "main".into());
                }
            }
            Message::WorktreeBaseChanged(s) => self.worktree_draft.base = s,
            Message::WorktreeBranchChanged(s) => self.worktree_draft.branch = s,
            Message::SubmitCreateWorktree => {
                let base = self.worktree_draft.base.trim().to_owned();
                let branch = self.worktree_draft.branch.trim().to_owned();
                if !base.is_empty() {
                    self.send_core(Event::CreateWorktree { base, branch });
                    self.worktree_draft.open = false;
                    self.worktree_draft.branch.clear();
                }
            }
            Message::ArtifactQuickEditChanged(s) => self.artifact_quick_edit = s,
            Message::SubmitArtifactQuickEdit => {
                let prompt = self.artifact_quick_edit.trim().to_owned();
                if !prompt.is_empty()
                    && let Some(active) = &self.vm.panel.artifacts.active {
                        self.send_core(Event::AskAboutArtifact { artifact_id: active.id.clone(), prompt });
                        self.artifact_quick_edit.clear();
                    }
            }
            // Pull requests
            Message::PrReviewVerdictChanged(v) => self.pr_review.verdict = v,
            Message::PrReviewBodyChanged(b) => self.pr_review.body = b,
            Message::PrCommentChanged(c) => self.pr_review.comment = c,
            Message::PrMergeMethodChanged(m) => self.pr_review.merge_method = m,
            Message::SubmitPrReview(key) => {
                let verdict = if self.pr_review.verdict.is_empty() { "comment".into() } else { self.pr_review.verdict.clone() };
                let body = std::mem::take(&mut self.pr_review.body);
                self.send_core(Event::ReviewPullRequest { key, verdict, body });
            }
            Message::SubmitPrComment(key) => {
                let body = std::mem::take(&mut self.pr_review.comment);
                if !body.trim().is_empty() {
                    self.send_core(Event::CommentOnPullRequest { key, body });
                }
            }
            // Settings & Companion
            Message::OpenSettings(section) => {
                self.settings_server_picker_open = false;
                if matches!(self.vm.connection.state.as_str(), "connected" | "reconnecting") {
                    self.load_settings_section(section);
                }
                self.overlay = Some(Overlay::Settings(section));
            }
            Message::ToggleSettingsServerPicker => {
                self.settings_server_picker_open = !self.settings_server_picker_open;
            }
            Message::SelectSettingsServer(id) => {
                self.settings_server_picker_open = false;
                if self.vm.connection.active_environment_id != id {
                    self.setting_text_drafts.clear();
                    self.send_core(Event::SelectEnvironment { id });
                }
            }
            Message::ConfigureServer(id) => {
                self.settings_server_picker_open = false;
                if self.vm.connection.active_environment_id != id {
                    self.setting_text_drafts.clear();
                    self.send_core(Event::SelectEnvironment { id });
                } else if matches!(self.vm.connection.state.as_str(), "connected" | "reconnecting") {
                    self.load_settings_section(SettingsSection::General);
                }
                self.overlay = Some(Overlay::Settings(SettingsSection::General));
            }
            Message::ProviderRedirectChanged { instance_id, url } => {
                self.provider_redirect_inputs.insert(instance_id, url);
            }
            Message::SettingTextChanged { key, value } => {
                self.setting_text_drafts.insert(key, value);
            }
            Message::CompanionLookChanged { env_id, look } => {
                self.send_core(Event::SetCompanion { id: env_id, look });
            }
            Message::PokeCompanion => {
                self.companion_poked = true;
                return Task::perform(
                    async {
                        tokio::time::sleep(Duration::from_millis(320)).await;
                    },
                    |()| Message::ClearCompanionPoke,
                );
            }
            Message::ClearCompanionPoke => {
                self.companion_poked = false;
            }
            // Overlays
            Message::Overlay(ov) => {
                if matches!(ov, Some(Overlay::FolderBrowser)) {
                    self.send_core(Event::BrowseFolders { path: String::new() });
                } else if matches!(ov, Some(Overlay::ImportSessions { .. })) {
                    self.send_core(Event::ScanAgentSessions);
                }
                self.overlay = ov;
            }
            Message::UpdateOverlay(ov) => {
                self.overlay = Some(*ov);
            }
            Message::SubmitOverlay => {
                if let Some(ov) = self.overlay.take() {
                    match ov {
                        Overlay::RenameThread { thread_id, value } => {
                            if !value.trim().is_empty() {
                                self.send_core(Event::RenameThread { thread_id, title: value.trim().to_owned() });
                            }
                        }
                        Overlay::RenameProject { project_id, value } => {
                            if !value.trim().is_empty() {
                                self.send_core(Event::RenameProject { project_id, title: value.trim().to_owned() });
                            }
                        }
                        Overlay::RenameEnvironment { env_id, value } => {
                            if !value.trim().is_empty() {
                                self.send_core(Event::RenameEnvironment { id: env_id, label: value.trim().to_owned() });
                            }
                        }
                        Overlay::ConfirmDeleteThread { thread_id, .. } => {
                            self.send_core(Event::DeleteThread { thread_id });
                        }
                        Overlay::ConfirmRemoveProject { project_id, .. } => {
                            self.send_core(Event::RemoveProject { project_id });
                        }
                        Overlay::ConfirmForgetServer { env_id, .. } => {
                            self.send_core(Event::ForgetEnvironment { id: env_id });
                        }
                        Overlay::ConfirmFullAccess => {
                            self.send_core(Event::SetRuntimeMode { mode: "full-access".into() });
                        }
                        Overlay::NewProject { name, path } => {
                            if !path.trim().is_empty() {
                                self.send_core(Event::AddProject { path: path.trim().to_owned() });
                            } else if !name.trim().is_empty() {
                                self.send_core(Event::CreateProject { name: name.trim().to_owned() });
                            }
                        }
                        Overlay::ImportSessions { selected } => {
                            if !selected.is_empty() {
                                self.send_core(Event::ImportAgentSessions { paths: selected });
                            }
                        }
                        Overlay::ProjectIconPicker { project_id, kind, text, symbol, color, .. } => {
                            self.send_core(Event::SetProjectIcon { project_id, kind, text, symbol, color });
                        }
                        Overlay::ScheduledTaskEditor(draft) => {
                            let (task, mode_change) = draft.finished();
                            if let Some((thread_id, mode)) = mode_change {
                                self.send_core(Event::SetThreadRuntimeMode { thread_id, mode });
                            }
                            self.send_core(Event::SaveScheduled { task: Box::new(task) });
                            self.overlay = Some(Overlay::Settings(SettingsSection::Scheduled));
                        }
                        Overlay::ProviderInstanceEditor(draft) => {
                            self.send_core(Event::SaveProviderInstance { edit: Box::new(draft.edit) });
                            self.overlay = Some(Overlay::Settings(SettingsSection::Providers));
                        }
                        Overlay::SavePlanDialog { item_id, path } => {
                            self.send_core(Event::SavePlan { item_id, path });
                        }
                        other => {
                            self.overlay = Some(other);
                        }
                    }
                }
            }
            Message::ImportTogglePath(path) => {
                if let Some(Overlay::ImportSessions { selected }) = &mut self.overlay {
                    if let Some(pos) = selected.iter().position(|p| p == &path) {
                        selected.remove(pos);
                    } else {
                        selected.push(path);
                    }
                }
            }
            Message::PickLocalProjectFolder => {
                return Task::perform(
                    async {
                        rfd::AsyncFileDialog::new()
                            .pick_folder()
                            .await
                            .map(|h| h.path().display().to_string())
                    },
                    Message::LocalProjectFolderPicked,
                );
            }
            Message::LocalProjectFolderPicked(Some(path)) => {
                if let Some(Overlay::NewProject { path: ref mut p, .. }) = self.overlay {
                    *p = path;
                }
            }
            Message::LocalProjectFolderPicked(None) => {}
            Message::OpenImages { images, index } => {
                if !images.is_empty() {
                    let index = index.min(images.len() - 1);
                    self.overlay = Some(Overlay::ImageViewer { images, index });
                    self.terminal_focused = false;
                    self.prefetch_media();
                    // The composer would keep ←/→ and ⌘C for itself.
                    return iced::advanced::widget::operate(iced::advanced::widget::operation::focusable::unfocus::<()>()).discard();
                }
            }
            Message::ShowImage(to) => {
                if let Some(Overlay::ImageViewer { images, index }) = &mut self.overlay {
                    *index = to.min(images.len().saturating_sub(1));
                }
            }
            Message::ViewerKey(key) => {
                let Some(Overlay::ImageViewer { images, index }) = &mut self.overlay else { return Task::none() };
                let last = images.len().saturating_sub(1);
                match key {
                    ViewerKey::Previous => *index = index.saturating_sub(1),
                    ViewerKey::Next => *index = (*index + 1).min(last),
                    ViewerKey::First => *index = 0,
                    ViewerKey::Last => *index = last,
                    ViewerKey::Copy | ViewerKey::Save => {
                        if let Some(image) = images.get(*index).cloned() {
                            let action = if key == ViewerKey::Copy { ImageAction::Copy } else { ImageAction::Save };
                            return self.update(Message::Image { action, image });
                        }
                    }
                }
            }
            Message::RetryImage(url) => {
                self.failed_images.remove(&url);
                if let Some(host) = &self.host {
                    host.fetch_image(url);
                }
            }
            Message::Image { action, image } => {
                if matches!(self.overlay, Some(Overlay::ImageMenu { .. })) {
                    self.overlay = None;
                }
                let Some(bytes) = self.images.get(&image.url).and_then(crate::image_actions::image_bytes).map(<[u8]>::to_vec) else {
                    self.send_core(Event::ShowNotice { kind: "error".into(), message: "The image hasn’t loaded yet.".into() });
                    return Task::none();
                };
                let name = image.name;
                return match action {
                    ImageAction::Copy => Task::perform(
                        async move { crate::image_actions::copy_image(bytes).await.map(|()| Some("Image copied".to_owned())) },
                        Message::ImageActionDone,
                    ),
                    ImageAction::Save => Task::perform(
                        async move { crate::image_actions::save_as(bytes, name).await.map(|saved| saved.map(|p| format!("Saved to {}", p.display()))) },
                        Message::ImageActionDone,
                    ),
                    ImageAction::OpenExternally => Task::perform(
                        async move { crate::image_actions::open_externally(bytes, name).await.map(|()| None) },
                        Message::ImageActionDone,
                    ),
                };
            }
            Message::ImageActionDone(result) => match result {
                Ok(Some(message)) => self.send_core(Event::ShowNotice { kind: "success".into(), message }),
                Ok(None) => {}
                Err(message) => self.send_core(Event::ShowNotice { kind: "error".into(), message }),
            },
            Message::CopyText(s) => copy_to_clipboard(&s),
            Message::OpenUrl(url) => {
                let _ = open::that(&url);
            }
            Message::OpenPreview { url, title } => {
                // In a window of the app's own when built with it, else in the browser.
                #[cfg(feature = "webview")]
                if crate::webview::open(&url, &title).is_ok() {
                    return Task::none();
                }
                let _ = title;
                let _ = open::that(&url);
            }
        }
        Task::none()
    }

    fn handle_host(&mut self, msg: HostMsg) -> Task<Message> {
        match msg {
            HostMsg::Ready(handle) => {
                self.host = Some(handle.clone());
                handle.send(Event::Tick { now_ms: now_ms() });
                let (iana, utc_offset_secs) = host::local_timezone();
                handle.send(Event::SetTimeZone { iana, utc_offset_secs });
                handle.send(Event::Launch {
                    surface: "desktop".into(),
                    device_label: host::device_label(),
                    device_os: host::device_os().into(),
                });
                if let Some(link) = self.pending_deep_link.take() {
                    handle.send(Event::OpenLink { link });
                }
                return Task::perform(probe_route(), Message::NetworkProbed);
            }
            HostMsg::Delta(delta) => {
                let prev_state = self.vm.connection.state.clone();
                let prev_env = self.vm.connection.active_environment_id.clone();
                delta.apply(&mut self.vm);
                if let Some(split) = &self.vm.panel.terminal.split {
                    if self.terminal_grid.stacked != split.stacked {
                        self.terminal_grid = TerminalSplitGrid::new(split.stacked);
                    }
                    self.terminal_grid.panes.resize(self.terminal_grid.split, split.percent as f32 / 100.0);
                }
                // A card answered or gone (another thread, another device) keeps nothing typed.
                let asked = self.vm.thread.as_ref().map(|t| &t.secret_requests);
                self.secret_drafts.retain(|id, _| asked.is_some_and(|asked| asked.iter().any(|r| r.item_id == *id)));

                let env_changed = self.vm.connection.active_environment_id != prev_env;
                let became_connected = self.vm.connection.state == "connected" && prev_state != "connected";
                if became_connected {
                    self.connect.adding_server = false;
                    self.connect.pairing_code.clear();
                    self.connect.remote_link.clear();
                }
                if env_changed {
                    self.setting_text_drafts.clear();
                }
                if (became_connected || (env_changed && self.vm.connection.state == "connected"))
                    && let Some(Overlay::Settings(sec)) = self.overlay
                {
                    self.load_settings_section(sec);
                }

                // Sync composer text editor when core explicitly replaces the draft.
                if self.vm.composer.draft_revision != self.last_draft_rev {
                    self.last_draft_rev = self.vm.composer.draft_revision;
                    if self.draft.text() != self.vm.composer.draft {
                        self.draft = text_editor::Content::with_text(&self.vm.composer.draft);
                    }
                }
                if self.vm.composer.model_picker_requested > self.last_model_picker_req {
                    self.last_model_picker_req = self.vm.composer.model_picker_requested;
                    self.composer_popover = Some(ComposerPopover::Model);
                }

                // Update QR code for Remote Access pairing link when present.
                let remote_link = &self.vm.settings.remote.link;
                if !remote_link.is_empty() && self.pairing_qr.as_ref().map(|(l, _)| l.as_str()) != Some(remote_link.as_str())
                    && let Ok(data) = iced::widget::qr_code::Data::new(remote_link) {
                        self.pairing_qr = Some((remote_link.clone(), data));
                    }

                // Preselect recent candidates when agent sessions scan completes.
                if let Some(Overlay::ImportSessions { selected }) = &mut self.overlay
                    && selected.is_empty() && self.vm.agent_sessions.scanned {
                        *selected = self
                            .vm
                            .agent_sessions
                            .candidates
                            .iter()
                            .filter(|c| !c.already_imported && c.recent)
                            .map(|c| c.path.clone())
                            .collect();
                    }

                // Request any newly visible server image URLs & project icon loads.
                self.prefetch_media();
                let mut scroll = self.transcript.after_render(self.vm.thread.as_ref());
                let now = now_ms();
                if let Some(until) = self.companion_reaction.observe(&self.vm.connection.active_environment_id, &self.vm.connection.activity, now) {
                    // Redraw once the reaction is over.
                    let wait = Duration::from_millis(u64::try_from(until - now).unwrap_or(0) + 16);
                    scroll = Task::batch([scroll, Task::perform(async move { tokio::time::sleep(wait).await }, |()| Message::Clock)]);
                }

                if let Some(h) = &self.host {
                    h.set_focus_context(self.focused, self.vm.thread.as_ref().map(|t| t.id.clone()));
                }

                // Schedule reconnect backoff timer when requested by core.
                let c = &self.vm.connection;
                if c.state == "reconnecting" && c.reconnect_in_seconds > 0 && !c.offline && self.scheduled_retry_gen != c.retry_gen {
                    let delay = Duration::from_secs(u64::from(c.reconnect_in_seconds));
                    let gen_id = c.retry_gen;
                    self.scheduled_retry_gen = gen_id;
                    return Task::batch([scroll, Task::perform(async move { tokio::time::sleep(delay).await }, move |()| Message::RetryFired(gen_id))]);
                }
                return scroll;
            }
            HostMsg::ImageLoaded { url, handle } => {
                self.failed_images.remove(&url);
                self.images.insert(url, handle);
                if self.images.len() > IMAGE_CACHE_LIMIT {
                    // Let go of what nothing shows now; what is shown stays,
                    // however many that is. A dropped one is fetched again
                    // when it shows (the host keeps only fetches in flight).
                    let wanted: HashSet<String> = self.wanted_media().into_iter().collect();
                    self.images.retain(|u, _| wanted.contains(u));
                    self.failed_images.retain(|u| wanted.contains(u));
                }
            }
            HostMsg::TerminalDetails { environment_id, outcome } => {
                if let Some(server) = self.terminal_servers.get_mut(&environment_id) {
                    server.busy = false;
                    server.loaded = true;
                    match outcome {
                        host::TerminalProbe::Current(terminals) => {
                            server.terminals = terminals;
                            server.stale = false;
                            server.failures = 0;
                            server.next_poll_ms = self.clock_ms + 15_000;
                        }
                        host::TerminalProbe::Unauthorized => {
                            server.terminals.clear();
                            server.stale = false;
                            server.failures = 0;
                            server.next_poll_ms = self.clock_ms + 60_000;
                        }
                        host::TerminalProbe::Unavailable => {
                            server.stale = true;
                            server.failures = server.failures.saturating_add(1);
                            server.next_poll_ms = self.clock_ms + (5_000_i64 << server.failures.min(4));
                        }
                    }
                }
                self.poll_terminal_servers();
            }
            HostMsg::ImageFailed { url } => {
                self.failed_images.insert(url);
            }
            HostMsg::OpenThreadFromNotification { environment_id, thread_id } => {
                self.send_core(Event::OpenNotification { environment_id, thread_id });
            }
            HostMsg::RespondFromNotification { environment_id, thread_id, request_id, decision } => {
                self.send_core(Event::RespondFromNotification { environment_id, thread_id, request_id, decision });
            }
        }
        Task::none()
    }

    fn prefetch_media(&mut self) {
        self.request_icon_thumbnails();
        let Some(host) = &self.host else { return };
        for proj in &self.vm.sidebar.projects {
            if proj.icon.load_key.is_some() {
                host.send(Event::LoadProjectIcon { project_id: proj.id.clone() });
            }
        }
        for url in self.wanted_media() {
            if !self.images.contains_key(&url) && !self.failed_images.contains(&url) {
                host.fetch_image(url);
            }
        }
    }

    /// Every server image URL something on screen (or the open viewer) shows.
    pub fn wanted_media(&self) -> Vec<String> {
        let mut urls: Vec<&str> = vec![];
        for proj in &self.vm.sidebar.projects {
            urls.extend(proj.icon.image_url.as_deref());
        }
        for sec in &self.vm.sidebar.sections {
            for row in &sec.rows {
                urls.extend(row.project_icon.image_url.as_deref());
            }
        }
        if let Some(thread) = &self.vm.thread {
            for item in &thread.items {
                urls.extend(item.images.iter().map(|i| i.url.as_str()));
                urls.extend(item.group_images.iter().map(|i| i.url.as_str()));
            }
        }
        if let Some(Overlay::ImageViewer { images, .. } | Overlay::ImageMenu { images, .. }) = &self.overlay {
            urls.extend(images.iter().map(|i| i.url.as_str()));
        }
        for diag in &self.vm.diagrams {
            urls.extend(diag.url.as_deref());
        }
        if let Some(prev) = &self.vm.panel.files.preview {
            urls.extend(prev.image_url.as_deref());
        }
        if let Some(art) = &self.vm.panel.artifacts.active
            && art.kind == "mermaid" {
                urls.extend(art.preview_url.as_deref());
            }
        for f in &self.vm.settings.project_icon_files.files {
            urls.extend(f.url.as_deref());
        }
        let mut seen = HashSet::new();
        urls.into_iter().filter(|u| !u.is_empty() && seen.insert(*u)).map(str::to_owned).collect()
    }

    /// The project icon picker's files whose thumbnails the core wants loaded.
    fn request_icon_thumbnails(&mut self) {
        let Some(host) = &self.host else { return };
        let files = &self.vm.settings.project_icon_files;
        if files.project_id.is_empty() {
            self.requested_thumbnails.clear();
            return;
        }
        for f in files.files.iter().filter(|f| f.refresh) {
            if self.requested_thumbnails.insert(format!("{}:{}", files.revision, f.path)) {
                host.send(Event::LoadProjectIconThumbnail { project_id: files.project_id.clone(), path: f.path.clone() });
            }
        }
    }

    fn handle_shortcut(&mut self, name: &'static str) -> Task<Message> {
        match name {
            "palette" => {
                if self.vm.palette.is_some() {
                    self.send_core(Event::ClosePalette);
                } else {
                    self.send_core(Event::OpenPalette);
                }
            }
            "palette-files" => {
                if self.vm.palette.is_none() {
                    self.send_core(Event::OpenPalette);
                }
                self.send_core(Event::SetPaletteMode { mode: "files".into() });
            }
            "palette-content" => {
                if self.vm.palette.is_none() {
                    self.send_core(Event::OpenPalette);
                }
                self.send_core(Event::SetPaletteMode { mode: "content".into() });
            }
            "toggle-panel" => self.send_core(Event::TogglePanel),
            "toggle-sidebar" => {
                self.prefs.show_sidebar = !self.prefs.show_sidebar;
                self.prefs.save();
            }
            "new-thread" => self.send_core(Event::NewThread { project_id: None }),
            "new-scratch" => self.send_core(Event::OpenScratch),
            "find" => {
                if self.vm.thread.is_some() {
                    self.send_core(Event::FindInThread { query: self.find_input.clone() });
                }
            }
            "find-next" => self.send_core(Event::FindNext { forward: true }),
            "find-prev" => self.send_core(Event::FindNext { forward: false }),
            "stop" => {
                if self.vm.composer.can_stop {
                    self.send_core(Event::Interrupt);
                }
            }
            "toggle-plan" => {
                let next = if self.vm.composer.interaction_mode == "plan" { "default" } else { "plan" };
                self.send_core(Event::SetInteractionMode { mode: next.into() });
            }
            "model-picker" => {
                self.composer_popover = Some(ComposerPopover::Model);
            }
            "panel-diff" => self.send_core(Event::ShowPanel { tab: "diff".into() }),
            "panel-artifacts" => self.send_core(Event::ShowPanel { tab: "artifacts".into() }),
            "panel-terminal" => {
                self.terminal_focused = true;
                self.send_core(Event::ShowPanel { tab: "terminal".into() });
            }
            "panel-files" => self.send_core(Event::ShowPanel { tab: "files".into() }),
            "panel-git" => self.send_core(Event::ShowPanel { tab: "git".into() }),
            "settings" => return self.update(Message::OpenSettings(SettingsSection::General)),
            "effort-up" => self.step_effort(1),
            "effort-down" => self.step_effort(-1),
            _ => {}
        }
        Task::none()
    }

    fn step_effort(&self, delta: i32) {
        let efforts = &self.vm.composer.efforts;
        if efforts.is_empty() {
            return;
        }
        let cur = self
            .vm
            .composer
            .selected_effort
            .as_deref()
            .and_then(|v| efforts.iter().position(|e| e.value == v))
            .unwrap_or(0) as i32;
        let next = (cur + delta).clamp(0, (efforts.len() as i32) - 1) as usize;
        if let Some(choice) = efforts.get(next) {
            self.send_core(Event::SelectEffort { value: choice.value.clone() });
        }
    }
}

fn map_global_event(ev: event::Event) -> Option<Message> {
    match ev {
        event::Event::Window(iced::window::Event::Focused) => Some(Message::WindowFocused(true)),
        event::Event::Window(iced::window::Event::Unfocused) => Some(Message::WindowFocused(false)),
        event::Event::Window(iced::window::Event::Resized(size)) => Some(Message::WindowResized(size)),
        event::Event::Window(iced::window::Event::CloseRequested) => Some(Message::CloseRequested),
        event::Event::Window(iced::window::Event::FileDropped(path)) => Some(Message::FileDropped(path)),
        event::Event::Keyboard(keyboard::Event::KeyPressed { key, modifiers, .. }) => {
            if matches!(key, Key::Named(key::Named::Escape)) {
                return Some(Message::Escape);
            }
            if modifiers.is_empty() {
                match key {
                    Key::Named(key::Named::ArrowUp) => return Some(Message::ArrowKey(-1)),
                    Key::Named(key::Named::ArrowDown) => return Some(Message::ArrowKey(1)),
                    // The image viewer's; ignored while it is closed.
                    Key::Named(key::Named::ArrowLeft) => return Some(Message::ViewerKey(ViewerKey::Previous)),
                    Key::Named(key::Named::ArrowRight) => return Some(Message::ViewerKey(ViewerKey::Next)),
                    Key::Named(key::Named::Home) => return Some(Message::ViewerKey(ViewerKey::First)),
                    Key::Named(key::Named::End) => return Some(Message::ViewerKey(ViewerKey::Last)),
                    _ => {}
                }
            }
            let cmd = modifiers.command();
            let shift = modifiers.shift();
            let alt = modifiers.alt();
            if cmd
                && let Key::Character(c) = &key {
                    return match (c.to_ascii_lowercase().as_str(), shift, alt) {
                        ("v", false, false) => Some(Message::PasteClipboardImage),
                        ("c", false, false) => Some(Message::ViewerKey(ViewerKey::Copy)),
                        ("s", false, false) => Some(Message::ViewerKey(ViewerKey::Save)),
                        ("k", false, false) => Some(Message::Shortcut("palette")),
                        ("p", false, false) => Some(Message::Shortcut("palette-files")),
                        ("p", true, false) => Some(Message::Shortcut("toggle-plan")),
                        ("f", false, false) => Some(Message::Shortcut("find")),
                        ("f", true, false) => Some(Message::Shortcut("palette-content")),
                        ("g", false, false) => Some(Message::Shortcut("find-next")),
                        ("g", true, false) => Some(Message::Shortcut("find-prev")),
                        ("g", false, true) => Some(Message::Shortcut("panel-git")),
                        ("j", false, false) => Some(Message::Shortcut("toggle-panel")),
                        ("b", false, false) => Some(Message::Shortcut("toggle-sidebar")),
                        ("n", false, false) => Some(Message::Shortcut("new-thread")),
                        ("n", true, false) => Some(Message::Shortcut("new-scratch")),
                        (".", false, false) => Some(Message::Shortcut("stop")),
                        ("m", true, false) => Some(Message::Shortcut("model-picker")),
                        ("d", true, false) => Some(Message::Shortcut("panel-diff")),
                        ("a", true, false) => Some(Message::Shortcut("panel-artifacts")),
                        ("t", true, false) => Some(Message::Shortcut("panel-terminal")),
                        ("e", true, false) => Some(Message::Shortcut("panel-files")),
                        (",", false, false) => Some(Message::Shortcut("settings")),
                        ("]", false, false) => Some(Message::Shortcut("effort-up")),
                        ("[", false, false) => Some(Message::Shortcut("effort-down")),
                        _ => None,
                    };
                }
            None
        }
        _ => None,
    }
}

/// The local address a packet to the internet would leave from, without
/// sending one: a UDP `connect` only looks up the route. `None` when there
/// is no route (no network).
async fn probe_route() -> Option<std::net::IpAddr> {
    async fn from(bind: &str, to: &str) -> Option<std::net::IpAddr> {
        let socket = tokio::net::UdpSocket::bind(bind).await.ok()?;
        socket.connect(to).await.ok()?;
        socket.local_addr().ok().map(|a| a.ip())
    }
    match from("0.0.0.0:0", "1.1.1.1:53").await {
        Some(ip) => Some(ip),
        None => from("[::]:0", "[2606:4700:4700::1111]:53").await,
    }
}

fn is_loopback_url(url: &str) -> bool {
    let host = url.split_once("://").map_or(url, |(_, rest)| rest).split(['/', '?']).next().unwrap_or_default();
    let host = host.strip_prefix('[').and_then(|h| h.split_once(']')).map_or_else(|| host.rsplit_once(':').map_or(host, |(h, _)| h), |(h, _)| h);
    host.eq_ignore_ascii_case("localhost") || host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

#[cfg(test)]
mod tests {
    #[test]
    fn loopback_servers() {
        for url in ["http://127.0.0.1:13773", "http://localhost:13773/", "http://[::1]:13773", "ws://127.0.0.1"] {
            assert!(super::is_loopback_url(url), "{url}");
        }
        for url in ["http://192.168.1.4:13773", "https://example.com", "http://[2001:db8::1]:80", ""] {
            assert!(!super::is_loopback_url(url), "{url}");
        }
    }
}

pub fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

fn copy_to_clipboard(text: &str) {
    crate::image_actions::copy_text(text.to_owned());
}

/// Reads a bitmap from the clipboard and encodes it as a PNG attachment.
/// `Err("")` means "no image on the clipboard" and is ignored by the UI.
fn clipboard_image_attachment() -> Result<(String, String, u64, String), String> {
    let mut cb = arboard::Clipboard::new().map_err(|_| String::new())?;
    let img = cb.get_image().map_err(|_| String::new())?;
    let (w, h) = (u32::try_from(img.width).map_err(|_| String::new())?, u32::try_from(img.height).map_err(|_| String::new())?);
    let mut bytes = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut bytes, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().map_err(|e| format!("Could not encode pasted image: {e}"))?;
        writer.write_image_data(&img.bytes).map_err(|e| format!("Could not encode pasted image: {e}"))?;
    }
    if bytes.len() > 20 * 1024 * 1024 {
        return Err("Pasted image is larger than 20 MB.".into());
    }
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Ok(("pasted-image.png".to_owned(), "image/png".to_owned(), bytes.len() as u64, format!("data:image/png;base64,{b64}")))
}

async fn load_image_attachment(path: PathBuf) -> Result<(String, String, u64, String), String> {
    let bytes = tokio::fs::read(&path).await.map_err(|e| format!("Could not read image: {e}"))?;
    if bytes.len() > 20 * 1024 * 1024 {
        return Err("Image is larger than 20 MB.".into());
    }
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("").to_ascii_lowercase();
    let mime = match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => return Err("Only PNG, JPEG, GIF, and WebP images can be attached.".into()),
    };
    let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("image.png").to_owned();
    let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
    let data_url = format!("data:{mime};base64,{b64}");
    Ok((name, mime.to_owned(), bytes.len() as u64, data_url))
}
