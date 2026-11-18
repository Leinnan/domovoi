//! Orchestration events: what the server records and streams as a thread changes.
//!
//! Every event has the same envelope (which thread, when, on whose behalf)
//! and a `type` that says what its `payload` is: nearly always the whole new
//! version of one record.
//!
//! Decoding is written by hand, like a turn item's: the event is read once
//! into its fields and each is moved into place, so a record's text is never
//! copied. An event of a known type must decode in full. One of a type this
//! build does not know still needs the envelope; everything else it carries is
//! kept and written back as it came.
//!
//! An event that is already parsed JSON is best decoded with
//! [`DomainEvent::from_value`], which takes the JSON apart. `Deserialize` on a
//! `serde_json::Value` gives the same event but rebuilds the JSON first.

use super::checkpoint::{Checkpoint, CheckpointScope};
use super::common::Timestamp;
use super::context::{ContextHandoff, ContextTransfer};
use super::ids::{
    CheckpointId, CheckpointScopeId, CommandId, EventId, NodeId, ProviderDriverKind, ProviderInstanceId, ProviderSessionId,
    RawEventId, RunId, ThreadId,
};
use super::message::ConversationMessage;
use super::plan::PlanArtifact;
use super::provider::{ProviderSession, ProviderThread, ProviderTurn, RuntimeRequest};
use super::pull_request::PullRequestLink;
use super::record::{Record, RecordRef};
use super::run::{CancelledBackgroundWork, ExecutionNode, Run, RunAttempt, Subagent};
use super::thread::AppThread;
use super::turn_item::TurnItem;
use super::wire::{optional, required};
use serde::de::Error;
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};

/// One change to a thread, as the server recorded it (`OrchestrationV2DomainEvent`).
#[derive(Clone, Debug, PartialEq)]
pub struct DomainEvent {
    pub id: EventId,
    pub thread_id: ThreadId,
    pub run_id: Option<RunId>,
    pub node_id: Option<NodeId>,
    /// The driver of the provider the event came from, if it came from one.
    pub driver: Option<ProviderDriverKind>,
    pub provider_instance_id: Option<ProviderInstanceId>,
    pub raw_event_id: Option<RawEventId>,
    pub occurred_at: Timestamp,
    pub body: EventBody,
}

impl DomainEvent {
    /// The wire name of the event's type, such as `turn-item.updated`.
    pub fn event_type(&self) -> &str {
        self.body.event_type()
    }

    /// The record this event carries the new version of, if it carries one of
    /// a thread's collections.
    pub fn record(&self) -> Option<RecordRef> {
        self.body.record()
    }

    /// The event that parsed JSON holds. The JSON is taken apart, not read:
    /// nothing in it is copied.
    pub fn from_value(event: Value) -> serde_json::Result<Self> {
        match event {
            Value::Object(fields) => Self::from_fields(fields),
            _ => Err(serde_json::Error::custom("an event is an object")),
        }
    }

    /// The event whose JSON object held `fields`.
    pub(crate) fn from_fields<E: Error>(mut fields: Map<String, Value>) -> Result<Self, E> {
        let event_type = match fields.remove("type") {
            Some(Value::String(event_type)) => event_type,
            Some(_) => return Err(E::custom("`type` is not a string")),
            None => return Err(E::missing_field("type")),
        };
        let in_event = |error: serde_json::Error| E::custom(format_args!("event `{event_type}`: {error}"));
        Ok(Self {
            id: required(&mut fields, "id").map_err(in_event)?,
            thread_id: required(&mut fields, "threadId").map_err(in_event)?,
            run_id: optional(&mut fields, "runId").map_err(in_event)?,
            node_id: optional(&mut fields, "nodeId").map_err(in_event)?,
            driver: optional(&mut fields, "driver").map_err(in_event)?,
            provider_instance_id: optional(&mut fields, "providerInstanceId").map_err(in_event)?,
            raw_event_id: optional(&mut fields, "rawEventId").map_err(in_event)?,
            occurred_at: required(&mut fields, "occurredAt").map_err(in_event)?,
            body: EventBody::decode(&event_type, fields).map_err(in_event)?,
        })
    }
}

impl Serialize for DomainEvent {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut event = serializer.serialize_map(None)?;
        event.serialize_entry("id", &self.id)?;
        event.serialize_entry("threadId", &self.thread_id)?;
        if let Some(run_id) = &self.run_id {
            event.serialize_entry("runId", run_id)?;
        }
        if let Some(node_id) = &self.node_id {
            event.serialize_entry("nodeId", node_id)?;
        }
        if let Some(driver) = &self.driver {
            event.serialize_entry("driver", driver)?;
        }
        if let Some(provider_instance_id) = &self.provider_instance_id {
            event.serialize_entry("providerInstanceId", provider_instance_id)?;
        }
        if let Some(raw_event_id) = &self.raw_event_id {
            event.serialize_entry("rawEventId", raw_event_id)?;
        }
        event.serialize_entry("occurredAt", &self.occurred_at)?;
        event.serialize_entry("type", self.event_type())?;
        self.body.serialize_payload(&mut event)?;
        event.end()
    }
}

impl<'de> Deserialize<'de> for DomainEvent {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::from_fields(Map::deserialize(deserializer)?)
    }
}

/// An event with its place in the server's log (`OrchestrationV2StoredEvent`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredEvent {
    pub sequence: u64,
    /// The command that caused the event, if a command did.
    pub command_id: Option<CommandId>,
    pub event: DomainEvent,
}

/// Declares the events that carry a payload of their own type: the wire name,
/// the variant, and what the payload decodes as. The thread's own events, all
/// carrying the thread record, are [`EventBody::Thread`].
macro_rules! event_bodies {
    ($($(#[$meta:meta])* $tag:literal => $variant:ident($payload:ty),)+) => {
        /// What an event says: its type and the payload that type carries.
        #[derive(Clone, Debug, PartialEq)]
        pub enum EventBody {
            /// The thread's own record after `change`.
            Thread { change: ThreadChange, thread: Box<AppThread> },
            $($(#[$meta])* $variant($payload),)+
            /// An event of a type this build does not know, kept whole.
            Unknown(Box<UnknownEvent>),
        }

        impl EventBody {
            /// The wire name of the event's type.
            pub fn event_type(&self) -> &str {
                match self {
                    Self::Thread { change, .. } => change.event_type(),
                    $(Self::$variant(_) => $tag,)+
                    Self::Unknown(unknown) => unknown.event_type(),
                }
            }

            /// Whether this build knows the event type `event_type`.
            pub fn knows(event_type: &str) -> bool {
                ThreadChange::of_event_type(event_type).is_some() || matches!(event_type, $($tag)|+)
            }

            /// The body of an event of type `event_type`, from what the event
            /// carries besides its envelope.
            fn decode(event_type: &str, mut fields: Map<String, Value>) -> serde_json::Result<Self> {
                if let Some(change) = ThreadChange::of_event_type(event_type) {
                    return Ok(Self::Thread { change, thread: required(&mut fields, "payload")? });
                }
                match event_type {
                    $($tag => required(&mut fields, "payload").map(Self::$variant),)+
                    _ => Ok(Self::Unknown(Box::new(UnknownEvent { event_type: event_type.into(), rest: fields }))),
                }
            }

            fn serialize_payload<M: SerializeMap>(&self, event: &mut M) -> Result<(), M::Error> {
                match self {
                    Self::Thread { thread, .. } => event.serialize_entry("payload", thread),
                    $(Self::$variant(payload) => event.serialize_entry("payload", payload),)+
                    Self::Unknown(unknown) => unknown.rest.iter().try_for_each(|(key, value)| event.serialize_entry(key, value)),
                }
            }
        }
    };
}

event_bodies! {
    /// A pull request linked to the thread has new state.
    "thread.pull-request-link-updated" => ThreadPullRequestLinkUpdated(Box<PullRequestLink>),
    "run.created" => RunCreated(Box<Run>),
    "run.updated" => RunUpdated(Box<Run>),
    "run.background-work-cancelled" => RunBackgroundWorkCancelled(RunBackgroundWorkCancelled),
    "run-attempt.created" => RunAttemptCreated(Box<RunAttempt>),
    "run-attempt.updated" => RunAttemptUpdated(Box<RunAttempt>),
    "node.updated" => NodeUpdated(Box<ExecutionNode>),
    "subagent.updated" => SubagentUpdated(Box<Subagent>),
    "provider-session.attached" => ProviderSessionAttached(Box<ProviderSession>),
    "provider-session.updated" => ProviderSessionUpdated(Box<ProviderSession>),
    "provider-session.detached" => ProviderSessionDetached(ProviderSessionDetached),
    "provider-thread.updated" => ProviderThreadUpdated(Box<ProviderThread>),
    "provider-turn.updated" => ProviderTurnUpdated(Box<ProviderTurn>),
    "runtime-request.updated" => RuntimeRequestUpdated(Box<RuntimeRequest>),
    "message.updated" => MessageUpdated(Box<ConversationMessage>),
    "turn-item.updated" => TurnItemUpdated(TurnItem),
    "plan.updated" => PlanUpdated(Box<PlanArtifact>),
    "checkpoint-scope.created" => CheckpointScopeCreated(Box<CheckpointScope>),
    "checkpoint.captured" => CheckpointCaptured(Box<Checkpoint>),
    /// The user asked to go back to a checkpoint; no record changes with it.
    "checkpoint.rollback-requested" => CheckpointRollbackRequested(CheckpointRollbackRequest),
    "context-handoff.updated" => ContextHandoffUpdated(Box<ContextHandoff>),
    "context-transfer.created" => ContextTransferCreated(Box<ContextTransfer>),
    "context-transfer.updated" => ContextTransferUpdated(Box<ContextTransfer>),
}

impl EventBody {
    /// The record this event carries the new version of, if it carries one of
    /// a thread's collections.
    pub fn record(&self) -> Option<RecordRef> {
        Some(match self {
            Self::RunCreated(run) | Self::RunUpdated(run) => RecordRef::Run(run.id.clone()),
            Self::RunAttemptCreated(attempt) | Self::RunAttemptUpdated(attempt) => RecordRef::Attempt(attempt.id.clone()),
            Self::NodeUpdated(node) => RecordRef::Node(node.id.clone()),
            Self::SubagentUpdated(subagent) => RecordRef::Subagent(subagent.id.clone()),
            Self::ProviderSessionAttached(session) | Self::ProviderSessionUpdated(session) => {
                RecordRef::ProviderSession(session.id.clone())
            }
            Self::ProviderThreadUpdated(provider_thread) => RecordRef::ProviderThread(provider_thread.id.clone()),
            Self::ProviderTurnUpdated(turn) => RecordRef::ProviderTurn(turn.id.clone()),
            Self::RuntimeRequestUpdated(request) => RecordRef::RuntimeRequest(request.id.clone()),
            Self::MessageUpdated(message) => RecordRef::Message(message.id.clone()),
            Self::TurnItemUpdated(item) => RecordRef::TurnItem(item.id().clone()),
            Self::PlanUpdated(plan) => RecordRef::Plan(Record::id(&**plan).clone()),
            Self::CheckpointScopeCreated(scope) => RecordRef::CheckpointScope(scope.id.clone()),
            Self::CheckpointCaptured(checkpoint) => RecordRef::Checkpoint(checkpoint.id.clone()),
            Self::ContextHandoffUpdated(handoff) => RecordRef::ContextHandoff(handoff.id.clone()),
            Self::ContextTransferCreated(transfer) | Self::ContextTransferUpdated(transfer) => {
                RecordRef::ContextTransfer(transfer.id.clone())
            }
            Self::Thread { .. }
            | Self::ThreadPullRequestLinkUpdated(_)
            | Self::RunBackgroundWorkCancelled(_)
            | Self::ProviderSessionDetached(_)
            | Self::CheckpointRollbackRequested(_)
            | Self::Unknown(_) => return None,
        })
    }
}

/// Declares the changes to a thread's own record, each with its event type.
macro_rules! thread_changes {
    ($($variant:ident = $tag:literal,)+) => {
        /// What happened to a thread's own record, for the events that carry
        /// the record as it is afterwards.
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum ThreadChange {
            $($variant,)+
        }

        impl ThreadChange {
            /// The wire name of the event type, such as `thread.archived`.
            pub const fn event_type(self) -> &'static str {
                match self {
                    $(Self::$variant => $tag,)+
                }
            }

            /// The change the event type `event_type` stands for.
            pub fn of_event_type(event_type: &str) -> Option<Self> {
                match event_type {
                    $($tag => Some(Self::$variant),)+
                    _ => None,
                }
            }
        }
    };
}

thread_changes! {
    Created = "thread.created",
    Archived = "thread.archived",
    Unarchived = "thread.unarchived",
    Deleted = "thread.deleted",
    Settled = "thread.settled",
    Unsettled = "thread.unsettled",
    Snoozed = "thread.snoozed",
    Unsnoozed = "thread.unsnoozed",
    Pinned = "thread.pinned",
    AutoSettleSet = "thread.auto-settle-set",
    Unpinned = "thread.unpinned",
    PinReordered = "thread.pin-reordered",
    ActiveReordered = "thread.active-reordered",
    TagsUpdated = "thread.tags-updated",
    Visited = "thread.visited",
    MarkedUnread = "thread.marked-unread",
    MetadataUpdated = "thread.metadata-updated",
    PullRequestSynced = "thread.pull-request-synced",
    RuntimeModeUpdated = "thread.runtime-mode-updated",
    InteractionModeUpdated = "thread.interaction-mode-updated",
    ModelSelectionUpdated = "thread.model-selection-updated",
    ProviderSwitched = "thread.provider-switched",
    GuestShareUpdated = "thread.guest-share-updated",
}

/// An event of a type this build does not know: its type, and everything it
/// carries besides the envelope, as it was sent.
#[derive(Clone, Debug, PartialEq)]
pub struct UnknownEvent {
    event_type: Box<str>,
    rest: Map<String, Value>,
}

impl UnknownEvent {
    /// The wire name of the event's type.
    pub fn event_type(&self) -> &str {
        &self.event_type
    }

    /// The event's payload, if it has one.
    pub fn payload(&self) -> Option<&Value> {
        self.rest.get("payload")
    }
}

/// The background work of a run that a server restart cancelled
/// (`OrchestrationV2RunBackgroundWorkCancelled`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunBackgroundWorkCancelled {
    pub run_id: RunId,
    pub restart_cancelled_background_work: Vec<CancelledBackgroundWork>,
}

/// A provider session that is no longer attached to the thread
/// (`OrchestrationV2ProviderSessionDetached`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSessionDetached {
    pub provider_session_id: ProviderSessionId,
    pub detached_at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// The user's request to take the workspace back to a checkpoint
/// (`OrchestrationV2CheckpointRollbackRequest`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckpointRollbackRequest {
    pub scope_id: CheckpointScopeId,
    pub checkpoint_id: CheckpointId,
    pub requested_at: Timestamp,
}
