//! Every orchestration record the TypeScript server sent in the recorded
//! sessions must decode into its typed record and encode back to exactly the
//! JSON it came from. A field missing from a record, a wrong optionality, or an
//! enum value the types do not list all show up here as a difference.

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Debug;
use domovoi_model::typed::{
    AppThread, Checkpoint, CheckpointScope, ContextHandoff, ContextTransfer, ConversationMessage, DomainEvent, EventBody,
    ExecutionNode, PlanArtifact, ProjectedTurnItem, ProviderSession, ProviderThread, ProviderTurn, RecordKind, Run, RunAttempt,
    RuntimeRequest, StoredEvent, Subagent, ThreadProjection, ThreadShell, ThreadStreamItem, TurnItem,
};


const FIXTURES: [&str; 1] = ["session1"];

/// The names of the fifteen collections of a thread's projection, in its order.
fn collections() -> [&'static str; 15] {
    RecordKind::ALL.map(RecordKind::collection)
}

/// Record kinds beyond the fifteen projection collections.
const THREAD: &str = "thread";
const THREAD_SHELLS: &str = "threadShells";
const VISIBLE_TURN_ITEMS: &str = "visibleTurnItems";

/// Kinds the recorded session is known to contain: those a client is sent
/// (the records only the server works with are not in it). Checking none of
/// one of these means the walk over the fixture broke, not that the types are right.
const KINDS_IN_FIXTURES: [&str; 8] = [THREAD, THREAD_SHELLS, "runs", "providerThreads", "runtimeRequests", "messages", "turnItems", "checkpoints"];

/// Every `type` of `OrchestrationV2TurnItem`.
const TURN_ITEM_TYPES: [&str; 25] = [
    "notification", "user_message", "assistant_message", "reasoning", "proposed_plan", "todo_list", "user_input_request",
    "file_change", "command_execution", "file_search", "web_search", "approval_request", "checkpoint",
    "run_interrupt_request", "run_interrupt_result", "system_notice", "error", "compaction", "handoff", "fork",
    "thread_created", "secret_request", "link_request", "subagent", "dynamic_tool",
];

/// Every `type` of `OrchestrationV2DomainEvent`.
const EVENT_TYPES: [&str; 46] = [
    "thread.created", "thread.archived", "thread.unarchived", "thread.deleted", "thread.settled", "thread.unsettled",
    "thread.snoozed", "thread.unsnoozed", "thread.pinned", "thread.auto-settle-set", "thread.unpinned", "thread.pin-reordered",
    "thread.active-reordered", "thread.tags-updated", "thread.visited", "thread.marked-unread", "thread.metadata-updated",
    "thread.pull-request-synced", "thread.runtime-mode-updated", "thread.interaction-mode-updated",
    "thread.model-selection-updated", "thread.provider-switched", "thread.guest-share-updated",
    "thread.pull-request-link-updated", "run.created", "run.updated", "run.background-work-cancelled", "run-attempt.created",
    "run-attempt.updated", "node.updated", "subagent.updated", "provider-session.attached", "provider-session.updated",
    "provider-session.detached", "provider-thread.updated", "provider-turn.updated", "runtime-request.updated",
    "message.updated", "turn-item.updated", "plan.updated", "checkpoint-scope.created", "checkpoint.captured",
    "checkpoint.rollback-requested", "context-handoff.updated", "context-transfer.created", "context-transfer.updated",
];

fn fixture(name: &str) -> Vec<Value> {
    let path = format!("{}/../../fixtures/native/{name}/wire.ndjson", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap().lines().map(|line| serde_json::from_str(line).unwrap()).collect()
}

/// Every item the server streamed in the fixtures, with the RPC it answered.
fn streamed() -> Vec<(String, Value)> {
    let mut items = Vec::new();
    for name in FIXTURES {
        let mut requests: BTreeMap<(i64, String), Value> = BTreeMap::new();
        for line in fixture(name) {
            let frame = &line["frame"];
            if line["kind"] != "ws" || !frame.is_object() {
                continue;
            }
            let connection = line["conn"].as_i64().unwrap_or(0);
            if frame["_tag"] == "Request" {
                requests.insert((connection, frame["id"].to_string()), frame.clone());
                continue;
            }
            let Some(request) = requests.get(&(connection, frame["requestId"].to_string())) else { continue };
            if frame["_tag"] != "Chunk" {
                continue;
            }
            let rpc = request["tag"].as_str().unwrap_or_default();
            items.extend(frame["values"].as_array().unwrap().iter().map(|value| (rpc.to_owned(), value.clone())));
        }
    }
    items
}

/// Every orchestration record in the fixtures, by kind, as it was sent.
fn recorded() -> BTreeMap<&'static str, Vec<Value>> {
    let mut records: BTreeMap<&'static str, Vec<Value>> = BTreeMap::new();
    for (rpc, value) in streamed() {
        match rpc.as_str() {
            "orchestration.subscribeThread" => collect_thread_item(&value, &mut records),
            "orchestration.subscribeShell" => collect_shell_item(&value, &mut records),
            _ => {}
        }
    }
    records
}

fn collect_thread_item(item: &Value, records: &mut BTreeMap<&'static str, Vec<Value>>) {
    if item["kind"] == "snapshot" {
        let projection = item["projection"].as_object().unwrap().clone();
        records.entry(THREAD).or_default().push(projection["thread"].clone());
        for kind in collections().into_iter().chain([VISIBLE_TURN_ITEMS]) {
            if let Some(list) = projection.get(kind).and_then(Value::as_array) {
                records.entry(kind).or_default().extend(list.iter().cloned());
            }
        }
    } else if item["kind"] == "event" {
        // Which record the payload is, by the event's own account.
        let event = &item["event"];
        let typed = DomainEvent::from_value(event.clone()).unwrap_or_else(|error| panic!("a recorded event does not decode: {error}"));
        let kind = match (&typed.body, typed.record()) {
            (EventBody::Thread { .. }, _) => THREAD,
            (_, Some(record)) => record.kind().collection(),
            _ => return,
        };
        records.entry(kind).or_default().push(event["payload"].clone());
    }
}

fn collect_shell_item(item: &Value, records: &mut BTreeMap<&'static str, Vec<Value>>) {
    let shells = records.entry(THREAD_SHELLS).or_default();
    if item["kind"] == "snapshot" {
        for list in ["threads", "archivedThreads"] {
            shells.extend(item["snapshot"][list].as_array().into_iter().flatten().cloned());
        }
    } else if item["kind"] == "thread.updated" {
        shells.push(item["thread"].clone());
    }
}

/// Where `sent` and `encoded` differ, one line per path, so a field the typed
/// record lacks reads as "dropped".
fn differences(path: &str, sent: &Value, encoded: &Value, out: &mut Vec<String>) {
    match (sent, encoded) {
        (Value::Object(sent), Value::Object(encoded)) => {
            let keys: BTreeSet<&String> = sent.keys().chain(encoded.keys()).collect();
            for key in keys {
                let at = format!("{path}.{key}");
                match (sent.get(key), encoded.get(key)) {
                    (Some(sent), Some(encoded)) => differences(&at, sent, encoded, out),
                    (Some(sent), None) => out.push(format!("{at}: dropped (sent {sent})")),
                    (None, Some(encoded)) => out.push(format!("{at}: added (encoded {encoded})")),
                    (None, None) => {}
                }
            }
        }
        (Value::Array(sent), Value::Array(encoded)) if sent.len() == encoded.len() => {
            for (index, (sent, encoded)) in sent.iter().zip(encoded).enumerate() {
                differences(&format!("{path}[{index}]"), sent, encoded, out);
            }
        }
        _ if sent != encoded => out.push(format!("{path}: sent {sent}, encoded {encoded}")),
        _ => {}
    }
}

/// The fallback variants (`Other(…)`) a decoded record holds, each with the
/// text leading up to it. Found in the record's `Debug` output outside string
/// literals, where the derived `Debug` of a variant is the only source of `Other(`.
fn fallbacks(record: &impl Debug) -> Vec<String> {
    let text = format!("{record:?}");
    let (mut found, mut in_string, mut escaped) = (Vec::new(), false, false);
    for (at, character) in text.char_indices() {
        if in_string {
            match character {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
        } else if character == '"' {
            in_string = true;
        } else if text[at..].starts_with("Other(") {
            let from = text[..at].char_indices().rev().nth(40).map_or(0, |(start, _)| start);
            let to = text[at..].char_indices().nth(60).map_or(text.len(), |(end, _)| at + end);
            found.push(format!("…{}…", &text[from..to]));
        }
    }
    found
}

/// Decodes `record` as `T` and encodes it again; `Err` says how the round trip failed.
fn round_trip<T: DeserializeOwned + Serialize + Debug>(record: &Value) -> Result<(), String> {
    let typed: T = serde_json::from_value(record.clone()).map_err(|error| format!("does not decode: {error}"))?;
    let unknown = fallbacks(&typed);
    if !unknown.is_empty() {
        return Err(format!("decodes into a fallback variant:\n    {}", unknown.join("\n    ")));
    }
    let encoded = serde_json::to_value(&typed).map_err(|error| format!("does not encode: {error}"))?;
    if encoded != *record {
        let mut lines = Vec::new();
        differences("", record, &encoded, &mut lines);
        return Err(format!("encodes differently:\n    {}", lines.join("\n    ")));
    }
    Ok(())
}

fn round_trip_as(kind: &str, record: &Value) -> Result<(), String> {
    match kind {
        THREAD => round_trip::<AppThread>(record),
        THREAD_SHELLS => round_trip::<ThreadShell>(record),
        VISIBLE_TURN_ITEMS => round_trip::<ProjectedTurnItem>(record),
        "runs" => round_trip::<Run>(record),
        "attempts" => round_trip::<RunAttempt>(record),
        "nodes" => round_trip::<ExecutionNode>(record),
        "subagents" => round_trip::<Subagent>(record),
        "providerSessions" => round_trip::<ProviderSession>(record),
        "providerThreads" => round_trip::<ProviderThread>(record),
        "providerTurns" => round_trip::<ProviderTurn>(record),
        "runtimeRequests" => round_trip::<RuntimeRequest>(record),
        "messages" => round_trip::<ConversationMessage>(record),
        "plans" => round_trip::<PlanArtifact>(record),
        "turnItems" => round_trip::<TurnItem>(record),
        "checkpointScopes" => round_trip::<CheckpointScope>(record),
        "checkpoints" => round_trip::<Checkpoint>(record),
        "contextHandoffs" => round_trip::<ContextHandoff>(record),
        "contextTransfers" => round_trip::<ContextTransfer>(record),
        other => Err(format!("no typed record for kind `{other}`")),
    }
}

/// The record's own id, or the id of the item a projected row wraps.
fn record_id(record: &Value) -> &str {
    record["id"].as_str().or_else(|| record["item"]["id"].as_str()).unwrap_or("<no id>")
}

#[test]
fn recorded_records_round_trip_through_their_types() {
    let records = recorded();
    let all_kinds: Vec<&str> = [THREAD, THREAD_SHELLS].into_iter().chain(collections()).chain([VISIBLE_TURN_ITEMS]).collect();

    let mut checked: BTreeMap<&str, usize> = BTreeMap::new();
    let mut item_types: BTreeMap<String, usize> = BTreeMap::new();
    // Records of one kind usually fail alike: one entry per distinct failure.
    let mut failures: BTreeMap<String, (usize, String)> = BTreeMap::new();
    for (kind, list) in &records {
        for record in list {
            *checked.entry(kind).or_default() += 1;
            let item = if *kind == VISIBLE_TURN_ITEMS { &record["item"] } else { record };
            if let (true, Some(item_type)) = (matches!(*kind, "turnItems" | VISIBLE_TURN_ITEMS), item["type"].as_str()) {
                *item_types.entry(item_type.to_owned()).or_default() += 1;
            }
            if let Err(why) = round_trip_as(kind, record) {
                let failure = failures.entry(format!("{kind} {why}")).or_insert_with(|| (0, record_id(record).to_owned()));
                failure.0 += 1;
            }
        }
    }

    println!("records checked, by kind:");
    for kind in &all_kinds {
        println!("  {kind:<18} {}", checked.get(kind).copied().unwrap_or(0));
    }
    println!("  {:<18} {}", "in all", checked.values().sum::<usize>());
    println!("turn items checked, by type:");
    for (item_type, count) in &item_types {
        println!("  {item_type:<18} {count}");
    }
    let uncovered_kinds: Vec<&str> = all_kinds.iter().copied().filter(|kind| !checked.contains_key(kind)).collect();
    let uncovered_types: Vec<&str> = TURN_ITEM_TYPES.into_iter().filter(|item_type| !item_types.contains_key(*item_type)).collect();
    println!("kinds the fixtures do not contain (verified against the schema only): {uncovered_kinds:?}");
    println!("turn item types the fixtures do not contain (verified against the schema only): {uncovered_types:?}");

    let report: Vec<String> =
        failures.iter().map(|(failure, (count, id))| format!("{failure}\n    ({count} records, first {id})")).collect();
    assert!(failures.is_empty(), "records that do not round-trip:\n{}", report.join("\n"));
    for kind in KINDS_IN_FIXTURES {
        assert!(checked.get(kind).copied().unwrap_or(0) > 0, "the fixtures contain `{kind}` records, but none was checked");
    }
    for item_type in item_types.keys() {
        assert!(TURN_ITEM_TYPES.contains(&item_type.as_str()), "the fixtures contain a turn item type the schema does not: {item_type}");
    }
}

/// Every item of every recorded thread subscription, the events they carry and
/// the projections of their snapshots go through their types unchanged, and no
/// recorded event is of a type the types do not know.
#[test]
fn recorded_thread_streams_round_trip_through_their_types() {
    let items: Vec<Value> = streamed().into_iter().filter(|(rpc, _)| rpc == "orchestration.subscribeThread").map(|(_, item)| item).collect();
    let mut item_kinds: BTreeMap<String, usize> = BTreeMap::new();
    let mut event_types: BTreeMap<String, usize> = BTreeMap::new();
    let mut failures: BTreeMap<String, usize> = BTreeMap::new();
    for item in &items {
        let kind = item["kind"].as_str().unwrap();
        *item_kinds.entry(kind.to_owned()).or_default() += 1;
        let sent = item.clone();
        assert_eq!(ThreadStreamItem::from_value(sent.clone()).ok(), serde_json::from_value(sent.clone()).ok(), "taking parsed JSON apart gives the same item as reading it");
        if let Err(why) = round_trip::<ThreadStreamItem>(&sent) {
            *failures.entry(format!("stream item `{kind}` {why}")).or_default() += 1;
        }
        if kind == "snapshot" {
            if let Err(why) = round_trip::<ThreadProjection>(&sent["projection"]) {
                *failures.entry(format!("projection {why}")).or_default() += 1;
            }
        }
        if kind == "event" {
            let event = &item["event"];
            let event_type = event["type"].as_str().unwrap();
            *event_types.entry(event_type.to_owned()).or_default() += 1;
            let outcome = round_trip::<DomainEvent>(event).and_then(|()| {
                let typed: DomainEvent = serde_json::from_value(event.clone()).map_err(|error| error.to_string())?;
                // Taking parsed JSON apart gives the same event as reading it.
                assert_eq!(DomainEvent::from_value(event.clone()).map_err(|error| error.to_string())?, typed);
                if matches!(typed.body, EventBody::Unknown(_)) { Err("decodes as an unknown event".to_owned()) } else { Ok(()) }
            });
            if let Err(why) = outcome {
                *failures.entry(format!("event `{event_type}` {why}")).or_default() += 1;
            }
        }
    }

    println!("thread stream items checked, by kind:");
    for (kind, count) in &item_kinds {
        println!("  {kind:<34} {count}");
    }
    println!("events checked, by type:");
    for (event_type, count) in &event_types {
        println!("  {event_type:<34} {count}");
    }
    let uncovered: Vec<&str> = EVENT_TYPES.into_iter().filter(|event_type| !event_types.contains_key(*event_type)).collect();
    println!("event types the fixtures do not contain (verified against the schema only): {uncovered:?}");

    let report: Vec<String> = failures.iter().map(|(failure, count)| format!("{failure}\n    ({count} times)")).collect();
    assert!(failures.is_empty(), "thread stream items that do not round-trip:\n{}", report.join("\n"));
    for kind in ["snapshot", "event", "synchronized"] {
        assert!(item_kinds.get(kind).copied().unwrap_or(0) > 0, "the fixtures contain `{kind}` items, but none was checked");
    }
    for event_type in event_types.keys() {
        assert!(EVENT_TYPES.contains(&event_type.as_str()), "the fixtures contain an event type the schema does not: {event_type}");
    }
    for event_type in EVENT_TYPES {
        assert!(EventBody::knows(event_type), "the schema's event type `{event_type}` is not known to the types");
    }
}

/// An event from a newer server survives a round trip whole; an event of a
/// known type that does not fit its schema is an error, never a fallback.
#[test]
fn unknown_events_survive_and_known_ones_decode_in_full() {
    let unknown = serde_json::json!({
        "id": "event-1", "threadId": "thread-1", "nodeId": "node-1", "occurredAt": "2026-01-01T00:00:00.000Z",
        "type": "hologram.projected", "payload": { "id": "hologram-1", "lumens": 1.5 }, "beam": [1, 2, 3],
    });
    let typed: DomainEvent = serde_json::from_value(unknown.clone()).unwrap();
    assert!(matches!(&typed.body, EventBody::Unknown(event) if event.event_type() == "hologram.projected"
        && event.payload() == Some(&unknown["payload"])));
    assert_eq!((typed.event_type(), typed.thread_id.as_str(), typed.record()), ("hologram.projected", "thread-1", None));
    assert_eq!(serde_json::to_value(&typed).unwrap(), unknown);

    // An unknown event of a familiar family is still unknown: its payload is not guessed at.
    let mut familiar = unknown.clone();
    familiar["type"] = "thread.teleported".into();
    let typed: DomainEvent = serde_json::from_value(familiar.clone()).unwrap();
    assert!(matches!(typed.body, EventBody::Unknown(_)));
    assert_eq!(serde_json::to_value(&typed).unwrap(), familiar);

    let mut known = unknown.clone();
    known["type"] = "run.updated".into();
    let error = serde_json::from_value::<DomainEvent>(known).unwrap_err();
    assert!(error.to_string().contains("run.updated"), "{error}");

    let mut no_envelope = unknown;
    no_envelope.as_object_mut().unwrap().remove("occurredAt");
    let error = serde_json::from_value::<DomainEvent>(no_envelope).unwrap_err();
    assert!(error.to_string().contains("occurredAt"), "{error}");

    let stored = serde_json::json!({
        "sequence": 7, "commandId": null,
        "event": { "id": "event-2", "threadId": "thread-1", "occurredAt": "2026-01-01T00:00:00.000Z",
                   "type": "checkpoint.rollback-requested",
                   "payload": { "scopeId": "scope-1", "checkpointId": "checkpoint-1", "requestedAt": "2026-01-01T00:00:00.000Z" } },
    });
    let typed: StoredEvent = serde_json::from_value(stored.clone()).unwrap();
    assert!(matches!(typed.event.body, EventBody::CheckpointRollbackRequested(_)));
    assert_eq!(serde_json::to_value(&typed).unwrap(), stored);
}

/// A record from a newer server: a turn item type, an enum value and an
/// attachment type this build does not know must survive a round trip.
#[test]
fn unknown_values_survive_a_round_trip() {
    let item = serde_json::json!({
        "id": "item-1", "threadId": "thread-1", "runId": null, "nodeId": null, "providerThreadId": null,
        "providerTurnId": null, "nativeItemRef": null, "parentItemId": null, "ordinal": 7, "status": "paused",
        "title": null, "startedAt": null, "completedAt": null, "updatedAt": "2026-01-01T00:00:00.000Z",
        "type": "hologram", "projector": { "lumens": 1.5 },
    });
    let typed: TurnItem = serde_json::from_value(item.clone()).unwrap();
    assert!(matches!(typed, TurnItem::Other(_)));
    assert_eq!((typed.item_type(), typed.id().as_str(), typed.ordinal(), typed.status().as_str()), ("hologram", "item-1", 7, "paused"));
    assert_eq!(serde_json::to_value(&typed).unwrap(), item);

    let mut message: Map<String, Value> = serde_json::from_value(serde_json::json!({
        "createdBy": "robot", "creationSource": "web", "id": "message-1", "threadId": "thread-1", "runId": null,
        "nodeId": null, "role": "user", "text": "", "streaming": false,
        "attachments": [{ "type": "audio", "id": "a1", "name": "note.ogg", "mimeType": "audio/ogg", "sizeBytes": 12 }],
        "createdAt": "2026-01-01T00:00:00.000Z", "updatedAt": "2026-01-01T00:00:00.000Z",
    }))
    .unwrap();
    let typed: ConversationMessage = serde_json::from_value(Value::Object(message.clone())).unwrap();
    assert_eq!(typed.created_by.as_str(), "robot");
    assert_eq!(fallbacks(&typed).len(), 2, "the unknown actor and the unknown attachment");
    assert_eq!(serde_json::to_value(&typed).unwrap(), Value::Object(message.clone()));

    // A known type must decode in full: it never degrades to the fallback.
    message["attachments"][0]["type"] = "image".into();
    message["attachments"][0].as_object_mut().unwrap().remove("sizeBytes");
    let error = serde_json::from_value::<ConversationMessage>(Value::Object(message)).unwrap_err();
    assert!(error.to_string().contains("sizeBytes"), "{error}");
}

/// Absent and `null` are different on the wire and stay different.
#[test]
fn absent_and_null_stay_distinct() {
    let run = serde_json::json!({
        "id": "run-1", "threadId": "thread-1", "ordinal": 1, "providerInstanceId": "codex",
        "modelSelection": { "instanceId": "codex", "model": "m" }, "providerThreadId": null, "userMessageId": "message-1",
        "rootNodeId": null, "activeAttemptId": null, "status": "queued", "requestedAt": "2026-01-01T00:00:00.000Z",
        "startedAt": null, "completedAt": null, "checkpointId": null, "contextHandoffId": null,
    });
    let absent: Run = serde_json::from_value(run.clone()).unwrap();
    assert_eq!(absent.queue_position, None);
    assert_eq!(serde_json::to_value(&absent).unwrap(), run);

    let mut with_null = run.clone();
    with_null["queuePosition"] = Value::Null;
    let null: Run = serde_json::from_value(with_null.clone()).unwrap();
    assert_eq!(null.queue_position, Some(None));
    assert_eq!(serde_json::to_value(&null).unwrap(), with_null);

    let mut with_value = run;
    with_value["queuePosition"] = 3.into();
    let value: Run = serde_json::from_value(with_value.clone()).unwrap();
    assert_eq!(value.queue_position, Some(Some(3)));
    assert_eq!(serde_json::to_value(&value).unwrap(), with_value);
}

/// A measurement, not a test of the types: how many of the records a server
/// has stored go through their types unchanged. `DOMOVOI_RECORDS` names a file of
/// `{"kind": …, "record": …}` lines, `thread` for a thread's own record:
///
/// ```sh
/// sqlite3 <home>/userdata/domovoi.sqlite "SELECT json_object('kind','thread','record',json(record)) FROM threads;
///   SELECT json_object('kind',kind,'record',json(record)) FROM thread_entities;" > records.ndjson
/// DOMOVOI_RECORDS=records.ndjson cargo test -p domovoi-model --test typed_round_trip stored -- --ignored --nocapture
/// ```
#[test]
#[ignore = "measurement over a server's own database; needs DOMOVOI_RECORDS"]
fn stored_records_go_through_their_types() {
    let path = std::env::var("DOMOVOI_RECORDS").expect("DOMOVOI_RECORDS names the exported records");
    let text = std::fs::read_to_string(&path).expect("the exported records");
    // By kind: records, and each way they fail with how many times and one example.
    let mut kinds: BTreeMap<String, (usize, BTreeMap<String, (usize, String)>)> = BTreeMap::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        let row: Value = serde_json::from_str(line).expect("a JSON line");
        let kind = if row["kind"] == "thread" { THREAD } else { row["kind"].as_str().unwrap_or("?") };
        let label = match row["record"]["type"].as_str().filter(|_| kind == "turnItems") {
            Some(ty) => format!("turnItems/{ty}"),
            None => kind.to_owned(),
        };
        let entry = kinds.entry(label).or_default();
        entry.0 += 1;
        if let Err(error) = round_trip_as(kind, &row["record"]) {
            // Ids and values vary; the first line or two name the field.
            let reason: String = error.lines().take(2).collect::<Vec<_>>().join(" ").chars().take(180).collect();
            let seen = entry.1.entry(reason).or_insert_with(|| (0, record_id(&row["record"]).to_owned()));
            seen.0 += 1;
        }
    }
    let (mut records, mut failed) = (0, 0);
    for (kind, (count, failures)) in &kinds {
        let bad: usize = failures.values().map(|(n, _)| n).sum();
        records += count;
        failed += bad;
        println!("{kind:<34} {count:>7} records, {bad:>6} not unchanged");
        let mut reasons: Vec<_> = failures.iter().collect();
        reasons.sort_by_key(|(_, (n, _))| std::cmp::Reverse(*n));
        for (reason, (n, example)) in reasons.into_iter().take(4) {
            println!("    {n:>6}  {reason}  (e.g. {example})");
        }
    }
    println!("{records} records, {failed} that do not go through their types unchanged");
}
