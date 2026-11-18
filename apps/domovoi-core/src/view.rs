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
    pub usage: UsageView,
    /// The import picker (after `ScanAgentSessions`).
    pub agent_sessions: AgentSessionsView,
    /// The server folder picker (after `BrowseFolders`).
    pub folder_browser: FolderBrowserView,
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
}

/// Settings → Remote Access: the connected server's iroh side.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct RemoteAccessView {
    /// The server supports it (Domovoi servers do; t3code servers do not).
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
    /// `status` (by what each thread needs: `sections`) | `project`
    /// (under project headers: `projects`, `settled`, `snoozed`).
    pub grouping: String,
    /// Inbox, Running, Hibernating, Archived (those with threads; Inbox
    /// always), when grouped by status.
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
    /// `inbox` | `running` | `hibernating` | `archived`
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
pub struct SidebarProjectView {
    pub id: String,
    pub title: String,
    pub workspace_root: String,
    pub monogram: String,
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
    pub id: String,
    pub project_id: String,
    pub project_title: String,
    pub title: String,
    /// The model's short name, such as `Opus 4.7`.
    pub model_label: String,
    /// What the thread is doing, when that beats its age: `Needs you`,
    /// `Question`, `Working 6m`, `Failed`; empty otherwise.
    pub status_label: String,
    /// `working` | `approval` | `failed` | `interrupted` | `completed` | `idle`
    pub status: String,
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
    pub id: String,
    pub title: String,
    pub project_id: String,
    pub project_title: String,
    pub branch: Option<String>,
    pub worktree_path: Option<String>,
    pub cwd: String,
    pub model_label: String,
    pub provider_instance_id: String,
    pub runtime_mode: String,
    pub interaction_mode: String,
    pub status: String,
    pub working: bool,
    pub loading: bool,
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
    /// Messages waiting for the turn under way, in the order they run.
    pub queued: Vec<QueuedMessageView>,
    /// A stop held the queue: nothing starts until `ResumeQueue` or a new message.
    pub queue_held: bool,
    /// The agent replied but work it started in the background goes on
    /// ("Waiting for 2 background agents"); empty when there is none.
    pub background: String,
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
    /// Nearly full (80 % or more): the agent will compact soon.
    pub high: bool,
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
    /// A finished code block's highlighting (empty: plain).
    pub spans: Vec<CodeSpan>,
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
}

/// A question the agent asks, answered with its options.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct QuestionView {
    /// What the answer is keyed by.
    pub id: String,
    pub header: String,
    pub question: String,
    pub multi_select: bool,
    pub options: Vec<QuestionOptionView>,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct QuestionOptionView {
    pub label: String,
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
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct ModelGroupView {
    pub instance_id: String,
    pub provider: String,
    pub status: String,
    /// Not favorites (those are in `favorite_models`); hidden ones only with
    /// `show_hidden_models` or a search (and the selected one always).
    pub models: Vec<ModelRowView>,
    /// The provider's hidden models (for "Show N hidden").
    pub hidden_count: u32,
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
    /// `diff` | `terminal` | `files` | `git`
    pub tab: String,
    pub diff: DiffPanelView,
    pub terminal: TerminalPanelView,
    pub files: FilesPanelView,
    pub git: GitPanelView,
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
    pub tabs: Vec<TerminalTabView>,
    pub active: Option<TerminalScreenView>,
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
    /// The text highlighted for its file type (empty: plain).
    pub spans: Vec<CodeSpan>,
    pub truncated: bool,
    pub error: Option<String>,
    pub loading: bool,
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
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct PaletteItemView {
    pub id: String,
    /// `thread` | `project` | `action`
    pub kind: String,
    pub title: String,
    pub subtitle: String,
    pub shortcut: String,
}

// ---- settings ----

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct SettingsView {
    pub groups: Vec<SettingGroupView>,
    pub providers: Vec<ProviderView>,
    pub keybindings: Vec<KeybindingView>,
    pub scheduled_tasks: Vec<ScheduledTaskView>,
    pub scheduled_time_zone: String,
    pub archived: Vec<ThreadRowView>,
    pub storage: StorageView,
    pub diagnostics: DiagnosticsView,
    pub access: Vec<AccessEntryView>,
    pub projects: Vec<ProjectSettingsView>,
    pub remote: RemoteAccessView,
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
    pub id: String,
    pub title: String,
    pub workspace_root: String,
    pub thread_count: u32,
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
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct CheckView {
    pub name: String,
    pub status: String,
    pub url: String,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct UsageView {
    pub loading: bool,
    pub range_label: String,
    pub rows: Vec<UsageRowView>,
    pub total_tokens: String,
    pub notice: String,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, Default, PartialEq, Eq)]
pub struct UsageRowView {
    pub day: String,
    pub provider: String,
    pub model: String,
    pub input: String,
    pub cached: String,
    pub output: String,
    pub total: String,
}
