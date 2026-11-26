//! `orchestration.subscribeThread`: a full snapshot (or a replay of the
//! thread's events after `afterSequence`), the completion marker, then live
//! events.
//!
//! A client is sent what it shows. The records only the server works with
//! (`EventBody::for_the_server_only`) stay here, in the snapshot and on the
//! stream, unless the subscription asks for `allRecords` (a tool that
//! inspects the server's records, and the tests).

use super::failure::Failure;
use crate::store::{Store, StoredEvent};
use futures::StreamExt;
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize, Serializer};
use std::sync::Arc;
use domovoi_model::typed::{DomainEvent, EventBody, ThreadId, ThreadSnapshot, ThreadStreamItem, Timestamp, TurnItemId};
use tokio::sync::broadcast::error::RecvError;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadSubscription {
    pub thread_id: String,
    pub after_sequence: Option<u64>,
    #[serde(default)]
    pub request_completion_marker: bool,
    /// Also the records only the server works with.
    #[serde(default)]
    pub all_records: bool,
}

/// One item of a subscription to a thread, as it is sent.
pub enum Sent {
    Item(ThreadStreamItem),
    /// An event as it was broadcast: every subscriber sends the same one,
    /// written as `ThreadStreamItem::Event` without a copy of its record, or,
    /// for a turn item that only grew, as `ThreadStreamItem::Append`.
    Live(Arc<StoredEvent>),
}

/// `ThreadStreamItem::Event` over an event that is not owned.
#[derive(Serialize)]
#[serde(tag = "kind", rename = "event")]
struct EventItem<'a> {
    sequence: u64,
    event: &'a DomainEvent,
}

/// `ThreadStreamItem::Append` over the text of an item that is not owned.
#[derive(Serialize)]
#[serde(tag = "kind", rename = "append", rename_all = "camelCase")]
struct AppendItem<'a> {
    sequence: u64,
    thread_id: &'a ThreadId,
    item_id: &'a TurnItemId,
    at: u64,
    text: &'a str,
    updated_at: &'a Timestamp,
}

/// The text the event's turn item gained, if the event is one that only added text.
fn appended(stored: &StoredEvent) -> Option<AppendItem<'_>> {
    let (at, event) = (stored.grew_from?, stored.thread_event.as_ref()?);
    let EventBody::TurnItemUpdated(item) = &event.body else { return None };
    Some(AppendItem {
        sequence: stored.sequence,
        thread_id: &event.thread_id,
        item_id: item.id(),
        at: at as u64,
        text: item.growing_text()?.get(at..)?,
        updated_at: item.updated_at(),
    })
}

impl Serialize for Sent {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Sent::Item(item) => item.serialize(serializer),
            Sent::Live(stored) => match (appended(stored), &stored.thread_event) {
                (Some(append), _) => append.serialize(serializer),
                (None, Some(event)) => EventItem { sequence: stored.sequence, event }.serialize(serializer),
                (None, None) => Err(serde::ser::Error::custom("not a thread's event")),
            },
        }
    }
}

impl Sent {
    /// The item as it is sent. A live event is written by the first
    /// subscriber that sends it, and every other sends that text.
    fn encoded(&self) -> Result<domovoi_rpc::Encoded, serde_json::Error> {
        match self {
            Sent::Item(_) => domovoi_rpc::encoded(self),
            Sent::Live(stored) => match stored.sent.get() {
                Some(written) => Ok(written.clone()),
                None => {
                    let written = domovoi_rpc::encoded(self)?;
                    Ok(stored.sent.get_or_init(|| written).clone())
                }
            },
        }
    }
}

/// [`thread_stream`] as the socket sends it.
pub fn thread_stream_sent(store: Store, input: ThreadSubscription) -> BoxStream<'static, Result<domovoi_rpc::Encoded, Failure>> {
    let thread_id = input.thread_id.clone();
    thread_stream(store, input)
        .map(move |item| item.and_then(|sent| sent.encoded().map_err(|e| projection_error(&thread_id, e.to_string()))))
        .boxed()
}

fn projection_error(thread_id: &str, message: String) -> Failure {
    Failure::ThreadProjection { thread_id: thread_id.to_owned(), message }
}

/// The thread as it is now, with the sequence events are to be sent after:
/// as a client is sent it, or with `all_records` whole.
pub fn snapshot(store: &Store, thread_id: &str, all_records: bool) -> Result<ThreadSnapshot, Failure> {
    // Read first: an event committed after it is in the thread already, and
    // is sent again, which changes nothing.
    let snapshot_sequence = store.latest_sequence().map_err(|e| projection_error(thread_id, e.to_string()))?;
    let thread = store
        .thread(thread_id)
        .map_err(|e| projection_error(thread_id, e.to_string()))?
        .ok_or_else(|| projection_error(thread_id, format!("Unknown thread {thread_id}")))?;
    let projection = if all_records { thread.projection() } else { thread.projection().for_client() };
    Ok(ThreadSnapshot {
        snapshot_sequence,
        projection,
        history_cursor: None,
        has_more_history: None,
        latest_local_turn_ordinal: None,
        turn_items_omit_local_visible: None,
    })
}

pub fn snapshot_item(store: &Store, thread_id: &str, all_records: bool) -> Result<(u64, ThreadStreamItem), Failure> {
    let snapshot = snapshot(store, thread_id, all_records)?;
    Ok((snapshot.snapshot_sequence, ThreadStreamItem::Snapshot(Box::new(snapshot))))
}

/// The thread's events in `(after, through]` as a subscription replays them.
pub fn replay(store: &Store, thread_id: &str, after: u64, through: u64, all_records: bool) -> Result<Vec<ThreadStreamItem>, crate::store::StoreError> {
    let events = store.thread_events_after(thread_id, after, through)?;
    Ok(events
        .into_iter()
        .filter_map(|e| e.thread_event.map(|event| (e.sequence, event)))
        .filter(|(_, event)| all_records || !event.body.for_the_server_only())
        .map(|(sequence, event)| ThreadStreamItem::Event { sequence, event: Box::new(event) })
        .collect())
}

pub fn thread_stream(store: Store, input: ThreadSubscription) -> BoxStream<'static, Result<Sent, Failure>> {
    async_stream::stream! {
        let mut live = store.subscribe();
        let (tid, all) = (input.thread_id.clone(), input.all_records);
        let high_water = match store.latest_sequence() {
            Ok(s) => s,
            Err(e) => { yield Err(projection_error(&tid, e.to_string())); return; }
        };
        let mut last_sent = match input.after_sequence {
            // What the client missed, if the log still reaches back to where it was.
            Some(after) if after <= high_water && store.replays_from(after).unwrap_or(false) => {
                match replay(&store, &tid, after, high_water, all) {
                    Ok(items) => for item in items { yield Ok(Sent::Item(item)); },
                    Err(e) => { yield Err(projection_error(&tid, e.to_string())); return; }
                }
                high_water
            }
            _ => match snapshot_item(&store, &tid, all) {
                Ok((seq, item)) => { yield Ok(Sent::Item(item)); seq }
                Err(e) => { yield Err(e); return; }
            },
        };
        if input.request_completion_marker {
            yield Ok(Sent::Item(ThreadStreamItem::Synchronized));
        }
        loop {
            match live.recv().await {
                Ok(e) if e.sequence <= last_sent || e.aggregate_id != tid => {}
                Ok(e) if !e.thread_event.as_ref().is_some_and(|event| all || !event.body.for_the_server_only()) => {}
                Ok(e) => {
                    last_sent = e.sequence;
                    yield Ok(Sent::Live(e));
                }
                Err(RecvError::Lagged(_)) => {
                    // Fell behind the broadcast: the missed events are in the log
                    // (an indexed read of this thread's), so send them instead of
                    // a whole new snapshot. Live events up to there are skipped above.
                    let missed = store.latest_sequence()
                        .and_then(|through| Ok((through, replay(&store, &tid, last_sent, through, all)?)));
                    match missed {
                        Ok((through, items)) => {
                            for item in items { yield Ok(Sent::Item(item)); }
                            last_sent = through;
                        }
                        Err(_) => {
                            if let Ok((seq, item)) = snapshot_item(&store, &tid, all) { last_sent = seq; yield Ok(Sent::Item(item)); }
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
    use serde_json::{Value, json};

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
            grew_from: None,
            sent: Default::default(),
        });
        let owned = serde_json::to_string(&ThreadStreamItem::Event { sequence: 7, event: Box::new(event) }).unwrap();
        assert_eq!(serde_json::to_string(&Sent::Live(stored.clone())).unwrap(), owned);
        // Written once: every subscriber sends the same text.
        let (first, second) = (Sent::Live(stored.clone()).encoded().unwrap(), Sent::Live(stored).encoded().unwrap());
        assert_eq!(first.get(), owned);
        assert!(Arc::ptr_eq(&first, &second));
    }

    /// A reply written through `Turn` as it streams: the first flush is sent
    /// as its turn item, the next ones as the text each added, and the last
    /// (the reply whole, its status changed) as its turn item again. A
    /// thread that applies what is sent holds the reply as the store does,
    /// and a replay of the log sends the items themselves.
    #[tokio::test]
    async fn a_streamed_reply_is_sent_as_the_text_each_flush_adds() {
        use crate::orchestration::commands::Orchestrator;
        use domovoi_model::typed::{Thread, TurnItem};
        let store = Store::new(crate::db::Db::open_in_memory().unwrap());
        store
            .create_project(domovoi_contracts::shell::ProjectShell {
                id: "p".into(), title: "p".into(), has_custom_title: false, workspace_root: "/tmp/p".into(), repository_identity: None,
                default_model_selection: None, default_thread_env_mode: None, auto_pull: false, favicon_path: None, project_icon: None,
                scripts: vec![], pinned_at: None, archived_at: None, created_at: "t".into(), updated_at: "t".into(),
            }, None)
            .unwrap();
        let (o, _effects) = Orchestrator::new(store.clone());
        o.launch_thread(crate::test_records::launch("c1", "t1", "p", "codex", "hello")).await.unwrap();
        let run = store.records::<domovoi_model::typed::Run>("t1").unwrap().first().unwrap().id.as_str().to_owned();
        let mut turn = crate::adapters::turn::Turn::begin(store.clone(), "t1", &run).unwrap();

        // A client that opened the thread before the reply began.
        let (_, opened) = snapshot_item(&store, "t1", false).unwrap();
        let ThreadStreamItem::Snapshot(opened) = opened else { panic!("a snapshot") };
        let mut held = Thread::from_snapshot(*opened);
        let mut live = store.subscribe();
        let mut sent: Vec<Value> = vec![];
        let mut deliver = |held: &mut Thread, sent: &mut Vec<Value>| {
            while let Ok(stored) = live.try_recv() {
                if !stored.thread_event.as_ref().is_some_and(|event| !event.body.for_the_server_only()) {
                    continue;
                }
                let item = serde_json::to_value(Sent::Live(stored)).unwrap();
                match ThreadStreamItem::from_value(item.clone()).unwrap() {
                    ThreadStreamItem::Event { event, .. } => drop(held.apply(*event)),
                    ThreadStreamItem::Append { item_id, at, text, updated_at, .. } => {
                        assert!(held.append(&item_id, at, &text, &updated_at).is_some(), "the text follows what is held: {item}");
                    }
                    other => panic!("not sent live: {other:?}"),
                }
                sent.push(item);
            }
        };
        turn.assistant("r1", "Hello", true, None);
        deliver(&mut held, &mut sent);
        std::thread::sleep(std::time::Duration::from_millis(130));
        turn.assistant("r1", "Hello, wörld", true, None);
        deliver(&mut held, &mut sent);
        std::thread::sleep(std::time::Duration::from_millis(130));
        turn.assistant("r1", "Hello, wörld. Done", true, None);
        deliver(&mut held, &mut sent);
        turn.assistant("r1", "Hello, wörld. Done.", false, None);
        deliver(&mut held, &mut sent);

        let kinds: Vec<(&str, &str)> = sent.iter().map(|item| (item["kind"].as_str().unwrap(), item["event"]["type"].as_str().unwrap_or(""))).collect();
        assert_eq!(kinds, [("event", "turn-item.updated"), ("append", ""), ("append", ""), ("event", "message.updated"), ("event", "turn-item.updated")]);
        assert_eq!((&sent[1]["at"], &sent[1]["text"]), (&json!(5), &json!(", wörld")), "{}", sent[1]);
        assert_eq!((&sent[2]["at"], &sent[2]["text"]), (&json!("Hello, wörld".len()), &json!(". Done")));
        assert_eq!(sent[1]["threadId"], "t1");

        // What the client holds is what the server stores.
        let stored = store.load_thread("t1").unwrap().unwrap();
        let reply = |thread: &Thread| thread.turn_items.iter().find_map(|item| match item { TurnItem::AssistantMessage(reply) => Some(reply.clone()), _ => None }).unwrap();
        assert_eq!(reply(&held), reply(&stored));
        assert_eq!(reply(&held).text, "Hello, wörld. Done.");
        assert_eq!(held.messages.iter().map(|m| m.text.as_str()).collect::<Vec<_>>(), stored.messages.iter().map(|m| m.text.as_str()).collect::<Vec<_>>());

        // Text a snapshot already held is not added twice; text that does not follow what is held says so.
        let item_id = reply(&held).id.clone();
        let at = reply(&held).updated_at.clone();
        assert_eq!(held.append(&item_id, 0, "Hello", &at), Some(domovoi_model::typed::Applied::Nothing));
        assert_eq!(held.append(&item_id, 99, "more", &at), None);
        assert_eq!(held.append(&item_id, 3, "xx", &at), None);

        // A replay of the log sends events, never the text added.
        let replayed = replay(&store, "t1", 0, store.latest_sequence().unwrap(), false).unwrap();
        assert!(replayed.iter().all(|item| matches!(item, ThreadStreamItem::Event { .. })));
    }

    /// A client that reconnects is sent the events it missed while the log
    /// still holds them all, and the thread whole once the log was trimmed
    /// past where the client was.
    #[tokio::test]
    async fn a_resume_from_before_the_log_starts_gets_a_snapshot() {
        use crate::test_records::{append, message, thread};
        let store = Store::new(crate::db::Db::open_in_memory().unwrap());
        let o = crate::orchestration::events::Origin::default();
        append(&store, "t1", "thread.created", thread("t1", "p"), &o).unwrap();
        for i in 0..6 {
            append(&store, "t1", "message.updated", message("t1", &format!("m{i}"), "user", "hi", false), &o).unwrap();
        }
        let first = |after: u64| {
            let store = store.clone();
            async move {
                let subscription = ThreadSubscription { thread_id: "t1".into(), after_sequence: Some(after), request_completion_marker: false, all_records: false };
                serde_json::to_value(thread_stream(store, subscription).next().await.unwrap().unwrap()).unwrap()
            }
        };
        assert_eq!(first(3).await["kind"], "event", "the log holds everything after 3");
        assert_eq!(store.trim_event_log(2).unwrap(), 5);
        assert!(store.replays_from(5).unwrap() && !store.replays_from(4).unwrap());
        assert_eq!(first(5).await["sequence"], 6, "still replayed: nothing after 5 is gone");
        let snapshot = first(3).await;
        assert_eq!((snapshot["kind"].as_str(), snapshot["snapshotSequence"].as_u64()), (Some("snapshot"), Some(7)));
        assert_eq!(snapshot["projection"]["messages"].as_array().map(Vec::len), Some(6), "the records are whole: only the log was cut");
    }
}
