//! `orchestration.subscribeThread`: a full snapshot (or a replay of the
//! thread's events after `afterSequence`), the completion marker, then live
//! events. Bounded/compact snapshot formats are client opt-ins we decline.

use super::failure::Failure;
use super::projection;
use crate::store::{Store, StoredEvent};
use futures::StreamExt;
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize, Serializer};
use std::sync::Arc;
use domovoi_model::typed::{DomainEvent, ThreadSnapshot, ThreadStreamItem};
use tokio::sync::broadcast::error::RecvError;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadSubscription {
    pub thread_id: String,
    pub after_sequence: Option<u64>,
    #[serde(default)]
    pub request_completion_marker: bool,
}

/// One item of a subscription to a thread, as it is sent.
pub enum Sent {
    Item(ThreadStreamItem),
    /// An event as it was broadcast: every subscriber sends the same one,
    /// written as `ThreadStreamItem::Event` without a copy of its record.
    Live(Arc<StoredEvent>),
}

/// `ThreadStreamItem::Event` over an event that is not owned.
#[derive(Serialize)]
#[serde(tag = "kind", rename = "event")]
struct EventItem<'a> {
    sequence: u64,
    event: &'a DomainEvent,
}

impl Serialize for Sent {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Sent::Item(item) => item.serialize(serializer),
            Sent::Live(stored) => match &stored.thread_event {
                Some(event) => EventItem { sequence: stored.sequence, event }.serialize(serializer),
                None => Err(serde::ser::Error::custom("not a thread's event")),
            },
        }
    }
}

fn projection_error(thread_id: &str, message: String) -> Failure {
    Failure::ThreadProjection { thread_id: thread_id.to_owned(), message }
}

/// The thread as it is now, with the sequence events are to be sent after.
pub fn snapshot(store: &Store, thread_id: &str) -> Result<ThreadSnapshot, Failure> {
    let snapshot_sequence = store.latest_sequence().map_err(|e| projection_error(thread_id, e.to_string()))?;
    let projection = projection::thread_projection(store, thread_id)
        .map_err(|e| projection_error(thread_id, e.to_string()))?
        .ok_or_else(|| projection_error(thread_id, format!("Unknown thread {thread_id}")))?;
    Ok(ThreadSnapshot {
        snapshot_sequence,
        projection,
        history_cursor: None,
        has_more_history: None,
        latest_local_turn_ordinal: None,
        turn_items_omit_local_visible: None,
    })
}

pub fn snapshot_item(store: &Store, thread_id: &str) -> Result<(u64, ThreadStreamItem), Failure> {
    let snapshot = snapshot(store, thread_id)?;
    Ok((snapshot.snapshot_sequence, ThreadStreamItem::Snapshot(Box::new(snapshot))))
}

/// The thread's events in `(after, through]` as a subscription replays them.
pub fn replay(store: &Store, thread_id: &str, after: u64, through: u64) -> Result<Vec<ThreadStreamItem>, crate::store::StoreError> {
    let events = store.thread_events_after(thread_id, after, through)?;
    Ok(events
        .into_iter()
        .filter_map(|e| e.thread_event.map(|event| ThreadStreamItem::Event { sequence: e.sequence, event: Box::new(event) }))
        .collect())
}

pub fn thread_stream(store: Store, input: ThreadSubscription) -> BoxStream<'static, Result<Sent, Failure>> {
    async_stream::stream! {
        let mut live = store.subscribe();
        let tid = input.thread_id.clone();
        let high_water = match store.latest_sequence() {
            Ok(s) => s,
            Err(e) => { yield Err(projection_error(&tid, e.to_string())); return; }
        };
        let mut last_sent = match input.after_sequence {
            Some(after) if after <= high_water => {
                match replay(&store, &tid, after, high_water) {
                    Ok(items) => for item in items { yield Ok(Sent::Item(item)); },
                    Err(e) => { yield Err(projection_error(&tid, e.to_string())); return; }
                }
                high_water
            }
            _ => match snapshot_item(&store, &tid) {
                Ok((seq, item)) => { yield Ok(Sent::Item(item)); seq }
                Err(e) => { yield Err(e); return; }
            },
        };
        if input.request_completion_marker {
            yield Ok(Sent::Item(ThreadStreamItem::Synchronized));
        }
        loop {
            match live.recv().await {
                Ok(e) if e.sequence <= last_sent || e.thread_event.is_none() || e.aggregate_id != tid => {}
                Ok(e) => {
                    last_sent = e.sequence;
                    yield Ok(Sent::Live(e));
                }
                Err(RecvError::Lagged(_)) => {
                    // Fell behind the broadcast: the missed events are in the log
                    // (an indexed read of this thread's), so send them instead of
                    // a whole new snapshot. Live events up to there are skipped above.
                    let missed = store.latest_sequence()
                        .and_then(|through| Ok((through, replay(&store, &tid, last_sent, through)?)));
                    match missed {
                        Ok((through, items)) => {
                            for item in items { yield Ok(Sent::Item(item)); }
                            last_sent = through;
                        }
                        Err(_) => {
                            if let Ok((seq, item)) = snapshot_item(&store, &tid) { last_sent = seq; yield Ok(Sent::Item(item)); }
                        }
                    }
                }
                Err(RecvError::Closed) => return,
            }
        }
    }
    .boxed()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_live_event_is_sent_as_the_stream_item_it_is() {
        let event: DomainEvent = serde_json::from_value(json!({
            "id": "e1", "threadId": "t1", "driver": "codex", "occurredAt": "2026-11-16T00:00:00.000Z", "type": "checkpoint.rollback-requested",
            "payload": { "scopeId": "s1", "checkpointId": "c1", "requestedAt": "2026-11-16T00:00:00.000Z" },
        }))
        .unwrap();
        let stored = Arc::new(StoredEvent {
            sequence: 7,
            aggregate_kind: "thread".into(),
            aggregate_id: "t1".into(),
            event_type: event.event_type().to_owned(),
            thread_event: Some(event.clone()),
        });
        let owned = ThreadStreamItem::Event { sequence: 7, event: Box::new(event) };
        assert_eq!(serde_json::to_string(&Sent::Live(stored)).unwrap(), serde_json::to_string(&owned).unwrap());
    }
}
