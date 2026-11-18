//! Provider adapters: run turns on Codex / Claude and record them through
//! [`turn::Turn`]. [`Host`] implements the orchestrator's `ProviderHost`.

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
    /// `default` | `plan` (plan first, change nothing).
    pub interaction_mode: String,
    pub cwd: String,
    pub runtime_mode: String,
    pub model_selection: ModelSelection,
    pub claude_binary: String,
    /// A native fork for the run's first turn: the source conversation (Claude
    /// session or Codex thread) and where in it (Claude message / Codex turn).
    pub fork: Option<(String, Option<String>)>,
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
    /// Threads whose Claude conversation was rewound: next turn starts a new CLI session.
    fresh_claude: Mutex<HashSet<String>>,
    active: ActiveTurns,
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
            orch, userdata, log_dir, codex: Mutex::new(None), opencode: Mutex::new(None), claude: Mutex::new(HashMap::new()), pi: Mutex::new(HashMap::new()),
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
        let active = self.active.lock().get(thread_id).map(|(_, tx)| tx.clone());
        if let Some(tx) = &active {
            let _ = tx.send(Control::Interrupt);
        }
        if let Some(s) = &claude {
            s.kill().await;
        }
        claude.is_some() || pi.is_some() || active.is_some()
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

    /// Whether this thread's Claude CLI is still running.
    pub async fn has_live_claude(&self, thread_id: &str) -> bool {
        self.claude.lock().await.contains_key(thread_id)
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
        let fork = outline
            .context_transfers
            .iter()
            .find(|t| t.target_run_id.as_ref() == Some(&run.id) && matches!(t.resolution, Some(ContextTransferResolution::NativeFork { .. })))
            .and_then(|t| match &t.resolution {
                Some(ContextTransferResolution::NativeFork { provider_thread_ref }) => {
                    Some((provider_thread_ref.native_id.clone()?, t.source_point.provider_turn_ref.as_ref().and_then(|turn| turn.native_id.clone())))
                }
                _ => None,
            });
        let images = attachment_files(&self.userdata, attachments);
        let cwd = self.thread_cwd(thread_id)?;
        Ok(TurnInput {
            text,
            images,
            interaction_mode: thread.interaction_mode.as_str().to_owned(),
            cwd,
            runtime_mode: thread.runtime_mode.as_str().to_owned(),
            model_selection: run.model_selection.clone(),
            claude_binary: String::new(),
            fork,
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

    /// Whether this thread's pi is still running.
    pub async fn has_live_pi(&self, thread_id: &str) -> bool {
        self.pi.lock().await.contains_key(thread_id)
    }

    async fn claude_session(&self, thread_id: &str, turn: &turn::Turn, input: &TurnInput) -> Result<Arc<claude::ClaudeSession>, String> {
        let existing = self.claude.lock().await.get(thread_id).cloned();
        if let Some(s) = existing {
            // A process that refuses this turn's permission mode (launched by an
            // older server without the flag that allows full access) is
            // replaced like one that exited.
            if s.usable() && s.prepare(input).await {
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
        let mut input = self.input_for(thread_id, &turn.run)?;
        let driver = ProviderDriver::parse(&turn.driver);
        if driver == Some(ProviderDriver::ClaudeAgent) {
            input.claude_binary = crate::providers::executable(crate::providers::CLAUDE_TOOL)?.to_string_lossy().into_owned();
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
                tokio::spawn(async move {
                    server.run_turn(turn, input, rx).await;
                    this.active.finish(&thread_id, &run_id);
                    finalize_run(&this.orch, &thread_id, &run_id).await;
                });
            }
            Some(ProviderDriver::ClaudeAgent) => {
                let session = self.claude_session(&thread_id, &turn, &input).await?;
                registration.hand_over();
                tokio::spawn(async move {
                    let (_, usable) = session.run_turn(turn, input, rx).await;
                    if usable {
                        session.start_idle_drain();
                    } else {
                        if let Some(s) = this.claude.lock().await.remove(&thread_id) {
                            s.kill().await;
                        }
                    }
                    this.active.finish(&thread_id, &run_id);
                    finalize_run(&this.orch, &thread_id, &run_id).await;
                });
            }
            Some(ProviderDriver::OpenCode) => {
                let server = self.opencode().await?;
                registration.hand_over();
                tokio::spawn(async move {
                    server.run_turn(turn, input, rx).await;
                    this.active.finish(&thread_id, &run_id);
                    finalize_run(&this.orch, &thread_id, &run_id).await;
                });
            }
            Some(ProviderDriver::Pi) => {
                let session = self.pi_session(&thread_id, &turn, &input).await?;
                registration.hand_over();
                tokio::spawn(async move {
                    let (_, usable) = session.run_turn(turn, input, rx).await;
                    if usable {
                        session.start_idle_drain();
                    } else if let Some(s) = this.pi.lock().await.remove(&thread_id) {
                        s.kill().await;
                    }
                    this.active.finish(&thread_id, &run_id);
                    finalize_run(&this.orch, &thread_id, &run_id).await;
                });
            }
            None => return Err(format!("Provider {} is not supported by domovoi-server yet.", turn.driver).into()),
        }
        Ok(())
    }

    async fn respond(&self, thread_id: &str, request_id: &str, decision: &str, answers: Option<Answers>) -> Result<(), ProviderError> {
        let active = self.active.lock();
        let (_, tx) = active.get(thread_id).ok_or(ProviderError::NoActiveTurn)?;
        tx.send(Control::Respond { request_id: request_id.into(), decision: decision.into(), answers }).map_err(|e| ProviderError::Failed(e.to_string()))
    }

    async fn steer(&self, thread_id: &str, run_id: &str, message_id: &str) -> Result<(), ProviderError> {
        let message = self.orch.store().record::<ConversationMessage>(thread_id, message_id).map_err(|e| e.to_string())?.ok_or("unknown message")?;
        let text = with_attached_files(&message.text, &file_attachments(&self.userdata, &message.attachments));
        let images = attachment_files(&self.userdata, &message.attachments);
        let (reply, rx) = tokio::sync::oneshot::channel();
        {
            let active = self.active.lock();
            let (active_run, tx) = active.get(thread_id).ok_or(ProviderError::NoActiveTurn)?;
            if active_run != run_id {
                return Err(ProviderError::TurnEnded);
            }
            tx.send(Control::Steer { text, images, reply }).map_err(|e| e.to_string())?;
        }
        rx.await.map_err(|_| "The turn ended before taking the message.".to_owned())?.map_err(ProviderError::from)
    }

    async fn stop_subagent(&self, thread_id: &str, native_id: &str) -> Result<(), ProviderError> {
        let (reply, rx) = tokio::sync::oneshot::channel();
        {
            let active = self.active.lock();
            let (_, tx) = active.get(thread_id).ok_or(ProviderError::NoActiveTurn)?;
            tx.send(Control::StopSubagent { native_id: native_id.into(), reply }).map_err(|e| e.to_string())?;
        }
        rx.await.map_err(|_| "The turn ended first.".to_owned())?.map_err(ProviderError::from)
    }

    async fn interrupt(&self, thread_id: &str, _run_id: &str) -> Result<(), ProviderError> {
        let active = self.active.lock();
        let (_, tx) = active.get(thread_id).ok_or(ProviderError::NoActiveTurn)?;
        tx.send(Control::Interrupt).map_err(|e| ProviderError::Failed(e.to_string()))
    }

    async fn runtime_mode_changed(&self, thread_id: &str) {
        let mode = self.orch.store().app_thread(thread_id).ok().flatten().map(|thread| thread.runtime_mode.as_str().to_owned());
        if let (Some(mode), Some(s)) = (mode, self.claude.lock().await.get(thread_id).cloned()) {
            s.set_runtime_mode(&mode).await;
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
                        pt.updated_at = Timestamp::from(domovoi_contracts::iso_now());
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
                pt.updated_at = Timestamp::from(domovoi_contracts::iso_now());
                let o = crate::orchestration::events::Origin { driver: Some("pi".into()), provider_instance_id: Some(pt.provider_instance_id.as_str().to_owned()), command_id: None };
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
    let mut runs: Vec<&Run> = outline.runs.iter().map(|run| &**run).filter(|run| run.ordinal > after && run.status != RunStatus::RolledBack).collect();
    runs.sort_by_key(|run| run.ordinal);
    runs.into_iter().find_map(|run| {
        let attempt = outline.attempts.iter().find(|attempt| Some(&attempt.id) == run.active_attempt_id.as_ref())?;
        let turn = outline.provider_turns.iter().find(|turn| Some(&turn.id) == attempt.provider_turn_id.as_ref())?;
        let r = turn.native_turn_ref.as_ref()?;
        (r.driver.as_str() == "pi").then(|| r.native_id.clone()).flatten().filter(|id| !id.starts_with(pi::PENDING_REF))
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
        assert!(text.contains(&format!("Attached file \"spec.pdf\": \"{}\"", dir.join("pending-b-pdf.pdf").display())));
        assert!(text.contains("~~~~\n# Notes\nuse ```rust```\n~~~~"), "inlined with a fence it doesn't contain");
        assert!(!text.contains("%PDF") && !text.contains("xxxx"), "binaries and big files are only named");
        assert!(with_attached_files("", &files).starts_with(ATTACHMENT_ONLY_PROMPT));
        assert_eq!(with_attached_files("hi", &[]), "hi");
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
