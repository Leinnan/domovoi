//! What a subscription to one thread delivers: the thread as it is, then each
//! event as it happens.
//!
//! An item that is already parsed JSON is best decoded with
//! [`ThreadStreamItem::from_value`], which takes the JSON apart. `Deserialize`
//! on a `serde_json::Value` gives the same item but rebuilds the JSON first.

use super::checkpoint::{Checkpoint, CheckpointScope};
use super::common::Timestamp;
use super::context::{ContextHandoff, ContextTransfer};
use super::event::DomainEvent;
use super::message::ConversationMessage;
use super::plan::PlanArtifact;
use super::provider::{ProviderSession, ProviderThread, ProviderTurn, RuntimeRequest};
use super::run::{ExecutionNode, Run, RunAttempt, Subagent};
use super::ids::{ThreadId, TurnItemId};
use super::thread::AppThread;
use super::turn_item::{ProjectedTurnItem, TurnItem};
use super::wire::{nullable, optional, required};
use serde::de::Error;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Value};
use std::sync::Arc;

/// Everything a thread consists of (`OrchestrationV2ThreadProjection`): its
/// record, each kind of its records, and its timeline as shown.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadProjection {
    pub thread: AppThread,
    pub runs: Vec<Arc<Run>>,
    pub attempts: Vec<Arc<RunAttempt>>,
    pub nodes: Vec<Arc<ExecutionNode>>,
    pub subagents: Vec<Arc<Subagent>>,
    pub provider_sessions: Vec<Arc<ProviderSession>>,
    pub provider_threads: Vec<Arc<ProviderThread>>,
    pub provider_turns: Vec<Arc<ProviderTurn>>,
    pub runtime_requests: Vec<Arc<RuntimeRequest>>,
    pub messages: Vec<Arc<ConversationMessage>>,
    pub plans: Vec<Arc<PlanArtifact>>,
    /// In a snapshot that says `turnItemsOmitLocalVisible`, only the items
    /// `visible_turn_items` does not already carry.
    pub turn_items: Vec<Arc<TurnItem>>,
    pub checkpoint_scopes: Vec<Arc<CheckpointScope>>,
    pub checkpoints: Vec<Arc<Checkpoint>>,
    pub context_handoffs: Vec<Arc<ContextHandoff>>,
    pub context_transfers: Vec<Arc<ContextTransfer>>,
    pub visible_turn_items: Vec<ProjectedTurnItem>,
    pub updated_at: Timestamp,
}

impl ThreadProjection {
    /// The projection as a client is sent it: without the records only the
    /// server works with (attempts, execution nodes, provider sessions and
    /// turns, checkpoint scopes, context handoffs and transfers). A client
    /// shows none of them, and for a long thread they are thousands of records.
    pub fn for_client(mut self) -> Self {
        self.attempts = Vec::new();
        self.nodes = Vec::new();
        self.provider_sessions = Vec::new();
        self.provider_turns = Vec::new();
        self.checkpoint_scopes = Vec::new();
        self.context_handoffs = Vec::new();
        self.context_transfers = Vec::new();
        self
    }
}

/// A thread as of one point in the server's log, which a subscription starts
/// from. With a `history_cursor` it is a window on a longer timeline.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadSnapshot {
    /// The last event the projection includes; events after it follow on the stream.
    pub snapshot_sequence: u64,
    pub projection: ThreadProjection,
    /// Where to ask for the timeline before this window. Opaque.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub history_cursor: Option<Option<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_more_history: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_local_turn_ordinal: Option<Option<u64>>,
    /// `projection.turn_items` leaves out the items its visible rows carry.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub turn_items_omit_local_visible: Option<bool>,
}

impl ThreadSnapshot {
    /// The snapshot in `fields`.
    fn decode<E: Error>(mut fields: Map<String, Value>) -> Result<Self, E> {
        Ok(Self {
            snapshot_sequence: required(&mut fields, "snapshotSequence")?,
            projection: required(&mut fields, "projection")?,
            history_cursor: nullable(&mut fields, "historyCursor")?,
            has_more_history: optional(&mut fields, "hasMoreHistory")?,
            latest_local_turn_ordinal: nullable(&mut fields, "latestLocalTurnOrdinal")?,
            turn_items_omit_local_visible: optional(&mut fields, "turnItemsOmitLocalVisible")?,
        })
    }
}

impl<'de> Deserialize<'de> for ThreadSnapshot {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::decode(Map::deserialize(deserializer)?)
    }
}

/// One item of a subscription to a thread (`OrchestrationV2ThreadStreamItem`).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ThreadStreamItem {
    /// Everything recorded so far has been delivered; what follows is live.
    Synchronized,
    Snapshot(Box<ThreadSnapshot>),
    /// An event, with its place in the server's log.
    Event { sequence: u64, event: Box<DomainEvent> },
    /// The event at `sequence` gave the turn item `item_id` a new version
    /// that is its last one with more text, and this is the text added: the
    /// item's growing text ([`TurnItem::growing_text`]) was `at` bytes long.
    /// A reply that streams is sent as its first version and then these, so
    /// each flush costs what it adds, not the whole reply so far. A replay of
    /// the log sends the events themselves.
    #[serde(rename_all = "camelCase")]
    Append { sequence: u64, thread_id: ThreadId, item_id: TurnItemId, at: u64, text: String, updated_at: Timestamp },
}

impl ThreadStreamItem {
    /// The item that parsed JSON holds. The JSON is taken apart, not read:
    /// nothing in it is copied.
    pub fn from_value(item: Value) -> serde_json::Result<Self> {
        match item {
            Value::Object(fields) => Self::from_fields(fields),
            _ => Err(serde_json::Error::custom("a thread stream item is an object")),
        }
    }

    /// The item whose JSON object held `fields`.
    fn from_fields<E: Error>(mut fields: Map<String, Value>) -> Result<Self, E> {
        let kind = match fields.remove("kind") {
            Some(Value::String(kind)) => kind,
            Some(_) => return Err(E::custom("`kind` is not a string")),
            None => return Err(E::missing_field("kind")),
        };
        match kind.as_str() {
            "synchronized" => Ok(Self::Synchronized),
            "snapshot" => ThreadSnapshot::decode(fields).map(|snapshot| Self::Snapshot(Box::new(snapshot))),
            "event" => {
                let sequence = required(&mut fields, "sequence")?;
                match fields.remove("event") {
                    Some(Value::Object(event)) => Ok(Self::Event { sequence, event: Box::new(DomainEvent::from_fields(event)?) }),
                    Some(_) => Err(E::custom("`event` is not an object")),
                    None => Err(E::missing_field("event")),
                }
            }
            "append" => Ok(Self::Append {
                sequence: required(&mut fields, "sequence")?,
                thread_id: required(&mut fields, "threadId")?,
                item_id: required(&mut fields, "itemId")?,
                at: required(&mut fields, "at")?,
                text: required(&mut fields, "text")?,
                updated_at: required(&mut fields, "updatedAt")?,
            }),
            other => Err(E::custom(format_args!("unknown thread stream item kind `{other}`"))),
        }
    }
}

impl<'de> Deserialize<'de> for ThreadStreamItem {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::from_fields(Map::deserialize(deserializer)?)
    }
}
