//! The core's state. What the server sends is held as the records of
//! `domovoi_model::typed`, read once where a reply or a stream item arrives
//! (`replies.rs`): the open thread as a `Thread`, the lists, the settings
//! and the pages as the records their RPCs answer with. Everything the UI
//! needs is derived from them in `present.rs`.

use crate::rpc::RpcClient;
use crate::terminal::Session;
use crate::view::ModelOption;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use domovoi_model::typed::{
    AgentSessionProjectCandidate, AuthAccessSnapshot, ConversationMessage, DatabaseStorageStatus, ExecutionEnvironmentDescriptor,
    FilesystemBrowseEntry, FindInThreadResult, HostResourcesSnapshot, ProjectEntry, ProjectShell, PullRequestChecks, PullRequestListEntry,
    PullRequestSummary, RemoteAccessStatus, RemotePairingLink, ResolvedKeybindingRule, ScheduledTask, ServerProcessDiagnosticsResult,
    ServerProvider, ServerSettings, ShellStatus, StorageCleanupReport, TerminalSummary, Thread, ThreadSearchMatch, ThreadShell, TurnItem,
    TurnItemId, UsageSummary, VcsStatusLocal, VcsStatusRemote,
};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum Connection {
    #[default]
    Disconnected,
    Pairing,
    Connecting,
    Connected,
    Reconnecting,
    Failed,
}

/// A server this device knows: local (an HTTP URL) or remote (an iroh
/// ticket). Saved by the shell; its bearer token is stored separately.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Environment {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub ticket: String,
    /// The server's own environment id (from its welcome), which push
    /// notifications name.
    #[serde(default)]
    pub server_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub companion: Option<crate::companion::CompanionLook>,
}

/// A `domovoi://connect` link that reached the app from outside, held until
/// the user says whether to follow it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkRequest {
    pub id: u32,
    pub link: crate::environments::Link,
}

/// Retries back off 2, 4, 8, 16 then 30 s, each spread by up to ±25 % so
/// clients of a restarted server do not all come back at once.
pub fn retry_delay(attempts: u32, jitter: f64) -> u32 {
    let base = (1u32 << attempts.clamp(1, 5)).min(30) as f64;
    (base * (0.75 + 0.5 * jitter.clamp(0.0, 1.0))).round().max(1.0) as u32
}

impl Environment {
    pub fn is_remote(&self) -> bool {
        !self.ticket.is_empty()
    }
}

/// The shell's iroh bridge for the active remote environment.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TunnelState {
    pub port: Option<u16>,
    pub connected: bool,
    /// `direct` | `relay` | empty
    pub path: String,
    pub rtt_ms: u32,
    pub error: String,
}

#[derive(Default)]
pub struct Model {
    pub now_ms: i64,
    /// The time the view's relative ages ("2 min ago") are told from: `now_ms`
    /// when the view was last built whole. A quiet tick moves `now_ms` only,
    /// so it changes nothing shown.
    pub shown_ms: i64,
    /// The device's IANA time zone (empty means UTC) and current UTC offset.
    pub time_zone: String,
    pub utc_offset_secs: i32,
    pub focused: bool,
    /// `desktop` | `mobile`
    pub surface: String,
    pub device_label: String,
    pub device_os: String,

    pub environments: Vec<Environment>,
    /// Environments in `environments` that are not saved yet: made for a
    /// sign-in that has not succeeded. One becomes saved when it signs in
    /// (`environments::signed_in`) and is dropped when another is picked, so
    /// a failed first pairing leaves nothing behind.
    pub unsaved: HashSet<String>,
    /// A newer ticket for a saved remote server (its id, the ticket), from a
    /// link: used for the attempt under way, and saved once the server has
    /// accepted this device through it.
    pub pending_ticket: Option<(String, String)>,
    /// The server running on this machine, as its descriptor says (desktop).
    pub local_server: Option<crate::local::LocalServer>,
    /// `localAuto` has been read from the store ...
    pub local_auto_known: bool,
    /// ... and this install has signed in to its machine's server by itself
    /// before (or a server was forgotten): it does not do so again.
    pub local_auto_done: bool,
    /// The server a silent sign-in was last tried for: none again until a
    /// connection succeeds or the descriptor changes, so a server that keeps
    /// refusing is not asked in a loop.
    pub local_tried: Option<crate::local::LocalServer>,
    /// A sign-in to this machine's server, under way.
    pub local_attempt: Option<crate::local::LocalAttempt>,
    /// A link from outside the app, waiting for the user's answer.
    pub link_request: Option<LinkRequest>,
    /// Numbers link requests: an answer to an earlier one is ignored.
    pub link_counter: u32,
    pub active_env: Option<String>,
    pub pending_terminal_target: Option<(String, String, String)>,
    /// The active environment's bearer token.
    pub token: Option<String>,
    /// A pairing code waiting for the tunnel to come up.
    pub pending_code: Option<String>,
    pub tunnel: TunnelState,
    /// In the background (mobile): no reconnects until `Resume`.
    pub suspended: bool,
    /// Numbers the current connection attempt. Replies (HTTP, socket) carry
    /// the attempt they belong to; one from an abandoned attempt (another
    /// server selected, suspended, retried) is ignored.
    pub attempt: u64,
    /// Numbers the current tunnel; its stream's outputs carry it.
    pub tunnel_gen: u64,
    /// An attempt is under way: `Reconnect` waits for it.
    pub in_flight: bool,
    /// `Launch` is restoring saved servers; links and notification taps
    /// arriving meanwhile wait in `deferred`.
    pub launching: bool,
    pub deferred: Vec<crate::app::Event>,
    /// Seconds until the next retry (with jitter), while reconnecting.
    pub retry_in: u32,
    /// Numbers each scheduled retry: the shell's timer carries it back, and
    /// a timer for an older one is ignored.
    pub retry_gen: u64,
    /// The device has no network: no retries until it is back.
    pub offline: bool,
    /// Consecutive 400/403/404 answers to the ticket request (a proxy or
    /// captive portal answers those too; only a repeat means it).
    pub refusals: u32,
    /// The last sequence the shell stream delivered, to resume from.
    pub shell_sequence: Option<u64>,
    /// Drafts and unsent messages of the other servers, by environment id.
    pub parked: HashMap<String, Parked>,
    /// Settings before a change the server has not confirmed yet.
    pub settings_before: Option<Option<ServerSettings>>,
    /// When the last tick rendered (ticks every few seconds; relative times
    /// only need a render now and then).
    pub tick_rendered_ms: i64,
    /// The server refuses this device for good (not found, forbidden): no
    /// retries; pair again or forget it.
    pub server_changed: bool,
    /// This device's APNs token and environment (`sandbox` | `production`).
    pub push_token: Option<(String, String)>,
    /// This device's UnifiedPush endpoint and Web Push keys (`p256dh`, `auth`).
    pub push_endpoint: Option<(String, String, String)>,
    /// Live Activities of this server's threads: (thread, push token, APNs host).
    pub activity_tokens: Vec<(String, String, String)>,

    pub connection: Connection,
    /// The HTTP base of the active environment: its URL, or the tunnel's
    /// loopback address.
    pub server_url: String,
    pub error: Option<String>,
    pub reconnect_attempts: u32,
    /// The user asked to disconnect: no automatic reconnects.
    pub manual_disconnect: bool,
    pub rpc: RpcClient,

    pub toast: Option<Toast>,
    /// Toasts waiting for the one showing to go (a few at most).
    pub toast_queue: std::collections::VecDeque<Toast>,
    pub toast_counter: u32,
    /// Drafts changed since they were last saved.
    pub drafts_dirty: bool,
    /// The thread list or open thread changed since the offline copy was saved.
    pub cache_dirty: bool,
    pub cache_saved_ms: i64,
    /// What's shown came from the offline copy (the server hasn't answered yet).
    pub from_cache: bool,

    // Server state
    /// The server's settings, once its config has come.
    pub settings: Option<ServerSettings>,
    /// Values typed into settings that their records cannot hold (words
    /// where a number goes), shown until the server answers.
    pub setting_misfits: Vec<(String, crate::settings::Typed)>,
    pub providers: Vec<ServerProvider>,
    pub keybindings: Vec<ResolvedKeybindingRule>,
    /// The server's own description of itself (from its welcome).
    pub environment: Option<ExecutionEnvironmentDescriptor>,
    pub projects: Vec<ProjectShell>,
    pub threads: Vec<ThreadShell>,
    pub archived: Vec<ThreadShell>,
    /// What the server sent that this app could not read, and so left out,
    /// by what it was: the user is told at the first of each.
    pub unreadable: HashMap<&'static str, u32>,
    /// What a thread's stream sent for another thread (or into another
    /// thread's records), and so left out. Always 0 unless something is wrong.
    pub misrouted: u32,

    // Navigation
    pub route: String,
    pub selected_project: Option<String>,
    pub selected_thread: Option<String>,
    pub thread: Option<Thread>,
    /// Items of the open thread's stream this app could not read, and so left
    /// out, since the thread was loaded: the user is told at the first.
    pub thread_gaps: u32,
    /// What changed since a view was last built.
    pub changes: Changes,
    /// The last sequence applied to `thread`, to resume its stream from
    /// after a reconnect instead of loading it again.
    pub thread_sequence: u64,
    pub thread_loading: bool,
    /// Signed URLs (relative to the server) of attachments, by id; ones
    /// asked for are in `asset_urls` as empty until the answer comes.
    pub asset_urls: HashMap<String, String>,
    pub asset_url_expiries: HashMap<String, i64>,
    pub project_icons: crate::project_icons::State,
    /// Mermaid diagrams the server draws, by the shells' keys.
    pub diagrams: crate::diagrams::State,
    /// How this device lays out the thread list (saved on the device).
    pub sidebar_layout: crate::sidebar::SidebarLayout,
    /// The thread list shows only unread threads (and ones waiting on you).
    pub unread_only: bool,
    pub show_all_projects: HashSet<String>,
    /// Sections showing all their rows, past "show more".
    pub show_all_sections: HashSet<crate::sidebar::Section>,
    /// The open thread's section when it was opened, kept while it is open.
    pub held_section: Option<(String, crate::sidebar::Section)>,
    /// A thread asked for (a fork) that opens once the server lists it.
    pub open_when_created: Option<String>,

    pub search: Search,
    pub find: Find,
    pub composer: Composer,
    pub panel: Panel,
    pub palette: Option<Palette>,
    pub pages: Pages,
    pub sessions: AgentSessions,
    pub browser: FolderBrowser,

    /// Rows already built for the transcript, by item id with what each was
    /// built from: a render only builds the rows that changed (markdown
    /// parsing and edit diffs are the costly part).
    pub item_cache: std::sync::Mutex<rustc_hash::FxHashMap<TurnItemId, BuiltRow>>,
    /// The change an edit waiting for approval would make, by request, with
    /// the edits it was worked out from (a line diff is costly).
    pub approval_diffs: std::sync::Mutex<HashMap<String, (Vec<Arc<TurnItem>>, Vec<crate::view::DiffFileView>)>>,
    /// The settled start of each streaming reply's markdown, by item id.
    pub markdown_prefixes: std::sync::Mutex<rustc_hash::FxHashMap<String, crate::markdown::Prefix>>,
    /// Rows whose long output is shown in full.
    pub expanded: HashSet<String>,
    /// Secret requests (`secret_request` item ids) answered and waiting for
    /// the server; never what was answered.
    pub secrets_answering: HashSet<String>,
    /// Threads opened lately, newest last, with the rows built for them: one
    /// opened again shows at once and only catches up on what happened since.
    pub recent_threads: std::collections::VecDeque<RecentThread>,
    /// The thread being started: shown, open and listed, from the moment it is
    /// sent until the server has it (its shell and its first message).
    pub starting_thread: Option<Launching>,

    /// Last status notified per thread, so each transition notifies once.
    pub notified: HashMap<String, ShellStatus>,
    pub companion_beat: (String, u64),
}

/// A transcript row as it was last built, with what it was built from.
///
/// A record that changes is replaced in its thread by a new one, so a row is
/// current exactly while the thread still holds the records named here.
/// They are kept, not just their addresses: a record that left the thread
/// stays where it is, and no later one can be mistaken for it.
/// A thread sent to be started, before the server has it (`Model::starting_thread`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launching {
    pub id: String,
    pub project_id: String,
    pub title: String,
    pub message_id: String,
    pub text: String,
}

impl Model {
    /// The thread being started, while `id` is it and the server's copy is not
    /// shown yet: no shell, or no first message in the open thread.
    pub fn launching_shown(&self, id: &str) -> Option<&Launching> {
        self.starting_thread.as_ref().filter(|l| l.id == id).filter(|l| !self.thread.as_ref().is_some_and(|t| t.messages.contains(l.message_id.as_str())))
    }
}

/// A thread no longer open, as it was when it was left (`Model::recent_threads`).
pub struct RecentThread {
    pub id: String,
    pub thread: domovoi_model::typed::Thread,
    /// The last event of it applied: the stream resumes after it.
    pub sequence: u64,
    /// Its transcript's rows, as built.
    pub rows: Vec<(TurnItemId, BuiltRow)>,
}

/// At most this many threads are kept once left ...
pub const RECENT_THREADS: usize = 8;
/// ... holding at most this many turn items between them (a long thread's
/// items with their rows are a few KB each).
pub const RECENT_TURN_ITEMS: usize = 12_000;

impl Model {
    /// Keeps the open thread, with its rows, among the recent ones, before
    /// another is opened. One with gaps in its events, or not from a stream,
    /// is not kept: it would be shown as it should not be.
    pub fn keep_open_thread(&mut self) {
        let Some(thread) = self.thread.take() else { return };
        // Kept under its own id, and only if it is the one selected: a thread
        // held under another's selection is not that thread.
        let id = thread.record.id.as_str().to_owned();
        if self.selected_thread.as_deref() != Some(id.as_str()) || self.thread_sequence == 0 || self.thread_gaps > 0 {
            return;
        }
        let rows: Vec<(TurnItemId, BuiltRow)> = self.item_cache.get_mut().unwrap_or_else(std::sync::PoisonError::into_inner).drain().collect();
        self.recent_threads.retain(|r| r.id != id);
        self.recent_threads.push_back(RecentThread { id, thread, sequence: self.thread_sequence, rows });
        let mut items: usize = self.recent_threads.iter().map(|r| r.thread.turn_items.len()).sum();
        while self.recent_threads.len() > RECENT_THREADS || (items > RECENT_TURN_ITEMS && self.recent_threads.len() > 1) {
            if let Some(old) = self.recent_threads.pop_front() {
                items -= old.thread.turn_items.len();
            }
        }
        // The newest alone may be over the budget: kept only if it fits.
        if items > RECENT_TURN_ITEMS {
            self.recent_threads.clear();
        }
    }

    /// Makes `id` (or none) the open thread. This is the one way the
    /// selection changes: everything held for the thread left goes with it
    /// (kept among the recent ones when it can be), so nothing of it is shown
    /// as, or resumed into, the next one. One opened lately comes back as it
    /// was, with the sequence to resume its stream after.
    pub fn open_thread(&mut self, id: Option<String>) {
        self.keep_open_thread();
        let (thread, sequence) = match id.as_deref().and_then(|id| self.take_recent_thread(id)) {
            Some((thread, sequence)) => (Some(thread), sequence),
            None => (None, 0),
        };
        if thread.is_none() {
            // Rows built for the thread left are of no use to this one.
            self.item_cache.get_mut().unwrap_or_else(std::sync::PoisonError::into_inner).clear();
        }
        self.selected_thread = id;
        self.thread = thread;
        self.thread_sequence = sequence;
        self.thread_gaps = 0;
        self.find = Default::default();
        self.panel.diff = Default::default();
        self.composer.steer_next = false;
        self.composer.recall = None;
    }

    /// The open thread, if what is held is `id`'s own.
    pub fn held_thread(&self, id: &str) -> Option<&domovoi_model::typed::Thread> {
        self.thread.as_ref().filter(|thread| thread.record.id.as_str() == id && self.selected_thread.as_deref() == Some(id))
    }

    /// The thread `id` as it was left, with its rows back in the cache.
    pub fn take_recent_thread(&mut self, id: &str) -> Option<(domovoi_model::typed::Thread, u64)> {
        let at = self.recent_threads.iter().position(|r| r.id == id)?;
        let recent = self.recent_threads.remove(at)?;
        let cache = self.item_cache.get_mut().unwrap_or_else(std::sync::PoisonError::into_inner);
        cache.clear();
        cache.extend(recent.rows);
        Some((recent.thread, recent.sequence))
    }
}

pub struct BuiltRow {
    pub item: Arc<TurnItem>,
    /// The message the item shows, if it shows one the thread holds.
    pub message: Option<Arc<ConversationMessage>>,
    /// Whether the row's long output was shown in full.
    pub expanded: bool,
    /// The row, before what the page writes on it from the rows around it.
    pub row: crate::view::ItemView,
}

/// What a server's composer keeps while another server is active.
#[derive(Default)]
pub struct Parked {
    pub drafts: HashMap<String, String>,
    pub outbox: Vec<Outgoing>,
}

/// The import picker: Claude Code, Codex and Pi sessions the server found.
#[derive(Default)]
pub struct AgentSessions {
    pub scanned: bool,
    pub scanning: bool,
    pub candidates: Vec<AgentSessionProjectCandidate>,
    pub error: Option<String>,
    /// Folders still to import: (path, existing project id).
    pub queue: Vec<(String, Option<String>)>,
    pub total: u32,
    pub done: u32,
    pub imported_sessions: u64,
    pub new_projects: u32,
    pub failures: Vec<String>,
}

impl AgentSessions {
    pub fn importing(&self) -> bool {
        self.total > 0 && self.done < self.total
    }
}

/// The server folder picker (`filesystem.browse`).
#[derive(Default)]
pub struct FolderBrowser {
    pub path: String,
    pub parent: String,
    pub entries: Vec<FilesystemBrowseEntry>,
    pub loading: bool,
    pub error: Option<String>,
    pub revision: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Toast {
    pub id: u32,
    pub kind: String,
    pub message: String,
    /// A button on the toast ("Undo"); empty for none.
    pub action_label: String,
    /// What the button does, for `ToastAction` (`unarchive:<thread id>`).
    pub action: String,
}

#[derive(Default)]
pub struct Search {
    pub query: String,
    pub revision: u32,
    pub results: Vec<ThreadSearchMatch>,
    pub searching: bool,
}

#[derive(Default)]
pub struct Find {
    pub open: bool,
    pub query: String,
    pub revision: u32,
    /// The server's answer to the search, once it has come.
    pub result: Option<FindInThreadResult>,
}

#[derive(Default)]
pub struct Attachment {
    pub local_id: String,
    pub name: String,
    pub mime_type: String,
    pub size_bytes: u64,
    pub data_url: String,
    /// The server's id once persisted.
    pub id: Option<String>,
    pub uploading: bool,
}

#[derive(Default)]
pub struct Composer {
    /// Drafts per thread id, or `new:<project id>` for a new thread.
    pub drafts: HashMap<String, String>,
    pub draft_revision: u32,
    pub attachments: Vec<Attachment>,
    pub model: Option<ModelOption>,
    /// The model picker's search.
    pub model_query: String,
    /// The model picker lists hidden models too.
    pub show_hidden_models: bool,
    pub efforts: HashMap<String, String>,
    pub runtime_mode: String,
    pub interaction_mode: String,
    pub workspace_mode: String,
    /// A new thread goes into this checked-out worktree (a pull request's):
    /// path, branch, and how the composer names it.
    pub existing_worktree: Option<(String, String, String)>,
    pub base_branch: Option<String>,
    pub branches: Vec<String>,
    pub branches_cwd: String,
    pub mention: Option<Mention>,
    /// Where the `@` of a mention the user dismissed (Escape) is: typing on
    /// after it doesn't open the list again until a new `@` starts.
    pub mention_dismissed_at: Option<usize>,
    pub sending: bool,
    /// A message waiting for its attachments to persist.
    pub pending_send: Option<String>,
    /// The message being sent goes into the turn under way (`Steer`).
    pub steer_next: bool,
    /// Messages and new threads sent but not yet answered. A dropped
    /// connection sends them again on reconnect (the server answers a
    /// repeated `commandId` from its receipt, so nothing runs twice).
    pub outbox: Vec<Outgoing>,
    /// Drafts put aside to send later, newest first (kept on the device).
    pub stash: Vec<String>,
    /// How far back `RecallPrompt` has gone in the open thread's own
    /// messages (1: the latest), and the draft from before it started.
    pub recall: Option<(usize, String)>,
    /// When Stop was asked of the turn under way (Unix ms; 0: not asked), to
    /// offer a force stop when it doesn't end.
    pub stop_asked_ms: i64,
    /// The run Stop was asked of (a later turn isn't offered a force stop).
    pub stop_asked_run: String,
    /// The workspace's worktrees, from its branches: (branch, path).
    pub worktrees: Vec<(String, String)>,
    /// Bumped by `/model` for the shell to open its model picker.
    pub model_picker_requested: u32,
}

/// A send waiting for the server's answer.
#[derive(Debug, Clone)]
pub struct Outgoing {
    /// The request as it is sent, and sent again after a dropped connection:
    /// a command or a launch, as the record the server reads.
    pub request: crate::requests::Request,
    /// The message it adds to a thread, if it is one.
    pub message_id: Option<String>,
    pub route: crate::rpc::Route,
    /// Names it in an error ("Send message", "Stop").
    pub label: String,
    /// Where the text goes back if it never arrives.
    pub draft_key: String,
    pub text: String,
    /// When it was last sent.
    pub sent_ms: i64,
}

#[derive(Default)]
pub struct Mention {
    pub query: String,
    pub revision: u32,
    pub results: Vec<ProjectEntry>,
    pub highlighted: usize,
}

#[derive(Default)]
pub struct Panel {
    pub open: bool,
    pub tab: String,
    pub diff: DiffState,
    pub terminals: Vec<Session>,
    pub terminal_owner: Option<String>,
    pub active_terminal: Option<String>,
    pub focus_terminal: Option<String>,
    pub terminal_counter: u32,
    pub terminal_splits: Vec<crate::terminal::SplitPreference>,
    pub interactive_terminals: std::collections::HashSet<(String, String, String)>,
    /// Every terminal the server runs (`subscribeTerminalMetadata`).
    pub terminal_summaries: Vec<TerminalSummary>,
    pub terminal_details: Vec<crate::view::TerminalDetailView>,
    pub terminal_details_available: bool,
    pub terminal_seen: HashMap<String, u64>,
    pub files: FilesState,
    pub git: GitState,
    pub artifacts: ArtifactsState,
}

#[derive(Default)]
pub struct ArtifactsState {
    pub thread_id: Option<String>,
    pub active_id: Option<String>,
    pub active_version: Option<u32>,
    /// `preview` | `code`
    pub mode: String,
    pub dark: bool,
    /// Workspace file content read for the active file-backed artifact.
    pub file_read: Option<ArtifactRead>,
    /// Signed server preview URL (`artifact-bundle`, `artifact-react`, `mermaid-diagram-svg`).
    pub preview_url: Option<ArtifactPreviewUrl>,
}

pub struct ArtifactPreviewUrl {
    pub request: String,
    pub thread: String,
    pub artifact_id: String,
    pub version: u32,
    pub dark: bool,
    pub url: Option<String>,
    pub loading: bool,
    pub error: Option<String>,
}

impl ArtifactPreviewUrl {
    pub fn matches(&self, thread_id: &str, artifact_id: &str, version: u32) -> bool {
        self.thread == thread_id && self.artifact_id == artifact_id && self.version == version
    }
}

#[derive(Default)]
pub struct DiffState {
    pub source: String,
    pub loading: bool,
    pub error: Option<String>,
    /// The diff's files, parsed once when the patch comes (not on every render).
    files: Vec<crate::view::DiffFileView>,
    pub files_override: Option<Vec<crate::view::DiffFileView>>,
    pub truncated: bool,
    pub for_thread: Option<String>,
}

impl DiffState {
    pub fn set_patch(&mut self, patch: &str) {
        self.files = crate::diff::parse_unified(patch);
    }

    pub fn files(&self) -> &[crate::view::DiffFileView] {
        &self.files
    }
}

#[derive(Default)]
pub struct FilesState {
    pub root: String,
    /// Children per directory path ("" = root).
    pub children: HashMap<String, Vec<ProjectEntry>>,
    pub expanded: HashSet<String>,
    pub loading: HashSet<String>,
    pub query: String,
    pub revision: u32,
    pub results: Vec<ProjectEntry>,
    pub preview: Option<Preview>,
    pub artifact_preview: Option<ArtifactRead>,
}

pub struct ArtifactRead {
    pub request: String,
    pub thread: String,
    pub root: String,
    pub preview: Preview,
}

impl ArtifactRead {
    pub fn current(&self, model: &Model) -> bool {
        model.selected_thread.as_deref() == Some(self.thread.as_str())
            && model.active_cwd().as_deref() == Some(self.root.as_str())
    }
}

#[derive(Default)]
pub struct Preview {
    pub path: String,
    pub request: String,
    pub text: String,
    pub image_url: Option<String>,
    /// Its highlighting, worked out once when it arrives.
    pub spans: Vec<crate::view::CodeSpan>,
    pub truncated: bool,
    pub error: Option<String>,
    pub loading: bool,
    /// The file's size (a Git LFS file's: its object's).
    pub byte_length: u64,
    /// The file is kept in Git LFS: its object, and whether the server has it.
    pub lfs: Option<domovoi_model::typed::ProjectFileLfs>,
    /// `projects.fetchLfsObject` is under way for it.
    pub fetching: bool,
    /// Why fetching its Git LFS object failed.
    pub fetch_error: Option<String>,
}

#[derive(Default)]
pub struct GitState {
    pub cwd: String,
    /// The checkout's own state, once the server has reported it.
    pub local: Option<VcsStatusLocal>,
    pub remote: Option<VcsStatusRemote>,
    pub busy: bool,
    pub progress: Vec<String>,
    pub error: Option<String>,
}

#[derive(Default)]
pub struct Palette {
    pub query: String,
    pub highlighted: usize,
    /// `commands` (empty too), `files` (go to file) or `content` (search
    /// the project's files).
    pub mode: String,
    /// Names the search asked last; an older answer is dropped.
    pub revision: u32,
    pub searching: bool,
    /// What the last search found, as the palette lists it.
    pub results: Vec<crate::view::PaletteItemView>,
}

/// A webhook delivery as the task's log lists it (`ScheduledTaskWebhookDeliverySummary`).
#[derive(Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct WebhookDelivery {
    pub id: String,
    pub received_at: String,
    pub method: String,
    pub outcome: String,
    pub signature_verified: bool,
    pub missing_fields: Vec<String>,
    pub error: Option<String>,
}

/// A scheduled task's run as `scheduledTasks.listRuns` answers it.
#[derive(Deserialize, Clone, Debug, PartialEq, Default)]
#[serde(rename_all = "camelCase", default)]
pub struct ScheduledRun {
    pub task_id: String,
    pub started_at: String,
    pub trigger: String,
    pub thread_id: Option<String>,
    pub status: String,
    pub error: Option<String>,
}

#[derive(Default)]
pub struct Pages {
    /// Each provider instance's sign-in and managed install, as last reported.
    pub provider_auth: std::collections::BTreeMap<String, domovoi_model::typed::ProviderAuthState>,
    pub provider_install: std::collections::BTreeMap<String, domovoi_model::typed::ProviderInstallState>,
    pub scheduled: Vec<ScheduledTask>,
    pub scheduled_time_zone: String,
    /// The deliveries of the webhook task last opened: its id and its log.
    pub webhook_deliveries: Option<(String, Vec<WebhookDelivery>)>,
    /// Every scheduled task's latest run (a server without run history sends none).
    pub scheduled_latest_runs: Vec<ScheduledRun>,
    /// The task whose runs are open, and its runs.
    pub scheduled_runs: Option<(String, Vec<ScheduledRun>)>,
    pub access: Option<AuthAccessSnapshot>,
    pub database: Option<DatabaseStorageStatus>,
    pub cleanup: Option<StorageCleanupReport>,
    pub storage_busy: bool,
    pub diagnostics: Option<ServerProcessDiagnosticsResult>,
    pub host: Option<HostResourcesSnapshot>,
    pub prs_loading: bool,
    pub prs: Vec<PullRequestListEntry>,
    pub prs_error: Option<String>,
    pub prs_notice: Option<String>,
    /// A pull request action or thread checkout under way (its key).
    pub pr_busy: Option<String>,
    pub pr_selected: Option<String>,
    pub pr_detail: Option<PullRequestSummary>,
    pub pr_checks: Option<PullRequestChecks>,
    pub usage_loading: bool,
    pub usage_error: Option<String>,
    pub usage: Option<UsageSummary>,
    pub usage_days: u32,
    /// Provider key (`"claude"`, `"codex"`, …) or empty for all providers.
    pub usage_provider_filter: String,
    pub usage_limits_refreshing: bool,
    /// `GET /api/domovoi/remote`; `None` on a server without remote access.
    pub remote: Option<RemoteAccessStatus>,
    /// The last minted pairing link.
    pub remote_link: Option<RemotePairingLink>,
    pub remote_busy: bool,
    pub remote_error: Option<String>,
}

/// How far an update's changes reach in the view. Nearly every update can
/// change anything; a streaming reply's frames change one row's text, a few
/// times a second, in a thread of any length. `FfiApp` builds the view again
/// from the rows named here when nothing wider happened since its last one.
///
/// Every update goes through `DomovoiApp::update`, which counts it as wide unless
/// it was a socket frame that says otherwise, or an update that said how far
/// it reaches (`composer`, `nothing_shown`): a keystroke or a quiet tick must
/// not make the next streamed token build the whole view. Code that changes a
/// `Model` some other way (a test, a bench) calls `wide` itself.
#[derive(Debug, Default)]
pub struct Changes {
    /// Counts the updates that may have changed anything.
    wide: u64,
    /// Counts the rows ever named in `rows`.
    narrow: u64,
    /// The open thread's rows whose text grew, by turn item id, with their
    /// count: the newest `ROWS_KEPT` of them.
    rows: std::collections::VecDeque<(u64, String)>,
    /// A socket frame is being applied, and something in it reaches further
    /// than a row's text.
    frame_wide: bool,
    /// Counts the updates that changed the composer and nothing else.
    composer: u64,
    /// The update being applied said how far it reaches.
    scoped: bool,
}

/// More rows than change between two views; a view further behind than
/// this is built whole.
const ROWS_KEPT: usize = 64;

impl Changes {
    /// Anything in the view may have changed.
    pub fn wide(&mut self) {
        self.wide += 1;
    }

    /// The text of this row of the open thread grew, and nothing else did.
    pub fn row(&mut self, item_id: &str) {
        self.narrow += 1;
        self.rows.push_back((self.narrow, item_id.to_owned()));
        if self.rows.len() > ROWS_KEPT {
            self.rows.pop_front();
        }
    }

    /// The frame being applied changes more than rows' text.
    pub fn frame_is_wide(&mut self) {
        self.frame_wide = true;
    }

    /// Ends a socket frame: counted as wide if anything in it was. Whether it was.
    pub fn end_frame(&mut self) -> bool {
        self.scoped = false;
        let wide = std::mem::take(&mut self.frame_wide);
        if wide {
            self.wide();
        }
        wide
    }

    /// The update being applied changes the composer and nothing else in the view.
    pub fn composer(&mut self) {
        self.composer += 1;
        self.scoped = true;
    }

    /// The update being applied changes nothing the view shows.
    pub fn nothing_shown(&mut self) {
        self.scoped = true;
    }

    /// Ends an update that was no socket frame: wide unless it said how far
    /// it reaches. Whether it was wide.
    pub fn end_update(&mut self) -> bool {
        let wide = !std::mem::take(&mut self.scoped);
        if wide {
            self.wide();
        }
        wide
    }

    /// How many composer-only updates there were: a view built at another
    /// count builds its composer again.
    pub fn composer_mark(&self) -> u64 {
        self.composer
    }

    /// Where the changes stand: what a view remembers to ask `rows_since` later.
    pub fn mark(&self) -> (u64, u64) {
        (self.wide, self.narrow)
    }

    /// The rows whose text grew since `mark`, each once, when nothing wider
    /// changed and none has left the log; `None` otherwise.
    pub fn rows_since(&self, mark: (u64, u64)) -> Option<Vec<&str>> {
        let (wide, narrow) = mark;
        let oldest_kept = self.narrow - self.rows.len() as u64;
        if wide != self.wide || narrow < oldest_kept {
            return None;
        }
        let mut ids: Vec<&str> = vec![];
        for (_, id) in self.rows.iter().filter(|(n, _)| *n > narrow) {
            if !ids.contains(&id.as_str()) {
                ids.push(id);
            }
        }
        Some(ids)
    }
}

impl Model {
    pub fn active_environment(&self) -> Option<&Environment> {
        let id = self.active_env.as_deref()?;
        self.environments.iter().find(|e| e.id == id)
    }

    /// A sign-in to this machine's server is under way for the current attempt.
    pub fn local_signing_in(&self) -> bool {
        self.local_attempt.as_ref().is_some_and(|local| local.attempt == self.attempt)
    }

    /// Forgets everything learned from the server (switching environments).
    pub fn clear_server_state(&mut self) {
        // This server's unsent work waits for it to be active again.
        let mut parked = std::mem::take(&mut self.parked);
        if let Some(id) = self.active_env.clone() {
            let drafts: HashMap<String, String> = std::mem::take(&mut self.composer.drafts).into_iter().filter(|(_, v)| !v.trim().is_empty()).collect();
            let outbox = std::mem::take(&mut self.composer.outbox);
            if drafts.is_empty() && outbox.is_empty() {
                parked.remove(&id);
            } else {
                parked.insert(id, Parked { drafts, outbox });
            }
        }
        let keep = Model {
            parked,
            offline: self.offline,
            now_ms: self.now_ms,
            shown_ms: self.shown_ms,
            time_zone: self.time_zone.clone(),
            utc_offset_secs: self.utc_offset_secs,
            // Kept: a view's mark must never match a later model's by chance.
            changes: std::mem::take(&mut self.changes),
            focused: self.focused,
            surface: std::mem::take(&mut self.surface),
            device_label: std::mem::take(&mut self.device_label),
            device_os: std::mem::take(&mut self.device_os),
            environments: std::mem::take(&mut self.environments),
            unsaved: std::mem::take(&mut self.unsaved),
            pending_ticket: self.pending_ticket.take(),
            // This machine's server and a link waiting for an answer are not the active server's.
            local_server: self.local_server.take(),
            local_auto_known: self.local_auto_known,
            local_auto_done: self.local_auto_done,
            local_tried: self.local_tried.take(),
            link_request: self.link_request.take(),
            link_counter: self.link_counter,
            active_env: self.active_env.take(),
            pending_terminal_target: self.pending_terminal_target.take(),
            panel: Panel {
                terminal_splits: std::mem::take(&mut self.panel.terminal_splits),
                interactive_terminals: std::mem::take(&mut self.panel.interactive_terminals),
                ..Default::default()
            },
            token: self.token.take(),
            tunnel: std::mem::take(&mut self.tunnel),
            push_token: self.push_token.take(),
            push_endpoint: self.push_endpoint.take(),
            toast_counter: self.toast_counter,
            companion_beat: (String::new(), self.companion_beat.1),
            // Counters keep counting, so no stale reply can match a new attempt.
            attempt: self.attempt,
            tunnel_gen: self.tunnel_gen,
            launching: self.launching,
            deferred: std::mem::take(&mut self.deferred),
            sidebar_layout: std::mem::take(&mut self.sidebar_layout),
            ..Default::default()
        };
        *self = keep;
    }

    /// Requests died with the socket: everything waiting on one stops
    /// waiting, so its screen can offer to try again.
    pub fn socket_lost_flags(&mut self) {
        crate::project_icons::disconnected(self);
        // Not sent again: the card can be answered anew.
        self.secrets_answering.clear();
        crate::diagrams::disconnected(self);
        if self.sessions.scanning {
            self.sessions.scanning = false;
            if !self.sessions.scanned {
                self.sessions.error = Some("The connection dropped while looking for sessions.".into());
            }
        }
        if self.sessions.importing() {
            let left = self.sessions.total - self.sessions.done;
            self.sessions.failures.push(format!("{left} folder(s) not imported: the connection dropped"));
            self.sessions.queue.clear();
            self.sessions.done = self.sessions.total;
        }
        self.search.searching = false;
        if self.browser.loading {
            self.browser.loading = false;
            self.browser.error = Some("The connection dropped. Pull to try again.".into());
        }
        self.pages.prs_loading = false;
        if self.pages.usage_loading {
            self.pages.usage_loading = false;
            if self.pages.usage.is_none() {
                self.pages.usage_error = Some("The connection dropped while loading usage.".into());
            }
        }
        self.pages.usage_limits_refreshing = false;
        self.pages.storage_busy = false;
        self.pages.remote_busy = false;
        self.panel.git.busy = false;
        self.panel.diff.loading = false;
        self.panel.files.loading.clear();
        if let Some(p) = self.panel.files.preview.as_mut().filter(|p| p.loading) {
            p.loading = false;
            p.error = Some("The connection dropped.".into());
        }
        if let Some(before) = self.settings_before.take() {
            self.set_settings(before);
        }
        self.composer.mention = None;
        // An upload died with the socket: nothing will finish its send, which
        // can be tried again (the draft is still there). A thread being
        // started is in the outbox and goes again on its own.
        if self.composer.pending_send.is_some() || self.composer.attachments.iter().any(|a| a.uploading) {
            self.composer.sending = false;
            self.composer.pending_send = None;
            for a in &mut self.composer.attachments {
                a.uploading = false;
            }
        }
    }

    /// Replaces the settings held; what was typed into the ones before goes with them.
    pub fn set_settings(&mut self, settings: Option<ServerSettings>) {
        self.settings = settings;
        self.setting_misfits.clear();
    }

    /// Something the server sent does not fit the schema this build knows
    /// and is left out: counted, and the user is told at the first of its kind.
    pub fn left_out(&mut self, what: &'static str, why: &dyn std::fmt::Display) {
        let count = self.unreadable.entry(what).or_default();
        *count += 1;
        if *count == 1 {
            self.toast("error", format!("This app can't read {what} from the server, so what it shows may be incomplete ({why})."));
        }
    }

    /// Brings back what the now active server's composer had.
    pub fn unpark(&mut self) {
        if let Some(p) = self.active_env.clone().and_then(|id| self.parked.remove(&id)) {
            self.composer.drafts = p.drafts;
            self.composer.outbox = p.outbox;
            self.composer.draft_revision += 1;
        }
    }

    pub fn project(&self, id: &str) -> Option<&ProjectShell> {
        self.projects.iter().find(|p| p.id.as_str() == id)
    }

    pub fn thread_shell(&self, id: &str) -> Option<&ThreadShell> {
        self.threads.iter().find(|t| t.id.as_str() == id).or_else(|| self.archived.iter().find(|t| t.id.as_str() == id))
    }

    /// The working directory of a thread: its worktree, else its project root.
    pub fn thread_cwd(&self, id: &str) -> Option<String> {
        let t = self.thread_shell(id)?;
        t.worktree_path.clone().or_else(|| Some(self.project(t.project_id.as_str())?.workspace_root.clone()))
    }

    /// The directory the panel tools (files, git, terminals) act on.
    pub fn active_cwd(&self) -> Option<String> {
        match &self.selected_thread {
            Some(t) => self.thread_cwd(t),
            None => self.selected_project.as_deref().and_then(|p| self.project(p)).map(|p| p.workspace_root.clone()),
        }
    }

    pub fn draft_key(&self) -> String {
        match &self.selected_thread {
            Some(t) => t.clone(),
            None => format!("new:{}", self.selected_project.clone().unwrap_or_default()),
        }
    }

    pub fn draft(&self) -> String {
        self.composer.drafts.get(&self.draft_key()).cloned().unwrap_or_default()
    }

    pub fn toast(&mut self, kind: &str, message: impl Into<String>) {
        self.toast_with_action(kind, message, "", "");
    }

    /// Shows a toast, or queues it behind the one showing (the same message
    /// twice is shown once; at most three wait).
    pub fn toast_with_action(&mut self, kind: &str, message: impl Into<String>, action_label: &str, action: &str) {
        self.toast_counter += 1;
        let toast = Toast { id: self.toast_counter, kind: kind.into(), message: message.into(), action_label: action_label.into(), action: action.into() };
        let shown = self.toast.as_ref().map(|t| t.message.as_str());
        if shown == Some(toast.message.as_str()) || self.toast_queue.iter().any(|t| t.message == toast.message) {
            return;
        }
        if self.toast.is_none() {
            self.toast = Some(toast);
            return;
        }
        if self.toast_queue.len() >= 3 {
            self.toast_queue.pop_front();
        }
        self.toast_queue.push_back(toast);
    }
}
