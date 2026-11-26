//! One thread held in memory and kept current, event by event.
//!
//! The rules are those of t3code's projection, for threads without run-forks:
//!
//! - an event carries the whole new version of one record, which takes the
//!   place of the version held, or joins its collection if it is new;
//! - an event of the thread itself replaces the thread's record;
//! - every other event moves the thread's `updatedAt` to when it occurred;
//! - a provider thread that belongs to this thread and is not a placeholder
//!   becomes the thread's active provider thread, and the thread takes its
//!   `updatedAt`;
//! - a detached provider session leaves its collection;
//! - an event that carries no record of a collection (a rollback request, an
//!   event of a type this build does not know) changes nothing else.

use super::checkpoint::{Checkpoint, CheckpointScope};
use super::context::{ContextHandoff, ContextTransfer};
use super::event::{DomainEvent, EventBody};
use super::ids::{MessageId, TurnItemId};
use super::message::ConversationMessage;
use super::plan::PlanArtifact;
use super::provider::{ProviderSession, ProviderThread, ProviderTurn, RuntimeRequest};
use super::pull_request::PullRequestLink;
use super::record::{Put, Record, RecordRef, Records, StoredRecord};
use super::run::{ExecutionNode, Run, RunAttempt, Subagent};
use super::stream::{ThreadProjection, ThreadSnapshot};
use super::thread::AppThread;
use super::common::Timestamp;
use super::turn_item::{Appended, TurnItem};

/// A thread: its own record and every record that belongs to it, each at its
/// latest version.
#[derive(Clone, Debug, PartialEq)]
pub struct Thread {
    /// The thread's own record.
    pub record: AppThread,
    pub runs: Records<Run>,
    pub attempts: Records<RunAttempt>,
    pub nodes: Records<ExecutionNode>,
    pub subagents: Records<Subagent>,
    pub provider_sessions: Records<ProviderSession>,
    pub provider_threads: Records<ProviderThread>,
    pub provider_turns: Records<ProviderTurn>,
    pub runtime_requests: Records<RuntimeRequest>,
    pub messages: Records<ConversationMessage>,
    pub plans: Records<PlanArtifact>,
    /// The timeline, in order.
    pub turn_items: Records<TurnItem>,
    pub checkpoint_scopes: Records<CheckpointScope>,
    pub checkpoints: Records<Checkpoint>,
    pub context_handoffs: Records<ContextHandoff>,
    pub context_transfers: Records<ContextTransfer>,
    /// The last event the snapshot this thread was built from included; 0 for
    /// a thread built from stored records.
    pub snapshot_sequence: u64,
}

/// What applying an event changed in a thread.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Applied {
    /// The thread's own record was replaced.
    Thread,
    /// A record joined its collection. `order_changed` if it went in before
    /// records already there, rather than at the end.
    Added { record: RecordRef, order_changed: bool },
    /// A record took the place of its earlier version.
    Replaced {
        record: RecordRef,
        /// The record moved to another place in its collection.
        order_changed: bool,
        /// The new version is the old one with more of the text that grows
        /// while a reply streams, and a later `updatedAt`: nothing else
        /// differs. Only ever `true` for a message, or for a turn item that
        /// is an assistant message, reasoning, a proposed plan, or a command
        /// that already had output.
        only_text_grew: bool,
    },
    /// A record left its collection.
    Removed { record: RecordRef },
    /// No record changed; the thread's `updatedAt` moved to when the event occurred.
    Touched,
    /// The event changed nothing the thread holds.
    Nothing,
}

/// Puts `record` into `records` and says what that did.
fn put<T: Record>(records: &mut Records<T>, record: T, reference: fn(T::Id) -> RecordRef) -> Applied {
    let record_ref = reference(record.id().clone());
    match records.put(record) {
        Put::Added { at_end } => Applied::Added { record: record_ref, order_changed: !at_end },
        Put::Replaced { moved, only_text_grew } => Applied::Replaced { record: record_ref, order_changed: moved, only_text_grew },
    }
}

impl Thread {
    /// A thread with no records but its own.
    pub fn new(record: AppThread) -> Self {
        Self {
            record,
            runs: Records::default(),
            attempts: Records::default(),
            nodes: Records::default(),
            subagents: Records::default(),
            provider_sessions: Records::default(),
            provider_threads: Records::default(),
            provider_turns: Records::default(),
            runtime_requests: Records::default(),
            messages: Records::default(),
            plans: Records::default(),
            turn_items: Records::default(),
            checkpoint_scopes: Records::default(),
            checkpoints: Records::default(),
            context_handoffs: Records::default(),
            context_transfers: Records::default(),
            snapshot_sequence: 0,
        }
    }

    /// The thread a subscription's snapshot describes.
    pub fn from_snapshot(snapshot: ThreadSnapshot) -> Self {
        Self { snapshot_sequence: snapshot.snapshot_sequence, ..Self::from_projection(snapshot.projection) }
    }

    /// The thread a projection describes.
    pub fn from_projection(projection: ThreadProjection) -> Self {
        let turn_items = projection.turn_items;
        Self {
            record: projection.thread,
            runs: Records::from_shared(projection.runs),
            attempts: Records::from_shared(projection.attempts),
            nodes: Records::from_shared(projection.nodes),
            subagents: Records::from_shared(projection.subagents),
            provider_sessions: Records::from_shared(projection.provider_sessions),
            provider_threads: Records::from_shared(projection.provider_threads),
            provider_turns: Records::from_shared(projection.provider_turns),
            runtime_requests: Records::from_shared(projection.runtime_requests),
            messages: Records::from_shared(projection.messages),
            plans: Records::from_shared(projection.plans),
            turn_items: Records::from_shared(turn_items),
            checkpoint_scopes: Records::from_shared(projection.checkpoint_scopes),
            checkpoints: Records::from_shared(projection.checkpoints),
            context_handoffs: Records::from_shared(projection.context_handoffs),
            context_transfers: Records::from_shared(projection.context_transfers),
            snapshot_sequence: 0,
        }
    }

    /// The thread the server stores as a thread record and rows of records,
    /// given in the order the records first appeared.
    pub fn from_records(record: AppThread, records: impl IntoIterator<Item = StoredRecord>) -> Self {
        let mut projection = ThreadProjection {
            updated_at: record.updated_at.clone(),
            thread: record,
            runs: Vec::new(),
            attempts: Vec::new(),
            nodes: Vec::new(),
            subagents: Vec::new(),
            provider_sessions: Vec::new(),
            provider_threads: Vec::new(),
            provider_turns: Vec::new(),
            runtime_requests: Vec::new(),
            messages: Vec::new(),
            plans: Vec::new(),
            turn_items: Vec::new(),
            checkpoint_scopes: Vec::new(),
            checkpoints: Vec::new(),
            context_handoffs: Vec::new(),
            context_transfers: Vec::new(),
        };
        for record in records {
            match record {
                StoredRecord::Run(run) => projection.runs.push(run),
                StoredRecord::Attempt(attempt) => projection.attempts.push(attempt),
                StoredRecord::Node(node) => projection.nodes.push(node),
                StoredRecord::Subagent(subagent) => projection.subagents.push(subagent),
                StoredRecord::ProviderSession(session) => projection.provider_sessions.push(session),
                StoredRecord::ProviderThread(provider_thread) => projection.provider_threads.push(provider_thread),
                StoredRecord::ProviderTurn(turn) => projection.provider_turns.push(turn),
                StoredRecord::RuntimeRequest(request) => projection.runtime_requests.push(request),
                StoredRecord::Message(message) => projection.messages.push(message),
                StoredRecord::Plan(plan) => projection.plans.push(plan),
                StoredRecord::TurnItem(item) => projection.turn_items.push(item),
                StoredRecord::CheckpointScope(scope) => projection.checkpoint_scopes.push(scope),
                StoredRecord::Checkpoint(checkpoint) => projection.checkpoints.push(checkpoint),
                StoredRecord::ContextHandoff(handoff) => projection.context_handoffs.push(handoff),
                StoredRecord::ContextTransfer(transfer) => projection.context_transfers.push(transfer),
            }
        }
        Self::from_projection(projection)
    }

    /// Applies one event, taking its record into the thread, and says what changed.
    pub fn apply(&mut self, event: DomainEvent) -> Applied {
        let applied = match event.body {
            EventBody::Thread { thread, .. } => {
                self.record = *thread;
                return Applied::Thread;
            }
            EventBody::ThreadPullRequestLinkUpdated(link) => return self.replace_pull_request_link(*link),
            EventBody::RunCreated(run) | EventBody::RunUpdated(run) => put(&mut self.runs, *run, RecordRef::Run),
            EventBody::RunBackgroundWorkCancelled(cancelled) => {
                let changed = self.runs.change(&cancelled.run_id, |run| {
                    run.restart_cancelled_background_work = Some(cancelled.restart_cancelled_background_work);
                });
                if changed {
                    Applied::Replaced { record: RecordRef::Run(cancelled.run_id), order_changed: false, only_text_grew: false }
                } else {
                    Applied::Touched
                }
            }
            EventBody::RunAttemptCreated(attempt) | EventBody::RunAttemptUpdated(attempt) => {
                put(&mut self.attempts, *attempt, RecordRef::Attempt)
            }
            EventBody::NodeUpdated(node) => put(&mut self.nodes, *node, RecordRef::Node),
            EventBody::SubagentUpdated(subagent) => put(&mut self.subagents, *subagent, RecordRef::Subagent),
            EventBody::ProviderSessionAttached(session) | EventBody::ProviderSessionUpdated(session) => {
                put(&mut self.provider_sessions, *session, RecordRef::ProviderSession)
            }
            EventBody::ProviderSessionDetached(detached) => match self.provider_sessions.remove(&detached.provider_session_id) {
                Some(_) => Applied::Removed { record: RecordRef::ProviderSession(detached.provider_session_id) },
                None => Applied::Touched,
            },
            EventBody::ProviderThreadUpdated(provider_thread) => {
                let active = provider_thread.app_thread_id.as_ref() == Some(&self.record.id) && !provider_thread.is_placeholder();
                let (id, updated_at) = (provider_thread.id.clone(), provider_thread.updated_at.clone());
                let applied = put(&mut self.provider_threads, *provider_thread, RecordRef::ProviderThread);
                self.record.updated_at = event.occurred_at;
                if active {
                    self.record.active_provider_thread_id = Some(id);
                    self.record.updated_at = updated_at;
                }
                return applied;
            }
            EventBody::ProviderTurnUpdated(turn) => put(&mut self.provider_turns, *turn, RecordRef::ProviderTurn),
            EventBody::RuntimeRequestUpdated(request) => put(&mut self.runtime_requests, *request, RecordRef::RuntimeRequest),
            EventBody::MessageUpdated(message) => put(&mut self.messages, *message, RecordRef::Message),
            EventBody::TurnItemUpdated(item) => put(&mut self.turn_items, item, RecordRef::TurnItem),
            EventBody::PlanUpdated(plan) => put(&mut self.plans, *plan, RecordRef::Plan),
            EventBody::CheckpointScopeCreated(scope) => put(&mut self.checkpoint_scopes, *scope, RecordRef::CheckpointScope),
            EventBody::CheckpointCaptured(checkpoint) => put(&mut self.checkpoints, *checkpoint, RecordRef::Checkpoint),
            EventBody::ContextHandoffUpdated(handoff) => put(&mut self.context_handoffs, *handoff, RecordRef::ContextHandoff),
            EventBody::ContextTransferCreated(transfer) | EventBody::ContextTransferUpdated(transfer) => {
                put(&mut self.context_transfers, *transfer, RecordRef::ContextTransfer)
            }
            EventBody::CheckpointRollbackRequested(_) | EventBody::Unknown(_) => Applied::Touched,
        };
        self.record.updated_at = event.occurred_at;
        applied
    }

    /// Adds `text` to the growing text of the turn item `item`, which must be
    /// `at` bytes long ([`ThreadStreamItem::Append`]), and says what changed:
    /// the item with more text, nothing (the thread already held the text),
    /// or `None` when the thread does not hold what the text follows. Then
    /// something was missed, and the thread is to be read again.
    pub fn append(&mut self, item: &TurnItemId, at: u64, text: &str, updated_at: &Timestamp) -> Option<Applied> {
        let mut appended = None;
        let known = self.turn_items.change(item, |held| appended = Some(held.append(usize::try_from(at).unwrap_or(usize::MAX), text, updated_at)));
        match (known, appended) {
            (true, Some(Appended::Added)) => {
                self.record.updated_at = updated_at.clone();
                Some(Applied::Replaced { record: RecordRef::TurnItem(item.clone()), order_changed: false, only_text_grew: true })
            }
            (true, Some(Appended::AlreadyThere)) => Some(Applied::Nothing),
            _ => None,
        }
    }

    /// Replaces the thread's link to the same pull request as `link` (same
    /// host, repository and number, as written) with `link`.
    fn replace_pull_request_link(&mut self, link: PullRequestLink) -> Applied {
        let same = |held: &&mut PullRequestLink| held.host == link.host && held.repository == link.repository && held.number == link.number;
        match self.record.pull_requests.iter_mut().flatten().find(same) {
            Some(held) => {
                *held = link;
                Applied::Thread
            }
            None => Applied::Nothing,
        }
    }

    /// Everything the thread consists of, as the server sends it.
    pub fn projection(&self) -> ThreadProjection {
        ThreadProjection {
            thread: self.record.clone(),
            runs: self.runs.as_slice().to_vec(),
            attempts: self.attempts.as_slice().to_vec(),
            nodes: self.nodes.as_slice().to_vec(),
            subagents: self.subagents.as_slice().to_vec(),
            provider_sessions: self.provider_sessions.as_slice().to_vec(),
            provider_threads: self.provider_threads.as_slice().to_vec(),
            provider_turns: self.provider_turns.as_slice().to_vec(),
            runtime_requests: self.runtime_requests.as_slice().to_vec(),
            messages: self.messages.as_slice().to_vec(),
            plans: self.plans.as_slice().to_vec(),
            turn_items: self.turn_items.as_slice().to_vec(),
            checkpoint_scopes: self.checkpoint_scopes.as_slice().to_vec(),
            checkpoints: self.checkpoints.as_slice().to_vec(),
            context_handoffs: self.context_handoffs.as_slice().to_vec(),
            context_transfers: self.context_transfers.as_slice().to_vec(),
            updated_at: self.record.updated_at.clone(),
        }
    }

    /// The newest turn item that shows the message `message`: the row a
    /// streaming reply grows in. Searched from the end of the timeline, where
    /// a streaming reply is.
    pub fn turn_item_of_message(&self, message: &MessageId) -> Option<&TurnItem> {
        self.turn_items.iter().rev().find(|item| item.message_id() == Some(message))
    }
}
