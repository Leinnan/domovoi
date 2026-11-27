//! View model handed to shells: plain strings, numbers, booleans, options and
//! lists (facet typegen). Every decision a shell would otherwise make (what to
//! show, labels, grouping, enabled states) is made in the core.

use facet::Facet;
use serde::{Deserialize, Serialize};

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct ViewModel {
    pub connection: ConnectionView,
    /// A transient message the shell shows and dismisses with `DismissToast`.
    pub toast: Option<ToastView>,
    pub sidebar: SidebarView,
    /// `thread` | `new-thread` | `pull-requests` | `usage` | `empty`
    pub route: String,
    pub thread: Option<ThreadPageView>,
    pub composer: ComposerView,
    pub panel: PanelView,
    pub palette: Option<PaletteView>,
    pub settings: SettingsView,
    pub pull_requests: PullRequestsView,
    pub issues: IssuesView,
    pub usage: UsageView,
    /// The import picker (after `ScanAgentSessions`).
    pub agent_sessions: AgentSessionsView,
    /// The server folder picker (after `BrowseFolders`).
    pub folder_browser: FolderBrowserView,
    /// Mermaid diagrams asked for with `LoadDiagram`.
    pub diagrams: Vec<DiagramView>,
}

/// A Mermaid diagram the server draws.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct DiagramView {
    /// The shell's key from `LoadDiagram`.
    pub key: String,
    /// The PNG, while the link is valid.
    pub url: Option<String>,
    /// Waiting for the server.
    pub pending: bool,
    /// The server could not draw it (or does not draw diagrams): show the source.
    pub failed: bool,
    /// Not connected: it is drawn once the server is back.
    pub offline: bool,
    /// Set when the diagram should be asked for again (`LoadDiagram`); it
    /// changes with each connection.
    pub load_key: Option<String>,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentSessionsView {
    pub scanned: bool,
    pub scanning: bool,
    pub importing: bool,
    /// Folders imported so far, of `total`.
    pub done: u32,
    pub total: u32,
    pub error: Option<String>,
    /// Newest activity first.
    pub candidates: Vec<AgentSessionCandidateView>,
    /// Candidates not imported yet.
    pub available: u32,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct AgentSessionCandidateView {
    pub path: String,
    pub title: String,
    /// `Claude Code`, `Codex`, `Pi`, or several joined with ` · `.
    pub sources: String,
    /// Sessions the import takes (the last 30 days).
    pub thread_count: u32,
    /// e.g. `3 sessions · 2d ago`.
    pub detail: String,
    pub already_imported: bool,
    /// Active in the last two weeks: worth preselecting.
    pub recent: bool,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct FolderBrowserView {
    /// The folder listed (absolute).
    pub path: String,
    /// The folder above it (empty at `/`).
    pub parent_path: String,
    pub entries: Vec<FolderEntryView>,
    pub loading: bool,
    pub error: Option<String>,
    /// `path` is already a project.
    pub is_project: bool,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct FolderEntryView {
    pub name: String,
    pub full_path: String,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct ConnectionView {
    /// `disconnected` | `pairing` | `connecting` | `connected` | `reconnecting` | `failed`
    pub state: String,
    pub server_url: String,
    pub error: Option<String>,
    pub server_version: String,
    pub environment_label: String,
    pub platform: String,
    /// The shell should schedule `Reconnect` after this many seconds (0 = none).
    pub reconnect_in_seconds: u32,
    pub environments: Vec<EnvironmentView>,
    pub active_environment_id: String,
    /// The list shown is the offline copy from the last time connected.
    pub showing_saved: bool,
    /// The active environment is remote (reached over iroh).
    pub remote: bool,
    /// `direct` | `relay` | empty: how remote traffic flows now.
    pub path: String,
    pub rtt_ms: u32,
    /// The active environment has no token: show the pairing form.
    pub needs_pairing: bool,
    /// The server refuses this device for good: offer Pair Again and Forget.
    pub server_changed: bool,
    /// A line of advice under the connection error (empty when none).
    pub hint: String,
    /// Numbers the scheduled retry: the shell restarts its timer when this
    /// changes and sends `RetryTimer` with it.
    pub retry_gen: u64,
    /// The device has no network.
    pub offline: bool,
    /// An attempt is under way now (no countdown).
    pub attempting: bool,
    pub companion: CompanionLookView,
    pub activity: ActivityView,
    /// A Domovoi server runs on this machine and this app can sign in to it
    /// with no pairing code: offer it (`ConnectLocalServer`). Desktop only,
    /// and only while the app is not already using that server.
    pub local_server: Option<LocalServerView>,
    /// A `domovoi://` link reached the app from outside and waits for the
    /// user: ask before anything connects (`AnswerLinkRequest`).
    pub link_request: Option<LinkRequestView>,
}

/// The server running on this machine.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct LocalServerView {
    /// The machine's name, as the server gives it.
    pub label: String,
    /// `http://127.0.0.1:13773`.
    pub url: String,
}

/// A link's request to connect, as a dialog asks it: `title`, `message`,
/// then what identifies the server, one line each, then Cancel and
/// `confirm_label`. The lines, the same on every client:
///
/// - `Server key` with `endpoint` and an ellipsis, in a fixed-width face;
/// - `Saved here as` with `known_label`, when the server is a saved one;
/// - otherwise `Name in the link` with `link_name`, when the link gives one.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct LinkRequestView {
    /// Goes back in `AnswerLinkRequest`.
    pub id: u32,
    pub title: String,
    pub message: String,
    /// The accepting button's label.
    pub confirm_label: String,
    /// The start of the server's key (its endpoint id), which the link cannot fake.
    pub endpoint: String,
    /// What the link calls the server (anyone can write anything there);
    /// empty when it gives no name. Never shown as a saved server's name.
    pub link_name: String,
    /// The saved server the link is for; empty for a server not saved here.
    pub known_label: String,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct CompanionLookView {
    pub look: crate::companion::CompanionLook,
    pub custom: bool,

}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct ActivityView {
    pub working: u32,
    pub attention: u32,
    /// The latest reaction: `finished`, `failed`, or empty.
    pub beat: String,
    pub beat_seq: u64,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct EnvironmentView {
    pub id: String,
    pub label: String,
    /// The URL, or `Remote · <endpoint id prefix>`.
    pub detail: String,
    pub remote: bool,
    pub active: bool,
    /// The server's own id (what its pushes and deep links name).
    pub server_id: String,
    pub companion: CompanionLookView,
}

/// Settings → Remote Access: the connected server's iroh side.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct RemoteAccessView {
    /// The server has remote access routes.
    pub available: bool,
    pub enabled: bool,
    pub running: bool,
    pub endpoint_id: String,
    pub home_relay: String,
    pub relays: Vec<String>,
    /// The last pairing link minted (shown as a QR code), and its code.
    pub link: String,
    pub code: String,
    pub link_expires_at: String,
    pub busy: bool,
    pub error: Option<String>,
    /// `off` | `starting` | `online` | `lanOnly` | `error` (older servers: empty).
    pub state: String,
    /// The server can send pushes (it has an APNs key).
    pub push_configured: bool,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct ToastView {
    pub id: u32,
    /// `error` | `info` | `success`
    pub kind: String,
    pub message: String,
    /// A button ("Undo") that sends `ToastAction`; empty for none.
    pub action_label: String,
    /// More toasts wait behind this one.
    pub queued: u32,
}

// ---- sidebar ----

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct SidebarView {
    #[serde(default)]
    pub terminal_attention: Vec<TerminalDetailView>,
    /// `status` (by what each thread needs: `sections`) | `project`
    /// (under project headers: `projects`, `settled`, `snoozed`).
    pub grouping: String,
    /// Inbox, Running, Inactive, Archived (only those with threads), when
    /// grouped by status.
    pub sections: Vec<SidebarSectionView>,
    /// Every project; grouped by status, without their threads (for
    /// pickers), so `threads` and `earlier` are empty.
    pub projects: Vec<SidebarProjectView>,
    /// Settled threads of every project, newest first.
    pub settled: Vec<ThreadRowView>,
    /// Snoozed threads, the next to wake first.
    pub snoozed: Vec<ThreadRowView>,
    /// The threads most worth a glance, whichever the grouping: waiting on
    /// you, then working, then unread, then the latest (widgets, shortcuts,
    /// live activities).
    pub surface_threads: Vec<ThreadRowView>,
    /// Only unread threads (and ones waiting on you) are listed.
    pub unread_only: bool,
    pub selected_thread_id: Option<String>,
    pub selected_project_id: Option<String>,
    pub search_query: String,
    /// Message matches from the server when a query is active.
    pub search_results: Vec<SearchHitView>,
    pub searching: bool,
    pub threads_needing_attention: u32,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct SidebarSectionView {
    /// `inbox` | `running` | `inactive` | `archived`
    pub kind: String,
    pub title: String,
    /// Every thread in the section, shown or not.
    pub count: u32,
    pub expanded: bool,
    /// Empty while collapsed.
    pub rows: Vec<ThreadRowView>,
    /// Threads past the "show more" limit.
    pub hidden_count: u32,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct ProjectIconView {
    pub kind: String,
    pub text: String,
    pub symbol: String,
    pub color: String,
    pub custom: bool,
    pub file_path: Option<String>,
    pub image_url: Option<String>,
    pub project_id: String,
    pub load_key: Option<String>,
    pub saving: bool,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct SidebarProjectView {
    pub id: String,
    pub title: String,
    pub workspace_root: String,
    pub icon: ProjectIconView,
    pub automatic_icon: ProjectIconView,
    pub expanded: bool,
    pub is_scratch: bool,
    pub pinned: bool,
    pub threads: Vec<ThreadRowView>,
    /// Threads hidden by the "show more" limit.
    pub hidden_count: u32,
    /// Settled threads of this project (imported sessions among them),
    /// newest first; all of them after `ShowAllThreads`.
    pub earlier: Vec<ThreadRowView>,
    pub earlier_count: u32,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct ThreadRowView {
    pub issue_badge: Option<String>,
    pub project_icon: ProjectIconView,
    pub id: String,
    pub project_id: String,
    pub project_title: String,
    pub title: String,
    /// The model's short name, such as `Opus 4.7`.
    pub model_label: String,
    /// What the thread is doing, when that beats its age: `Needs you`,
    /// `Question`, `Working 6m`, `Planning 6m`, `Failed`; empty otherwise.
    pub status_label: String,
    /// `working` | `approval` | `failed` | `interrupted` | `completed` | `idle`
    pub status: String,
    /// When the turn under way started (Unix ms; 0: none, or not known).
    pub working_since_ms: i64,
    /// What the thread waits on (status `approval`) is a question, not an approval.
    pub question: bool,
    pub unread: bool,
    pub pinned: bool,
    pub settled: bool,
    pub archived: bool,
    pub branch: Option<String>,
    pub in_worktree: bool,
    /// Relative age such as `5m`, `3h`, `2d`.
    pub age: String,
    /// Imported from `Claude Code`, `Codex` or `Pi`; empty for Domovoi's own threads.
    pub source: String,
    /// When a snoozed thread wakes (Unix ms; 0: not snoozed).
    pub snoozed_until_ms: i64,
    /// `fork` for a fork of another thread; empty otherwise.
    pub relation: String,
    /// The agent proposed a plan that waits to be carried out.
    pub plan_ready: bool,
    /// The thread is in plan mode (its turns plan rather than act); while
    /// it works, `status_label` says `Planning`.
    pub plan_mode: bool,
    /// The thread's pull request (`#12`); empty when it has none.
    pub pull_request: String,
    /// `open` | `draft` | `merged` | `closed` (empty when not known).
    pub pull_request_state: String,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct SearchHitView {
    pub thread_id: String,
    pub title: String,
    pub project_title: String,
    pub snippet: String,
    pub source: String,
}

// ---- thread ----

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct ThreadPageView {
    pub can_create_pull_request: bool,
    pub pull_request_sheet: Option<PullRequestSheetView>,
    pub pull_request_status: Option<PullRequestStatusView>,
    pub id: String,
    pub title: String,
    pub project_id: String,
    pub project_title: String,
    pub branch: Option<String>,
    pub worktree_path: Option<String>,
    pub cwd: String,
    pub model_label: String,
    pub provider_instance_id: String,
    /// The thread's model selection (what a task scheduled from it starts with).
    pub model_instance_id: String,
    pub model: String,
    pub runtime_mode: String,
    pub interaction_mode: String,
    pub status: String,
    pub working: bool,
    pub loading: bool,
    pub has_earlier: bool,
    pub loading_earlier: bool,
    pub pinned: bool,
    pub settled: bool,
    pub archived: bool,
    pub items: Vec<ItemView>,
    pub pending_requests: Vec<RequestView>,
    /// Messages sent from here the server hasn't taken yet.
    pub pending_sends: Vec<PendingSendView>,
    /// When the turn under way started (Unix ms; 0 when none), for an
    /// elapsed time next to "Working".
    pub working_since_ms: i64,
    pub working_activity: Option<WorkingActivityView>,
    pub checkpoints: Vec<CheckpointView>,
    pub find: Option<FindView>,
    /// The thread this one was forked from, or whose subagent it is.
    pub parent: Option<ThreadLinkView>,
    /// A fork: its work can go back to the thread it came from (`MergeBack`).
    pub can_merge_back: bool,
    /// A finished turn to fork from (`ForkThread`).
    pub can_fork: bool,
    /// How full the agent's context window is (when the provider says).
    pub context: Option<ContextUsageView>,
    /// The thread's usage at a glance, for the header meter and its
    /// details: the context window and its provider's limits. `None` when
    /// neither is known.
    pub usage: Option<SessionUsageView>,
    /// Messages waiting for the turn under way, in the order they run.
    pub queued: Vec<QueuedMessageView>,
    /// A stop held the queue: nothing starts until `ResumeQueue` or a new message.
    pub queue_held: bool,
    /// The agent replied but work it started in the background goes on
    /// ("Waiting for 2 background agents"); empty when there is none.
    pub background: String,
    /// Provider-reported background work for the open thread, in provider order.
    pub background_tasks: Vec<BackgroundTaskView>,
    /// The last turn stopped on a usage limit: when it resets ("in 3h", or
    /// "now" once it has); empty when it isn't limited.
    pub limit_resets: String,
    /// What is set for the reset: `resume`, `snooze`, or empty.
    pub limit_plan: String,
    pub plan: Option<PlanProgressView>,
    pub tasks: Option<TaskProgressView>,
    /// Subagents and side threads of this one that wait on you (an approval
    /// or a question): a banner opens each.
    pub waiting_children: Vec<ThreadLinkView>,
    /// Number of distinct artifacts discovered or published in this thread.
    pub artifact_count: u32,
    /// Secrets the agent asked for (`request_secret`) that wait for an
    /// answer: `AnswerSecret` or `DeclineSecret`, by `item_id`.
    pub secret_requests: Vec<SecretRequestView>,
    /// The pull requests and issues linked to the thread (by an agent or by
    /// hand), pull requests first; each opens its `url` in the browser.
    pub links: Vec<WorkLinkView>,
}

/// A secret the agent asked for, as its card shows it. The value typed is
/// the shell's until it is sent: never part of the view or the model.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct SecretRequestView {
    /// The `secret_request` item's id (`secrets.answerRequest`'s `turnItemId`).
    pub item_id: String,
    /// What is asked for ("OpenAI API key").
    pub label: String,
    /// Why the agent needs it.
    pub reason: String,
    /// Where to get one, to open in the browser.
    pub url: Option<String>,
    /// The field's placeholder (empty: the shell's own).
    pub placeholder: String,
    /// `file`: the agent reads the value from an owner-only file; empty:
    /// the agent gets a reference to it, never the value.
    pub delivery: String,
    /// One line under the field on how the value is kept and handed over.
    pub hint: String,
    /// An answer is on its way: the buttons wait.
    pub sending: bool,
}

/// A pull request or issue linked to a thread.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkLinkView {
    /// `pull_request` | `issue`
    pub kind: String,
    /// As the host writes it: `#12`, `ENG-123`.
    pub label: String,
    /// The title when it was linked (or last synced); may be empty.
    pub title: String,
    pub url: String,
    /// A pull request's `open` | `draft` | `merged` | `closed`; empty when
    /// not known, and for issues (a link keeps no issue state).
    pub state: String,
}

/// The latest plan, with the Markdown blocks used by the transcript.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct PlanProgressView {
    pub plan_id: String,
    pub label: String,
    pub blocks: std::sync::Arc<Vec<MarkdownBlock>>,
    pub can_implement: bool,
}

/// How far the agent is through its to-do list.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct TaskProgressView {
    pub done: u32,
    pub total: u32,
    /// The step in progress (or the next one); empty when all are done.
    pub current: String,
    /// `Tasks 2/5 · Write the tests`.
    pub label: String,
    pub previous_run: bool,
    pub explanation: Option<String>,
    pub todos: std::sync::Arc<Vec<TodoView>>,
}

/// What the current run is doing and the step the reader can inspect.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkingActivityView {
    /// `starting` or the kind of a live transcript item.
    pub kind: Option<String>,
    pub step_item_id: Option<String>,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct BackgroundTaskView {
    pub id: String,
    pub kind: String,
    pub label: String,
    pub status: String,
    pub progress: Option<String>,
    pub child_thread_id: Option<String>,
}

/// Another thread this one comes from.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct ThreadLinkView {
    pub thread_id: String,
    pub title: String,
    /// `fork` | `subagent`
    pub relation: String,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct ContextUsageView {
    pub used_tokens: u64,
    /// 0 when the window size isn't known.
    pub max_tokens: u64,
    /// Of the window, 0–100 (0 when its size isn't known).
    pub percent: u32,
    /// `48k of 200k tokens` or `48k tokens`.
    pub label: String,
    /// `152k left` or `Full`; empty when the window size isn't known.
    pub remaining_label: String,
    /// Share of input tokens served from prompt cache, 0–100 (0 when not known).
    pub cache_hit_percent: u32,
    /// `78%`; empty when not known.
    pub cache_hit_label: String,
    /// Nearly full (80 % or more): the agent will compact soon.
    pub high: bool,
    /// All but full (90 % or more).
    pub critical: bool,
    /// Where the agent compacts on its own, 0–100 of the window (0: not known).
    pub compact_at_percent: u32,
    /// The counts behind `used_tokens` the provider reported: `Input 41k`,
    /// `Cached 30k`, `Output 6k`, `Reasoning 2k`, for a details popover.
    pub breakdown: Vec<StatLineView>,
    /// What the conversation has cost so far (`$1.24`); empty when not known.
    pub cost: String,
    /// The provider has `/compact`: `Compact` frees the window now.
    pub can_compact: bool,
}

/// A named figure in a list of them.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct StatLineView {
    pub label: String,
    pub value: String,
}

/// One usage window of a provider account (`Session`, `Weekly`).
#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct LimitWindowView {
    pub label: String,
    /// Used, 0–100.
    pub percent: u32,
    /// When it refills (`in 3h`); empty when not known.
    pub resets: String,
    /// 75 % or more: worth showing by the composer.
    pub high: bool,
    /// How far through the window the period is, 0–100, for a pace tick
    /// on its bar (0: not known).
    pub elapsed_percent: u32,
    /// Using it faster than time passes: `ahead`, `on` or `under` (within
    /// 5 points is `on`); empty when the window's length isn't known.
    pub pace: String,
    /// When it refills, as the provider said (RFC 3339 UTC), for a local
    /// clock time; empty when not known.
    pub resets_at: String,
}

/// The open thread's usage: how full its context window is and how much of
/// its provider account's limits is used.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct SessionUsageView {
    /// The context window (the same as `ThreadPageView.context`).
    pub context: Option<ContextUsageView>,
    /// The windows of the provider the thread runs on, fullest first.
    pub limits: Option<ProviderLimitsView>,
    /// The fullest of those windows, for the meter when there is no context.
    pub fullest: Option<LimitWindowView>,
    /// A dot on the meter: `caution` (a window at 75 % or more), `danger`
    /// (90 % or more), or empty.
    pub limit_alert: String,
    /// Why the limits can't be shown (`Could not read limits.`); empty when they can.
    pub limits_notice: String,
    /// When the limits were read (`5m`, `now`); empty when not known.
    pub limits_checked: String,
}

/// A provider's usage windows.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct ProviderLimitsView {
    pub instance_id: String,
    pub provider: String,
    pub windows: Vec<LimitWindowView>,
    /// Where the account's usage is shown on the web (empty: nowhere known).
    pub external_label: String,
    pub external_url: String,
    /// When the limits were last read (`now`, `5m`); empty when not known.
    pub checked: String,
    /// Why limits could not be read or are absent; empty when windows are present.
    pub notice: String,
}

/// A message queued behind the turn under way.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct QueuedMessageView {
    /// Its run (what `CancelQueued`, `EditQueued`, `SteerQueued`, `MoveQueued` take).
    pub run_id: String,
    pub text: String,
    /// It can join the turn under way now (`SteerQueued`).
    pub can_steer: bool,
    pub first: bool,
    pub last: bool,
}

/// A message on its way (in the outbox).
#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct PendingSendView {
    /// Its command id (`CancelSend` takes it).
    pub id: String,
    pub text: String,
    /// `sending` | `waiting` (no connection; goes once it's back)
    pub state: String,
}

// The heavy parts (diff, markdown, to-dos, output) are shared behind `Arc`:
// a row from the render cache is copied without copying them. `Arc`
// serializes as what it holds, so shells see the same types and bytes.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct ItemView {
    pub issue_card: Option<IssueCardView>,
    pub id: String,
    /// `user` | `assistant` | `reasoning` | `command` | `file_change` | `tool` |
    /// `approval` | `question` | `plan` | `checkpoint` | `error` | `notice`
    /// (a `question`'s title is its questions, its text the answers given)
    pub kind: String,
    /// `running` | `waiting` | `completed` | `failed` | `cancelled` | `interrupted` | ...
    pub status: String,
    pub title: String,
    /// Plain text (user messages, reasoning, tool input).
    pub text: String,
    /// Markdown blocks (assistant messages, plans).
    pub blocks: std::sync::Arc<Vec<MarkdownBlock>>,
    pub output: Option<std::sync::Arc<str>>,
    pub exit_code: Option<i64>,
    pub diff: Option<std::sync::Arc<DiffFileView>>,
    pub attachments: Vec<String>,
    /// A message's images whose URLs have come (full URLs to load).
    pub images: Vec<ImageView>,
    pub checkpoint: Option<CheckpointView>,
    /// An error that ended the thread's last turn: its message can be sent again.
    pub can_retry: bool,
    pub streaming: bool,
    /// Whether the shell should start this row collapsed.
    pub collapsed: bool,
    /// Matches of the active find query in this row.
    pub find_matches: u32,
    pub find_active: bool,
    /// `output` (or a tool's `text`) shows only its end; `ExpandItem` shows all.
    pub truncated: bool,
    /// Consecutive tool steps of one turn share a group id (empty: none);
    /// the shell folds them under one summary row.
    pub group: String,
    /// On a group's first row: "Ran 3 commands · edited 2 files".
    pub group_summary: String,
    /// A to-do list the agent keeps (Claude's `TodoWrite`), kind `todo`.
    pub todos: std::sync::Arc<Vec<TodoView>>,
    /// On the last row of a finished turn: when it started (Unix ms) and how
    /// long it took (ms); 0 elsewhere.
    pub run_started_ms: i64,
    pub run_duration_ms: i64,
    /// A `subagent` row: the child thread with its work (empty: none yet).
    pub child_thread_id: String,
    /// A running `subagent` that `StopSubagent` (with `id`) can stop.
    pub can_stop: bool,
    /// What a running subagent is doing now ("Reading app.rs", "Running in the background").
    pub progress: String,
    /// A `plan` row the agent proposed: `ImplementPlan` (with `plan_id`) carries it out.
    pub plan_id: String,
    pub can_implement: bool,
    pub artifact_paths: Vec<String>,
    /// A `plan` row a later plan replaced (shown dimmed).
    pub superseded: bool,
    /// The last reply of a finished turn: `ForkFromItem` forks from here.
    pub can_fork_here: bool,
    /// A step's verb in the tense of its status: "Reading", "Read", "Ran".
    /// The shell shows it before `title` (what the step worked on).
    pub verb: String,
    /// What kind of step this is, for its icon: `read` | `image` | `search` |
    /// `list` | `command` | `edit` | `web` | `fetch` | `mcp` | `skill` |
    /// `agent` | `question` | `wait` | `tool` (empty: not a step).
    pub icon: String,
    /// The title is code (a command, a path, a pattern): set it monospaced.
    pub mono: bool,
    /// What else the step's line says: "lines 1–60", "in src · 14 matches".
    pub subtitle: String,
    /// A short mark at the line's end: "exit 101", "No matches", "1m 12s".
    pub badge: String,
    /// `failed` (shown as an error) | `neutral` | "" (plain).
    pub badge_tone: String,
    /// Opening the step shows more (output, a file, a diff, links, fields,
    /// images); when false it is one plain line with no disclosure.
    pub expandable: bool,
    /// A tool's arguments by name, shown as a two-column list when opened.
    pub fields: std::sync::Arc<Vec<FieldView>>,
    /// A web search's results.
    pub links: std::sync::Arc<Vec<LinkView>>,
    /// On a group's first row: the images its steps viewed (at most six), shown
    /// even while the group is folded.
    pub group_images: Vec<ImageView>,
    /// Populated when this row represents or created an artifact (MCP or Claude `Artifact`).
    pub artifact_id: Option<String>,
    pub artifact_title: String,
    pub artifact_kind: String,
    pub artifact_version: u32,
    pub artifact_hosted_url: Option<String>,
}

/// One argument of a tool call.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct FieldView {
    pub label: String,
    pub value: String,
    /// The value is code or a path: set it monospaced.
    pub mono: bool,
}

/// One result of a web search.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct LinkView {
    pub title: String,
    pub url: String,
    /// The site, as shown beside the title ("docs.rs").
    pub host: String,
    pub snippet: String,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct ImageView {
    pub name: String,
    pub url: String,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct TodoView {
    pub text: String,
    /// `pending` | `in_progress` | `completed`
    pub status: String,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct MarkdownBlock {
    /// `paragraph` | `heading` | `code` | `list_item` | `quote` | `rule` | `table_row`
    pub kind: String,
    /// Inline markdown (paragraphs, headings, list items, quotes) or raw code.
    pub text: String,
    pub level: u32,
    pub language: String,
    pub ordered: bool,
    pub number: u32,
    /// Table cells (inline markdown) for `table_row`.
    pub cells: Vec<String>,
    pub header: bool,
    pub runs: Vec<MarkdownRun>,
    pub cell_runs: Vec<Vec<MarkdownRun>>,
    pub alignments: Vec<String>,
    pub task_checked: Option<bool>,
    pub continuation: bool,
    pub quote_depth: u32,
    pub fence_closed: bool,
    /// A finished code block's highlighting (empty: plain).
    pub spans: Vec<CodeSpan>,
    /// Populated when this block is a previewable inline artifact (HTML/SVG/React/Mermaid).
    pub artifact_id: Option<String>,
    pub artifact_title: String,
    pub artifact_kind: String,
    pub artifact_version: u32,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct MarkdownRun {
    pub text: String,
    pub bold: bool,
    pub italic: bool,
    pub strike: bool,
    pub code: bool,
    pub link: Option<String>,
    pub workspace_link: Option<String>,
}

/// A piece of highlighted code: `plain` | `keyword` | `string` | `comment` |
/// `number` | `type` | `function` | `added` | `removed`.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct CodeSpan {
    pub text: String,
    pub kind: String,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct RequestView {
    pub id: String,
    /// `command` | `file-change` | `file-read` | `permission` | ...
    pub kind: String,
    pub prompt: String,
    pub detail: Option<String>,
    /// A `user_input` request's questions (empty: answered by a message).
    pub questions: Vec<QuestionView>,
    /// The change an edit approval would make, when the provider sent it.
    pub files: Vec<DiffFileView>,
    /// The button that allows more than this call: "Allow for Thread" where
    /// the server keeps the grant for the thread's later sessions (Claude,
    /// Antigravity), else "Allow for Session".
    pub session_label: String,
}

/// A question the agent asks, answered with its options.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct QuestionView {
    /// What the answer is keyed by.
    pub id: String,
    pub header: String,
    pub question: String,
    pub multi_select: bool,
    pub allow_custom_answer: bool,
    pub required: bool,
    pub options: Vec<QuestionOptionView>,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct QuestionOptionView {
    pub label: String,
    pub value: String,
    pub description: String,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct CheckpointView {
    pub id: String,
    pub scope_id: String,
    /// The turn count this checkpoint closes (diffs are requested by turn count).
    pub turn_count: u32,
    pub label: String,
    pub files_changed: u32,
    pub additions: u32,
    pub deletions: u32,
    pub can_revert: bool,
    /// The files the turn changed, for a card under it.
    pub files: Vec<ChangedFileView>,
    /// What `SelectDiffSource` takes to show this turn's diff (`turn:3`).
    pub diff_source: String,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct FindView {
    pub query: String,
    pub total: u32,
    /// 1-based index of the active match (0 when none).
    pub active: u32,
    pub active_item_id: Option<String>,
}

// ---- composer ----

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct ComposerView {
    pub issue_chip: Option<String>,
    pub issue_available: bool,
    pub issue_picker: Option<String>,
    pub issue_assign_to_me: bool,
    pub workspace_required: bool,
    pub issue_handoff: u32,
    pub draft: String,
    /// Bumped whenever the core replaces the draft (the shell then reloads it).
    pub draft_revision: u32,
    pub placeholder: String,
    pub attachments: Vec<AttachmentView>,
    pub model_groups: Vec<ModelGroupView>,
    /// The models marked favorite, across providers, shown before the groups
    /// (filtered by `model_query` like the groups).
    pub favorite_models: Vec<ModelRowView>,
    /// What the model picker's search field holds.
    pub model_query: String,
    /// Models the picker can show (not hidden), to decide whether it offers
    /// search.
    pub model_count: u32,
    /// The groups list the models hidden from the picker too.
    pub show_hidden_models: bool,
    pub selected_model: Option<ModelOption>,
    pub efforts: Vec<ChoiceView>,
    pub selected_effort: Option<String>,
    /// What the effort control is called: "Effort", or the model's own name
    /// for it ("Thinking Level").
    pub effort_label: String,
    /// The model's other options: fast mode, context window, agent…
    pub traits: Vec<TraitView>,
    pub runtime_modes: Vec<ChoiceView>,
    pub runtime_mode: String,
    pub interaction_mode: String,
    /// New threads only: `local` or `worktree`.
    pub workspace_mode: String,
    pub base_branch: Option<String>,
    pub branches: Vec<String>,
    pub is_new_thread: bool,
    pub mention: Option<MentionView>,
    /// A new thread goes into this pull request's worktree ("PR #12 · fix");
    /// empty otherwise.
    pub pr_worktree: String,
    /// Commands matching a draft that is `/` and a word so far (Claude's
    /// commands and skills); empty when the draft isn't one.
    pub slash_commands: Vec<SlashCommandView>,
    pub can_send: bool,
    pub can_stop: bool,
    /// A turn is under way that can take the message now (`Steer`), rather
    /// than after it (`Send` queues it).
    pub can_steer: bool,
    /// The selected provider has plan mode (OpenCode doesn't).
    pub shows_interaction_mode: bool,
    pub sending: bool,
    /// Why the selected provider can't run on the server (not installed, not
    /// signed in); sending is off while set.
    pub blocked_reason: String,
    /// A line under the composer about sending now (not connected, starting
    /// the thread); empty when there's nothing to say.
    pub send_hint: String,
    /// Not connected, but what's sent waits in the outbox and goes once the
    /// connection is back (the connection banner says so).
    pub queues_offline: bool,
    /// The selected provider's usage windows (account limits).
    pub limits: Vec<LimitWindowView>,
    /// Skills matching a `$word` being typed at the end of the draft (picked
    /// with `PickSkill`); empty when none is being typed.
    pub skills: Vec<SlashCommandView>,
    /// Drafts put aside (`StashDraft`), newest first, as their first line.
    pub stash: Vec<String>,
    /// Stop was asked a while ago and the turn goes on: `ForceStop` ends it.
    pub can_force_stop: bool,
    /// Bumped when `/model` asks the shell to open the model picker.
    pub model_picker_requested: u32,
    /// A short chip by the model picker when a window is nearly used up
    /// (`Weekly 92% · resets in 2d`); empty otherwise.
    pub limits_chip: String,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct AttachmentView {
    pub id: String,
    pub name: String,
    pub size_label: String,
    pub uploading: bool,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq, Hash)]
pub struct ModelOption {
    pub instance_id: String,
    pub model: String,
    pub label: String,
}

/// A model in the picker.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct ModelRowView {
    pub option: ModelOption,
    /// The model's own name (without the provider).
    pub name: String,
    /// A second line: OpenCode's upstream provider (`openrouter`) in a
    /// group; the provider (and upstream provider) under Favorites.
    pub detail: String,
    pub favorite: bool,
    pub hidden: bool,
    /// The model selected now (also when selected by one of its aliases).
    pub chosen: bool,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct ModelGroupView {
    pub instance_id: String,
    pub provider: String,
    pub status: String,
    /// Why a provider that isn't ready can't be used (sign in…), else empty.
    pub message: String,
    /// Not favorites (those are in `favorite_models`); hidden ones only with
    /// `show_hidden_models` or a search (and the selected one always).
    pub models: Vec<ModelRowView>,
    /// The provider's hidden models (for "Show N hidden").
    pub hidden_count: u32,
}

/// An option of the selected model besides its effort (`SetModelOption`).
#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct TraitView {
    pub id: String,
    pub label: String,
    pub description: String,
    /// `switch` (on or off) or `choice` (one of `choices`).
    pub kind: String,
    /// A switch that is on.
    pub on: bool,
    /// A choice's id as set (or the default).
    pub value: String,
    pub choices: Vec<ChoiceView>,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct ChoiceView {
    pub value: String,
    pub label: String,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct SlashCommandView {
    pub name: String,
    pub description: String,
    /// `builtin` (the app's own: `/model`, `/plan`, `/fork`, `/clear`),
    /// `command` (the provider's) or `skill` (`$name`).
    pub kind: String,
    /// What the command takes after its name (`[model]`); empty for none.
    pub hint: String,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct MentionView {
    pub query: String,
    pub suggestions: Vec<FileEntryView>,
    pub highlighted: u32,
}

// ---- right panel ----

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct PanelView {
    pub open: bool,
    /// `diff` | `terminal` | `files` | `git` | `artifacts`
    pub tab: String,
    pub diff: DiffPanelView,
    pub terminal: TerminalPanelView,
    pub files: FilesPanelView,
    pub git: GitPanelView,
    pub artifacts: ArtifactsPanelView,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct ArtifactsPanelView {
    pub items: Vec<ArtifactSummaryView>,
    pub active: Option<ActiveArtifactView>,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct ArtifactSummaryView {
    pub id: String,
    pub title: String,
    /// `html` | `svg` | `react` | `mermaid` | `markdown` | `code` | `hosted`
    pub kind: String,
    pub language: String,
    pub latest_version: u32,
    pub version_count: u32,
    pub item_id: String,
    pub path: Option<String>,
    pub hosted_url: Option<String>,
    pub streaming: bool,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct ActiveArtifactView {
    pub id: String,
    pub title: String,
    pub kind: String,
    pub language: String,
    pub version: u32,
    pub versions: Vec<ArtifactVersionView>,
    /// `preview` | `code`
    pub mode: String,
    pub source: String,
    pub spans: Vec<CodeSpan>,
    /// Signed server URL for multi-file bundles, React runtime previews, or vector Mermaid SVGs.
    pub preview_url: Option<String>,
    pub hosted_url: Option<String>,
    pub path: Option<String>,
    pub item_id: String,
    pub loading: bool,
    pub truncated: bool,
    pub streaming: bool,
    pub error: Option<String>,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct ArtifactVersionView {
    pub version: u32,
    pub summary: String,
    pub item_id: String,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct DiffPanelView {
    pub sources: Vec<ChoiceView>,
    pub selected_source: String,
    pub loading: bool,
    pub error: Option<String>,
    pub files: Vec<DiffFileView>,
    pub truncated: bool,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct DiffFileView {
    pub path: String,
    pub old_path: Option<String>,
    /// `modified` | `added` | `deleted` | `renamed` | `binary`
    pub change: String,
    pub additions: u32,
    pub deletions: u32,
    pub hunks: Vec<DiffHunkView>,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct DiffHunkView {
    pub header: String,
    pub lines: Vec<DiffLineView>,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct DiffLineView {
    /// `add` | `del` | `ctx` | `note`
    pub kind: String,
    pub old_line: Option<u32>,
    pub new_line: Option<u32>,
    pub text: String,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct TerminalPanelView {
    #[serde(default)]
    pub can_rename: bool,
    #[serde(default)]
    pub split: Option<TerminalSplitView>,
    pub tabs: Vec<TerminalTabView>,
    pub active: Option<TerminalScreenView>,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct TerminalSplitView {
    pub stacked: bool,
    pub percent: u32,
    pub left: TerminalScreenView,
    pub right: TerminalScreenView,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TerminalDetailView {
    pub thread_id: String,
    pub terminal_id: String,
    pub instance_id: String,
    pub cwd: String,
    pub reported_cwd: Option<String>,
    pub label: String,
    pub title: Option<String>,
    pub running: bool,
    #[serde(default)]
    pub exit_code: Option<i32>,
    pub restored: bool,
    pub has_saved_screen: bool,
    pub agent: Option<TerminalAgentView>,
    #[serde(default)]
    pub agent_thread_id: Option<String>,
    pub updated_at: String,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct TerminalAgentView {
    pub agent: String,
    pub activity: String,
    pub revision: u64,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct TerminalTabView {
    pub id: String,
    pub label: String,
    pub running: bool,
    pub active: bool,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct TerminalScreenView {
    pub id: String,
    #[serde(default)]
    pub watching: bool,
    #[serde(default)]
    pub restored: bool,
    #[serde(default)]
    pub has_saved_screen: bool,
    pub rows: u32,
    pub cols: u32,
    pub lines: Vec<TerminalLineView>,
    pub cursor_row: u32,
    pub cursor_col: u32,
    pub cursor_visible: bool,
    pub exited: bool,
    pub status: String,
    /// Rows scrolled back into the history (0: the live screen).
    pub scrollback_offset: u32,
    /// How far back the history goes.
    pub scrollback_max: u32,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct TerminalLineView {
    pub spans: Vec<TerminalSpanView>,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct TerminalSpanView {
    pub text: String,
    /// -1 default, 0..=255 palette index, or 0x1000000 | 0xRRGGBB.
    pub fg: i32,
    pub bg: i32,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub inverse: bool,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct FilesPanelView {
    pub root: String,
    pub entries: Vec<FileEntryView>,
    pub loading: bool,
    pub query: String,
    pub results: Vec<FileEntryView>,
    pub preview: Option<FilePreviewView>,
    pub artifact_preview: Option<FilePreviewView>,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct FileEntryView {
    pub path: String,
    pub name: String,
    pub depth: u32,
    pub directory: bool,
    pub expanded: bool,
    pub ignored: bool,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct FilePreviewView {
    pub path: String,
    pub text: String,
    pub image_url: Option<String>,
    /// The text highlighted for its file type (empty: plain).
    pub spans: Vec<CodeSpan>,
    pub truncated: bool,
    pub error: Option<String>,
    pub loading: bool,
    /// Text that can be edited and saved back (`SaveFile`): whole, not an
    /// image, not kept in Git LFS.
    pub editable: bool,
    /// The file's size ("2.6 KB"), once read.
    pub size_label: Option<String>,
    /// The file is kept in Git LFS.
    pub lfs: Option<LfsView>,
}

/// A file kept in Git LFS, as its preview's card shows it.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct LfsView {
    /// The object's SHA-256.
    pub oid: String,
    /// Its first 10 characters.
    pub oid_short: String,
    pub size_label: String,
    /// `cached`, `missing`, `too-large`, `mismatch` or `unsupported`.
    pub state: String,
    /// "Stored locally", "Not downloaded", …
    pub state_label: String,
    /// What it means for the preview, or why fetching failed.
    pub message: Option<String>,
    /// Fetch is offered: the server doesn't have the object.
    pub can_fetch: bool,
    /// A fetch is under way.
    pub fetching: bool,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct GitPanelView {
    /// Status hasn't arrived yet (don't say "not a repository").
    pub loading: bool,
    pub is_repo: bool,
    pub branch: Option<String>,
    pub has_upstream: bool,
    pub ahead: u32,
    pub behind: u32,
    pub changed_files: Vec<ChangedFileView>,
    pub insertions: u32,
    pub deletions: u32,
    pub pull_request: Option<PullRequestRowView>,
    pub branches: Vec<String>,
    pub busy: bool,
    pub progress: Vec<String>,
    pub error: Option<String>,
    pub has_remote: bool,
    /// The repository's worktrees (with a branch checked out in each).
    pub worktrees: Vec<WorktreeView>,
    /// The project's scripts (`RunScript`).
    pub scripts: Vec<ScriptView>,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct WorktreeView {
    pub path: String,
    pub branch: String,
    /// The open thread works here.
    pub current: bool,
    /// Threads that work in it, by title.
    pub threads: Vec<String>,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct ScriptView {
    pub id: String,
    pub name: String,
    pub command: String,
    /// `play` | `test` | `lint` | `configure` | `build` | `debug` | …
    pub icon: String,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct ChangedFileView {
    pub path: String,
    pub insertions: u32,
    pub deletions: u32,
}

// ---- palette ----

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct PaletteView {
    pub query: String,
    pub items: Vec<PaletteItemView>,
    pub highlighted: u32,
    /// `commands` | `files` | `content` (`SetPaletteMode`).
    pub mode: String,
    /// What the search field says when it is empty.
    pub placeholder: String,
    /// A file search is under way.
    pub searching: bool,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct PaletteItemView {
    pub id: String,
    /// `thread` | `project` | `action` | `file` | `match`
    pub kind: String,
    pub title: String,
    pub subtitle: String,
    pub shortcut: String,
}

// ---- settings ----

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct SettingsView {
    pub linear_settings: Option<LinearSettingsView>,
    pub groups: Vec<SettingGroupView>,
    pub providers: Vec<ProviderView>,
    /// Another Antigravity instance can be added (with an API key or Vertex).
    pub can_add_antigravity: bool,
    pub keybindings: Vec<KeybindingView>,
    pub scheduled_tasks: Vec<ScheduledTaskView>,
    /// The threads a scheduled task can post into, newest first, from every
    /// project (the editor shows its project's): every live thread, whatever
    /// the thread list's layout, and any thread a task is bound to. Empty
    /// until the scheduled tasks are loaded (`LoadSettings("scheduled")`).
    pub scheduled_thread_choices: Vec<ScheduledThreadChoiceView>,
    pub scheduled_time_zone: String,
    /// The webhook task whose deliveries are open, and the deliveries.
    pub webhook_deliveries_task: String,
    pub webhook_deliveries: Vec<WebhookDeliveryView>,
    /// The task whose runs are open (`LoadScheduledRuns`), and its runs, newest first.
    pub scheduled_runs_task: String,
    pub scheduled_runs: Vec<ScheduledRunView>,
    pub archived: Vec<ThreadRowView>,
    pub storage: StorageView,
    pub diagnostics: DiagnosticsView,
    pub access: Vec<AccessEntryView>,
    pub projects: Vec<ProjectSettingsView>,
    pub project_icon_files: ProjectIconFilesView,
    pub remote: RemoteAccessView,
    pub failover_pools: Vec<FailoverPoolView>,
    pub failover_instances: Vec<FailoverInstanceView>,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct FailoverPoolView {
    pub id: String,
    pub name: String,
    pub instance_ids: Vec<String>,
    pub same_driver_first: bool,
    pub allow_cross_provider: bool,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct FailoverInstanceView {
    pub instance_id: String,
    pub driver: String,
    pub display_name: String,
    pub available: bool,
    pub limited_until: Option<String>,
    pub last_error: Option<String>,
    pub active_jobs: u32,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct SettingGroupView {
    pub title: String,
    pub fields: Vec<SettingFieldView>,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct SettingFieldView {
    pub key: String,
    pub label: String,
    pub help: String,
    /// `bool` | `choice` | `text`
    pub kind: String,
    pub bool_value: bool,
    pub text_value: String,
    pub choices: Vec<ChoiceView>,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct ProviderView {
    pub instance_id: String,
    pub name: String,
    pub driver: String,
    pub status: String,
    pub version: String,
    pub auth: String,
    pub message: String,
    pub model_count: u32,
    pub enabled: bool,
    /// The account's usage limits as last reported ("Session 45% · Weekly
    /// 97%, resets in 2d"); empty when the provider has said none.
    pub limits: String,
    /// The server can sign this instance in (`StartProviderSignIn`).
    pub can_authenticate: bool,
    /// The server can install its runtime (`InstallProvider`).
    pub can_install: bool,
    pub signed_in: bool,
    pub sign_in: ProviderSignInView,
    pub install: ProviderInstallView,
    /// Its settings, for an editor (`SaveProviderInstance`).
    pub config: ProviderConfigView,
}

/// A provider's sign-in under way (or as it ended).
#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct ProviderSignInView {
    /// `idle` | `starting` | `waiting` | `verifying` | `succeeded` | `failed` | `cancelled`.
    pub phase: String,
    pub message: String,
    /// The page to sign in on, while it waits.
    pub url: String,
    /// The redirect URL from a browser on another device can be pasted
    /// (`CompleteProviderSignIn`).
    pub takes_redirect: bool,
    pub busy: bool,
}

/// A provider's managed runtime.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct ProviderInstallView {
    /// `idle` | `downloading` | `extracting` | `verifying` | `succeeded` | `failed` | `cancelled`.
    pub phase: String,
    pub message: String,
    pub busy: bool,
    /// How far a download is, in thousandths; 0 when unknown.
    pub progress: u32,
    /// "12 MB of 120 MB".
    pub progress_label: String,
    /// The release Domovoi installed (empty: none of its own).
    pub installed_version: String,
    /// The release offered.
    pub offered_version: String,
    pub update_available: bool,
    pub can_remove: bool,
}

/// A provider instance's settings as its editor shows them.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct ProviderConfigView {
    /// Domovoi has an editor for this driver's settings.
    pub editable: bool,
    /// The driver's built-in instance (it can't be removed).
    pub built_in: bool,
    pub display_name: String,
    pub auth_method: String,
    /// The sign-in methods this instance can use.
    pub auth_methods: Vec<ChoiceView>,
    /// A key is saved (it is never shown).
    pub has_api_key: bool,
    pub gcp_project: String,
    pub gcp_location: String,
    pub binary_path: String,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct KeybindingView {
    pub command: String,
    pub shortcut: String,
    /// The context the shortcut applies in (`when` on the wire).
    pub condition: String,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct ScheduledTaskView {
    pub id: String,
    pub name: String,
    pub prompt: String,
    pub schedule: String,
    pub enabled: bool,
    pub next_run: String,
    pub last_run: String,
    pub project_title: String,
    /// What the editor opens with (see `ScheduledTaskEdit`).
    pub project_id: String,
    pub schedule_kind: String,
    pub interval_minutes: u32,
    pub time_of_day: String,
    pub weekdays: Vec<u8>,
    pub run_at: String,
    pub thread_mode: String,
    pub thread_id: String,
    /// The bound thread's title; empty when the task has no thread yet.
    pub thread_title: String,
    /// What its latest run is doing when that is still going ("Waiting in
    /// its thread", "Running", "Needs you" on an approval); empty otherwise.
    pub run_state: String,
    /// The thread its latest run went to (a new-thread task's last one).
    pub last_thread_id: String,
    pub workspace: String,
    pub base_branch: String,
    pub model_instance: String,
    pub model: String,
    pub runtime_mode: String,
    pub interaction_mode: String,
    /// Why the last run failed; empty otherwise.
    pub last_error: String,
    /// A webhook task's URL (or its path, when the server has no public address).
    pub webhook_url: String,
}

/// A scheduled task's run, as its history lists it.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct ScheduledRunView {
    /// When it started: how long ago, and the time.
    pub started: String,
    /// "On schedule", "Run now" or "Webhook".
    pub trigger: String,
    /// "Waiting in its thread", "Running", "Needs you", "Finished", "Stopped",
    /// "Failed", "Skipped" or "Sent".
    pub status: String,
    pub failed: bool,
    /// Why it failed; empty otherwise.
    pub error: String,
    pub thread_id: String,
    pub thread_title: String,
}

/// A thread a scheduled task can be bound to.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct ScheduledThreadChoiceView {
    pub id: String,
    pub project_id: String,
    pub title: String,
    /// What it is doing (when that says more than its age) and when it was
    /// last active, or "archived" for an archived bound thread.
    pub detail: String,
    /// The model its runs use (a bound task's runs follow the thread's).
    pub model_label: String,
    /// Its permissions (`approval-required` | `auto-accept-edits` |
    /// `full-access`), which a bound task's runs follow too.
    pub runtime_mode: String,
}

/// A webhook delivery as the task's log shows it.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct WebhookDeliveryView {
    pub id: String,
    pub received: String,
    pub method: String,
    pub outcome: String,
    pub detail: String,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct StorageView {
    pub database_size: String,
    pub reclaimable: String,
    pub last_cleanup: String,
    pub busy: bool,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct DiagnosticsView {
    pub cpu: String,
    pub memory: String,
    pub processes: Vec<ProcessView>,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct ProcessView {
    pub pid: u32,
    pub command: String,
    pub cpu: String,
    pub memory: String,
    pub depth: u32,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct AccessEntryView {
    pub id: String,
    pub kind: String,
    pub label: String,
    pub detail: String,
    pub current: bool,
    /// `desktop` | `mobile` | `tablet` | `unknown` (sessions only)
    pub device_type: String,
    pub connected: bool,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct ProjectSettingsView {
    pub linear_scope: Option<String>,
    pub id: String,
    pub title: String,
    pub workspace_root: String,
    pub thread_count: u32,
    /// The "No project" folder, where threads without a project go.
    pub is_scratch: bool,
    pub icon: ProjectIconView,
    pub automatic_icon: ProjectIconView,
    pub index: ProjectIndexView,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct ProjectIndexView {
    /// `idle` | `indexing` | `ready` | `failed`
    pub status: String,
    pub commit_sha: String,
    pub indexed_at: String,
    pub file_count: u32,
    pub package_count: u32,
    pub edge_count: u32,
    pub symbol_count: u32,
    pub convention_paths: Vec<String>,
    pub error: Option<String>,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct ProjectIconFilesView {
    pub project_id: String,
    pub revision: String,
    pub loading: bool,
    pub error: Option<String>,
    pub truncated: bool,
    pub files: Vec<ProjectIconFileView>,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct ProjectIconFileView {
    pub path: String,
    pub url: Option<String>,
    pub unavailable: bool,
    pub refresh: bool,
}

// ---- pull requests / usage ----

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct PullRequestsView {
    pub loading: bool,
    pub error: Option<String>,
    pub notice: Option<String>,
    pub items: Vec<PullRequestRowView>,
    pub selected: Option<PullRequestDetailView>,
    /// An action on the selected pull request, or its checkout, is under way.
    pub busy: bool,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct PullRequestRowView {
    pub key: String,
    pub repository: String,
    pub number: u32,
    pub title: String,
    pub author: String,
    pub state: String,
    pub draft: bool,
    pub url: String,
    pub head: String,
    pub base: String,
    pub age: String,
    pub checks: String,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct PullRequestDetailView {
    pub key: String,
    pub title: String,
    pub body: Vec<MarkdownBlock>,
    pub url: String,
    pub state: String,
    pub additions: u32,
    pub deletions: u32,
    pub changed_files: u32,
    pub checks: Vec<CheckView>,
    pub loading: bool,
    pub draft: bool,
    pub number: u32,
    /// The branch it merges into.
    pub base: String,
    /// The branch it comes from.
    pub head: String,
    pub ai_review: Option<PrReviewView>,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct PrReviewRiskDriverView {
    pub points: i32,
    pub label: String,
    pub detail: String,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct PrReviewFindingView {
    pub id: String,
    /// `finding` | `suggestion` | `question`
    pub kind: String,
    /// `high` | `medium` | `low`
    pub severity: String,
    pub title: String,
    pub detail: String,
    pub action: String,
    pub path: String,
    pub line: u32,
    pub side: String,
    pub evidence_quotes: Vec<String>,
    pub selected: bool,
    pub source_stage: String,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct PrReviewView {
    pub run_id: String,
    /// `idle` | `queued` | `running` | `validated` | `failed` | `cancelled` | `published` | `superseded`
    pub status: String,
    pub phase: String,
    pub phase_message: Option<String>,
    pub scope_mode: String,
    pub head_sha: String,
    pub is_stale: bool,
    pub risk_score: u32,
    pub risk_drivers: Vec<PrReviewRiskDriverView>,
    pub summary: String,
    pub findings: Vec<PrReviewFindingView>,
    pub selected_count: u32,
    pub published_review_id: Option<String>,
    pub error: Option<String>,
    pub can_publish: bool,
    pub busy: bool,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct CheckView {
    pub name: String,
    pub status: String,
    pub url: String,
}

/// One slice of the token composition bar (`input`, `cached`, `cache_write`, `output`, `reasoning`).
#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct UsageSliceView {
    pub kind: String,
    pub label: String,
    pub tokens: u64,
    pub tokens_label: String,
    pub percent: u32,
}

/// One provider's share of usage in the selected window, for filter chips and summary bars.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct UsageProviderSummaryView {
    /// Raw provider key (`claude`, `codex`, `pi`, `antigravity`, …).
    pub key: String,
    /// Friendly display name (`Claude`, `Codex`, `Pi`, `Antigravity`, …).
    pub label: String,
    pub tokens: u64,
    pub tokens_label: String,
    pub share_percent: u32,
    pub cost: String,
}

/// One provider's contribution inside a chart bucket.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct UsageProviderPointView {
    pub provider_key: String,
    pub provider: String,
    pub tokens: u64,
}

/// One chronological slot of the usage trend chart (an hour for Today, a day otherwise).
#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct UsageSeriesPointView {
    /// Stable slot key (`YYYY-MM-DD` or `HH:00`).
    pub key: String,
    /// Short axis label (`Nov 22`, `14:00`).
    pub label: String,
    /// Readable label for callouts and accessibility (`Nov 22, 2026`, `14:00–15:00`).
    pub detail_label: String,
    pub total_tokens: u64,
    pub total_label: String,
    pub cost: String,
    pub by_provider: Vec<UsageProviderPointView>,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct UsageView {
    pub loading: bool,
    pub error: Option<String>,
    pub range_label: String,
    /// The span asked for (`LoadUsage`), in days: 1, 7, 30 or 90.
    pub days: u32,
    /// True when the series and rows are bucketed by hour (Today).
    pub hourly: bool,
    /// Compact total (`14.8M`).
    pub total_tokens: String,
    /// Exact comma-grouped total (`14,823,901`), for tooltips and accessibility.
    pub total_tokens_exact: String,
    /// What the span cost (`$12.40`); empty when the server has no prices.
    pub total_cost: String,
    /// Cache read share of total input tokens, 0–100.
    pub cache_hit_percent: u32,
    /// `74%`; empty when there are no input tokens.
    pub cache_hit_label: String,
    /// Average tokens per active day (`2.1M/day`) or active hour (`620k/hr`).
    pub daily_avg_tokens: String,
    pub active_days: u32,
    pub sessions: u64,
    pub turns: u64,
    /// Non-empty token categories for the stacked mix bar.
    pub token_mix: Vec<UsageSliceView>,
    /// Active provider filter key, or empty for all providers.
    pub selected_provider: String,
    /// Providers present in the loaded window (always unfiltered, so chips stay stable).
    pub providers: Vec<UsageProviderSummaryView>,
    /// Chronological, zero-filled series points for the activity bar chart.
    pub series: Vec<UsageSeriesPointView>,
    /// Activity rows sorted newest-first.
    pub rows: Vec<UsageRowView>,
    /// The span's totals by model, most tokens first.
    pub by_model: Vec<UsageRowView>,
    /// Every provider's account windows, as last reported.
    pub limits: Vec<ProviderLimitsView>,
    pub limits_refreshing: bool,
    /// Non-empty scan warnings from partial/failed provider log sources.
    pub source_notices: Vec<String>,
    pub notice: String,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct UsageRowView {
    pub day: String,
    /// Short hour label (`14:00`) for hourly rows; empty for daily or by-model rows.
    pub hour_label: String,
    /// Raw provider key (`claude`, `codex`, `pi`, `antigravity`, …).
    pub provider_key: String,
    /// Friendly provider display name (`Claude`, `Codex`, …).
    pub provider: String,
    pub model: String,
    pub input: String,
    pub cached: String,
    pub cache_write: String,
    pub output: String,
    pub reasoning: String,
    pub total: String,
    pub total_tokens: u64,
    /// Share of the filtered total, 0–100.
    pub share_percent: u32,
    /// Cache hit share of this row's input tokens, 0–100.
    pub cache_hit_percent: u32,
    pub sessions: u64,
    /// `$0.42`; empty when the server has no prices.
    pub cost: String,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct IssuesView {
    pub issue_project: Option<String>,
    pub issue_filter: String,
    pub issue_rows: Vec<IssueRowView>,
    pub issue_detail: Option<IssueDetailView>,
    pub issues_loading: bool,
    pub issues_notice: Option<String>,
    pub more_issues: bool,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct IssueRowView {
    pub issue_index: u32,
    pub issue_label: String,
    pub issue_url: String,
    pub linked_thread_id: Option<String>,
    pub linked_thread_title: Option<String>,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct IssueDetailView {
    pub issue_body: String,
    pub issue_comments: Vec<String>,
    pub closing_pull_request_url: Option<String>,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct IssueCardView {
    pub issue_label: String,
    pub issue_url: String,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct PullRequestSheetView {
    pub pr_sheet_title: String,
    pub pr_sheet_body: String,
    pub pr_sheet_draft: bool,
    pub pr_sheet_supports_draft: bool,
    pub pr_sheet_base: String,
    pub pr_sheet_bases: Vec<String>,
    pub pr_sheet_files: Vec<PullRequestFileView>,
    pub pr_sheet_branch: Option<String>,
    pub pr_sheet_existing_url: Option<String>,
    pub pr_sheet_notice: Option<String>,
    pub pr_sheet_loading: bool,
    pub pr_sheet_can_create: bool,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct PullRequestFileView {
    pub pr_file_path: String,
    pub pr_file_selected: bool,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct PullRequestStatusView {
    pub pr_status_label: String,
    pub pr_status_url: String,
    pub pr_status_checks: Option<String>,
    pub pr_status_review: Option<String>,
    pub pr_status_following: bool,
    pub pr_status_has_comments: bool,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct LinearSettingsView {
    pub linear_account: Option<String>,
    pub linear_notice: Option<String>,
    pub linear_connected: bool,
    pub linear_teams: Vec<LinearTeamView>,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct LinearTeamView {
    pub linear_team_key: String,
    pub linear_team_name: String,
}
