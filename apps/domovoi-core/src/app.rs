//! The Crux app: events (user intents and shell callbacks), effects, and the
//! `App` impl that hands off to `update` and `present`.

use crate::model::Model;
use crate::notify::NotifyOperation;
use crate::socket::{SocketEvent, SocketOperation};
use crate::store::StoreOperation;
use crate::tunnel::{TunnelOperation, TunnelOutput};
use crate::view::ViewModel;
use crux_core::{App, Command, macros::effect, render::RenderOperation};
use crux_http::protocol::HttpRequest;
use facet::Facet;
use serde::{Deserialize, Serialize};

/// The answer to one question: its id and the options chosen (or text typed).
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct QuestionAnswer {
    pub question_id: String,
    pub values: Vec<String>,
}

/// A scheduled task as its editor saves it (`id` empty for a new one).
///
/// `schedule_kind`: `interval` (`interval_minutes`), `fixed_time`
/// (`time_of_day` "HH:MM", `weekdays` 0 = Sunday, none for every day), `once`
/// (`run_at`, ISO with an offset) or `webhook`. `thread_mode`: `new_thread`,
/// `continue` or `fresh_session` (both with `thread_id`). `workspace`: `root`,
/// or `worktree` from `base_branch`. An empty model uses the composer's.
/// A provider instance's settings as an editor saves them (Antigravity's:
/// `auth_method` `oauth-personal` | `oauth-business` | `gemini-api-key` |
/// `agent-platform`). An empty `instance_id` adds a new instance; an empty
/// `api_key` keeps the one saved.
#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct ProviderInstanceEdit {
    pub instance_id: String,
    pub driver: String,
    pub display_name: String,
    pub enabled: bool,
    pub auth_method: String,
    pub api_key: String,
    pub gcp_project: String,
    pub gcp_location: String,
    pub binary_path: String,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq, Default)]
pub struct ScheduledTaskEdit {
    pub id: String,
    pub name: String,
    pub prompt: String,
    pub project_id: String,
    pub schedule_kind: String,
    pub interval_minutes: u32,
    pub time_of_day: String,
    pub weekdays: Vec<u8>,
    pub run_at: String,
    pub thread_mode: String,
    pub thread_id: String,
    pub workspace: String,
    pub base_branch: String,
    pub model_instance: String,
    pub model: String,
    pub runtime_mode: String,
    pub interaction_mode: String,
    pub enabled: bool,
}

#[derive(Facet, Serialize, Deserialize, Clone, Debug, PartialEq)]
#[repr(C)]
pub enum Event {
    // Connection and lifecycle
    /// At launch: restore saved environments and reconnect to the last one.
    /// `surface` is `desktop` or `mobile`; the label and OS describe this
    /// device in the server's access list.
    Launch { surface: String, device_label: String, device_os: String },
    /// A local server by URL: pair with a one-time code, or (empty code)
    /// reuse this device's saved token.
    Connect { server_url: String, pairing_code: String },
    /// A remote server from a `domovoi://connect?ticket=…&code=…` link (over iroh).
    ConnectRemote { link: String },
    SelectEnvironment { id: String },
    /// Signs this device out of the server (when reachable) and forgets it.
    ForgetEnvironment { id: String },
    RenameEnvironment { id: String, label: String },
    SetCompanion { id: String, look: crate::companion::CompanionLook },
    ResetCompanion { id: String },
    /// Moves a saved server up (`delta` < 0) or down the list.
    MoveEnvironment { id: String, delta: i32 },
    /// The user asked to connect now: skips the backoff, undoes Disconnect.
    Reconnect,
    /// Pull to refresh: connected, the shell and the open thread catch up
    /// on anything missed; otherwise the same as `Reconnect`.
    Resync,
    /// The shell's backoff timer for the retry numbered `retry_gen` fired.
    RetryTimer { retry_gen: u64 },
    Disconnect,
    /// The app went to the background / came back (mobile).
    Suspend,
    Resume,
    /// The device's network changed (Wi-Fi to cellular).
    NetworkChanged,
    /// The device has a network path (`online`) or none.
    NetworkStatus { online: bool },
    /// The device's APNs token (`environment`: `sandbox` | `production`);
    /// registered with each server this device connects to.
    PushToken { token: String, environment: String },
    /// A UnifiedPush endpoint this device got from its distributor, with its
    /// Web Push keys (`p256dh`, `auth`, base64url); registered with each
    /// server this device connects to (`/api/domovoi/push/webpush`). An empty
    /// endpoint: the distributor dropped it.
    PushEndpoint { endpoint: String, p256dh: String, auth: String },
    /// A Live Activity started for a thread, and its push token (APNs host
    /// `environment`: `sandbox` or `production`).
    ActivityToken { thread_id: String, token: String, environment: String },
    /// A push notification was opened: `environment_id` is the server's.
    OpenNotification { environment_id: String, thread_id: String },
    /// Allow or Decline on an approval's notification: answered without
    /// opening the app (queued until the connection is back).
    RespondFromNotification { environment_id: String, thread_id: String, request_id: String, decision: String },
    /// Allow / Decline from a Live Activity, which knows the thread but not
    /// the request: the thread's pending approval is answered.
    RespondToThread { environment_id: String, thread_id: String, decision: String },
    /// A question answered by typing in its notification (`QUESTION`).
    AnswerFromNotification { environment_id: String, thread_id: String, request_id: String, question_id: String, text: String },
    /// Wall clock from the shell, at launch and periodically (for ages).
    Tick { now_ms: i64 },
    AppFocus { focused: bool },
    DismissToast,
    /// The toast's button ("Undo") was tapped.
    ToastAction,
    /// Something the shell did itself (an image copied or saved) to tell the
    /// user about, in the same toast as the core's own. `kind` is `error` |
    /// `info` | `success`.
    ShowNotice { kind: String, message: String },

    // Navigation and sidebar
    SelectThread { thread_id: String },
    NewThread { project_id: Option<String> },
    /// `pull-requests` | `usage`
    ShowRoute { route: String },
    ToggleProject { project_id: String },
    ShowAllThreads { project_id: String },
    /// Groups the thread list by `status` (Inbox, Running, Inactive,
    /// Archived) or by `project`; saved on this device.
    SetSidebarGrouping { grouping: String },
    /// Collapses or expands a status section: `inbox` | `running` |
    /// `inactive` | `archived`; saved on this device.
    ToggleSection { section: String },
    /// Shows all of a section's rows (or the first ones again).
    ShowAllInSection { section: String },
    SetSearch { query: String },
    RenameThread { thread_id: String, title: String },
    /// The open thread, stopped by a usage limit: continue it when the limit
    /// resets (`snooze` false), or snooze it until then (`snooze` true).
    PlanForLimitReset { snooze: bool },
    /// Takes back what `PlanForLimitReset` set.
    CancelLimitPlan,
    PinThread { thread_id: String, pinned: bool },
    SettleThread { thread_id: String, settled: bool },
    /// Hides the thread until `until_ms` (Unix ms); 0 wakes it now.
    SnoozeThread { thread_id: String, until_ms: i64 },
    MarkThreadUnread { thread_id: String },
    /// Shows only unread threads (and ones waiting on you) in the list.
    SetUnreadOnly { on: bool },
    ArchiveThread { thread_id: String, archived: bool },
    DeleteThread { thread_id: String },
    AddProject { path: String },
    CreateProject { name: String },
    SetProjectIcon { project_id: String, kind: String, text: String, symbol: String, color: String },
    LoadProjectIcon { project_id: String },
    /// Draw a completed Mermaid block (`key` is the shell's own, one per
    /// source and theme); its URL comes in `ViewModel::diagrams`.
    LoadDiagram { key: String, source: String, dark: bool },
    SearchProjectIconFiles { project_id: String, query: String },
    LoadProjectIconThumbnail { project_id: String, path: String },
    CloseProjectIconFiles { project_id: String },
    SetProjectIconFile { project_id: String, path: String },
    RenameProject { project_id: String, title: String },
    RemoveProject { project_id: String },
    OpenScratch,
    PromoteScratchThread { thread_id: String },
    /// Lists folders with Claude Code / Codex / Pi sessions on the server.
    ScanAgentSessions,
    /// Makes each folder a project (unless it is one) and imports its recent sessions.
    ImportAgentSessions { paths: Vec<String> },
    /// The server's folders at `path` (empty: the home folder).
    BrowseFolders { path: String },

    // Thread
    /// `accept` | `acceptForSession` | `decline`
    Respond { request_id: String, decision: String },
    /// Answers a question (`user_input` request with questions).
    RespondUserInput { request_id: String, answers: Vec<QuestionAnswer> },
    /// Leaves a question unanswered (the agent carries on).
    DismissUserInput { request_id: String },
    /// Answers the open thread's secret request (`SecretRequestView.item_id`)
    /// with `value`, sent once (`secrets.answerRequest`) and kept nowhere:
    /// not in the model, the outbox or anything saved, and not sent again
    /// after a dropped connection. The shell clears its field once sent.
    AnswerSecret { item_id: String, value: String },
    /// Declines a secret request: the agent is told no value comes.
    DeclineSecret { item_id: String },
    Interrupt,
    Revert { checkpoint_id: String },
    /// Sends the draft into the turn under way (`can_steer`), instead of
    /// queueing it behind the turn as `Send` does.
    Steer,
    /// A queued message (`QueuedMessageView.run_id`): dropped, changed,
    /// sent into the turn under way now, or moved up (`delta` < 0) or down.
    CancelQueued { run_id: String },
    EditQueued { run_id: String, text: String },
    SteerQueued { run_id: String },
    MoveQueued { run_id: String, delta: i32 },
    /// Starts the queue a stop held.
    ResumeQueue,
    /// A new thread with the conversation so far (its last finished turn),
    /// opened once the server has made it.
    ForkThread { thread_id: String },
    /// A new thread with the conversation up to the turn of a row
    /// (`ItemView.can_fork_here`), opened once the server has made it.
    ForkFromItem { item_id: String },
    /// Sends the open fork's work back to the thread it came from.
    MergeBack,
    /// Stops a running subagent (its row's `id`).
    StopSubagent { subagent_id: String },
    /// Carries out the plan the agent proposed (a `plan` row's `plan_id`).
    ImplementPlan { plan_id: String },
    /// Saves a `plan` row's markdown to the thread's workspace at `path`
    /// (relative; `plans/<title>.md` when empty).
    SavePlan { item_id: String, path: String },
    /// Has the agent compact its context now (`/compact`), leaving the draft
    /// as it is (`ContextUsageView.can_compact`).
    Compact,
    /// Has the server name the thread again from its conversation.
    RegenerateTitle { thread_id: String },
    /// Forgets the tools "Allow for Thread" allowed in the thread: its next
    /// agent sessions ask again (Domovoi's own).
    ForgetThreadPermissions { thread_id: String },
    /// Changes a thread's permissions without opening it (a scheduled task's thread).
    SetThreadRuntimeMode { thread_id: String, mode: String },
    SetRuntimeMode { mode: String },
    SetInteractionMode { mode: String },
    FindInThread { query: String },
    FindNext { forward: bool },
    CloseFind,

    // Composer
    SetDraft { text: String },
    SelectModel { instance_id: String, model: String },
    /// The model picker's search field.
    SetModelQuery { query: String },
    /// Lists the models hidden from the picker too.
    SetShowHiddenModels { show: bool },
    /// Marks a model favorite (shown first, on every client) or not.
    SetModelFavorite { instance_id: String, model: String, favorite: bool },
    /// Hides a model from the picker (it no longer shows unless searched
    /// for), or shows it again. Hiding a favorite also unmarks it.
    SetModelHidden { instance_id: String, model: String, hidden: bool },
    SelectEffort { value: String },
    /// Sets one of the model's other options (`TraitView.id`): `true` or
    /// `false` for a switch, a choice's id for a list.
    SetModelOption { id: String, value: String },
    /// `local` | `worktree`
    SetWorkspaceMode { mode: String },
    SetBaseBranch { branch: String },
    AddAttachment { name: String, mime_type: String, size_bytes: u64, data_url: String },
    RemoveAttachment { id: String },
    MoveMention { delta: i32 },
    PickMention { path: String },
    /// Completes the draft to `/name ` from the command list.
    PickSlashCommand { name: String },
    /// Replaces the `$word` being typed with `$name ` (a skill).
    PickSkill { name: String },
    /// Puts one of the open thread's earlier messages in the composer (`delta`
    /// -1: an older one, 1: a newer one, back to the draft past the latest).
    RecallPrompt { delta: i32 },
    /// Puts the draft aside for later and empties the composer.
    StashDraft,
    /// Takes a stashed draft back into the composer (`index` in `stash`).
    RestoreStash { index: u32 },
    DropStash { index: u32 },
    /// Ends the turn under way even if the agent doesn't stop (after `Interrupt`).
    ForceStop,
    /// Closes the mention list (Escape) and keeps it closed for this `@`.
    DismissMention,
    Send,
    /// Sends the thread's last message of yours again (after a failed turn).
    Retry,
    /// Takes a message that hasn't gone yet (no connection) back into the
    /// composer; `id` is its `PendingSendView` id.
    CancelSend { id: String },
    /// Shows a row's long output in full.
    ExpandItem { id: String },
    /// Puts a message's text in the composer to change and send again.
    EditMessage { text: String },
    /// Adds `text` to the end of the draft (a quoted diff line), on its own paragraph.
    AppendToDraft { text: String },
    /// Loads the open thread again (after it failed to load).
    ReloadThread,

    // Right panel
    TogglePanel,
    ShowPanel { tab: String },
    SelectDiffSource { source: String },
    NewTerminal,
    ReopenTerminal,
    RenameTerminal { label: String },
    SetTerminalSplit { mode: String },
    ResizeTerminalSplit { percent: u32 },
    TerminalSplitsLoaded { value: Option<String> },
    TerminalPaneResize { terminal_id: String, rows: u32, cols: u32 },
    OpenTerminalAttention { thread_id: String, terminal_id: String },
    OpenTerminalOnEnvironment { environment_id: String, thread_id: String, terminal_id: String },
    SetTerminalWatch { watching: bool },
    SelectTerminal { terminal_id: String },
    CloseTerminal { terminal_id: String },
    /// `key` is `char` (with `text`) or a named key (`enter`, `up`, ...).
    TerminalKey { key: String, text: String, ctrl: bool, alt: bool },
    TerminalPaste { text: String },
    /// Clears the active terminal's screen and scrollback.
    ClearTerminal,
    TerminalResize { rows: u32, cols: u32 },
    /// Looks `lines` rows back into the active terminal's history
    /// (negative: towards the live screen).
    TerminalScroll { lines: i32 },
    ToggleDirectory { path: String },
    OpenFile { path: String },
    /// Fetches the open preview's Git LFS object onto the server, then shows
    /// the file again.
    FetchLfsObject { path: String },
    /// Writes an edited file back to the workspace (the open preview's
    /// text, changed), then shows it again.
    SaveFile { path: String, text: String },
    OpenArtifactFile { thread_id: String, path: String },
    CloseArtifactFile,
    /// Opens an artifact (by its unified id) in the Artifacts panel/viewer.
    OpenArtifact { thread_id: String, artifact_id: String, version: Option<u32>, dark: bool },
    /// Opens a workspace file artifact by relative or absolute path in the active thread.
    OpenArtifactWorkspacePath { thread_id: String, path: String, dark: bool },
    /// Switches the active artifact to a specific 1-based version.
    SelectArtifactVersion { version: u32, dark: bool },
    /// Switches the active artifact view between `preview` and `code`.
    SetArtifactViewMode { mode: String },
    /// Closes the active artifact selection.
    CloseArtifact,
    /// Re-fetches signed preview URLs (React / Mermaid SVG / multi-file bundle) for the current theme.
    RefreshArtifactPreview { dark: bool },
    /// Pre-fills the thread composer with context referencing the artifact (e.g. a runtime error or edit ask).
    AskAboutArtifact { artifact_id: String, prompt: String },
    SetFileQuery { query: String },
    RefreshGit,
    /// `commit` | `push` | `create_pr` | `commit_push` | `commit_push_pr`
    GitAction { action: String, message: String },
    GitPull,
    SwitchBranch { name: String },
    /// A worktree of the workspace from `base`, on the new branch `branch`
    /// (empty: `base` itself, checked out there).
    CreateWorktree { base: String, branch: String },
    /// Removes a worktree (`force`: even with changes in it).
    RemoveWorktree { path: String, force: bool },
    /// Runs a project script (`ScriptView.id`) in a new terminal of the open thread.
    RunScript { id: String },
    CreateBranch { name: String },

    // Command palette
    OpenPalette,
    ClosePalette,
    SetPaletteQuery { query: String },
    /// `commands`, `files` (go to file) or `content` (search the project).
    SetPaletteMode { mode: String },
    MovePalette { delta: i32 },
    RunPaletteItem { id: String },

    // Settings window
    /// `general` | `providers` | `keybindings` | `scheduled` | `archived` |
    /// `storage` | `diagnostics` | `access` | `projects`
    LoadSettings { section: String },
    SetSettingBool { key: String, value: bool },
    SetSettingText { key: String, value: String },
    RefreshProviders,
    /// A provider's sign-in: started, finished with the redirect URL pasted
    /// from another device's browser, or given up.
    StartProviderSignIn { instance_id: String },
    CompleteProviderSignIn { instance_id: String, callback_url: String },
    CancelProviderSignIn { instance_id: String },
    /// The provider's managed runtime: installed (or updated), the install
    /// under way stopped, or removed.
    InstallProvider { instance_id: String },
    CancelProviderInstall { instance_id: String },
    RemoveProviderInstall { instance_id: String },
    /// A provider instance's settings saved (or a new instance added), or the instance removed.
    SaveProviderInstance { edit: Box<ProviderInstanceEdit> },
    RemoveProviderInstance { instance_id: String },
    RunStorageCleanup,
    ReclaimDatabase,
    SetScheduledEnabled { id: String, enabled: bool },
    RunScheduledNow { id: String },
    DeleteScheduled { id: String },
    CreateScheduled { name: String, prompt: String, project_id: String, interval_minutes: u32 },
    /// Creates or changes a scheduled task with every setting it has.
    SaveScheduled { task: Box<ScheduledTaskEdit> },
    /// A webhook task's URL gets a new token (the old one stops working).
    RotateWebhookToken { id: String },
    /// The deliveries a webhook task received, newest first.
    LoadWebhookDeliveries { id: String },
    /// A scheduled task's last runs, newest first (`settings.scheduledRuns`);
    /// an empty id closes them.
    LoadScheduledRuns { id: String },
    RemoveKeybinding { command: String, shortcut: String, condition: String },

    // Remote access (the connected server's iroh side)
    LoadRemoteAccess,
    SetRemoteAccess { enabled: bool },
    CreatePairingLink,
    RevokeClient { session_id: String },

    // Pull requests and usage
    LoadPullRequests,
    /// `merge` (with `merge_method` `merge` | `squash` | `rebase`) | `close` |
    /// `reopen` | `ready` | `draft`.
    PullRequestAction { key: String, action: String, merge_method: String },
    /// `approve` | `request-changes` | `comment`, with a summary.
    ReviewPullRequest { key: String, verdict: String, body: String },
    CommentOnPullRequest { key: String, body: String },
    /// Checks the pull request out in a worktree and starts a new thread there.
    StartPullRequestThread { key: String },
    SelectPullRequest { key: String },
    LoadUsage { days: u32 },
    /// Restricts the Usage page to one provider (`""` shows all).
    FilterUsageProvider { provider: String },
    /// Re-reads every provider's live subscription limits.
    RefreshUsageLimits,
    /// The device's IANA time zone and current UTC offset in seconds, for
    /// local calendar boundaries on Usage.
    SetTimeZone { iana: String, utc_offset_secs: i32 },

    #[serde(skip)]
    #[facet(skip)]
    TokenIssued {
        attempt: u64,
        environment_id: String,
        #[facet(opaque)]
        res: crux_http::Result<crux_http::Response<Vec<u8>>>,
    },
    #[serde(skip)]
    #[facet(skip)]
    TicketIssued {
        attempt: u64,
        #[facet(opaque)]
        res: crux_http::Result<crux_http::Response<Vec<u8>>>,
    },
    /// The saved token, looked up after a pairing code was refused.
    #[serde(skip)]
    #[facet(skip)]
    FallbackTokenLoaded { attempt: u64, environment_id: String, value: Option<String> },
    /// The server was told this device is leaving (or could not be).
    #[serde(skip)]
    #[facet(skip)]
    SignedOut { id: String },
    #[serde(skip)]
    #[facet(skip)]
    RemoteAccessLoaded(#[facet(opaque)] crux_http::Result<crux_http::Response<Vec<u8>>>),
    #[serde(skip)]
    #[facet(skip)]
    PairingLinkCreated(#[facet(opaque)] crux_http::Result<crux_http::Response<Vec<u8>>>),
    #[serde(skip)]
    #[facet(skip)]
    ClientRevoked(#[facet(opaque)] crux_http::Result<crux_http::Response<Vec<u8>>>),
    #[serde(skip)]
    #[facet(skip)]
    Tunnel {
        tunnel_gen: u64,
        #[facet(opaque)]
        out: TunnelOutput,
    },
    #[serde(skip)]
    #[facet(skip)]
    EnvironmentsLoaded { value: Option<String> },
    #[serde(skip)]
    #[facet(skip)]
    ActiveEnvironmentLoaded { value: Option<String> },
    #[serde(skip)]
    #[facet(skip)]
    SidebarLayoutLoaded { value: Option<String> },
    #[serde(skip)]
    #[facet(skip)]
    TokenLoaded { environment_id: String, value: Option<String> },
    #[serde(skip)]
    #[facet(skip)]
    DraftsLoaded { environment_id: String, value: Option<String> },
    /// The offline copy of a server's thread list (and open thread).
    CacheLoaded { environment_id: String, value: Option<String> },
    #[serde(skip)]
    #[facet(skip)]
    Stored,
    #[serde(skip)]
    #[facet(skip)]
    PushRegistered(#[facet(opaque)] crux_http::Result<crux_http::Response<Vec<u8>>>),
    #[serde(skip)]
    #[facet(skip)]
    Socket {
        attempt: u64,
        #[facet(opaque)]
        event: SocketEvent,
    },
    #[serde(skip)]
    #[facet(skip)]
    Notified,
}

#[effect(facet_typegen)]
#[derive(Debug)]
pub enum Effect {
    Render(RenderOperation),
    Http(HttpRequest),
    Socket(SocketOperation),
    Notify(NotifyOperation),
    Tunnel(TunnelOperation),
    Store(StoreOperation),
}

#[derive(Default)]
pub struct DomovoiApp;

impl App for DomovoiApp {
    type Event = Event;
    type Model = Model;
    type ViewModel = ViewModel;
    type Effect = Effect;

    fn update(&self, event: Event, model: &mut Model) -> Command<Effect, Event> {
        // A socket frame says how far it reaches (`replies`); anything else
        // may change any part of the view.
        let frame = matches!(event, Event::Socket { event: SocketEvent::Message { .. }, .. });
        let command = crate::update::update(event, model);
        // The records held as the open thread are always the selected thread's own.
        debug_assert!(
            model.thread.as_ref().is_none_or(|t| model.selected_thread.as_deref() == Some(t.record.id.as_str())),
            "open thread {:?} held under selection {:?}",
            model.thread.as_ref().map(|t| t.record.id.as_str()),
            model.selected_thread,
        );
        let wide = if frame { model.changes.end_frame() } else { model.changes.end_update() };
        if wide {
            model.shown_ms = model.now_ms;
        }
        command
    }

    fn view(&self, model: &Model) -> ViewModel {
        crate::present::view(model)
    }
}

/// `YYYY-MM-DDTHH:MM:SS.mmmZ` from Unix milliseconds (civil-from-days).
pub fn iso_from_millis(ms: i64) -> String {
    let days = ms.div_euclid(86_400_000);
    let rem = ms.rem_euclid(86_400_000);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.{:03}Z", rem / 3_600_000, rem / 60_000 % 60, rem / 1000 % 60, rem % 1000)
}

/// Unix milliseconds from an RFC 3339 UTC timestamp (`...Z`); 0 if unparsable.
pub fn millis_from_iso(s: &str) -> i64 {
    let b = s.as_bytes();
    if b.len() < 19 {
        return 0;
    }
    let n = |r: std::ops::Range<usize>| s.get(r).and_then(|x| x.parse::<i64>().ok()).unwrap_or(0);
    let (y, m, d) = (n(0..4), n(5..7), n(8..10));
    let (hh, mm, ss) = (n(11..13), n(14..16), n(17..19));
    let ms = if b.get(19) == Some(&b'.') { n(20..23.min(s.len())) } else { 0 };
    let (y, m) = if m <= 2 { (y - 1, m + 9) } else { (y, m - 3) };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * m + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    days * 86_400_000 + hh * 3_600_000 + mm * 60_000 + ss * 1000 + ms
}

/// How long until `iso`: `in 5m`, `in 3h`, `in 2d`, or `now` once it passed.
pub fn until_label(now_ms: i64, iso: &str) -> String {
    let t = millis_from_iso(iso);
    if t == 0 || now_ms == 0 {
        return String::new();
    }
    let secs = (t - now_ms) / 1000;
    match secs {
        ..60 => "now".into(),
        60..3600 => format!("in {}m", secs / 60),
        3600..86_400 => format!("in {}h", secs / 3600),
        _ => format!("in {}d", secs / 86_400),
    }
}

/// `now`, `5m`, `3h`, `2d`, `6w`.
pub fn age_label(now_ms: i64, iso: &str) -> String {
    let t = millis_from_iso(iso);
    if t == 0 || now_ms == 0 {
        return String::new();
    }
    let secs = ((now_ms - t) / 1000).max(0);
    match secs {
        0..60 => "now".into(),
        60..3600 => format!("{}m", secs / 60),
        3600..86_400 => format!("{}h", secs / 3600),
        86_400..1_209_600 => format!("{}d", secs / 86_400),
        _ => format!("{}w", secs / 604_800),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamps_round_trip() {
        assert_eq!(iso_from_millis(1794583650705), "2026-11-13T15:27:30.705Z");
        assert_eq!(millis_from_iso("2026-11-13T15:27:30.705Z"), 1794583650705);
        assert_eq!(millis_from_iso("1970-01-01T00:00:00Z"), 0);
        let now = millis_from_iso("2026-11-13T12:00:00.000Z");
        assert_eq!(age_label(now, "2026-11-13T11:55:00.000Z"), "5m");
        assert_eq!(age_label(now, "2026-11-11T12:00:00.000Z"), "2d");
        assert_eq!(age_label(now, "garbage"), "");
    }
}
