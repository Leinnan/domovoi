//! Provider adapters: run turns on Codex / Claude and record them through
//! [`turn::Turn`]. [`Host`] implements the orchestrator's `ProviderHost`.

pub mod acp;
pub mod acp_terminals;
pub mod claude;
pub mod codex;
pub mod opencode;
pub mod pi;
pub mod repair;
pub mod subagents;
pub mod turn;

use crate::orchestration::commands::{Answers, Orchestrator};
use crate::orchestration::effects::{ProviderError, ProviderHost, finalize_run};
use crate::orchestration::events::{Kept, Outline};
use std::collections::{HashMap, HashSet};
use domovoi_model::ProviderDriver;
use domovoi_model::typed::{
    ChatAttachment, ContextHandoff, ContextTransferResolution, ConversationMessage, HandoffStatus, ModelSelection, NativeRefStrength, ProviderRef,
    ProviderThread, ProviderThreadStatus, Run, RunStatus, Timestamp,
};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::{Mutex, mpsc};

#[derive(Debug)]
pub enum Control {
    /// `answers` only for a `user_input` request.
    Respond { request_id: String, decision: String, answers: Option<Answers> },
    Interrupt,
    /// More input for the running turn; `reply` says whether the provider took it.
    Steer { text: String, images: Vec<(String, PathBuf)>, reply: tokio::sync::oneshot::Sender<Result<(), String>> },
    /// Stop one subagent (its native id: Claude's tool use id).
    StopSubagent { native_id: String, reply: tokio::sync::oneshot::Sender<Result<(), String>> },
}

#[derive(Debug, Clone)]
pub struct TurnInput {
    pub text: String,
    /// Images attached to the message: (MIME type, file on disk).
    pub images: Vec<(String, PathBuf)>,
    /// Other files attached to the message (the text already names them):
    /// (name, MIME type, file on disk). Pasted text is not among them.
    pub files: Vec<(String, String, PathBuf)>,
    /// `default` | `plan` (plan first, change nothing).
    pub interaction_mode: String,
    pub cwd: String,
    pub runtime_mode: String,
    pub model_selection: ModelSelection,
    pub claude_binary: String,
    /// Enable Claude Code's native hosted `Artifact` toolset (`CLAUDE_CODE_ARTIFACTS=1`).
    pub claude_artifacts: bool,
    /// A native fork for the run's first turn: the source conversation (Claude
    /// session or Codex thread) and where in it (Claude message / Codex turn).
    pub fork: Option<(String, Option<String>)>,
    /// With `fork` of an OpenCode session: the prompt of the source's next run
    /// after the fork point, which OpenCode's fork copies history before;
    /// `None` to copy all of it.
    pub fork_before: Option<String>,
    /// The thread the turn is for (its agent's MCP session).
    pub thread_id: String,
    /// The thread's title (pi names its session after it).
    pub title: Option<String>,
}

pub struct Host {
    orch: Arc<Orchestrator>,
    /// Where attachments are stored.
    userdata: PathBuf,
    log_dir: PathBuf,
    codex: Mutex<Option<Arc<codex::CodexServer>>>,
    opencode: Mutex<Option<Arc<opencode::OpenCodeServer>>>,
    claude: Mutex<HashMap<String, Arc<claude::ClaudeSession>>>,
    /// One pi process per thread, as Claude's.
    pi: Mutex<HashMap<String, Arc<pi::PiSession>>>,
    /// One ACP agent process per thread.
    acp: Mutex<HashMap<String, Arc<acp::AcpSession>>>,
    /// Threads whose Claude conversation was rewound: next turn starts a new CLI session.
    fresh_claude: Mutex<HashSet<String>>,
    active: ActiveTurns,
}

/// What a run shows when its turn's task died without settling it.
const DEAD_TURN: &str = "The server hit an internal error during this turn, so it was stopped. Send a message to continue.";

/// The text a panic was raised with.
fn panic_message(panic: &(dyn std::any::Any + Send)) -> &str {
    panic.downcast_ref::<&str>().copied().or_else(|| panic.downcast_ref::<String>().map(String::as_str)).unwrap_or("(no message)")
}

/// Provider turns running now, by thread: the run and its control channel.
/// A plain mutex: every use is a short lookup or insert, never across an await.
#[derive(Default)]
struct ActiveTurns(std::sync::Mutex<HashMap<String, (String, mpsc::UnboundedSender<Control>)>>);

impl ActiveTurns {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, (String, mpsc::UnboundedSender<Control>)>> {
        crate::util::locked(&self.0)
    }

    /// Records the thread's turn as running. Dropping the returned
    /// registration takes it out again, unless the turn's task took it over
    /// with [`Registration::hand_over`].
    #[must_use = "dropping the registration ends the turn"]
    fn register(&self, thread_id: &str, run_id: &str, controls: mpsc::UnboundedSender<Control>) -> Registration<'_> {
        self.lock().insert(thread_id.to_owned(), (run_id.to_owned(), controls));
        Registration { turns: self, thread_id: thread_id.to_owned(), run_id: run_id.to_owned(), armed: true }
    }

    /// The run's turn ended. A later run of the same thread that already
    /// took its place stays.
    fn finish(&self, thread_id: &str, run_id: &str) {
        let mut turns = self.lock();
        if turns.get(thread_id).is_some_and(|(run, _)| run == run_id) {
            turns.remove(thread_id);
        }
    }
}

/// A turn registered in [`ActiveTurns`] whose task has not started yet.
struct Registration<'a> {
    turns: &'a ActiveTurns,
    thread_id: String,
    run_id: String,
    armed: bool,
}

impl Registration<'_> {
    /// The spawned turn task now owns the entry and calls `finish` when it ends.
    fn hand_over(mut self) {
        self.armed = false;
    }
}

impl Drop for Registration<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.turns.finish(&self.thread_id, &self.run_id);
        }
    }
}

impl Host {
    pub fn new(orch: Arc<Orchestrator>, userdata: PathBuf, log_dir: PathBuf) -> Arc<Self> {
        let _ = std::fs::create_dir_all(&log_dir);
        Arc::new(Host {
            orch, userdata, log_dir, codex: Mutex::new(None), opencode: Mutex::new(None), claude: Mutex::new(HashMap::new()), pi: Mutex::new(HashMap::new()), acp: Mutex::new(HashMap::new()),
            fresh_claude: Mutex::new(HashSet::new()), active: ActiveTurns::default(),
        })
    }

    /// Force stop: ends the thread's own agent process. Claude runs one CLI per
    /// thread, so it is killed; the shared Codex app-server only interrupts.
    /// Returns whether anything was live.
    pub async fn force_stop(&self, thread_id: &str) -> bool {
        let claude = self.claude.lock().await.remove(thread_id);
        let pi = self.pi.lock().await.remove(thread_id);
        if let Some(s) = &pi {
            s.kill().await;
        }
        let acp = self.acp.lock().await.remove(thread_id);
        if let Some(s) = &acp {
            s.kill().await;
        }
        let active = self.active.lock().get(thread_id).map(|(_, tx)| tx.clone());
        if let Some(tx) = &active {
            let _ = tx.send(Control::Interrupt);
        }
        if let Some(s) = &claude {
            s.kill().await;
        }
        claude.is_some() || pi.is_some() || acp.is_some() || active.is_some()
    }

    /// Runs a turn's task: once `turn` ends, the turn leaves the active ones
    /// and its run is finalized. A turn that panics (a bug) would otherwise
    /// leave its run running for good, its controls closed and its agent
    /// waiting on an answer: it is failed, and its agent process goes.
    fn spawn_turn(self: Arc<Self>, thread_id: String, run_id: String, turn: impl Future<Output = ()> + Send + 'static) {
        use futures::FutureExt as _;
        tokio::spawn(async move {
            match std::panic::AssertUnwindSafe(turn).catch_unwind().await {
                Ok(()) => {
                    self.active.finish(&thread_id, &run_id);
                    finalize_run(&self.orch, &thread_id, &run_id).await;
                }
                Err(panic) => {
                    tracing::error!(%thread_id, %run_id, "the turn's task panicked: {}", panic_message(panic.as_ref()));
                    self.active.finish(&thread_id, &run_id);
                    crate::orchestration::recovery::settle_dead_run(self.orch.store(), &thread_id, &run_id, DEAD_TURN);
                    self.drop_thread_process(&thread_id).await;
                    finalize_run(&self.orch, &thread_id, &run_id).await;
                }
            }
        });
    }

    /// Sends a control to the thread's live turn (of `run_id`, when given).
    /// A turn whose task is gone without settling its run is settled here
    /// instead (`TurnEnded`), so its run can't stay running.
    async fn send_control(&self, thread_id: &str, run_id: Option<&str>, control: Control) -> Result<(), ProviderError> {
        let dead_run = {
            let active = self.active.lock();
            let (active_run, tx) = active.get(thread_id).ok_or(ProviderError::NoActiveTurn)?;
            if run_id.is_some_and(|r| r != active_run) {
                return Err(ProviderError::TurnEnded);
            }
            match tx.send(control) {
                Ok(()) => return Ok(()),
                Err(_) => active_run.clone(),
            }
        };
        // Also the moment between a turn ending and its task finishing it:
        // a run that already settled is left to that.
        if crate::orchestration::recovery::settle_dead_run(self.orch.store(), thread_id, &dead_run, DEAD_TURN) {
            tracing::warn!(%thread_id, run_id = %dead_run, "the turn's task was gone with its run still live: settled it");
            self.active.finish(thread_id, &dead_run);
            self.drop_thread_process(thread_id).await;
            finalize_run(&self.orch, thread_id, &dead_run).await;
        }
        Err(ProviderError::TurnEnded)
    }

    /// Ends the thread's own agent process (Claude, pi or ACP), if it has one;
    /// the next turn starts a new one. The shared Codex and OpenCode servers stay.
    async fn drop_thread_process(&self, thread_id: &str) {
        let claude = self.claude.lock().await.remove(thread_id);
        let pi = self.pi.lock().await.remove(thread_id);
        let acp = self.acp.lock().await.remove(thread_id);
        if let Some(s) = claude {
            s.kill().await;
        }
        if let Some(s) = pi {
            s.kill().await;
        }
        if let Some(s) = acp {
            s.kill().await;
        }
    }

    /// Stops every ACP agent of provider instance `instance` (its sign-in or
    /// settings changed). Their next turns start new ones.
    pub async fn stop_instance(&self, instance: &str) {
        let stopped: Vec<_> = {
            let mut sessions = self.acp.lock().await;
            let ids: Vec<String> = sessions.iter().filter(|(_, s)| s.instance() == instance).map(|(id, _)| id.clone()).collect();
            ids.iter().filter_map(|id| sessions.remove(id)).collect()
        };
        for s in stopped {
            s.kill().await;
        }
    }

    /// (thread, run) of every provider turn running now.
    pub fn live_runs(&self) -> Vec<(String, String)> {
        self.active.lock().iter().map(|(t, (r, _))| (t.clone(), r.clone())).collect()
    }

    /// How many provider turns are running now.
    pub fn live_turns(&self) -> usize {
        self.active.lock().len()
    }

    /// The shared Codex app-server, only if it is already running.
    pub async fn live_codex(&self) -> Option<Arc<codex::CodexServer>> {
        self.codex.lock().await.clone()
    }

    async fn codex(&self) -> Result<Arc<codex::CodexServer>, String> {
        let mut slot = self.codex.lock().await;
        if let Some(c) = slot.as_ref() {
            return Ok(c.clone());
        }
        let c = codex::CodexServer::start(self.log_dir.clone()).await?;
        *slot = Some(c.clone());
        Ok(c)
    }

    /// Stops the provider servers Domovoi started that would outlive it
    /// (`opencode serve` doesn't watch its stdin as Codex's app-server does).
    pub async fn shutdown(&self) {
        if let Some(s) = self.opencode.lock().await.take() {
            s.kill().await;
        }
        let pis: Vec<_> = self.pi.lock().await.drain().map(|(_, s)| s).collect();
        for s in pis {
            s.kill().await;
        }
        let agents: Vec<_> = self.acp.lock().await.drain().map(|(_, s)| s).collect();
        for s in agents {
            s.kill().await;
        }
    }

    /// The shared `opencode serve`, started (again) when needed.
    pub async fn opencode(&self) -> Result<Arc<opencode::OpenCodeServer>, String> {
        let mut slot = self.opencode.lock().await;
        if let Some(s) = slot.as_ref() {
            if s.alive().await {
                return Ok(s.clone());
            }
        }
        let s = opencode::OpenCodeServer::start().await?;
        *slot = Some(s.clone());
        Ok(s)
    }

    /// Reads the OpenCode workspace's usage limits, from the `opencode serve`
    /// already running (none is started for this).
    pub async fn read_opencode_limits(&self) {
        let Some(server) = self.opencode.lock().await.clone() else { return };
        let directory = std::env::current_dir().map(|d| d.to_string_lossy().into_owned()).unwrap_or_else(|_| "/".to_owned());
        server.read_usage_limits(crate::providers::OPENCODE, &directory).await;
    }

    fn input_for(&self, thread_id: &str, run: &Run) -> Result<TurnInput, String> {
        let store = self.orch.store();
        let thread = store.app_thread(thread_id).map_err(|e| e.to_string())?.ok_or("unknown thread")?;
        let outline = store.thread_outline(thread_id).map_err(|e| e.to_string())?;
        let message = store.record::<ConversationMessage>(thread_id, run.user_message_id.as_str()).map_err(|e| e.to_string())?;
        let (said, attachments) = message.as_ref().map_or(("", &[][..]), |message| (message.text.as_str(), &message.attachments[..]));
        let mut text = with_attached_files(said, &file_attachments(&self.userdata, attachments));
        // A portable handoff (fork, merge-back, provider switch) goes first.
        if let Some(id) = &run.context_handoff_id
            && let Some(handoff) = store.record::<ContextHandoff>(thread_id, id.as_str()).map_err(|e| e.to_string())?
            && handoff.status == HandoffStatus::Ready
        {
            text = format!("{}{text}", handoff.summary_text);
        }
        let transfer = outline
            .context_transfers
            .iter()
            .find(|t| t.target_run_id.as_ref() == Some(&run.id) && matches!(t.resolution, Some(ContextTransferResolution::NativeFork { .. })));
        let fork = transfer.and_then(|t| match &t.resolution {
            Some(ContextTransferResolution::NativeFork { provider_thread_ref }) => {
                Some((provider_thread_ref.native_id.clone()?, t.source_point.provider_turn_ref.as_ref().and_then(|turn| turn.native_id.clone())))
            }
            _ => None,
        });
        let fork_before = transfer.filter(|_| fork.is_some()).and_then(|t| {
            let source = store.thread_outline(t.source_thread_id.as_str()).ok()?;
            let at = source.runs.iter().find(|r| Some(&r.id) == t.source_point.run_id.as_ref())?;
            native_turn_after(&source, at.ordinal, "opencode", at.provider_thread_id.as_ref(), opencode::is_prompt_id)
        });
        let images = attachment_files(&self.userdata, attachments);
        let files = typed_file_attachments(&self.userdata, attachments);
        let cwd = self.thread_cwd(thread_id)?;
        Ok(TurnInput {
            text,
            images,
            files,
            interaction_mode: thread.interaction_mode.as_str().to_owned(),
            cwd,
            runtime_mode: thread.runtime_mode.as_str().to_owned(),
            model_selection: run.model_selection.clone(),
            claude_binary: String::new(),
            claude_artifacts: false,
            fork,
            fork_before,
            thread_id: thread_id.to_owned(),
            title: Some(thread.title.clone()),
        })
    }

    /// The folder a thread's agent works in: its worktree, else its first
    /// checkpoint scope, else its project's root.
    fn thread_cwd(&self, thread_id: &str) -> Result<String, String> {
        let store = self.orch.store();
        let thread = store.app_thread(thread_id).map_err(|e| e.to_string())?.ok_or("unknown thread")?;
        let outline = store.thread_outline(thread_id).map_err(|e| e.to_string())?;
        thread
            .worktree_path
            .clone()
            .or_else(|| outline.checkpoint_scopes.first().map(|scope| scope.cwd.clone()))
            .or_else(|| store.get_project(thread.project_id.as_str()).ok().flatten().map(|p| p.workspace_root))
            .ok_or_else(|| "no workspace for thread".to_owned())
    }

    /// The thread's pi, started (again) when it has none, or one that exited
    /// or runs in another runtime mode or folder.
    async fn pi_session(&self, thread_id: &str, turn: &turn::Turn, input: &TurnInput) -> Result<Arc<pi::PiSession>, String> {
        let existing = self.pi.lock().await.get(thread_id).cloned();
        if let Some(s) = existing {
            if s.usable() && s.runtime_mode == input.runtime_mode && s.cwd == input.cwd {
                return Ok(s);
            }
            self.pi.lock().await.remove(thread_id);
            s.kill().await;
        }
        let s = pi::PiSession::start(input, turn.native_thread_id(), &self.userdata, self.log_dir.clone()).await?;
        self.pi.lock().await.insert(thread_id.to_owned(), s.clone());
        Ok(s)
    }

    /// The thread's ACP agent, started (again) when it has none, or one that
    /// exited or runs in another runtime mode, folder or instance.
    async fn acp_session(&self, thread_id: &str, turn: &turn::Turn, input: &TurnInput) -> Result<Arc<acp::AcpSession>, String> {
        let existing = self.acp.lock().await.get(thread_id).cloned();
        if let Some(s) = existing {
            // Antigravity answers approvals from the runtime mode each turn: a
            // change of mode keeps its process. Changed sign-in settings don't.
            let same_mode = s.runtime_mode == input.runtime_mode || crate::antigravity::is_instance(input.model_selection.instance_id.as_str());
            if s.usable() && same_mode && s.cwd == input.cwd && s.current() {
                return Ok(s);
            }
            self.acp.lock().await.remove(thread_id);
            s.kill().await;
        }
        let s = acp::AcpSession::start(input, turn.native_thread_id(), self.log_dir.clone(), &crate::attachments::dir(&self.userdata)).await?;
        self.acp.lock().await.insert(thread_id.to_owned(), s.clone());
        Ok(s)
    }

    async fn claude_session(&self, thread_id: &str, turn: &turn::Turn, input: &TurnInput) -> Result<Arc<claude::ClaudeSession>, String> {
        let existing = self.claude.lock().await.get(thread_id).cloned();
        if let Some(s) = existing {
            // A process that refuses this turn's permission mode (launched by an
            // older server without the flag that allows full access) is
            // replaced like one that exited.
            if s.usable() && s.cwd == input.cwd && s.prepare(input).await {
                return Ok(s);
            }
            // Exited, stuck on a request it could not be told about, or in the
            // wrong mode: a fresh process resumes the conversation.
            self.claude.lock().await.remove(thread_id);
            s.kill().await;
        }
        let fresh = self.fresh_claude.lock().await.remove(thread_id);
        let (session_id, resume) = match turn.native_thread_id().filter(|_| !fresh) {
            Some(id) => (id, true),
            None => (crate::util::new_uuid(), false),
        };
        let resume_at = match (&input.fork, resume) {
            (_, true) => turn.conversation_head(),
            (Some((_, at)), false) => at.clone(),
            (None, false) => None,
        };
        let s = claude::ClaudeSession::start(input, &session_id, resume, resume_at, self.log_dir.clone()).await?;
        self.claude.lock().await.insert(thread_id.to_owned(), s.clone());
        Ok(s)
    }
}

/// The image files a message's attachments were stored as (by
/// `assets.persistChatAttachments`), those that exist.
pub fn attachment_files(userdata: &std::path::Path, attachments: &[ChatAttachment]) -> Vec<(String, PathBuf)> {
    attachments
        .iter()
        .filter_map(|attachment| match attachment {
            ChatAttachment::Image(image) => Some(image),
            _ => None,
        })
        .filter_map(|image| {
            let path = crate::attachments::dir(userdata).join(format!("{}{}", image.id, crate::attachments::image_extension(&image.mime_type, &image.name)));
            path.is_file().then(|| (image.mime_type.clone(), path))
        })
        .collect()
}

/// The non-image files a message's attachments were uploaded as (by
/// `attachments::store_upload`), those that exist: (name, path).
/// Extracts the text of every attached document that has none yet (`crate::document_text`).
async fn ensure_document_text(files: &[(String, PathBuf)]) {
    for (_, path) in files {
        crate::document_text::ensure(path).await;
    }
}

pub fn file_attachments(userdata: &std::path::Path, attachments: &[ChatAttachment]) -> Vec<(String, PathBuf)> {
    attachments
        .iter()
        .filter_map(|attachment| match attachment {
            ChatAttachment::File(file) => Some(file),
            _ => None,
        })
        .filter_map(|file| Some((file.name.clone(), crate::attachments::stored_file(userdata, &file.id)?)))
        .collect()
}

/// [`file_attachments`] with their MIME types, pasted text left out (its
/// file stays for the agent to open, not to embed).
pub fn typed_file_attachments(userdata: &std::path::Path, attachments: &[ChatAttachment]) -> Vec<(String, String, PathBuf)> {
    attachments
        .iter()
        .filter_map(|attachment| match attachment {
            ChatAttachment::File(file) if file.source.is_none() => Some(file),
            _ => None,
        })
        .filter_map(|file| Some((file.name.clone(), file.mime_type.clone(), crate::attachments::stored_file(userdata, &file.id)?)))
        .collect()
}

/// t3code's prompt for a message that is only attachments.
const ATTACHMENT_ONLY_PROMPT: &str = "[User attached one or more files without additional text. Respond using the conversation context and the attached files.]";
/// A text file up to this size goes in the prompt itself; larger files and
/// binaries (PDFs, archives) are named by path for the agent to open.
const INLINE_FILE_MAX: u64 = 32 * 1024;

/// The message text with its attached files: each named by its path (as
/// t3code words it), small text files also inlined.
pub fn with_attached_files(text: &str, files: &[(String, PathBuf)]) -> String {
    if files.is_empty() {
        return text.to_owned();
    }
    let quote = |s: &str| serde_json::to_string(s).unwrap_or_default();
    let mut out = if text.trim().is_empty() { ATTACHMENT_ONLY_PROMPT.to_owned() } else { text.to_owned() };
    out.push_str("\n\n<attached_files>");
    for (name, path) in files {
        let shown = path.to_string_lossy();
        out.push_str(&format!("\nAttached file {}: {}", quote(name), quote(&shown)));
        // A document's text, extracted to a sidecar file (t3code names it the same way).
        if let Some(extracted) = crate::document_text::extracted_text_path(path) {
            out.push_str(&format!(" (text extracted to: {})", quote(&extracted.to_string_lossy())));
        }
        let small = std::fs::metadata(path).is_ok_and(|m| m.len() <= INLINE_FILE_MAX);
        if let Some(content) = small.then(|| std::fs::read(path).ok()).flatten().and_then(|b| String::from_utf8(b).ok()).filter(|c| !c.contains('\0')) {
            let fence = if content.contains("```") { "~~~~" } else { "```" };
            out.push_str(&format!("\n{fence}\n{}\n{fence}", content.trim_end()));
        }
    }
    out.push_str("\n</attached_files>");
    out
}

impl ProviderHost for Host {
    async fn start_turn(&self, thread_id: &str, run_id: &str) -> Result<(), ProviderError> {
        let store = self.orch.store().clone();
        let turn = turn::Turn::begin(store, thread_id, run_id).ok_or("The run or its provider thread is missing.")?;
        // The managed policy's provider allow-list (`crate::policy`).
        if let Some(reason) = crate::policy::current().disallowed_driver_reason(&turn.driver) {
            return Err(reason.into());
        }
        // Documents attached to the message have their text extracted before the prompt names it.
        if let Ok(Some(message)) = self.orch.store().record::<ConversationMessage>(thread_id, turn.run.user_message_id.as_str()) {
            ensure_document_text(&file_attachments(&self.userdata, &message.attachments)).await;
        }
        let mut input = self.input_for(thread_id, &turn.run)?;
        let driver = ProviderDriver::parse(&turn.driver);
        if driver == Some(ProviderDriver::ClaudeAgent) {
            input.claude_binary = crate::providers::executable(crate::providers::CLAUDE_TOOL)?.to_string_lossy().into_owned();
            let env_enabled = std::env::var("CLAUDE_CODE_ARTIFACTS").is_ok_and(|v| v == "1" || v.eq_ignore_ascii_case("true"));
            let settings_enabled = match crate::state::STATE.get() {
                Some(state) => state.settings.read().await.unknown_bool("claudeCodeArtifacts"),
                None => false,
            };
            input.claude_artifacts = env_enabled || settings_enabled;
        }
        // Registered before the provider is ready, so an interrupt or answer
        // sent meanwhile waits in the channel. If starting fails, dropping
        // the registration takes the turn out again.
        #[expect(clippy::disallowed_methods, reason = "a turn's controls (stop, steer, an approval's answer) are few, and sent from code that cannot wait")]
        let (tx, rx) = mpsc::unbounded_channel();
        let registration = self.active.register(thread_id, run_id, tx);
        let this: Arc<Host> = HOST.get().cloned().ok_or("provider host not initialized")?;
        let (thread_id, run_id) = (thread_id.to_owned(), run_id.to_owned());
        match driver {
            Some(ProviderDriver::Codex) => {
                let server = self.codex().await?;
                registration.hand_over();
                this.spawn_turn(thread_id, run_id, async move {
                    server.run_turn(turn, input, rx).await;
                });
            }
            Some(ProviderDriver::ClaudeAgent) => {
                let session = self.claude_session(&thread_id, &turn, &input).await?;
                registration.hand_over();
                let (host, tid) = (this.clone(), thread_id.clone());
                this.spawn_turn(thread_id, run_id, async move {
                    let (_, usable) = session.run_turn(turn, input, rx).await;
                    if usable {
                        session.start_idle_drain();
                    } else {
                        // Taken out first: the map's lock is not held while it dies.
                        let gone = host.claude.lock().await.remove(&tid);
                        if let Some(s) = gone {
                            s.kill().await;
                        }
                    }
                });
            }
            Some(ProviderDriver::OpenCode) => {
                let server = self.opencode().await?;
                registration.hand_over();
                this.spawn_turn(thread_id, run_id, async move {
                    server.run_turn(turn, input, rx).await;
                });
            }
            Some(ProviderDriver::Pi) => {
                let session = self.pi_session(&thread_id, &turn, &input).await?;
                registration.hand_over();
                let (host, tid) = (this.clone(), thread_id.clone());
                this.spawn_turn(thread_id, run_id, async move {
                    let (_, usable) = session.run_turn(turn, input, rx).await;
                    if usable {
                        session.start_idle_drain();
                    } else {
                        let gone = host.pi.lock().await.remove(&tid);
                        if let Some(s) = gone {
                            s.kill().await;
                        }
                    }
                });
            }
            Some(d) if d.is_acp() => {
                let session = self.acp_session(&thread_id, &turn, &input).await?;
                registration.hand_over();
                let (host, tid) = (this.clone(), thread_id.clone());
                this.spawn_turn(thread_id, run_id, async move {
                    let (_, usable) = session.run_turn(turn, input, rx).await;
                    if !usable {
                        let gone = host.acp.lock().await.remove(&tid);
                        if let Some(s) = gone {
                            s.kill().await;
                        }
                    }
                });
            }
            Some(_) | None => return Err(format!("Provider {} is not supported by domovoi-server yet.", turn.driver).into()),
        }
        Ok(())
    }

    async fn respond(&self, thread_id: &str, request_id: &str, decision: &str, answers: Option<Answers>) -> Result<(), ProviderError> {
        self.send_control(thread_id, None, Control::Respond { request_id: request_id.into(), decision: decision.into(), answers }).await
    }

    async fn steer(&self, thread_id: &str, run_id: &str, message_id: &str) -> Result<(), ProviderError> {
        let message = self.orch.store().record::<ConversationMessage>(thread_id, message_id).map_err(|e| e.to_string())?.ok_or("unknown message")?;
        let files = file_attachments(&self.userdata, &message.attachments);
        ensure_document_text(&files).await;
        let text = with_attached_files(&message.text, &files);
        let images = attachment_files(&self.userdata, &message.attachments);
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_control(thread_id, Some(run_id), Control::Steer { text, images, reply }).await?;
        rx.await.map_err(|_| "The turn ended before taking the message.".to_owned())?.map_err(ProviderError::from)
    }

    async fn stop_subagent(&self, thread_id: &str, native_id: &str) -> Result<(), ProviderError> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        self.send_control(thread_id, None, Control::StopSubagent { native_id: native_id.into(), reply }).await?;
        rx.await.map_err(|_| "The turn ended first.".to_owned())?.map_err(ProviderError::from)
    }

    async fn interrupt(&self, thread_id: &str, _run_id: &str) -> Result<(), ProviderError> {
        match self.send_control(thread_id, None, Control::Interrupt).await {
            // The turn was already gone: its run is settled, which is what a stop wants.
            Err(ProviderError::TurnEnded) => Ok(()),
            sent => sent,
        }
    }

    /// The thread's runtime or interaction mode changed: its live Claude takes
    /// the mode both make now (plan mode wins), even in the middle of a run.
    async fn runtime_mode_changed(&self, thread_id: &str) {
        let Some(thread) = self.orch.store().app_thread(thread_id).ok().flatten() else { return };
        if let Some(s) = self.claude.lock().await.get(thread_id).cloned() {
            s.set_thread_mode(thread.interaction_mode.as_str(), thread.runtime_mode.as_str()).await;
        }
    }

    async fn rollback_conversation(&self, thread_id: &str, checkpoint_id: &str) -> Result<(), ProviderError> {
        let store = self.orch.store();
        let outline = store.thread_outline(thread_id).map_err(|e| e.to_string())?;
        let target = outline.checkpoints.iter().find(|c| c.id.as_str() == checkpoint_id).ok_or("unknown checkpoint")?;
        let after = target.app_run_ordinal.unwrap_or(0);
        let Some(first_reverted) = outline.runs.iter().filter(|run| run.ordinal > after && run.status != RunStatus::RolledBack).min_by_key(|run| run.ordinal) else {
            return Ok(());
        };
        let provider_thread = outline.provider_threads.iter().find(|pt| Some(&pt.id) == first_reverted.provider_thread_id.as_ref());
        match provider_thread.map(|pt| pt.driver.as_str()) {
            Some("codex") => {
                let native_thread = provider_thread.and_then(|pt| pt.native_thread_ref.as_ref()).and_then(|native| native.native_id.clone());
                let native_turn = outline
                    .attempts
                    .iter()
                    .find(|attempt| Some(&attempt.id) == first_reverted.active_attempt_id.as_ref())
                    .and_then(|attempt| outline.provider_turns.iter().find(|turn| Some(&turn.id) == attempt.provider_turn_id.as_ref()))
                    .and_then(|turn| turn.native_turn_ref.as_ref()?.native_id.clone());
                if let (Some(th), Some(tu)) = (native_thread, native_turn) {
                    self.codex().await?.revert(&th, &tu).await?;
                }
            }
            Some("claudeAgent") => {
                // Close the live CLI; the next turn resumes the session at the
                // target turn's last assistant message (t3code ClaudeAdapterV2).
                if let Some(s) = self.claude.lock().await.remove(thread_id) {
                    s.kill().await;
                }
                let cursor = (after > 0).then(|| claude_cursor_through(&outline, after)).flatten();
                match (after, cursor, provider_thread) {
                    (0, _, _) | (_, None, _) | (_, _, None) => {
                        // Back to the thread start, or no cursor recorded: a new session.
                        self.fresh_claude.lock().await.insert(thread_id.to_owned());
                    }
                    (_, Some(cursor), Some(provider_thread)) => {
                        let mut pt = ProviderThread::clone(provider_thread);
                        pt.native_conversation_head_ref =
                            Some(ProviderRef { driver: "claudeAgent".into(), native_id: Some(cursor), strength: NativeRefStrength::Weak, fingerprint: None, ordinal: None });
                        pt.status = ProviderThreadStatus::Idle;
                        pt.last_run_ordinal = Some(after);
                        pt.updated_at = Timestamp::from(crate::time_fmt::iso_now());
                        let o = crate::orchestration::events::Origin {
                            driver: Some("claudeAgent".into()),
                            provider_instance_id: Some(pt.provider_instance_id.as_str().to_owned()),
                            command_id: None,
                        };
                        store.append_event(thread_id, pt.updated(), &o).map_err(|e| e.to_string())?;
                    }
                }
            }
            Some("pi") => {
                let Some(provider_thread) = provider_thread else { return Ok(()) };
                let Some(source) = provider_thread.native_thread_ref.as_ref().and_then(|n| n.native_id.clone()) else { return Ok(()) };
                // The first reverted turn whose user entry is known: pi forks just before it.
                let Some(cut) = pi_cut_after(&outline, after) else { return Ok(()) };
                let live = self.pi.lock().await.get(thread_id).cloned();
                let file = match live {
                    Some(s) if s.usable() => s.rollback(&cut).await?,
                    other => {
                        if let Some(s) = other {
                            self.pi.lock().await.remove(thread_id);
                            s.kill().await;
                        }
                        pi::rollback_file(&source, &self.thread_cwd(thread_id)?, &cut).await?
                    }
                };
                let mut pt = ProviderThread::clone(provider_thread);
                pt.native_thread_ref = Some(ProviderRef { driver: "pi".into(), native_id: Some(file), strength: NativeRefStrength::Strong, fingerprint: None, ordinal: None });
                pt.status = ProviderThreadStatus::Idle;
                pt.last_run_ordinal = Some(after);
                pt.updated_at = Timestamp::from(crate::time_fmt::iso_now());
                let o = crate::orchestration::events::Origin { driver: Some("pi".into()), provider_instance_id: Some(pt.provider_instance_id.as_str().to_owned()), command_id: None };
                store.append_event(thread_id, pt.updated(), &o).map_err(|e| e.to_string())?;
            }
            // OpenCode reverts its session to before the first undone turn's
            // prompt (t3code `OpenCode2Adapter.rollbackThread`); files are the checkpoint's.
            Some("opencode") => {
                let Some(provider_thread) = provider_thread else { return Ok(()) };
                let Some(session) = provider_thread.native_thread_ref.as_ref().and_then(|n| n.native_id.clone()) else { return Ok(()) };
                let Some(cut) = native_turn_after(&outline, after, "opencode", Some(&provider_thread.id), opencode::is_prompt_id) else { return Ok(()) };
                self.opencode().await?.rollback(&session, &cut).await?;
            }
            // ACP has no truncation: the checkpoint restores the files and the
            // next turn opens a new agent session (t3code `makeAcpAdapterV2`
            // `rollbackThread`), so the agent no longer remembers what was undone.
            Some(driver @ ("acpRegistry" | "antigravity" | "grok" | "kiro" | "fx")) => {
                if let Some(s) = self.acp.lock().await.remove(thread_id) {
                    s.kill().await;
                }
                let Some(provider_thread) = provider_thread else { return Ok(()) };
                let mut pt = ProviderThread::clone(provider_thread);
                pt.native_thread_ref = None;
                pt.native_conversation_head_ref = None;
                pt.status = ProviderThreadStatus::Idle;
                pt.last_run_ordinal = Some(after);
                pt.updated_at = Timestamp::from(crate::time_fmt::iso_now());
                let o = crate::orchestration::events::Origin { driver: Some(driver.into()), provider_instance_id: Some(pt.provider_instance_id.as_str().to_owned()), command_id: None };
                store.append_event(thread_id, pt.updated(), &o).map_err(|e| e.to_string())?;
            }
            _ => {}
        }
        Ok(())
    }
}

/// The pi user entry of the first run after ordinal `after` (not rolled back)
/// whose turn recorded one: rollback cuts the session just before it.
fn pi_cut_after(outline: &Outline, after: u64) -> Option<String> {
    native_turn_after(outline, after, "pi", None, |id| !id.starts_with(pi::PENDING_REF))
}

/// The `driver` native turn of the first run after ordinal `after` (not
/// rolled back; on provider thread `on`, when given) whose turn recorded one
/// that `usable` takes.
fn native_turn_after(outline: &Outline, after: u64, driver: &str, on: Option<&domovoi_model::typed::ProviderThreadId>, usable: impl Fn(&str) -> bool) -> Option<String> {
    let mut runs: Vec<&Run> = outline.runs.iter().map(|run| &**run)
        .filter(|run| run.ordinal > after && run.status != RunStatus::RolledBack && on.is_none_or(|on| run.provider_thread_id.as_ref() == Some(on)))
        .collect();
    runs.sort_by_key(|run| run.ordinal);
    runs.into_iter().find_map(|run| {
        let attempt = outline.attempts.iter().find(|attempt| Some(&attempt.id) == run.active_attempt_id.as_ref())?;
        let turn = outline.provider_turns.iter().find(|turn| Some(&turn.id) == attempt.provider_turn_id.as_ref())?;
        let r = turn.native_turn_ref.as_ref()?;
        (r.driver.as_str() == driver).then(|| r.native_id.clone()).flatten().filter(|id| usable(id))
    })
}

/// The Claude cursor (last assistant message uuid) of the newest provider turn
/// belonging to a run with ordinal <= `through`.
fn claude_cursor_through(outline: &Outline, through: u64) -> Option<String> {
    let mut candidates: Vec<&Run> = outline.runs.iter().map(|run| &**run).filter(|run| run.ordinal <= through && run.status != RunStatus::RolledBack).collect();
    candidates.sort_by_key(|run| std::cmp::Reverse(run.ordinal));
    candidates.into_iter().find_map(|run| {
        let attempt = outline.attempts.iter().find(|attempt| Some(&attempt.id) == run.active_attempt_id.as_ref())?;
        let turn = outline.provider_turns.iter().find(|turn| Some(&turn.id) == attempt.provider_turn_id.as_ref())?;
        let cursor = turn.native_turn_ref.as_ref()?;
        (cursor.driver.as_str() == "claudeAgent").then(|| cursor.native_id.clone()).flatten()
    })
}

/// The process-wide host (spawned turn tasks need an owned handle).
pub static HOST: std::sync::OnceLock<Arc<Host>> = std::sync::OnceLock::new();

#[cfg(test)]
mod tests {
    use super::{ActiveTurns, Control};
    use tokio::sync::mpsc;

    #[test]
    fn a_turn_that_fails_to_start_is_not_left_running() {
        let turns = ActiveTurns::default();
        let (tx, mut rx) = mpsc::unbounded_channel();
        {
            let registration = turns.register("t1", "r1", tx);
            // While the provider starts, the turn is running and takes controls.
            assert_eq!(turns.lock().len(), 1);
            turns.lock()["t1"].1.send(Control::Interrupt).unwrap();
            drop(registration); // the provider failed to start
        }
        assert!(turns.lock().is_empty(), "no phantom turn keeps the Mac awake or is continued after a restart");
        assert!(matches!(rx.try_recv(), Ok(Control::Interrupt)));
    }

    #[test]
    fn a_started_turn_stays_until_its_task_finishes_it() {
        let turns = ActiveTurns::default();
        turns.register("t1", "r1", mpsc::unbounded_channel().0).hand_over();
        assert_eq!(turns.lock()["t1"].0, "r1");
        // A later run of the thread took its place before the old task finished.
        turns.register("t1", "r2", mpsc::unbounded_channel().0).hand_over();
        turns.finish("t1", "r1");
        assert_eq!(turns.lock()["t1"].0, "r2", "the old run's end leaves the new run");
        turns.finish("t1", "r2");
        assert!(turns.lock().is_empty());
    }

    /// A host over an in-memory store whose thread `t1` has a run under way: (host, store, run).
    async fn host_with_a_running_turn() -> (std::sync::Arc<super::Host>, crate::store::Store, String) {
        use domovoi_model::typed::Run;
        let store = crate::store::Store::new(crate::db::Db::open_in_memory().expect("db"));
        let dir = std::env::temp_dir().join(format!("domovoi-dead-turn-{}", crate::util::new_uuid()));
        store
            .create_project(crate::wire::shell::ProjectShell {
                id: "p".into(), title: "p".into(), has_custom_title: false, workspace_root: dir.to_string_lossy().into(),
                repository_identity: None, default_model_selection: None, default_thread_env_mode: None, auto_pull: false,
                favicon_path: None, project_icon: None, scripts: vec![], pinned_at: None, archived_at: None,
                created_at: "t".into(), updated_at: "t".into(),
            }, None)
            .expect("project");
        let (orchestrator, _effects) = crate::orchestration::commands::Orchestrator::new(store.clone());
        orchestrator.launch_thread(crate::test_records::launch("c1", "t1", "p", "antigravity", "hello")).await.expect("launch");
        let run = store.records::<Run>("t1").expect("runs").first().expect("a run").id.as_str().to_owned();
        let mut turn = super::turn::Turn::begin(store.clone(), "t1", &run).expect("the run");
        turn.turn_started("pending:1");
        let host = super::Host::new(std::sync::Arc::new(orchestrator), dir.join("userdata"), dir.join("logs"));
        (host, store, run)
    }

    fn run_status(store: &crate::store::Store, run: &str) -> String {
        store.record::<domovoi_model::typed::Run>("t1", run).expect("read").expect("the run").status.as_str().to_owned()
    }

    #[tokio::test]
    async fn a_turn_whose_task_panics_fails_its_run_and_frees_the_thread() {
        use crate::orchestration::effects::ProviderHost as _;
        let (host, store, run) = host_with_a_running_turn().await;
        assert_eq!(run_status(&store, &run), "running");
        let (tx, rx) = mpsc::unbounded_channel();
        host.active.register("t1", &run, tx).hand_over();
        host.clone().spawn_turn("t1".into(), run.clone(), async move {
            let _controls = rx;
            panic!("a bug in the turn");
        });
        for _ in 0..200 {
            if run_status(&store, &run) != "running" {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(run_status(&store, &run), "failed");
        for _ in 0..200 {
            if host.active.lock().is_empty() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(host.active.lock().is_empty(), "the thread takes a new turn");
        assert!(matches!(host.interrupt("t1", &run).await, Err(super::ProviderError::NoActiveTurn)));
    }

    #[tokio::test]
    async fn a_stop_for_a_turn_whose_task_is_gone_settles_its_run() {
        use crate::orchestration::effects::ProviderHost as _;
        let (host, store, run) = host_with_a_running_turn().await;
        // The task died without finishing the turn: its controls are closed.
        host.active.register("t1", &run, mpsc::unbounded_channel().0).hand_over();
        assert!(host.interrupt("t1", &run).await.is_ok(), "no `channel closed`");
        assert_eq!(run_status(&store, &run), "failed");
        assert!(host.active.lock().is_empty());
        // A turn that settled on its own meanwhile is left as it is.
        host.active.register("t1", &run, mpsc::unbounded_channel().0).hand_over();
        assert!(host.interrupt("t1", &run).await.is_ok());
        assert_eq!(run_status(&store, &run), "failed");
    }
}

#[cfg(test)]
mod attachment_tests {
    use super::{ATTACHMENT_ONLY_PROMPT, INLINE_FILE_MAX, claude_cursor_through, file_attachments, with_attached_files};
    use crate::orchestration::events::Outline;
    use crate::test_records::{attempt, run, with};
    use serde_json::json;
    use std::sync::Arc;
    use domovoi_model::typed::ChatAttachment;

    #[test]
    fn attached_files_are_named_by_path_and_small_text_is_inlined() {
        let u = std::env::temp_dir().join(format!("domovoi-files-{}", crate::util::new_uuid()));
        let dir = crate::attachments::dir(&u);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("pending-a-md.md"), "# Notes\nuse ```rust```").unwrap();
        std::fs::write(dir.join("pending-b-pdf.pdf"), b"%PDF\0binary").unwrap();
        std::fs::write(dir.join("pending-c-txt.txt"), "x".repeat(INLINE_FILE_MAX as usize + 1)).unwrap();
        let attachment = |kind: &str, id: &str, name: &str| json!({ "type": kind, "id": id, "name": name, "mimeType": "application/octet-stream", "sizeBytes": 1 });
        let attachments: Vec<ChatAttachment> = serde_json::from_value(json!([
            attachment("file", "pending-a-md", "notes.md"),
            attachment("file", "pending-b-pdf", "spec.pdf"),
            attachment("file", "pending-c-txt", "big.txt"),
            attachment("file", "pending-gone", "gone.txt"),
            attachment("image", "img", "a.png"),
            attachment("a type of a newer server", "pending-a-md", "notes.md"),
        ]))
        .unwrap();
        let files = file_attachments(&u, &attachments);
        assert_eq!(files.iter().map(|(n, _)| n.as_str()).collect::<Vec<_>>(), ["notes.md", "spec.pdf", "big.txt"]);
        let text = with_attached_files("Summarize these", &files);
        assert!(text.starts_with("Summarize these\n\n<attached_files>"));
        // The path as a JSON string: on Windows its backslashes are escaped.
        assert!(text.contains(&format!("Attached file \"spec.pdf\": {}", serde_json::to_string(&dir.join("pending-b-pdf.pdf").to_string_lossy()).unwrap())));
        assert!(text.contains("~~~~\n# Notes\nuse ```rust```\n~~~~"), "inlined with a fence it doesn't contain");
        assert!(!text.contains("%PDF") && !text.contains("xxxx"), "binaries and big files are only named");
        assert!(with_attached_files("", &files).starts_with(ATTACHMENT_ONLY_PROMPT));
        assert_eq!(with_attached_files("hi", &[]), "hi");
        // A document whose text was extracted names its sidecar.
        let sidecar = crate::document_text::sidecar(&dir.join("pending-b-pdf.pdf"));
        std::fs::write(&sidecar, "## Page 1\nHello").unwrap();
        let text = with_attached_files("Summarize these", &files);
        assert!(text.contains(&format!("(text extracted to: {})", serde_json::to_string(&sidecar.to_string_lossy()).unwrap())), "{text}");
        let _ = std::fs::remove_dir_all(&u);
    }

    #[test]
    fn picks_the_newest_claude_cursor_at_or_before_the_target_run() {
        fn record<T: serde::de::DeserializeOwned>(value: serde_json::Value) -> Arc<T> {
            Arc::new(serde_json::from_value(value).unwrap())
        }
        let mut outline = Outline::default();
        for n in 1..=3u64 {
            outline.runs.push(record(with(run("t", &format!("r{n}"), "completed"), json!({ "ordinal": n, "activeAttemptId": format!("a{n}") }))));
            outline.attempts.push(record(with(attempt(&format!("r{n}"), &format!("a{n}"), "completed"), json!({ "providerTurnId": format!("t{n}") }))));
            outline.provider_turns.push(Arc::new(serde_json::from_value(json!({ "id": format!("t{n}"), "providerThreadId": "pt", "nodeId": "n", "runAttemptId": null,
                "nativeTurnRef": { "driver": "claudeAgent", "nativeId": format!("uuid-{n}"), "strength": "strong" }, "ordinal": n, "status": "completed",
                "startedAt": null, "completedAt": null })).unwrap()));
        }
        assert_eq!(claude_cursor_through(&outline, 2).as_deref(), Some("uuid-2"));
        assert_eq!(claude_cursor_through(&outline, 1).as_deref(), Some("uuid-1"));
        assert_eq!(claude_cursor_through(&outline, 0), None);
    }
}
