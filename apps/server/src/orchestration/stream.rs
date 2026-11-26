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

fn projection_error(thread_id: &str, message: String) -> Failure {
    Failure::ThreadProjection { thread_id: thread_id.to_owned(), message }
}

/// The thread as it is now, with the sequence events are to be sent after:
/// as a client is sent it, or with `all_records` whole.
pub fn snapshot(store: &Store, thread_id: &str, all_records: bool) -> Result<ThreadSnapshot, Failure> {
    let cached = store.db().with(|connection| {
        let Some(thread) = store.thread_cache().thread(thread_id) else { return Ok(None) };
        // Commits publish to the cache while holding this same lock.
        let at: i64 = connection.query_row("SELECT COALESCE(MAX(sequence), 0) FROM events", [], |row| row.get(0))?;
        Ok(Some((at as u64, thread)))
    }).map_err(|e| projection_error(thread_id, e.to_string()))?;
    let (snapshot_sequence, projection) = if let Some((at, thread)) = cached {
        (at, thread.projection())
    } else {
        store.db().read(|connection| {
            let tx = connection.unchecked_transaction()?;
            let at: i64 = tx.query_row("SELECT COALESCE(MAX(sequence), 0) FROM events", [], |row| row.get(0))?;
            Ok(super::events::load_thread_records(&tx, thread_id, all_records, true)?.map(|thread| (at as u64, thread.projection())))
        }).map_err(|e| projection_error(thread_id, e.to_string()))?
            .ok_or_else(|| projection_error(thread_id, format!("Unknown thread {thread_id}")))?
    };
    let projection = if all_records { projection } else { projection.for_client() };
    Ok(ThreadSnapshot { snapshot_sequence, projection, history_before: None })
}

pub fn snapshot_item(store: &Store, thread_id: &str, all_records: bool) -> Result<(u64, ThreadStreamItem), Failure> {
    let snapshot = snapshot(store, thread_id, all_records)?;
    Ok((snapshot.snapshot_sequence, ThreadStreamItem::Snapshot(Box::new(snapshot))))
}

/// The thread's events in `(after, through]` as a subscription replays them.
#[cfg(test)]
pub fn replay(store: &Store, thread_id: &str, after: u64, through: u64, all_records: bool) -> Result<Vec<ThreadStreamItem>, crate::store::StoreError> {
    Ok(replay_items(store.thread_events_after(thread_id, after, through)?, all_records).collect())
}

fn replay_items(events: Vec<StoredEvent>, all_records: bool) -> impl Iterator<Item = ThreadStreamItem> {
    events.into_iter()
        .filter_map(|e| e.thread_event.map(|event| (e.sequence, event)))
        .filter(move |(_, event)| all_records || !event.body.for_the_server_only())
        .map(|(sequence, event)| ThreadStreamItem::Event { sequence, event: Box::new(event) })
}

const REPLAY_EVENTS: i64 = 128;
const REPLAY_BYTES: i64 = 256 * 1024;
pub(super) static SYNC_READS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);

async fn synchronize(store: &Store, tid: &str, after: Option<u64>, all: bool, windowed: bool) -> Result<(u64, Vec<domovoi_rpc::Encoded>), Failure> {
    let store = store.clone();
    let thread_id = tid.to_owned();
    let permit = SYNC_READS.acquire().await.map_err(|e| projection_error(tid, e.to_string()))?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        if let Some(after) = after
            && let Some((through, events)) = store.bounded_thread_replay(&thread_id, after, REPLAY_EVENTS, REPLAY_BYTES, all)
                .map_err(|e| projection_error(&thread_id, e.to_string()))?
        {
            let items = replay_items(events, all)
                .map(|item| encode_item(&thread_id, &Sent::Item(item)))
                .collect::<Result<_, _>>()?;
            return Ok((through, items));
        }
        if windowed {
            let snapshot = super::history::snapshot(&store, &thread_id)?;
            let through = snapshot.snapshot_sequence;
            let item = encode_item(&thread_id, &Sent::Item(ThreadStreamItem::Snapshot(Box::new(snapshot))))?;
            return Ok((through, vec![item]));
        }
        let (through, item) = snapshot_item(&store, &thread_id, all)?;
        Ok((through, vec![encode_item(&thread_id, &Sent::Item(item))?]))
    }).await.map_err(|e| projection_error(tid, e.to_string()))?
}

fn encode_item(tid: &str, sent: &Sent) -> Result<domovoi_rpc::Encoded, Failure> {
    sent.encoded().map_err(|error| projection_error(tid, error.to_string()))
}

pub fn thread_stream(store: Store, input: ThreadSubscription, windowed: bool) -> BoxStream<'static, Result<domovoi_rpc::Encoded, Failure>> {
    async_stream::stream! {
        let mut live = store.subscribe();
        let (tid, all) = (input.thread_id.clone(), input.all_records);
        let mut last_sent = match synchronize(&store, &tid, input.after_sequence, all, windowed).await {
            Ok((through, items)) => {
                for item in items { yield Ok(item); }
                through
            }
            Err(e) => { yield Err(e); return; }
        };
        if input.request_completion_marker {
            if windowed {
                #[derive(Serialize)]
                #[serde(rename_all = "camelCase")]
                struct Synchronized {
                    kind: &'static str,
                    snapshot_sequence: u64,
                }
                yield domovoi_rpc::encoded(&Synchronized { kind: "synchronized", snapshot_sequence: last_sent })
                    .map_err(|error| projection_error(&tid, error.to_string()));
            } else {
                yield encode_item(&tid, &Sent::Item(ThreadStreamItem::Synchronized));
            }
        }
        loop {
            match live.recv().await {
                Ok(e) if e.sequence <= last_sent || e.aggregate_id != tid => {}
                Ok(e) if !e.thread_event.as_ref().is_some_and(|event| all || !event.body.for_the_server_only()) => {}
                Ok(e) => {
                    last_sent = e.sequence;
                    yield encode_item(&tid, &Sent::Live(e));
                }
                Err(RecvError::Lagged(_)) => {
                    match synchronize(&store, &tid, Some(last_sent), all, windowed).await {
                        Ok((through, items)) => {
                            for item in items { yield Ok(item); }
                            last_sent = through;
                        }
                        Err(e) => { yield Err(e); return; }
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
            .create_project(crate::wire::shell::ProjectShell {
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

    #[tokio::test]
    async fn a_resume_larger_than_the_replay_budget_starts_with_a_window() {
        use crate::test_records::{append, message, thread};
        let store = Store::new(crate::db::Db::open_in_memory().unwrap());
        let origin = crate::orchestration::events::Origin::default();
        append(&store, "t1", "thread.created", thread("t1", "p"), &origin).unwrap();
        let large = "x".repeat(REPLAY_BYTES as usize);
        append(&store, "t1", "message.updated", message("t1", "m1", "user", &large, false), &origin).unwrap();
        assert!(store.bounded_thread_replay("t1", 1, REPLAY_EVENTS, REPLAY_BYTES, false).unwrap().is_none());

        let input = ThreadSubscription { thread_id: "t1".into(), after_sequence: Some(1), request_completion_marker: false, all_records: false };
        let encoded = thread_stream(store, input, true).next().await.unwrap().unwrap();
        let item = serde_json::from_str::<ThreadStreamItem>(encoded.get()).unwrap();
        assert!(matches!(item, ThreadStreamItem::Snapshot(snapshot) if snapshot.snapshot_sequence == 2));
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
                serde_json::from_str::<Value>(thread_stream(store, subscription, false).next().await.unwrap().unwrap().get()).unwrap()
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
