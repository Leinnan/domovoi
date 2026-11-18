//! Thread read model over a real session and over grown copies of it.
//!
//! The recorded Codex thread of `fixtures/golden/ts-session1` (snapshot plus
//! the events streamed after it) is replayed as-is, then grown to N turn items
//! and messages to show how `apply` and `shell()` scale with thread length.
//! The server derives a shell after every event, the client applies every
//! event; both pay these costs per streamed token.
//!
//! What a bench is given (the thread, the event) is decoded before the clock
//! starts; `typed_decode_event` and `typed_event_from_parsed_json` time the
//! decoding on its own.
//!
//! The benches are named `typed_…` because each was first measured beside the
//! same work on the untyped JSON model this crate once held. The names are
//! kept so their history in `docs/verification.md` still reads.
//!
//! `cargo bench -p domovoi-model`

use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::sync::{Arc, LazyLock};
use domovoi_model::typed::{DomainEvent, Thread, ThreadProjection, ThreadSnapshot};

const THREAD: &str = "6421f1e6-14f6-4d7a-89b3-6e82dac38210";

/// The sizes the grown thread is measured at.
const SIZES: [usize; 3] = [100, 1000, 5000];

/// What `grown` makes at each of `SIZES`: turn items, messages, and records of
/// every kind. The numbers the benches' history was measured on.
const GROWN: [(usize, usize, usize); 3] = [(100, 100, 225), (1000, 1000, 2025), (5000, 5000, 10025)];

struct Recorded {
    snapshot: Value,
    events: Vec<Value>,
}

static RECORDED: LazyLock<Recorded> = LazyLock::new(|| {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/golden/ts-session1/wire.ndjson");
    let text = std::fs::read_to_string(path).expect("fixture");
    let mut requests: BTreeMap<(i64, String), Value> = BTreeMap::new();
    let mut snapshot = None;
    let mut events = BTreeMap::new();
    for line in text.lines() {
        let rec: Value = serde_json::from_str(line).expect("ndjson");
        let f = &rec["frame"];
        if rec["kind"] != "ws" || !f.is_object() {
            continue;
        }
        let conn = rec["conn"].as_i64().unwrap_or(0);
        if f["_tag"] == "Request" {
            requests.insert((conn, f["id"].to_string()), f.clone());
            continue;
        }
        let Some(req) = requests.get(&(conn, f["requestId"].to_string())) else { continue };
        if f["_tag"] != "Chunk" || req["tag"] != "orchestration.subscribeThread" || req["payload"]["threadId"] != THREAD {
            continue;
        }
        for item in f["values"].as_array().into_iter().flatten() {
            match item["kind"].as_str() {
                Some("snapshot") => {
                    snapshot.get_or_insert_with(|| item.clone());
                }
                Some("event") => {
                    events.insert(item["sequence"].as_u64().unwrap_or(0), item["event"].clone());
                }
                _ => {}
            }
        }
    }
    let snapshot = snapshot.expect("thread snapshot recorded");
    let start = snapshot["snapshotSequence"].as_u64().unwrap_or(0);
    Recorded { snapshot, events: events.range(start + 1..).map(|(_, e)| e.clone()).collect() }
});

fn recorded_snapshot() -> ThreadSnapshot {
    serde_json::from_value(RECORDED.snapshot.clone()).expect("snapshot")
}

fn recorded_events() -> Vec<DomainEvent> {
    RECORDED.events.iter().map(|event| DomainEvent::from_value(event.clone()).expect("event")).collect()
}

/// The recorded thread grown to about `n` turn items, by repeating its items
/// and messages under new ids and later ordinals.
fn grown(n: usize) -> Thread {
    let mut recorded = Thread::from_snapshot(recorded_snapshot());
    for event in recorded_events() {
        recorded.apply(event);
    }
    // Grown as JSON, where a copy of a record of any kind is its fields with another id.
    let mut projection = serde_json::to_value(recorded.projection()).expect("projection");
    for kind in ["turnItems", "messages"] {
        let Some(list) = projection[kind].as_array_mut() else { continue };
        let base: Vec<Value> = list.clone();
        if base.is_empty() {
            continue;
        }
        let mut copy = 0;
        while list.len() < n {
            copy += 1;
            for r in &base {
                let mut r = r.clone();
                let id = format!("{}:copy{copy}", r["id"].as_str().unwrap_or_default());
                r["id"] = json!(id);
                if let Some(m) = r["messageId"].as_str() {
                    r["messageId"] = json!(format!("{m}:copy{copy}"));
                }
                if let Some(o) = r["ordinal"].as_u64() {
                    r["ordinal"] = json!(o + 100_000 * copy);
                }
                list.push(r);
                if list.len() >= n {
                    break;
                }
            }
        }
    }
    let projection: ThreadProjection = serde_json::from_value(projection).expect("the grown thread fits its types");
    Thread::from_projection(projection)
}

/// How many turn items, messages, and records of every kind `thread` holds.
fn sizes(thread: &Thread) -> (usize, usize, usize) {
    let others = thread.runs.len()
        + thread.attempts.len()
        + thread.nodes.len()
        + thread.subagents.len()
        + thread.provider_sessions.len()
        + thread.provider_threads.len()
        + thread.provider_turns.len()
        + thread.runtime_requests.len()
        + thread.plans.len()
        + thread.checkpoint_scopes.len()
        + thread.checkpoints.len()
        + thread.context_handoffs.len()
        + thread.context_transfers.len();
    (thread.turn_items.len(), thread.messages.len(), thread.turn_items.len() + thread.messages.len() + others)
}

/// A streaming update of the newest message of `thread`, as the server sends
/// it: same id, longer text.
fn streaming_update_json(thread: &Thread) -> Value {
    let mut payload = serde_json::to_value(thread.messages.last().expect("the thread has messages")).expect("message");
    payload["text"] = json!("token ".repeat(201));
    payload["streaming"] = json!(true);
    json!({ "id": "event-streaming", "threadId": THREAD, "type": "message.updated", "payload": payload, "occurredAt": "2026-11-16T00:00:00.000Z" })
}

/// `streaming_update_json` for `thread`, decoded.
fn streaming_update(thread: &Thread) -> DomainEvent {
    DomainEvent::from_value(streaming_update_json(thread)).expect("the update fits its type")
}

/// A copy of `thread` that shares no record with it: every record copied, text
/// and all. What a clone of the thread would cost if its records were not shared.
fn unshared(thread: &Thread) -> Thread {
    fn own<T: Clone>(records: &[Arc<T>]) -> Vec<Arc<T>> {
        records.iter().map(|record| Arc::new(T::clone(record))).collect()
    }
    Thread::from_projection(ThreadProjection {
        thread: thread.record.clone(),
        runs: own(thread.runs.as_slice()),
        attempts: own(thread.attempts.as_slice()),
        nodes: own(thread.nodes.as_slice()),
        subagents: own(thread.subagents.as_slice()),
        provider_sessions: own(thread.provider_sessions.as_slice()),
        provider_threads: own(thread.provider_threads.as_slice()),
        provider_turns: own(thread.provider_turns.as_slice()),
        runtime_requests: own(thread.runtime_requests.as_slice()),
        messages: own(thread.messages.as_slice()),
        plans: own(thread.plans.as_slice()),
        turn_items: own(thread.turn_items.as_slice()),
        checkpoint_scopes: own(thread.checkpoint_scopes.as_slice()),
        checkpoints: own(thread.checkpoints.as_slice()),
        context_handoffs: own(thread.context_handoffs.as_slice()),
        context_transfers: own(thread.context_transfers.as_slice()),
        visible_turn_items: Vec::new(),
        updated_at: thread.record.updated_at.clone(),
    })
}

fn main() {
    eprintln!("recorded thread: {} events after the snapshot", RECORDED.events.len());
    // The grown threads are the ones every earlier measurement was taken on.
    for (n, expected) in SIZES.into_iter().zip(GROWN) {
        assert_eq!(sizes(&grown(n)), expected, "the thread grown to {n} (turn items, messages, records)");
    }
    divan::main();
}

/// The thread from the snapshot, then every recorded event applied and the
/// shell derived after each (what the server's shell stream does per event).
/// The snapshot and the events are decoded before the clock starts.
#[divan::bench]
fn typed_replay_recorded_with_shells(b: divan::Bencher) {
    let (snapshot, events) = (recorded_snapshot(), recorded_events());
    b.with_inputs(|| (snapshot.clone(), events.clone())).bench_local_values(|(snapshot, events)| {
        let mut thread = Thread::from_snapshot(snapshot);
        let mut n = 0;
        for event in events {
            thread.apply(event);
            n += thread.shell().item_count;
        }
        (n, thread)
    });
}

/// One streamed token on a thread of `n` items that nothing else holds (a
/// client's), applied and the shell derived. Times `apply` (which moves the
/// event's record in) and `shell()`; the thread and the decoded event are made
/// before the clock starts and dropped after it stops.
#[divan::bench(args = [100, 1000, 5000])]
fn typed_streaming_token(b: divan::Bencher, n: usize) {
    let base = grown(n);
    let update = streaming_update(&base);
    b.with_inputs(|| (unshared(&base), update.clone())).bench_local_values(|(mut thread, update)| {
        let applied = thread.apply(update);
        let shell = thread.shell();
        (applied, shell, thread)
    });
}

/// `shell()` alone on a thread of `n` items.
#[divan::bench(args = [100, 1000, 5000])]
fn typed_shell_only(b: divan::Bencher, n: usize) {
    let thread = grown(n);
    b.bench_local(|| thread.shell());
}

/// Applying one streamed token alone, without deriving anything (what the
/// client does per token before it redraws one row), on a thread nothing else holds.
#[divan::bench(args = [100, 1000, 5000])]
fn typed_apply_only(b: divan::Bencher, n: usize) {
    let base = grown(n);
    let update = streaming_update(&base);
    b.with_inputs(|| (unshared(&base), update.clone())).bench_local_values(|(mut thread, update)| {
        let applied = thread.apply(update);
        (applied, thread)
    });
}

/// `typed_apply_only` on one long-lived thread: the same token applied again
/// and again, so the thread stays in the processor's cache from one to the
/// next. What is left is the work `apply` itself does. Each token's event is
/// copied before the clock starts, since `apply` takes it.
#[divan::bench(args = [100, 1000, 5000])]
fn typed_apply_only_in_place(b: divan::Bencher, n: usize) {
    let mut thread = unshared(&grown(n));
    let update = streaming_update(&thread);
    b.with_inputs(|| update.clone()).bench_local_values(|update| thread.apply(update));
}

/// A copy of the whole thread of `n` items: the records are shared, so their
/// text is not copied.
#[divan::bench(args = [100, 1000, 5000])]
fn typed_clone_state(b: divan::Bencher, n: usize) {
    let thread = grown(n);
    b.bench_local(|| thread.clone());
}

/// What `typed_clone_state` would cost if the records were held by value:
/// every record copied.
#[divan::bench(args = [100, 1000, 5000])]
fn typed_clone_state_copying_records(b: divan::Bencher, n: usize) {
    let thread = grown(n);
    b.bench_local(|| unshared(&thread));
}

/// One streamed token applied to a thread a reader still holds, as the
/// server's thread cache does (`Arc::make_mut`): the thread is copied first.
/// The copy shares the records, and the one record replaced is copied to tell
/// whether only its text grew.
#[divan::bench(args = [100, 1000, 5000])]
fn typed_apply_while_read(b: divan::Bencher, n: usize) {
    let base = Arc::new(grown(n));
    let update = streaming_update(&base);
    b.with_inputs(|| (Arc::clone(&base), update.clone())).bench_local_values(|(mut held, update)| {
        let applied = Arc::make_mut(&mut held).apply(update);
        (applied, held)
    });
}

/// Parsing one streamed token's event from its JSON text into what `apply` takes.
#[divan::bench]
fn typed_decode_event(b: divan::Bencher) {
    let text = streaming_update_json(&grown(100)).to_string();
    b.bench_local(|| serde_json::from_str::<DomainEvent>(&text).expect("event"));
}

/// The extra step for a client that is handed each event as parsed JSON:
/// turning that JSON into the event `apply` takes. The JSON is consumed, so no
/// text is copied.
#[divan::bench]
fn typed_event_from_parsed_json(b: divan::Bencher) {
    let event = streaming_update_json(&grown(100));
    b.with_inputs(|| event.clone()).bench_local_values(|event| DomainEvent::from_value(event).expect("event"));
}
