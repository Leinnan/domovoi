//! The queue: messages sent while the thread was busy wait as queued runs,
//! which can be edited, reordered, cancelled, held and started, and the
//! oldest of which starts when the thread comes free.

use super::message::{Steered, user_turn_item};
use super::{DispatchResult, Effect, Orchestrator, Refusal, driver_for_instance, is_active, now, provider_session_id, root_node_id};
use crate::orchestration::events::{Origin, Outline, enc};
use crate::store::StoreError;
use domovoi_model::typed::command::{QueueResume, QueuedMessagePromoteToSteer, QueuedRunCancel, QueuedRunEdit, QueuedRunReorder, QueuedRunStart};
use domovoi_model::typed::{
    CommandId, ConversationMessage, EventBody, InputIntent, NodeStatus, ProviderInstanceId, ProviderThread, ProviderThreadStatus, Run, RunId,
    RunStatus, ThreadId, Timestamp,
};

/// The provider thread a run is given before the provider has one of its own
/// for it: not loaded, with no native thread yet.
pub(super) fn placeholder_provider_thread(thread_id: &str, instance: &ProviderInstanceId, run_id: &str, ordinal: u64, at: &Timestamp) -> ProviderThread {
    let driver = driver_for_instance(instance.as_str());
    ProviderThread {
        id: format!("provider-thread:provider:{driver}:native-thread:{}", enc(&format!("pending:{run_id}"))).into(),
        driver: driver.into(),
        provider_instance_id: instance.clone(),
        provider_session_id: Some(provider_session_id(instance.as_str(), thread_id).into()),
        app_thread_id: Some(thread_id.into()),
        owner_node_id: None,
        native_thread_ref: None,
        native_conversation_head_ref: None,
        status: ProviderThreadStatus::NotLoaded,
        first_run_ordinal: Some(ordinal),
        last_run_ordinal: Some(ordinal),
        handoff_ids: vec![],
        forked_from: None,
        pending_background_tasks: None,
        context_usage: None,
        native_metadata: None,
        goal: None,
        created_at: at.clone(),
        updated_at: at.clone(),
    }
}

/// The queued runs of `runs` that `keep` lets through, first in the queue first.
fn queued_in_order(runs: &Outline, keep: impl Fn(&Run) -> bool) -> Vec<Run> {
    let mut queued: Vec<Run> = runs.runs.iter().filter(|run| run.status == RunStatus::Queued && keep(run)).map(|run| (**run).clone()).collect();
    queued.sort_by_key(|run| run.queue_position.flatten().unwrap_or(u64::MAX));
    queued
}

impl Orchestrator {
    /// The thread's records and its queued run `run_id`, for a command on that run.
    fn queued_run(&self, thread_id: &ThreadId, run_id: &RunId) -> Result<(Outline, Run), Refusal> {
        self.thread(thread_id)?;
        let outline = self.store.thread_outline(thread_id.as_str())?;
        let run = outline.runs.iter().find(|run| run.id == *run_id).ok_or_else(|| Refusal::UnknownRun(run_id.clone()))?;
        if run.status != RunStatus::Queued {
            return Err(Refusal::RunNotQueued(run_id.clone()));
        }
        let run = (**run).clone();
        Ok((outline, run))
    }

    pub(super) fn queued_cancel(&self, c: &QueuedRunCancel) -> Result<DispatchResult, Refusal> {
        let (outline, run) = self.queued_run(&c.thread_id, &c.run_id)?;
        let origin = Self::run_origin(&c.command_id, &run);
        let sequence = self.cancel_run(c.thread_id.as_str(), &outline, run, &origin)?;
        self.renumber_queue(c.thread_id.as_str(), &c.command_id)?;
        self.answer(Some(sequence))
    }

    /// Cancels a queued run and its root node. Returns the sequence of the last event.
    pub(super) fn cancel_run(&self, thread_id: &str, outline: &Outline, mut run: Run, origin: &Origin) -> Result<u64, StoreError> {
        let at = now();
        run.status = RunStatus::Cancelled;
        run.queue_position = Some(None);
        run.completed_at = Some(at.clone());
        let root = run.root_node_id.clone();
        let mut sequence = self.store.append_event(thread_id, EventBody::RunUpdated(Box::new(run)), origin)?.sequence;
        if let Some(node) = outline.nodes.iter().find(|node| Some(&node.id) == root.as_ref()) {
            let mut node = (**node).clone();
            node.status = NodeStatus::Cancelled;
            node.completed_at = Some(at);
            sequence = self.store.append_event(thread_id, EventBody::NodeUpdated(Box::new(node)), origin)?.sequence;
        }
        Ok(sequence)
    }

    /// Positions 1..n in the current order (a restart's 0 stays first).
    pub(super) fn renumber_queue(&self, thread_id: &str, command_id: &CommandId) -> Result<(), StoreError> {
        let outline = self.store.thread_outline(thread_id)?;
        self.write_order(thread_id, command_id, queued_in_order(&outline, |_| true))
    }

    /// Gives the runs the positions 1..n in the order given, writing those that move.
    fn write_order(&self, thread_id: &str, command_id: &CommandId, order: Vec<Run>) -> Result<(), StoreError> {
        for (at, mut run) in order.into_iter().enumerate() {
            let position = Some(Some(at as u64 + 1));
            if run.queue_position != position {
                run.queue_position = position;
                let origin = Self::run_origin(command_id, &run);
                self.store.append_event(thread_id, EventBody::RunUpdated(Box::new(run)), &origin)?;
            }
        }
        Ok(())
    }

    /// The message a queued run will send.
    fn message_of(&self, thread_id: &ThreadId, run: &Run) -> Result<ConversationMessage, Refusal> {
        self.store.record::<ConversationMessage>(thread_id.as_str(), run.user_message_id.as_str())?.ok_or(Refusal::QueuedRunWithoutMessage)
    }

    /// New text (and attachments, when given) for a queued message.
    pub(super) fn queued_edit(&self, c: &QueuedRunEdit) -> Result<DispatchResult, Refusal> {
        let (_, run) = self.queued_run(&c.thread_id, &c.run_id)?;
        let mut message = self.message_of(&c.thread_id, &run)?;
        message.text = c.text.clone();
        if let Some(attachments) = &c.attachments {
            message.attachments = attachments.clone();
        }
        if let Some(context) = &c.context {
            message.context = Some(context.clone());
        }
        message.updated_at = now();
        let origin = Self::run_origin(&c.command_id, &run);
        let sequence = self.store.append_event(c.thread_id.as_str(), EventBody::MessageUpdated(Box::new(message)), &origin)?.sequence;
        self.answer(Some(sequence))
    }

    /// Moves a queued run before another (or to the end).
    pub(super) fn queued_reorder(&self, c: &QueuedRunReorder) -> Result<DispatchResult, Refusal> {
        let (outline, run) = self.queued_run(&c.thread_id, &c.run_id)?;
        let mut queued = queued_in_order(&outline, |other| other.id != run.id);
        let at = match &c.before_run_id {
            Some(before) => queued.iter().position(|other| other.id == *before).ok_or_else(|| Refusal::RunNotQueued(before.clone()))?,
            None => queued.len(),
        };
        queued.insert(at, run);
        self.write_order(c.thread_id.as_str(), &c.command_id, queued)?;
        self.answer(None)
    }

    /// Sends a queued run next (and starts it if the thread is idle).
    pub(super) fn queued_start(&self, c: &QueuedRunStart) -> Result<DispatchResult, Refusal> {
        let (outline, mut run) = self.queued_run(&c.thread_id, &c.run_id)?;
        let mut queued = queued_in_order(&outline, |other| other.id != run.id);
        if run.queue_held == Some(true) {
            run.queue_held = Some(false);
            run.queue_position = Some(None);
        }
        queued.insert(0, run);
        self.write_order(c.thread_id.as_str(), &c.command_id, queued)?;
        self.start_queued_locked(c.thread_id.as_str())?;
        self.answer(None)
    }

    /// Releases a queue a stop held, and starts it if the thread is idle.
    pub(super) fn queue_resume(&self, c: &QueueResume) -> Result<DispatchResult, Refusal> {
        self.thread(&c.thread_id)?;
        let outline = self.store.thread_outline(c.thread_id.as_str())?;
        for held in outline.runs.iter().filter(|run| run.status == RunStatus::Queued && run.queue_held == Some(true)) {
            let mut run = (**held).clone();
            run.queue_held = Some(false);
            let origin = Self::run_origin(&c.command_id, held);
            self.store.append_event(c.thread_id.as_str(), EventBody::RunUpdated(Box::new(run)), &origin)?;
        }
        self.start_queued_locked(c.thread_id.as_str())?;
        self.answer(None)
    }

    /// A queued message joins the running turn instead of waiting.
    pub(super) fn promote_to_steer(&self, c: &QueuedMessagePromoteToSteer) -> Result<DispatchResult, Refusal> {
        let (outline, run) = self.queued_run(&c.thread_id, &c.queued_run_id)?;
        let target = super::message::steerable_run(&outline, &c.target_run_id).ok_or(Refusal::TargetNotSteerable)?;
        let message = self.message_of(&c.thread_id, &run)?;
        let thread_id = c.thread_id.as_str();
        self.cancel_run(thread_id, &outline, run, &Self::run_origin(&c.command_id, &target))?;
        self.renumber_queue(thread_id, &c.command_id)?;
        let outline = self.store.thread_outline(thread_id)?;
        let steered = Steered {
            message_id: &message.id,
            text: &message.text,
            attachments: &message.attachments,
            created_by: &message.created_by,
            creation_source: &message.creation_source,
            intent: InputIntent::PromotedQueuedToSteer,
        };
        self.steer_into(&c.command_id, &c.thread_id, steered, &outline, &target)
    }

    /// Starts the oldest queued run of a thread if nothing is active (called by
    /// the effect worker when a run settles).
    pub async fn start_next_queued(&self, thread_id: &str) -> Result<Option<String>, StoreError> {
        let _g = self.threads.lock(thread_id).await;
        self.start_queued_locked(thread_id)
    }

    /// `start_next_queued` with the command lock already held.
    pub(super) fn start_queued_locked(&self, thread_id: &str) -> Result<Option<String>, StoreError> {
        let outline = self.store.thread_outline(thread_id)?;
        let Some(thread) = self.store.app_thread(thread_id)? else { return Ok(None) };
        if thread.deleted_at.is_some() || outline.runs.iter().any(|run| is_active(&run.status)) {
            return Ok(None);
        }
        let waiting = outline.runs.iter().filter(|run| run.status == RunStatus::Queued && run.queue_held != Some(true));
        let Some(next) = waiting.min_by_key(|run| run.queue_position.flatten().unwrap_or(u64::MAX)) else { return Ok(None) };
        let run_id = next.id.as_str();
        let (ordinal, instance) = (next.ordinal, &next.provider_instance_id);
        let at = now();
        let origin = Origin { provider_instance_id: Some(instance.as_str().to_owned()), ..Default::default() };
        // The thread's provider thread for this instance, or a new one not yet loaded.
        let held = outline.provider_threads.iter().find(|pt| thread.active_provider_thread_id.as_ref() == Some(&pt.id) && pt.provider_instance_id == *instance);
        let mut provider_thread = match held {
            Some(held) => (**held).clone(),
            None => placeholder_provider_thread(thread_id, instance, run_id, ordinal, &at),
        };
        provider_thread.last_run_ordinal = Some(ordinal);
        provider_thread.updated_at = at.clone();
        let provider_thread_id = provider_thread.id.clone();
        self.store.append_event(thread_id, EventBody::ProviderThreadUpdated(Box::new(provider_thread)), &origin)?;
        let mut run = (**next).clone();
        run.status = RunStatus::Starting;
        run.queue_position = Some(None);
        run.provider_thread_id = Some(provider_thread_id.clone());
        self.store.append_event(thread_id, EventBody::RunUpdated(Box::new(run)), &origin)?;
        let message = self
            .store
            .record::<ConversationMessage>(thread_id, next.user_message_id.as_str())?
            .ok_or_else(|| StoreError::Invalid(format!("the queued run {run_id} has no message to start with")))?;
        let sent = Steered {
            message_id: &message.id,
            text: &message.text,
            attachments: &message.attachments,
            created_by: &message.created_by,
            creation_source: &message.creation_source,
            intent: InputIntent::QueuedTurn,
        };
        let item = user_turn_item(thread_id, run_id, &root_node_id(run_id), Some(&provider_thread_id), ordinal, sent, &at);
        self.store.append_event(thread_id, EventBody::TurnItemUpdated(item), &origin)?;
        self.send_effect(Effect::StartTurn { thread_id: thread_id.to_owned(), run_id: run_id.to_owned() });
        Ok(Some(run_id.to_owned()))
    }
}
