//! What a subscription to one thread delivers: the thread as it is, then each
//! event as it happens.
//!
//! A snapshot may arrive in t3code's compact form, where every id of its
//! projection is a number indexing an `ids` table sent alongside. Decoding
//! expands that form, exactly as t3code's own decoder does, so a
//! [`ThreadSnapshot`] always holds real ids and is written back without the table.
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
    /// The snapshot in `fields`, whose compact ids, if it has an `ids` table, are expanded first.
    fn decode<E: Error>(mut fields: Map<String, Value>) -> Result<Self, E> {
        if let Some(Value::Array(ids)) = fields.remove("ids")
            && let Some(Value::Object(projection)) = fields.get_mut("projection")
        {
            expand_record_ids(projection, &ids);
        }
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

/// Expands t3code's opt-in compact id table in place (`orchestrationV2IdTable`):
/// on each record (array element or object directly under `container`, and its
/// `item`), numbers under `id` / `*Id` index `ids`, as do numbers in `*Ids` lists.
fn expand_record_ids(container: &mut Map<String, Value>, ids: &[Value]) {
    fn is_id_key(k: &str) -> bool {
        k == "id" || k.ends_with("Id")
    }
    fn expand_record(record: &mut Map<String, Value>, ids: &[Value]) {
        for (k, v) in record.iter_mut() {
            match v {
                Value::Number(n) if is_id_key(k) => {
                    if let Some(x) = n.as_u64().and_then(|i| ids.get(i as usize)) {
                        *v = x.clone();
                    }
                }
                Value::Array(list) if k.ends_with("Ids") => {
                    for e in list.iter_mut() {
                        if let Some(x) = e.as_u64().and_then(|i| ids.get(i as usize)) {
                            *e = x.clone();
                        }
                    }
                }
                Value::Object(item) if k == "item" => expand_record(item, ids),
                _ => {}
            }
        }
    }
    for v in container.values_mut() {
        match v {
            Value::Array(records) => {
                for r in records.iter_mut() {
                    if let Value::Object(m) = r {
                        expand_record(m, ids);
                    }
                }
            }
            Value::Object(m) => expand_record(m, ids),
            _ => {}
        }
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
            other => Err(E::custom(format_args!("unknown thread stream item kind `{other}`"))),
        }
    }
}

impl<'de> Deserialize<'de> for ThreadStreamItem {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::from_fields(Map::deserialize(deserializer)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn expands_compact_ids_like_t3code() {
        let mut projection = json!({
            "thread": { "id": 0, "projectId": 1, "title": "t", "updatedAt": "u" },
            "runs": [{ "id": 2, "ordinal": 1, "status": "completed", "handoffIds": [3, 0] }],
            "visibleTurnItems": [{ "position": 0, "visibility": "local", "sourceThreadId": 0, "sourceItemId": 3,
                                   "item": { "id": 3, "runId": 2, "type": "user_message", "ordinal": 1000001 } }],
            "updatedAt": "u",
        });
        let ids = [json!("thread-a"), json!("project-b"), json!("run-c"), json!("item-d")];
        expand_record_ids(projection.as_object_mut().unwrap(), &ids);
        assert_eq!(projection["thread"], json!({ "id": "thread-a", "projectId": "project-b", "title": "t", "updatedAt": "u" }));
        assert_eq!(projection["runs"][0], json!({ "id": "run-c", "ordinal": 1, "status": "completed", "handoffIds": ["item-d", "thread-a"] }), "non-id numbers untouched");
        let row = &projection["visibleTurnItems"][0];
        assert_eq!((&row["position"], &row["sourceThreadId"], &row["sourceItemId"]), (&json!(0), &json!("thread-a"), &json!("item-d")));
        assert_eq!(row["item"], json!({ "id": "item-d", "runId": "run-c", "type": "user_message", "ordinal": 1000001 }), "a row's item is expanded too");
    }
}
