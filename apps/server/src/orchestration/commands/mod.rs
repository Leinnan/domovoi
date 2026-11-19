//! The orchestrator: serializes commands, decides domain events, and queues
//! effects (provider work) that run after the events are committed.
//!
//! Event sequences follow t3code's Orchestrator (`dispatchThreadCreate`,
//! `dispatchMessage` immediate/queue paths, `thread.visit`, runtime mode).
//! Differences: launches start immediately (no deferred workspace-preparation
//! step). A message to a busy thread steers the running turn when asked and
//! the provider can (Claude, Codex), restarts it, or queues behind it.
//!
//! A command arrives typed (`domovoi_model::typed::Command`, decoded where it
//! enters the server) and its handler works on typed records. The handlers
//! are grouped by what they act on:
//!
//! - `launch`: `orchestration.launchThread`;
//! - `message`: `message.dispatch`, steering, and the handoff a run starts with;
//! - `thread`: the thread's own fields, stamps, settling and snoozing;
//! - `queue`: queued runs, and starting the next one;
//! - `runtime`: answers to runtime requests, stops, rollback;
//! - `fork`: `thread.fork` and `thread.merge_back`.

#![expect(clippy::disallowed_types, reason = "`Answers`: each answer to a question has a shape the schema leaves open")]

mod failure;
mod fork;
mod launch;
mod message;
mod queue;
mod runtime;
#[cfg(test)]
mod tests;
mod thread;

pub use failure::{DispatchFailure, LaunchFailure, LaunchRefusal, Refusal};
pub use launch::LaunchResult;

use super::events::{Origin, enc};
use crate::store::Store;
use domovoi_model::typed::{AppThread, Command, CommandId, DispatchCommandResult, ProviderInstanceId, Run, RunStatus, ThreadId, Timestamp};
use tokio::sync::{Mutex, mpsc};

/// What a command that was carried out is answered with.
pub type DispatchResult = DispatchCommandResult;

/// What the user answered a `user_input` request with: question id to
/// answer. The schema leaves an answer's shape open (a string, a list of
/// choices, whatever the client's form gives), so each provider reads it as it can.
pub type Answers = serde_json::Map<String, serde_json::Value>;

pub const ACTIVE_RUN: [&str; 4] = ["preparing", "starting", "running", "waiting"];

/// Whether a run in `status` holds the thread: nothing else starts meanwhile.
fn is_active(status: &RunStatus) -> bool {
    ACTIVE_RUN.contains(&status.as_str())
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "effect")]
pub enum Effect {
    /// Start the provider turn for a run that is `starting`.
    StartTurn { thread_id: String, run_id: String },
    /// Deliver a user decision for a pending runtime request.
    /// `answers` answers a `user_input` request.
    RespondToRequest { thread_id: String, request_id: String, decision: String, answers: Option<Answers> },
    /// Interrupt the active run.
    Interrupt { thread_id: String, run_id: String },
    /// Runtime mode changed; live sessions should apply it.
    RuntimeModeChanged { thread_id: String },
    /// Restore files to a checkpoint and roll the run(s) back.
    Rollback { thread_id: String, checkpoint_id: String, restore_files: bool, command_id: String },
    /// Add a message to the run's live provider turn; if that fails the
    /// message is queued as a run of its own (`requeue_steer`).
    Steer { thread_id: String, run_id: String, message_id: String },
    /// Close a deleted thread's terminals.
    CloseTerminals { thread_id: String },
    /// A thread settled: stop its stop-on-settle scripts, run the settle script.
    ThreadSettled { thread_id: String },
    /// Write the thread's title with the text generation model (`request_id`
    /// is the thread's `titleRegeneration`; `regenerate` reads the whole thread).
    GenerateTitle { thread_id: String, request_id: String, regenerate: bool },
    /// Rename a new worktree's temporary branch after the thread's first message.
    NameBranch { thread_id: String },
    /// Stop one running subagent (`native_id`: the provider's id for it).
    StopSubagent { thread_id: String, native_id: String },
}

impl Effect {
    /// The thread the effect acts on.
    pub fn thread_id(&self) -> &str {
        match self {
            Effect::StartTurn { thread_id, .. }
            | Effect::RespondToRequest { thread_id, .. }
            | Effect::Interrupt { thread_id, .. }
            | Effect::RuntimeModeChanged { thread_id }
            | Effect::Rollback { thread_id, .. }
            | Effect::Steer { thread_id, .. }
            | Effect::CloseTerminals { thread_id }
            | Effect::ThreadSettled { thread_id }
            | Effect::GenerateTitle { thread_id, .. }
            | Effect::NameBranch { thread_id }
            | Effect::StopSubagent { thread_id, .. } => thread_id,
        }
    }

    /// Whether the effect must run after the thread's earlier effects
    /// (everything that drives the provider turn or the workspace: an
    /// interrupt must not overtake the start of the turn it stops). Text
    /// generation and settle scripts take seconds and depend on no other
    /// effect, so they run alongside.
    pub fn is_ordered(&self) -> bool {
        !matches!(self, Effect::GenerateTitle { .. } | Effect::NameBranch { .. } | Effect::ThreadSettled { .. })
    }

    /// The outbox key of an effect that must happen even across a restart;
    /// `None` for those bound to a live provider process (they mean nothing
    /// once it is gone).
    pub fn outbox_key(&self) -> Option<String> {
        match self {
            Effect::Rollback { command_id, .. } => Some(format!("rollback:{command_id}")),
            Effect::GenerateTitle { request_id, .. } => Some(format!("title:{request_id}")),
            Effect::NameBranch { thread_id } => Some(format!("branch:{thread_id}")),
            Effect::ThreadSettled { thread_id } => Some(format!("settled:{thread_id}")),
            _ => None,
        }
    }
}

/// Drivers whose live turns take more input (Claude's `priority: now`
/// messages, Codex `turn/steer`).
pub fn can_steer(driver: &str) -> bool {
    domovoi_model::ProviderDriver::parse(driver).is_some_and(domovoi_model::ProviderDriver::can_steer)
}

/// Drivers that can continue a copy of another provider thread's conversation
/// (Claude `--fork-session`, Codex `thread/fork`).
pub fn can_native_fork(driver: &str) -> bool {
    domovoi_model::ProviderDriver::parse(driver).is_some_and(domovoi_model::ProviderDriver::can_native_fork)
}

pub struct Orchestrator {
    store: Store,
    /// Commands on one thread run one at a time (each reads the thread,
    /// decides and writes); commands on other threads go on meanwhile.
    threads: ThreadLocks,
    effects: mpsc::UnboundedSender<Effect>,
}

/// One command lock per thread (or per pending launch), dropped from the map
/// once nobody holds or waits for it.
#[derive(Default)]
struct ThreadLocks(std::sync::Mutex<std::collections::HashMap<String, std::sync::Arc<Mutex<()>>>>);

impl ThreadLocks {
    async fn lock(&self, key: &str) -> tokio::sync::OwnedMutexGuard<()> {
        let lock = {
            let mut locks = crate::util::locked(&self.0);
            locks.retain(|_, l| std::sync::Arc::strong_count(l) > 1);
            locks.entry(key.to_owned()).or_default().clone()
        };
        lock.lock_owned().await
    }
}

pub fn driver_for_instance(instance_id: &str) -> String {
    match instance_id {
        "claudeAgent" | "codex" | "opencode" | "pi" => instance_id.to_owned(),
        // An instance settings add runs the driver it names.
        other => crate::providers::instance(other).map_or_else(|| other.to_owned(), |c| c.driver.as_str().to_owned()),
    }
}

fn now() -> Timestamp {
    Timestamp::from(domovoi_contracts::iso_now())
}

pub fn run_id(thread_id: &str, ordinal: u64) -> String {
    format!("run:thread:{}:ordinal:{ordinal}", enc(thread_id))
}
pub fn attempt_id(run_id: &str, n: u64) -> String {
    format!("run-attempt:run:{}:attempt:{n}", enc(run_id))
}
pub fn root_node_id(run_id: &str) -> String {
    format!("node:run:{}:root", enc(run_id))
}
pub fn scope_id(thread_id: &str) -> String {
    format!("checkpoint-scope:thread:{}:name:root", enc(thread_id))
}
pub fn provider_session_id(instance_id: &str, thread_id: &str) -> String {
    if instance_id == "codex" {
        format!("provider-session:provider-instance:{instance_id}:shared")
    } else {
        format!("provider-session:provider-instance:{instance_id}:thread:{}:{}", enc(thread_id), crate::util::new_uuid())
    }
}

/// Next free turn-item ordinal for a run: `runOrdinal * 1e6 + k` (t3code
/// TurnItemPositionStore). The first of the run's, should its items not be readable.
pub fn next_item_ordinal(store: &Store, thread_id: &str, run_ordinal: u64) -> u64 {
    let base = run_ordinal * 1_000_000;
    store.last_item_ordinal(thread_id, base..base + 1_000_000).ok().flatten().map_or(base + 1, |last| last + 1)
}

impl Orchestrator {
    pub fn new(store: Store) -> (Self, mpsc::UnboundedReceiver<Effect>) {
        #[expect(clippy::disallowed_methods, reason = "effects are queued inside a command's transaction, which cannot wait; there are as many as commands accepted")]
        let (tx, rx) = mpsc::unbounded_channel();
        (Orchestrator { store, threads: ThreadLocks::default(), effects: tx }, rx)
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    /// Queues an effect for the worker; one that must survive a restart is
    /// recorded in the outbox first (the worker clears it when done).
    pub fn send_effect(&self, effect: Effect) {
        if let Some(key) = effect.outbox_key()
            && let Err(e) = self.store.outbox_put(&key, &effect)
        {
            tracing::error!(%key, "could not record an effect: {e}");
        }
        // Only once the command that queued it has committed.
        let effects = self.effects.clone();
        self.store.db().after_commit(move || {
            let _ = effects.send(effect);
        });
    }

    /// Startup: queues the effects a stopped server left undone. Returns how many.
    pub fn replay_outbox(&self) -> usize {
        // An entry that no longer decodes is left out there, with a warning.
        let pending: Vec<Effect> = self.store.outbox_all().unwrap_or_default();
        let n = pending.len();
        for effect in pending {
            let _ = self.effects.send(effect);
        }
        n
    }

    /// On whose behalf a command's events are written.
    fn origin(command_id: &CommandId, provider_instance_id: Option<&ProviderInstanceId>) -> Origin {
        Origin {
            driver: None,
            provider_instance_id: provider_instance_id.map(|id| id.as_str().to_owned()),
            command_id: Some(command_id.as_str().to_owned()),
        }
    }

    /// The origin of an event about `run`, written for a command.
    fn run_origin(command_id: &CommandId, run: &Run) -> Origin {
        Self::origin(command_id, Some(&run.provider_instance_id))
    }

    /// The thread's own record, for a command that needs the thread to be there.
    fn thread(&self, thread_id: &ThreadId) -> Result<AppThread, Refusal> {
        let thread = self.store.app_thread(thread_id.as_str())?.ok_or_else(|| Refusal::UnknownThread(thread_id.clone()))?;
        if thread.deleted_at.is_some() {
            return Err(Refusal::ThreadDeleted);
        }
        Ok(thread)
    }

    /// `orchestration.dispatchCommand`: carries the command out once, however
    /// often it is sent, and answers with the sequence of its last event.
    pub async fn dispatch(&self, command: Command) -> Result<DispatchResult, DispatchFailure> {
        let failed = |refusal: Refusal| DispatchFailure {
            command_id: Some(command.command_id().clone()),
            command_type: Some(command.type_name().to_owned()),
            refusal,
        };
        let command_id = command.command_id().as_str();
        let _g = self.threads.lock(command.thread_id().map_or("", ThreadId::as_str)).await;
        if let Some(answered) = self.store.command_receipt::<DispatchResult>(command_id).map_err(|e| failed(e.into()))? {
            return Ok(answered);
        }
        // The command's events, its outbox entries and its receipt commit
        // together: a command that fails part way leaves nothing, and a retry
        // does not repeat what it already wrote.
        self.store
            .db()
            .clone()
            .batch(|| -> Result<DispatchResult, Refusal> {
                let result = self.handle(&command)?;
                self.store.put_command_receipt(command_id, &result)?;
                Ok(result)
            })
            .map_err(failed)
    }

    /// Applies one command (inside `dispatch`'s batch).
    fn handle(&self, command: &Command) -> Result<DispatchResult, Refusal> {
        use domovoi_model::typed::ThreadChange as Change;
        match command {
            Command::MessageDispatch(c) => self.message_dispatch(c),
            Command::ThreadVisit(c) => self.thread_visit(c),
            Command::ThreadRuntimeModeSet(c) => self.edit_thread(&c.thread_id, &c.command_id, Change::RuntimeModeUpdated, true, |t| t.runtime_mode = c.runtime_mode.clone()),
            Command::ThreadInteractionModeSet(c) => {
                self.edit_thread(&c.thread_id, &c.command_id, Change::InteractionModeUpdated, true, |t| t.interaction_mode = c.interaction_mode.clone())
            }
            Command::ThreadProjectAdoptFolder(c) => self.adopt_folder(c),
            Command::ThreadMetadataUpdate(c) => self.metadata_update(c),
            Command::ThreadTitleRegenerationComplete(c) => self.title_regeneration_complete(c),
            Command::ThreadFork(c) => self.fork(fork::Fork::new_thread(c)),
            Command::ThreadMergeBack(c) => self.fork(fork::Fork::merge_back(c)),
            Command::ProviderSwitch(c) => self.model_selection_set(&c.thread_id, &c.command_id, &c.model_selection),
            Command::ThreadModelSelectionSet(c) => self.model_selection_set(&c.thread_id, &c.command_id, &c.model_selection),
            Command::SubagentStop(c) => self.stop_subagent(c),
            Command::ThreadArchive(c) => self.edit_thread(&c.thread_id, &c.command_id, Change::Archived, true, |t| t.archived_at = Some(now())),
            Command::ThreadUnarchive(c) => self.edit_thread(&c.thread_id, &c.command_id, Change::Unarchived, true, |t| t.archived_at = None),
            Command::ThreadDelete(c) => self.delete_thread(c),
            Command::ThreadPin(c) => self.edit_thread(&c.thread_id, &c.command_id, Change::Pinned, true, |t| t.pinned_at = Some(Some(now()))),
            Command::ThreadUnpin(c) => self.edit_thread(&c.thread_id, &c.command_id, Change::Unpinned, true, |t| t.pinned_at = Some(None)),
            Command::ThreadSnooze(c) => self.snooze(c),
            Command::ThreadUnsnooze(c) => self.unsnooze(c),
            Command::ThreadMarkUnread(c) => self.mark_unread(c),
            Command::ThreadSettle(c) => self.settle(&c.thread_id, &c.command_id, true),
            Command::ThreadAutoSettle(c) => self.settle(&c.thread_id, &c.command_id, true),
            Command::ThreadUnsettle(c) => self.settle(&c.thread_id, &c.command_id, false),
            Command::ThreadAutoSettleSet(c) => self.auto_settle_set(c),
            Command::ThreadTagSet(c) => self.tag_set(c),
            Command::ThreadPinReorder(c) => {
                self.edit_thread(&c.thread_id, &c.command_id, Change::PinReordered, true, |t| t.pin_order_key = Some(Some(c.order_key.clone())))
            }
            Command::ThreadActiveReorder(c) => {
                self.edit_thread(&c.thread_id, &c.command_id, Change::ActiveReordered, false, |t| t.active_order_key = Some(Some(c.order_key.clone())))
            }
            Command::QueuedRunCancel(c) => self.queued_cancel(c),
            Command::QueuedRunEdit(c) => self.queued_edit(c),
            Command::QueuedRunReorder(c) => self.queued_reorder(c),
            Command::QueuedRunStart(c) => self.queued_start(c),
            Command::QueueResume(c) => self.queue_resume(c),
            Command::QueuedMessagePromoteToSteer(c) => self.promote_to_steer(c),
            Command::RuntimeRequestRespond(c) => self.respond(&c.thread_id, &c.request_id, Some(c)),
            Command::ThreadUserInputDismiss(c) => self.respond(&c.thread_id, &c.request_id, None),
            Command::ThreadStop(c) => self.interrupt(&c.thread_id, &c.command_id, false),
            Command::RunInterrupt(c) => self.interrupt(&c.thread_id, &c.command_id, c.hold_queue == Some(true)),
            Command::CheckpointRollback(c) => self.rollback(c),
            other => Err(Refusal::Unsupported(other.type_name().to_owned())),
        }
    }

    /// A command's answer: the sequence of its last event (or the log's newest).
    fn answer(&self, last: Option<u64>) -> Result<DispatchResult, Refusal> {
        let sequence = match last {
            Some(sequence) => sequence,
            None => self.store.latest_sequence()?,
        };
        Ok(DispatchResult { sequence })
    }
}
