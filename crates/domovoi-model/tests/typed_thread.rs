//! A thread held in memory (`domovoi_model::typed::Thread`) follows the rules its
//! module states, after every event it is given:
//!
//! - what it says an event did (`Applied`) is what its records show happened;
//! - the record an event carries is held as it was sent, the thread's own
//!   record is replaced by a thread event, and every other event moves the
//!   thread's `updatedAt`;
//! - turn items stay in timeline order and every other kind in order of
//!   arrival, and each record is found by its id at its place;
//! - its projection, encoded and read back, describes the same thread, and so
//!   do its records stored as rows.
//!
//! Three sources of input: every thread subscription the TypeScript server
//! streamed in the recorded sessions; a generated sequence of the cases those
//! sessions lack; and, as a measurement, the threads and events of a real
//! database. That the shell derived from a thread is the one t3code derives
//! is `parity.rs`.

use serde_json::{Value, json};
use std::collections::BTreeMap;
use domovoi_model::typed::{
    self, AppThread, Applied, DomainEvent, EventBody, RecordKind, RecordRef, ShellStatus, StoredRecord, Thread, ThreadProjection,
    ThreadSnapshot, ThreadStreamItem,
};

const FIXTURES: [&str; 1] = ["session1"];

/// The collections of a thread's projection, in its order.
const KINDS: [RecordKind; 15] = RecordKind::ALL;

fn fixture(name: &str) -> Vec<Value> {
    let path = format!("{}/../../fixtures/native/{name}/wire.ndjson", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap().lines().map(|line| serde_json::from_str(line).unwrap()).collect()
}

/// What the server streamed in answer to the `orchestration.subscribeThread`
/// calls of a recorded session, in the order it was sent: each item with the
/// thread subscribed to.
fn thread_stream(name: &str) -> Vec<(String, Value)> {
    let mut requests: BTreeMap<(i64, String), Value> = BTreeMap::new();
    let mut items = Vec::new();
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
        if frame["_tag"] == "Chunk" && request["tag"] == "orchestration.subscribeThread" {
            let thread = request["payload"]["threadId"].as_str().unwrap();
            items.extend(frame["values"].as_array().unwrap().iter().map(|item| (thread.to_owned(), item.clone())));
        }
    }
    items
}

fn ids(records: &Value) -> Vec<&str> {
    records.as_array().into_iter().flatten().map(|record| record["id"].as_str().unwrap()).collect()
}

/// The record `id` of the collection `kind` in an encoded projection.
fn find<'a>(projection: &'a Value, kind: &str, id: &str) -> Option<&'a Value> {
    projection[kind].as_array()?.iter().find(|record| record["id"] == id)
}

/// The rule for a reply streaming in, on the records as they are sent: the
/// same fields, differing at most in the text that grows and the time of the change.
fn only_text_differs(old: &Value, new: &Value) -> bool {
    const GROWING: [&str; 4] = ["text", "markdown", "output", "updatedAt"];
    let (Some(old), Some(new)) = (old.as_object(), new.as_object()) else { return false };
    old.len() == new.len() && new.iter().all(|(key, value)| old.get(key).is_some_and(|was| was == value || GROWING.contains(&key.as_str())))
}

/// What `event` did, read off the thread's encoded projection before and
/// after it. `None` for the two events that change part of a record, which
/// `events_that_are_not_a_record` covers by name.
fn expected_applied(before: &Value, after: &Value, event: &DomainEvent) -> Option<Applied> {
    Some(match &event.body {
        EventBody::Thread { .. } => Applied::Thread,
        EventBody::ThreadPullRequestLinkUpdated(_) | EventBody::RunBackgroundWorkCancelled(_) => return None,
        EventBody::ProviderSessionDetached(detached) => match find(before, "providerSessions", detached.provider_session_id.as_str()) {
            Some(_) => Applied::Removed { record: RecordRef::ProviderSession(detached.provider_session_id.clone()) },
            None => Applied::Touched,
        },
        body => {
            let Some(record) = body.record() else { return Some(Applied::Touched) };
            let (kind, id) = (record.kind().collection(), record.id().to_owned());
            let (was, now) = (ids(&before[kind]), ids(&after[kind]));
            match find(before, kind, &id) {
                None => {
                    let appended: Vec<&str> = was.iter().copied().chain([id.as_str()]).collect();
                    Applied::Added { record, order_changed: appended != now }
                }
                Some(old) => {
                    let new = find(after, kind, &id).expect("a record that was replaced is still held");
                    let streams = kind == "messages"
                        || (kind == "turnItems"
                            && matches!(new["type"].as_str(), Some("assistant_message" | "reasoning" | "proposed_plan" | "command_execution")));
                    Applied::Replaced { record, order_changed: was != now, only_text_grew: streams && only_text_differs(old, new) }
                }
            }
        }
    })
}

/// A thread under test, with what it holds written out.
struct Checked {
    thread: Thread,
    /// The thread's projection as of the last check, encoded as a server sends it.
    held: Value,
    checks: usize,
}

impl Checked {
    fn new(thread: Thread) -> Self {
        let held = serde_json::to_value(thread.projection()).unwrap();
        Self { thread, held, checks: 0 }
    }

    fn from_snapshot(item: &Value) -> Self {
        let snapshot: ThreadSnapshot = serde_json::from_value(item.clone()).unwrap_or_else(|error| panic!("the snapshot decodes: {error}"));
        let checked = Self::new(Thread::from_snapshot(snapshot));
        assert_eq!(Some(checked.thread.snapshot_sequence), item["snapshotSequence"].as_u64());
        checked
    }

    /// Requires what holds of a thread whatever it was given.
    fn check(&mut self, after: &str) {
        self.checks += 1;
        let projection = self.thread.projection();
        self.held = serde_json::to_value(&projection).unwrap();

        // The projection, sent and read back, describes this thread.
        let read: ThreadProjection =
            serde_json::from_value(self.held.clone()).unwrap_or_else(|error| panic!("the projection does not decode after {after}: {error}"));
        let rebuilt = Thread { snapshot_sequence: self.thread.snapshot_sequence, ..Thread::from_projection(read) };
        assert!(rebuilt == self.thread, "the projection describes another thread after {after}");

        // The timeline as shown: each row carries the item at its place, and the shell counts the rows.
        let visible = self.thread.visible_turn_item_refs();
        assert_eq!(projection.visible_turn_items.len(), visible.len(), "the visible turn items differ after {after}");
        for (position, (row, item)) in (0..).zip(projection.visible_turn_items.iter().zip(&visible)) {
            let own = row.position == position && row.visibility == typed::ItemVisibility::Local;
            assert!(own && *row.item == **item && row.source_item_id == *item.id(), "the visible row {position} is not its item after {after}");
        }
        let shell = self.thread.shell();
        assert_eq!((shell.item_count, shell.visible_item_count), (visible.len() as u64, visible.len() as u64), "the shell's counts after {after}");
        assert_eq!(self.held["updatedAt"], self.held["thread"]["updatedAt"], "the projection's time is the thread's after {after}");

        // Turn items are in timeline order; every record is found by its id, at its place.
        let timeline: Vec<(u64, &str)> =
            self.held["turnItems"].as_array().unwrap().iter().map(|item| (item["ordinal"].as_u64().unwrap(), item["id"].as_str().unwrap())).collect();
        assert!(timeline.is_sorted(), "the turn items are out of order after {after}");
        for kind in KINDS {
            for (at, id) in ids(&self.held[kind.collection()]).into_iter().enumerate() {
                assert_eq!(position(&self.thread, kind, id), Some(at), "{} {id} is not found at its place after {after}", kind.collection());
            }
        }
    }

    /// Applies `event`, checks the thread, and checks that the event did what
    /// its rule says and that the thread says so.
    fn apply(&mut self, event: &Value) -> Applied {
        let (event_type, payload) = (event["type"].as_str().unwrap(), &event["payload"]);
        let label = format!("{event_type} {}", payload["id"].as_str().unwrap_or(""));
        let typed: DomainEvent = serde_json::from_value(event.clone()).unwrap_or_else(|error| panic!("{label} decodes: {error}"));
        let before = self.held.clone();
        let applied = self.thread.apply(typed.clone());
        self.check(&label);
        let (was, now) = (&before["thread"], &self.held["thread"]);
        if let Some(expected) = expected_applied(&before, &self.held, &typed) {
            assert_eq!(applied, expected, "what {label} did");
        }

        // The record the event carries is held as it was sent, in a collection that keeps its order.
        if let Some(record) = typed.record() {
            let kind = record.kind().collection();
            assert_eq!(find(&self.held, kind, record.id()), Some(payload), "the record of {label} is held as sent");
            if record.kind() != RecordKind::TurnItem {
                let mut arrived = ids(&before[kind]);
                if !arrived.contains(&record.id()) {
                    arrived.push(record.id());
                }
                assert_eq!(ids(&self.held[kind]), arrived, "{kind} are in order of arrival after {label}");
            }
        }
        // What became of the thread's own record.
        let mut expected = was.clone();
        match &typed.body {
            EventBody::Thread { .. } => expected = payload.clone(),
            EventBody::ThreadPullRequestLinkUpdated(_) => expected = now.clone(),
            EventBody::ProviderThreadUpdated(provider_thread) if payload["appThreadId"] == was["id"] && !provider_thread.is_placeholder() => {
                expected["activeProviderThreadId"] = payload["id"].clone();
                expected["updatedAt"] = payload["updatedAt"].clone();
            }
            _ => expected["updatedAt"] = event["occurredAt"].clone(),
        }
        assert_eq!(*now, expected, "the thread's record after {label}");
        applied
    }

    /// The thread's records as the server stores them, a row each, build the same thread.
    fn check_stored(&self) {
        let record: AppThread = serde_json::from_value(self.held["thread"].clone()).unwrap();
        let rows = KINDS.into_iter().flat_map(|kind| {
            let records = self.held[kind.collection()].as_array().unwrap();
            records.iter().map(move |record| StoredRecord::decode(kind, record).unwrap_or_else(|error| panic!("{}: {error}", kind.collection())))
        });
        let stored = Thread { snapshot_sequence: self.thread.snapshot_sequence, ..Thread::from_records(record, rows) };
        assert!(stored == self.thread, "the stored rows build another thread");
    }
}

/// Where the thread keeps the record `id` of the kind `kind`.
fn position(thread: &Thread, kind: RecordKind, id: &str) -> Option<usize> {
    match kind {
        RecordKind::Run => thread.runs.position(id),
        RecordKind::Attempt => thread.attempts.position(id),
        RecordKind::Node => thread.nodes.position(id),
        RecordKind::Subagent => thread.subagents.position(id),
        RecordKind::ProviderSession => thread.provider_sessions.position(id),
        RecordKind::ProviderThread => thread.provider_threads.position(id),
        RecordKind::ProviderTurn => thread.provider_turns.position(id),
        RecordKind::RuntimeRequest => thread.runtime_requests.position(id),
        RecordKind::Message => thread.messages.position(id),
        RecordKind::Plan => thread.plans.position(id),
        RecordKind::TurnItem => thread.turn_items.position(id),
        RecordKind::CheckpointScope => thread.checkpoint_scopes.position(id),
        RecordKind::Checkpoint => thread.checkpoints.position(id),
        RecordKind::ContextHandoff => thread.context_handoffs.position(id),
        RecordKind::ContextTransfer => thread.context_transfers.position(id),
    }
}

/// How many records of every kind an encoded projection holds.
fn record_count(projection: &Value) -> usize {
    KINDS.iter().map(|kind| ids(&projection[kind.collection()]).len()).sum()
}

/// The name of what an event did, for counting.
fn applied_name(applied: &Applied) -> String {
    format!("{applied:?}").split([' ', '{', '(']).next().unwrap().to_owned()
}

/// Every thread of the recorded sessions, as a client that stays subscribed
/// holds it: the snapshot, then each event of each subscription (a
/// subscription resumed after a reconnect carries on from the last event),
/// checked after each.
#[test]
fn recorded_threads_follow_the_rules() {
    // By thread: the thread and the sequence of the last event it holds.
    let mut threads: BTreeMap<String, (Checked, u64)> = BTreeMap::new();
    let (mut snapshots, mut events, mut repeated, mut unheld) = (0, 0, 0, 0);
    let mut applied_kinds: BTreeMap<String, usize> = BTreeMap::new();
    for name in FIXTURES {
        for (thread, item) in thread_stream(name) {
            // Each item also goes through its own type.
            let typed_item: ThreadStreamItem = serde_json::from_value(item.clone()).unwrap();
            match item["kind"].as_str() {
                Some("snapshot") => {
                    assert!(matches!(typed_item, ThreadStreamItem::Snapshot(_)));
                    let mut checked = Checked::from_snapshot(&item);
                    checked.check("the snapshot");
                    assert_eq!(checked.thread.record.id.as_str(), thread);
                    checked.checks += threads.get(&thread).map_or(0, |(earlier, _)| earlier.checks);
                    threads.insert(thread, (checked, item["snapshotSequence"].as_u64().unwrap()));
                    snapshots += 1;
                }
                Some("event") => {
                    let sequence = item["sequence"].as_u64().unwrap();
                    match threads.get_mut(&thread) {
                        Some((checked, held)) if sequence > *held => {
                            let applied = checked.apply(&item["event"]);
                            *applied_kinds.entry(applied_name(&applied)).or_default() += 1;
                            *held = sequence;
                            events += 1;
                        }
                        Some(_) => repeated += 1,
                        None => unheld += 1,
                    }
                }
                _ => {}
            }
        }
    }
    let checks: usize = threads.values().map(|(checked, _)| checked.checks).sum();
    println!("recorded sessions: {} threads, {snapshots} snapshots, {events} events applied, {checks} checks", threads.len());
    println!("  events already held (sent again on another connection): {repeated}; events of a thread with no snapshot: {unheld}");
    println!("  what the events did: {applied_kinds:?}");
    for (thread, (checked, held)) in &threads {
        checked.check_stored();
        println!("  {thread}: {} turn items, {} records in all, up to event {held}", checked.thread.turn_items.len(), record_count(&checked.held));
    }
    assert_eq!((snapshots, unheld), (4, 0), "the fixtures hold 4 snapshots, and every event follows one");
    assert!(events >= 220, "the fixtures hold over 220 events");
}

/// A small deterministic generator (xorshift64*).
struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound as u64) as usize
    }

    fn chance(&mut self, one_in: usize) -> bool {
        self.below(one_in) == 0
    }

    fn pick<'a, T: ?Sized>(&mut self, options: &[&'a T]) -> &'a T {
        options[self.below(options.len())]
    }
}

const THREAD: &str = "thread-1";
const RUN_STATUSES: [&str; 10] =
    ["preparing", "queued", "starting", "running", "waiting", "completed", "interrupted", "failed", "cancelled", "rolled_back"];
const ITEM_STATUSES: [&str; 8] = ["idle", "pending", "running", "waiting", "completed", "failed", "cancelled", "interrupted"];
const ITEM_TYPES: [&str; 10] = [
    "assistant_message", "reasoning", "command_execution", "proposed_plan", "user_message", "error", "run_interrupt_request",
    "run_interrupt_result", "system_notice", "hologram",
];

/// The capabilities of a provider session, as the TypeScript server sent them.
fn capabilities() -> Value {
    thread_stream("session1")
        .into_iter()
        .find_map(|(_, item)| (item["event"]["type"] == "provider-session.updated").then(|| item["event"]["payload"]["capabilities"].clone()))
        .expect("the fixture holds a provider session")
}

/// Generates events for a thread: records of every kind that fit the schema,
/// new and changed.
struct Generator {
    checked: Checked,
    random: Random,
    step: u64,
    capabilities: Value,
    /// How often each case came up.
    cases: BTreeMap<&'static str, usize>,
    /// How often the thread's shell showed each status after an event.
    statuses: BTreeMap<&'static str, usize>,
}

impl Generator {
    fn new(seed: u64) -> Self {
        let thread = json!({
            "createdBy": "user", "creationSource": "web", "id": THREAD, "projectId": "project-1", "title": "Generated",
            "providerInstanceId": "codex", "modelSelection": { "instanceId": "codex", "model": "model-1" },
            "runtimeMode": "full-access", "interactionMode": "default", "branch": null, "worktreePath": null,
            "activeProviderThreadId": null,
            "lineage": { "parentThreadId": null, "relationshipToParent": null, "rootThreadId": THREAD }, "forkedFrom": null,
            "createdAt": "2026-01-01T00:00:00.000Z", "updatedAt": "2026-01-01T00:00:00.000Z", "archivedAt": null, "deletedAt": null,
        });
        let mut projection = json!({ "thread": thread, "visibleTurnItems": [], "updatedAt": "2026-01-01T00:00:00.000Z" });
        for kind in KINDS {
            projection[kind.collection()] = json!([]);
        }
        let mut checked = Checked::from_snapshot(&json!({ "kind": "snapshot", "snapshotSequence": 7, "projection": projection }));
        checked.check("the empty snapshot");
        Self { checked, random: Random(seed), step: 0, capabilities: capabilities(), cases: BTreeMap::new(), statuses: BTreeMap::new() }
    }

    /// A time later than every one before it.
    fn now(&mut self) -> String {
        self.step += 1;
        format!("2026-02-01T{:02}:{:02}:{:02}.000Z", self.step / 3600, self.step / 60 % 60, self.step % 60)
    }

    fn fresh_id(&mut self, kind: &str) -> String {
        self.step += 1;
        // Not in order of creation, so that ties on an ordinal fall either way.
        format!("{kind}-{:03}-{}", self.random.below(1000), self.step)
    }

    /// A record of `kind` the thread holds, if it holds any.
    fn held(&mut self, kind: &str) -> Option<Value> {
        let records = self.checked.held[kind].as_array().unwrap();
        (!records.is_empty()).then(|| records[self.random.below(records.len())].clone())
    }

    fn held_id(&mut self, kind: &str) -> Option<String> {
        self.held(kind).map(|record| record["id"].as_str().unwrap().to_owned())
    }

    /// Sends `payload` to the thread as an event of type `event_type`.
    fn send(&mut self, case: &'static str, event_type: &str, payload: Value) -> Applied {
        *self.cases.entry(case).or_default() += 1;
        let occurred_at = self.now();
        // Now and then the envelope names another thread: the thread applies what it is given.
        let thread_id = if self.random.chance(40) { "thread-elsewhere" } else { THREAD };
        let event = json!({ "id": format!("event-{}", self.step), "threadId": thread_id, "occurredAt": occurred_at, "type": event_type, "payload": payload });
        let applied = self.checked.apply(&event);
        *self.statuses.entry(typed::shell_status(&self.checked.thread.shell()).as_str()).or_default() += 1;
        applied
    }

    /// A new record of `kind`, fitting its schema, with the event type that announces it.
    fn new_record(&mut self, kind: &str) -> (&'static str, Value) {
        let id = self.fresh_id(kind);
        let now = self.now();
        let run = self.held("runs");
        let run_id = run.as_ref().filter(|_| !self.random.chance(4)).map(|run| run["id"].clone()).unwrap_or(Value::Null);
        // The node a record belongs to: its run's root, or some other; `null` only where a record may have none.
        let some_node = json!(format!("node-{}", self.random.below(4)));
        let own_node = run.as_ref().filter(|_| !run_id.is_null() && !self.random.chance(3)).map_or(some_node.clone(), |run| run["rootNodeId"].clone());
        let node_id = if own_node.is_null() { some_node } else { own_node.clone() };
        // Sometimes a record of another thread: the thread holds what it is sent.
        let thread_id = if self.random.chance(30) { "thread-elsewhere" } else { THREAD };
        match kind {
            "runs" => {
                let ordinal = self.run_ordinal();
                let root = if self.random.chance(3) { Value::Null } else { json!(format!("node-{}", self.random.below(4))) };
                let mut run = json!({
                    "id": id, "threadId": thread_id, "ordinal": ordinal, "providerInstanceId": "codex",
                    "modelSelection": { "instanceId": "codex", "model": "model-1" }, "providerThreadId": null,
                    "userMessageId": "message-0", "rootNodeId": root, "activeAttemptId": null,
                    "status": self.random.pick(&RUN_STATUSES), "requestedAt": now, "startedAt": null, "completedAt": null,
                    "checkpointId": null, "contextHandoffId": null,
                });
                self.vary_run(&mut run);
                ("run.created", run)
            }
            "attempts" => ("run-attempt.created", json!({
                "id": id, "runId": if run_id.is_null() { json!("run-none") } else { run_id }, "attemptOrdinal": 1, "rootNodeId": node_id,
                "providerInstanceId": "codex", "providerThreadId": "provider-thread-0", "providerTurnId": null, "reason": "initial",
                "status": self.random.pick(&["pending", "running", "completed", "superseded", "superseded"]), "startedAt": null, "completedAt": null,
            })),
            "nodes" => ("node.updated", json!({
                "id": id, "threadId": thread_id, "runId": run_id, "parentNodeId": null, "rootNodeId": id, "kind": "root_turn",
                "status": "running", "countsForRun": true, "providerThreadId": null, "providerTurnId": null, "nativeItemRef": null,
                "runtimeRequestId": null, "checkpointScopeId": null, "startedAt": null, "completedAt": null,
            })),
            "subagents" => ("subagent.updated", json!({
                "id": id, "threadId": thread_id, "runId": run_id, "parentNodeId": node_id, "origin": "provider_native",
                "createdBy": "agent", "driver": "codex", "providerInstanceId": "codex", "providerThreadId": null,
                "childThreadId": null, "nativeTaskRef": null, "prompt": "look into it", "title": null, "model": null,
                "status": "running", "result": null, "startedAt": null, "completedAt": null, "updatedAt": now,
            })),
            "providerSessions" => (if self.random.chance(2) { "provider-session.attached" } else { "provider-session.updated" }, json!({
                "id": id, "driver": "codex", "providerInstanceId": self.random.pick(&["codex", "codex", "claudeAgent"]), "status": "ready",
                "cwd": "/work", "model": null, "capabilities": self.capabilities, "createdAt": now, "updatedAt": now,
                "lastError": if self.random.chance(2) { json!(format!("session error {}", self.step)) } else { Value::Null },
            })),
            "providerThreads" => {
                let placeholder = self.random.chance(3);
                let mut provider_thread = json!({
                    "id": id, "driver": "codex", "providerInstanceId": format!("instance-{}", self.random.below(12)),
                    "providerSessionId": null, "appThreadId": self.random.pick(&[THREAD, THREAD, THREAD, "thread-elsewhere"]),
                    "ownerNodeId": if self.random.chance(3) { json!("node-0") } else { Value::Null },
                    "nativeThreadRef": null, "nativeConversationHeadRef": null, "status": "not_loaded", "firstRunOrdinal": null,
                    "lastRunOrdinal": null, "handoffIds": [], "forkedFrom": null, "createdAt": now, "updatedAt": now,
                });
                if !placeholder {
                    self.make_real(&mut provider_thread);
                }
                ("provider-thread.updated", provider_thread)
            }
            "providerTurns" => ("provider-turn.updated", json!({
                "id": id, "providerThreadId": "provider-thread-0", "nodeId": node_id, "runAttemptId": null, "nativeTurnRef": null,
                "ordinal": 1, "status": "running", "startedAt": null, "completedAt": null,
            })),
            "runtimeRequests" => ("runtime-request.updated", json!({
                "id": id, "nodeId": node_id, "providerTurnId": null, "nativeRequestRef": null,
                "kind": self.random.pick(&["command", "user_input", "file-change", "user_input"]),
                "status": self.random.pick(&["pending", "pending", "resolved"]),
                "responseCapability": if self.random.chance(2) { json!({ "type": "message" }) } else { json!({ "type": "live", "providerSessionId": "session-0" }) },
                "createdAt": now, "resolvedAt": null,
            })),
            "messages" => {
                let user = self.random.chance(2);
                let mut message = json!({
                    "createdBy": if user { self.random.pick(&["user", "user", "agent", "system"]) } else { "agent" },
                    "creationSource": "web", "id": id, "threadId": thread_id, "runId": run_id, "nodeId": null,
                    "role": if user { "user" } else { "assistant" }, "text": "hello", "attachments": [], "streaming": !user,
                    "createdAt": now, "updatedAt": now,
                });
                if user && self.random.chance(4) {
                    message["idleCompaction"] = json!("onReturn");
                }
                ("message.updated", message)
            }
            "plans" => ("plan.updated", if self.random.chance(2) {
                json!({ "kind": "proposed_plan", "id": id, "threadId": thread_id, "runId": run_id, "nodeId": node_id,
                        "status": self.random.pick(&["draft", "active", "completed"]), "markdown": "# Plan" })
            } else {
                json!({ "kind": "todo_list", "id": id, "threadId": thread_id, "runId": run_id, "nodeId": node_id, "status": "active",
                        "steps": [{ "id": "step-1", "text": "first", "status": "pending" }] })
            }),
            "turnItems" => {
                let item_type = self.random.pick(&ITEM_TYPES);
                let items = self.checked.held["turnItems"].as_array().unwrap();
                let last = items.last().map_or(0, |item| item["ordinal"].as_u64().unwrap());
                // Mostly at the end of the timeline; sometimes inside it, or level with another item.
                let ordinal = match self.random.below(6) {
                    0 => self.random.below(last as usize + 1) as u64,
                    1 if !items.is_empty() => items[self.random.below(items.len())]["ordinal"].as_u64().unwrap(),
                    _ => last + 1 + self.random.below(3) as u64,
                };
                let mut item = json!({
                    "id": id, "threadId": thread_id, "runId": run_id, "nodeId": own_node, "providerThreadId": null,
                    "providerTurnId": null, "nativeItemRef": null, "parentItemId": null, "ordinal": ordinal,
                    "status": self.random.pick(&ITEM_STATUSES), "title": null, "startedAt": null, "completedAt": null,
                    "updatedAt": now, "type": item_type,
                });
                let message_id = self.held_id("messages").unwrap_or_else(|| "message-0".to_owned());
                let own = match item_type {
                    "assistant_message" => json!({ "messageId": message_id, "text": "so far", "streaming": true }),
                    "reasoning" => json!({ "text": "thinking", "streaming": true }),
                    "command_execution" if self.random.chance(2) => json!({ "input": "ls" }),
                    "command_execution" => json!({ "input": "ls", "output": "a\n" }),
                    "proposed_plan" => json!({ "planId": "plan-0", "markdown": "# Plan", "streaming": true }),
                    "user_message" => json!({
                        "createdBy": "user", "creationSource": "web", "messageId": message_id,
                        "inputIntent": self.random.pick(&["turn_start", "queued_turn", "queued_turn", "steer"]), "text": "do it", "attachments": [],
                    }),
                    "error" => json!({ "failure": {
                        "class": self.random.pick(&["usage_limit", "provider_error"]), "message": format!("failure {}", self.step),
                        "code": if self.random.chance(2) { json!("E1") } else { Value::Null }, "retryable": null,
                        "resetAt": "2026-03-01T00:00:00.000Z",
                    } }),
                    "run_interrupt_request" | "run_interrupt_result" | "system_notice" => json!({ "message": "stopped" }),
                    _ => json!({ "projector": { "lumens": 1.5 } }),
                };
                item.as_object_mut().unwrap().extend(own.as_object().unwrap().clone());
                ("turn-item.updated", item)
            }
            "checkpointScopes" => ("checkpoint-scope.created", json!({
                "id": id, "threadId": thread_id, "runId": run_id, "nodeId": node_id, "parentScopeId": null, "providerThreadId": null,
                "kind": "root_run", "ordinalWithinParent": 0, "advancesAppRunCount": true, "cwd": "/work", "createdAt": now,
            })),
            "checkpoints" => ("checkpoint.captured", json!({
                "id": id, "threadId": thread_id, "scopeId": "scope-0", "runId": run_id, "nodeId": node_id, "parentCheckpointId": null,
                "ordinalWithinScope": 0, "appRunOrdinal": null, "ref": "refs/t3/checkpoints/x", "status": "ready",
                "files": [{ "path": "a.txt", "kind": "modified", "additions": 1, "deletions": 0 }], "capturedAt": now,
            })),
            "contextHandoffs" => ("context-handoff.updated", json!({
                "id": id, "threadId": thread_id, "targetRunId": "run-0", "fromProviderThreadIds": [], "toProviderThreadId": "provider-thread-0",
                "coveredRunOrdinals": { "from": 1, "to": 2 }, "strategy": "full_thread_summary", "status": "ready",
                "summaryMessageId": null, "summaryText": "summary", "createdByProviderInstanceId": null, "createdAt": now, "updatedAt": now,
            })),
            "contextTransfers" => (if self.random.chance(2) { "context-transfer.created" } else { "context-transfer.updated" }, json!({
                "id": id, "type": "fork", "sourceThreadId": THREAD, "targetThreadId": "thread-2", "sourcePoint": { "threadId": THREAD },
                "basePoint": null, "sourceProviderInstanceId": null, "targetProviderInstanceId": null, "targetRunId": null,
                "status": "pending", "resolution": null, "createdBy": "user", "error": null, "createdAt": now, "updatedAt": now,
                "consumedAt": null,
            })),
            other => panic!("no generator for {other}"),
        }
    }

    /// An ordinal for a run: mostly the next one, sometimes level with the
    /// latest run or among the earlier ones.
    fn run_ordinal(&mut self) -> u64 {
        let latest = self.checked.held["runs"].as_array().unwrap().iter().map(|run| run["ordinal"].as_u64().unwrap()).max().unwrap_or(0);
        match self.random.below(4) {
            0 => latest,
            1 => self.random.below(latest as usize + 1) as u64,
            _ => latest + 1,
        }
    }

    /// Gives a run one of the states the shell's rules tell apart.
    fn vary_run(&mut self, run: &mut Value) {
        let now = self.now();
        run["status"] = json!(self.random.pick(&RUN_STATUSES));
        run["startedAt"] = if self.random.chance(2) { json!(now) } else { Value::Null };
        let fields = run.as_object_mut().unwrap();
        match self.random.below(3) {
            0 => drop(fields.insert("queueHeld".into(), json!(true))),
            1 => drop(fields.remove("queueHeld")),
            _ => {}
        }
        if self.random.chance(3) {
            fields.insert("workStartedAt".into(), json!(now));
        }
    }

    /// Turns a placeholder provider thread into one that ran.
    fn make_real(&mut self, provider_thread: &mut Value) {
        provider_thread["updatedAt"] = json!(self.now());
        match self.random.below(3) {
            0 => provider_thread["status"] = json!("active"),
            1 => provider_thread["firstRunOrdinal"] = json!(1),
            _ => provider_thread["providerSessionId"] = json!("session-0"),
        }
        if self.random.chance(2) {
            provider_thread["goal"] = json!({ "objective": "ship it", "status": "active" });
        }
    }

    fn add(&mut self, kind: &str) -> Applied {
        let (event_type, record) = self.new_record(kind);
        self.send("a new record", event_type, record)
    }

    /// The event type that carries a new version of a record of `kind`.
    fn update_type(kind: &str) -> &'static str {
        match kind {
            "runs" => "run.updated",
            "attempts" => "run-attempt.updated",
            "nodes" => "node.updated",
            "subagents" => "subagent.updated",
            "providerSessions" => "provider-session.updated",
            "providerThreads" => "provider-thread.updated",
            "providerTurns" => "provider-turn.updated",
            "runtimeRequests" => "runtime-request.updated",
            "messages" => "message.updated",
            "plans" => "plan.updated",
            "turnItems" => "turn-item.updated",
            "checkpointScopes" => "checkpoint-scope.created",
            "checkpoints" => "checkpoint.captured",
            "contextHandoffs" => "context-handoff.updated",
            "contextTransfers" => "context-transfer.updated",
            other => panic!("no event for {other}"),
        }
    }

    /// A streaming record the thread holds, with more text: the name of the
    /// text field, and the record's kind.
    fn grown(&mut self, record_type: &str) -> Option<(&'static str, Value)> {
        let (kind, field) = match record_type {
            "message" => ("messages", "text"),
            "assistant_message" | "reasoning" => ("turnItems", "text"),
            "proposed_plan" => ("turnItems", "markdown"),
            _ => ("turnItems", "output"),
        };
        let records = self.checked.held[kind].as_array().unwrap();
        let fitting: Vec<&Value> = records.iter().filter(|record| kind == "messages" || record["type"] == record_type).collect();
        let mut record = (*fitting.get(self.random.below(fitting.len().max(1)))?).clone();
        record[field] = json!(format!("{}{} more", record[field].as_str().unwrap_or_default(), self.step));
        record["updatedAt"] = json!(self.now());
        Some((kind, record))
    }

    fn grow(&mut self, record_type: &'static str) -> Option<Applied> {
        let (kind, record) = self.grown(record_type)?;
        Some(self.send("text grows", Self::update_type(kind), record))
    }

    /// Changes something other than a record's growing text.
    fn flip(&mut self, kind: &str) -> Option<Applied> {
        let mut record = self.held(kind)?;
        let now = self.now();
        match kind {
            "runs" => self.vary_run(&mut record),
            "attempts" => record["status"] = json!(self.random.pick(&["running", "completed", "superseded", "failed"])),
            "runtimeRequests" => record["status"] = json!(self.random.pick(&["pending", "resolved", "expired", "cancelled"])),
            "messages" => {
                record["streaming"] = json!(false);
                record["updatedAt"] = json!(now);
            }
            "plans" => record["status"] = json!(self.random.pick(&["draft", "active", "completed", "superseded"])),
            "turnItems" => {
                record["status"] = json!(self.random.pick(&ITEM_STATUSES));
                record["updatedAt"] = json!(now);
            }
            "providerSessions" => {
                record["lastError"] = if self.random.chance(2) { json!(format!("session error {}", self.step)) } else { Value::Null };
                record["updatedAt"] = json!(now);
            }
            "providerThreads" => self.make_real(&mut record),
            "nodes" | "providerTurns" => record["status"] = json!(self.random.pick(&["running", "completed", "failed"])),
            "subagents" => record["status"] = json!(self.random.pick(&["running", "completed", "failed"])),
            "checkpoints" => record["status"] = json!(self.random.pick(&["ready", "stale"])),
            "contextHandoffs" => record["status"] = json!(self.random.pick(&["ready", "superseded"])),
            "contextTransfers" => record["status"] = json!(self.random.pick(&["pending", "consumed"])),
            _ => {}
        }
        Some(self.send("a status flips", Self::update_type(kind), record))
    }

    /// Moves a turn item to another ordinal.
    fn reorder(&mut self) -> Option<Applied> {
        let mut item = self.held("turnItems")?;
        let last = self.checked.held["turnItems"].as_array().unwrap().last().map_or(0, |item| item["ordinal"].as_u64().unwrap());
        item["ordinal"] = json!(self.random.below(last as usize + 3));
        Some(self.send("an ordinal changes", "turn-item.updated", item))
    }

    fn detach(&mut self, held: bool) -> Applied {
        let session = self.held_id("providerSessions").filter(|_| held).unwrap_or_else(|| "session-nowhere".to_owned());
        let payload = json!({ "providerSessionId": session, "detachedAt": self.now(), "reason": "closed" });
        self.send("a provider session is detached", "provider-session.detached", payload)
    }

    fn thread_event(&mut self) -> Applied {
        let mut thread = self.checked.held["thread"].clone();
        let now = self.now();
        let event_type = self.random.pick(&["thread.visited", "thread.metadata-updated", "thread.archived", "thread.unarchived", "thread.pinned"]);
        match event_type {
            "thread.visited" => thread["lastVisitedAt"] = json!(now),
            "thread.metadata-updated" => thread["title"] = json!(format!("Generated {}", self.step)),
            "thread.archived" => thread["archivedAt"] = json!(now),
            "thread.unarchived" => thread["archivedAt"] = Value::Null,
            _ => thread["pinnedAt"] = json!(now),
        }
        thread["updatedAt"] = json!(now);
        self.send("the thread's record changes", event_type, thread)
    }

    /// Makes a run the thread's latest and ends it; a run that fails gets the
    /// error item that says why.
    fn end_run(&mut self) -> Option<Applied> {
        let mut run = self.held("runs")?;
        let status = self.random.pick(&["failed", "failed", "completed", "completed", "interrupted", "cancelled", "waiting", "queued", "rolled_back"]);
        run["status"] = json!(status);
        run["ordinal"] = json!(self.run_ordinal().max(run["ordinal"].as_u64().unwrap()));
        run["startedAt"] = json!(self.now());
        let (run_id, root) = (run["id"].clone(), run["rootNodeId"].clone());
        let ended = self.send("a run ends", "run.updated", run);
        if status != "failed" {
            return Some(ended);
        }
        // Mostly the error of the run's own root; sometimes one from deeper in the run.
        let node = if self.random.chance(4) { json!("node-deeper") } else { root };
        let mut failure = json!({ "class": self.random.pick(&["usage_limit", "provider_error"]), "message": format!("failure {}", self.step), "code": "E2", "retryable": true });
        if self.random.chance(2) {
            failure["resetAt"] = json!("2026-03-01T00:00:00.000Z");
        }
        let status = self.random.pick(&["failed", "failed", "failed", "completed"]);
        Some(self.send_item("a run ends", "error", run_id, node, status, json!({ "failure": failure })))
    }

    /// Sends a new turn item of `item_type` at the end of the timeline.
    fn send_item(&mut self, case: &'static str, item_type: &str, run_id: Value, node_id: Value, status: &str, own: Value) -> Applied {
        let last = self.checked.held["turnItems"].as_array().unwrap().last().map_or(0, |item| item["ordinal"].as_u64().unwrap());
        let mut item = json!({
            "id": self.fresh_id("turnItems"), "threadId": THREAD, "runId": run_id, "nodeId": node_id, "providerThreadId": null,
            "providerTurnId": null, "nativeItemRef": null, "parentItemId": null, "ordinal": last + 1, "status": status, "title": null,
            "startedAt": null, "completedAt": null, "updatedAt": self.now(), "type": item_type,
        });
        item.as_object_mut().unwrap().extend(own.as_object().unwrap().clone());
        self.send(case, "turn-item.updated", item)
    }

    /// An interrupted run: the attempt it was on, the result, and now and then the request for it.
    fn interrupt_run(&mut self) -> Option<Applied> {
        let run = self.held("runs")?;
        let root = if run["rootNodeId"].is_null() { json!("node-0") } else { run["rootNodeId"].clone() };
        let attempt = json!({
            "id": self.fresh_id("attempts"), "runId": run["id"], "attemptOrdinal": 1,
            "rootNodeId": if self.random.chance(3) { json!("node-other") } else { root.clone() },
            "providerInstanceId": "codex", "providerThreadId": "provider-thread-0", "providerTurnId": null, "reason": "steering_restart",
            "status": self.random.pick(&["superseded", "superseded", "interrupted"]), "startedAt": null, "completedAt": null,
        });
        self.send("a run is interrupted", "run-attempt.created", attempt);
        if self.random.chance(3) {
            self.send_item("a run is interrupted", "run_interrupt_request", run["id"].clone(), root.clone(), "completed", json!({ "message": "stop" }));
        }
        Some(self.send_item("a run is interrupted", "run_interrupt_result", run["id"].clone(), root, "completed", json!({ "message": "stopped" })))
    }

    /// Brings the thread to rest: every pending request answered, and now and
    /// then every run that is under way ended or left waiting.
    fn settle(&mut self) -> Option<Applied> {
        let mut last = None;
        for mut request in self.checked.held["runtimeRequests"].as_array().unwrap().clone() {
            if request["status"] == "pending" {
                request["status"] = json!("resolved");
                request["resolvedAt"] = json!(self.now());
                last = Some(self.send("the thread comes to rest", "runtime-request.updated", request));
            }
        }
        if self.random.chance(2) {
            for mut run in self.checked.held["runs"].as_array().unwrap().clone() {
                if matches!(run["status"].as_str(), Some("preparing" | "starting" | "running")) {
                    run["status"] = json!(self.random.pick(&["completed", "waiting", "interrupted", "cancelled"]));
                    last = Some(self.send("the thread comes to rest", "run.updated", run));
                }
            }
        }
        last
    }

    /// One step chosen at random.
    fn step(&mut self) {
        let kind = KINDS[self.random.below(KINDS.len())].collection();
        let _ = match self.random.below(27) {
            25 | 26 => self.settle(),
            20 => Some(self.add("runtimeRequests")),
            21 => self.end_run(),
            24 => self.interrupt_run(),
            22 => {
                let kind = self.random.pick(&["providerSessions", "providerThreads", "messages", "runs"]);
                Some(self.add(kind))
            }
            23 => {
                let kind = self.random.pick(&["runtimeRequests", "providerSessions", "plans", "attempts"]);
                self.flip(kind)
            }
            0..=2 => Some(self.add(kind)),
            3 | 4 => Some(self.add("turnItems")),
            5 | 6 => self.flip("turnItems"),
            7..=10 => {
                let record_type = self.random.pick(&["message", "assistant_message", "reasoning", "proposed_plan", "command_execution"]);
                self.grow(record_type)
            }
            11..=13 => self.flip(kind),
            14 => self.flip("runs"),
            15 => self.reorder(),
            16 => {
                let held = self.random.chance(4);
                Some(self.detach(held))
            }
            17 => Some(self.thread_event()),
            18 => {
                let payload = json!({ "id": "hologram-1", "lumens": self.step });
                Some(self.send("an event of an unknown type", "hologram.projected", payload))
            }
            _ => {
                let payload = json!({ "scopeId": "scope-0", "checkpointId": "checkpoint-0", "requestedAt": self.now() });
                Some(self.send("a rollback is requested", "checkpoint.rollback-requested", payload))
            }
        };
    }
}

/// The cases the recorded sessions lack, each once by name and then all of
/// them at random, with every step checked.
#[test]
fn generated_events_follow_the_rules() {
    let mut generator = Generator::new(0x5EED_1DEA_0DD5_EED5);

    // New records of every kind.
    for kind in KINDS.map(RecordKind::collection) {
        assert!(matches!(generator.add(kind), Applied::Added { .. }), "a new record of {kind} is added");
        assert_eq!(generator.checked.held[kind].as_array().unwrap().len(), 1);
    }

    // Text growing on a message and on each kind of item that streams.
    let now = generator.now();
    for (kind, record) in [
        ("messages", json!({ "createdBy": "agent", "creationSource": "web", "id": "message-s", "threadId": THREAD, "runId": null, "nodeId": null,
            "role": "assistant", "text": "a", "attachments": [], "streaming": true, "createdAt": now, "updatedAt": now })),
        ("turnItems", json!({ "id": "item-s-message", "threadId": THREAD, "runId": null, "nodeId": null, "providerThreadId": null, "providerTurnId": null,
            "nativeItemRef": null, "parentItemId": null, "ordinal": 900, "status": "running", "title": null, "startedAt": null, "completedAt": null,
            "updatedAt": now, "type": "assistant_message", "messageId": "message-s", "text": "a", "streaming": true })),
        ("turnItems", json!({ "id": "item-s-reasoning", "threadId": THREAD, "runId": null, "nodeId": null, "providerThreadId": null, "providerTurnId": null,
            "nativeItemRef": null, "parentItemId": null, "ordinal": 901, "status": "running", "title": null, "startedAt": null, "completedAt": null,
            "updatedAt": now, "type": "reasoning", "text": "a", "streaming": true })),
        ("turnItems", json!({ "id": "item-s-command", "threadId": THREAD, "runId": null, "nodeId": null, "providerThreadId": null, "providerTurnId": null,
            "nativeItemRef": null, "parentItemId": null, "ordinal": 902, "status": "running", "title": null, "startedAt": null, "completedAt": null,
            "updatedAt": now, "type": "command_execution", "input": "ls" })),
    ] {
        let event_type = Generator::update_type(kind);
        let id = record["id"].as_str().unwrap().to_owned();
        assert!(matches!(generator.send("a new record", event_type, record.clone()), Applied::Added { .. }));
        let mut grown = record.clone();
        let field = if id == "item-s-command" { "output" } else { "text" };
        grown[field] = json!("a\n");
        grown["updatedAt"] = json!(generator.now());
        let first = generator.send("text grows", event_type, grown.clone());
        // A command's first output is more than text growing: the row gains a part.
        assert_eq!(matches!(first, Applied::Replaced { only_text_grew: true, order_changed: false, .. }), id != "item-s-command", "{id}: {first:?}");
        grown[field] = json!("a\nb\n");
        grown["updatedAt"] = json!(generator.now());
        let second = generator.send("text grows", event_type, grown.clone());
        assert!(matches!(second, Applied::Replaced { only_text_grew: true, order_changed: false, .. }), "{id}: {second:?}");
        // The row a streaming message grows in is found from the message.
        assert_eq!(generator.checked.thread.turn_item_of_message(&"message-s".into()).map(|item| item.id().as_str()), (id != "message-s").then_some("item-s-message"));
        // A status flip along with the text is not text growing.
        grown[field] = json!("a\nb\nc\n");
        grown[if kind == "messages" { "streaming" } else { "status" }] = if kind == "messages" { json!(false) } else { json!("completed") };
        let flipped = generator.send("a status flips", event_type, grown);
        assert!(matches!(flipped, Applied::Replaced { only_text_grew: false, order_changed: false, .. }), "{id}: {flipped:?}");
    }

    // An ordinal change that re-sorts, and one that does not.
    let mut item = find(&generator.checked.held, "turnItems", "item-s-reasoning").unwrap().clone();
    item["ordinal"] = json!(0);
    let moved = generator.send("an ordinal changes", "turn-item.updated", item.clone());
    assert!(matches!(moved, Applied::Replaced { order_changed: true, only_text_grew: false, .. }), "{moved:?}");
    assert_eq!(generator.checked.thread.turn_items.first().map(|item| item.id().as_str()), Some("item-s-reasoning"));
    let stayed = generator.send("an ordinal changes", "turn-item.updated", item.clone());
    assert!(matches!(stayed, Applied::Replaced { order_changed: false, only_text_grew: true, .. }), "{stayed:?}");
    // A new item that belongs inside the timeline.
    item["id"] = json!("item-s-inside");
    item["ordinal"] = json!(450);
    let inside = generator.send("a new record", "turn-item.updated", item);
    assert!(matches!(inside, Applied::Added { order_changed: true, .. }), "{inside:?}");

    // A placeholder provider thread does not become the active one; the real one does.
    let now = generator.now();
    let mut provider_thread = json!({
        "id": "provider-thread-s", "driver": "codex", "providerInstanceId": "codex", "providerSessionId": null, "appThreadId": THREAD,
        "ownerNodeId": null, "nativeThreadRef": null, "nativeConversationHeadRef": null, "status": "not_loaded", "firstRunOrdinal": null,
        "lastRunOrdinal": null, "handoffIds": [], "forkedFrom": null, "createdAt": now, "updatedAt": "2026-01-15T00:00:00.000Z",
    });
    let active_before = generator.checked.thread.record.active_provider_thread_id.clone();
    generator.send("a placeholder provider thread", "provider-thread.updated", provider_thread.clone());
    assert_eq!(generator.checked.thread.record.active_provider_thread_id, active_before);
    provider_thread["status"] = json!("active");
    generator.send("a provider thread becomes real", "provider-thread.updated", provider_thread.clone());
    assert_eq!(generator.checked.thread.record.active_provider_thread_id.as_ref().map(|id| id.as_str()), Some("provider-thread-s"));
    assert_eq!(generator.checked.thread.record.updated_at.as_str(), "2026-01-15T00:00:00.000Z", "the thread takes the provider thread's time");
    // A provider thread of another thread is held, and does not become active here.
    provider_thread["id"] = json!("provider-thread-elsewhere");
    provider_thread["appThreadId"] = json!("thread-elsewhere");
    generator.send("a record of another thread", "provider-thread.updated", provider_thread);
    assert_eq!(generator.checked.thread.record.active_provider_thread_id.as_ref().map(|id| id.as_str()), Some("provider-thread-s"));
    assert!(generator.checked.thread.provider_threads.contains("provider-thread-elsewhere"));

    // A detached provider session leaves; detaching one that is not held changes no record.
    assert!(matches!(generator.detach(true), Applied::Removed { record: RecordRef::ProviderSession(_) }));
    assert!(generator.checked.thread.provider_sessions.is_empty());
    assert_eq!(generator.detach(true), Applied::Touched);

    // A failed run shows its newest error, and when its limit resets only if the failure is a usage limit.
    let mut run = generator.held("runs").unwrap();
    for (field, value) in [("status", json!("failed")), ("ordinal", json!(50)), ("rootNodeId", json!("node-0"))] {
        run[field] = value;
    }
    generator.send("a run ends", "run.updated", run.clone());
    let failure = |class: &str, message: &str| json!({ "failure": { "class": class, "message": message, "code": "E1", "retryable": false, "resetAt": "2026-03-01T00:00:00.000Z" } });
    generator.send_item("a run ends", "error", run["id"].clone(), json!("node-0"), "failed", failure("provider_error", "the provider failed"));
    let shell = generator.checked.thread.shell();
    assert_eq!((shell.status.as_str(), shell.last_error, shell.usage_limit_reset_at), ("failed", Some(Some("the provider failed".to_owned())), Some(None)));
    generator.send_item("a run ends", "error", run["id"].clone(), json!("node-0"), "failed", failure("usage_limit", "out of quota"));
    let shell = generator.checked.thread.shell();
    assert_eq!(shell.last_error, Some(Some("out of quota".to_owned())));
    assert_eq!(shell.usage_limit_reset_at.flatten().as_ref().map(|at| at.as_str()), Some("2026-03-01T00:00:00.000Z"));

    // An event of an unknown type, and one that carries no record, only move the thread's time.
    let unknown = generator.send("an event of an unknown type", "hologram.projected", json!({ "id": "hologram-1" }));
    assert_eq!(unknown, Applied::Touched);
    assert_eq!(generator.thread_event(), Applied::Thread);

    // `DOMOVOI_GENERATED_STEPS` asks for a longer run than the everyday one.
    let steps = std::env::var("DOMOVOI_GENERATED_STEPS").ok().and_then(|steps| steps.parse().ok()).unwrap_or(1200);
    for _ in 0..steps {
        generator.step();
    }
    let sizes: Vec<String> = KINDS.iter().map(|kind| format!("{} {}", kind.collection(), ids(&generator.checked.held[kind.collection()]).len())).collect();
    println!("generated: {} events, {} checks", generator.cases.values().sum::<usize>(), generator.checked.checks);
    println!("  cases: {:?}", generator.cases);
    println!("  the shell's status after each event: {:?}", generator.statuses);
    println!("  the thread at the end: {}", sizes.join(", "));
    let shell = generator.checked.thread.shell();
    println!("  its shell: status {:?}, {} of {} items visible", shell.status.as_str(), shell.visible_item_count, generator.checked.thread.turn_items.len());
    for status in ["approval", "working", "failed", "interrupted", "completed", "idle"] {
        assert!(generator.statuses.get(status).copied().unwrap_or(0) > 5, "the generated thread's shell was `{status}` too rarely");
    }

    // A snapshot of the thread as it is now, sent the compact way: the items of
    // its visible rows left out of `turnItems`. A row inherited from another
    // thread is not one of this thread's items, and is not restored as one.
    generator.checked.check_stored();
    let mut projection = generator.checked.held.clone();
    let visible: Vec<Value> = projection["visibleTurnItems"].as_array().unwrap().clone();
    assert!(visible.len() > 20 && visible.len() < generator.checked.thread.turn_items.len(), "some items are visible and some are hidden");
    projection["turnItems"].as_array_mut().unwrap().retain(|item| !visible.iter().any(|row| row["sourceItemId"] == item["id"]));
    let mut inherited = visible[0].clone();
    inherited["visibility"] = json!("inherited");
    inherited["sourceThreadId"] = json!("thread-parent");
    inherited["sourceItemId"] = json!("item-inherited");
    inherited["item"]["id"] = json!("item-inherited");
    inherited["item"]["threadId"] = json!("thread-parent");
    projection["visibleTurnItems"].as_array_mut().unwrap().insert(0, inherited);
    let mut restored = Checked::from_snapshot(&json!({ "kind": "snapshot", "snapshotSequence": 9, "projection": projection, "turnItemsOmitLocalVisible": true }));
    restored.check("a snapshot that leaves visible items out");
    assert_eq!(restored.held, generator.checked.held, "the thread is the one the snapshot was taken of");
    assert!(!restored.thread.turn_items.contains("item-inherited"));

    for case in [
        "a new record", "text grows", "a status flips", "an ordinal changes", "a provider session is detached", "an event of an unknown type",
        "the thread's record changes", "a rollback is requested", "a run ends", "a run is interrupted", "the thread comes to rest",
    ] {
        assert!(generator.cases.get(case).copied().unwrap_or(0) > 10, "the generator reached `{case}` too rarely");
    }
}

/// Events that are not a record's new version, and input that does not fit:
/// part of a record changes or nothing does, and nothing is taken on trust.
#[test]
fn events_that_are_not_a_record() {
    let mut generator = Generator::new(1);
    generator.add("runs");
    let run_id = generator.held_id("runs").unwrap();
    let event = |event_type: &str, payload: Value| -> DomainEvent {
        serde_json::from_value(json!({ "id": "event-x", "threadId": THREAD, "occurredAt": "2026-05-01T00:00:00.000Z", "type": event_type, "payload": payload }))
            .unwrap()
    };
    let mut thread = generator.checked.thread.clone();

    // Cancelled background work is set on its run (t3code's rule): the payload is not a run to hold.
    let cancelled = event("run.background-work-cancelled", json!({ "runId": run_id, "restartCancelledBackgroundWork": [{ "kind": "shell", "label": "sleep" }] }));
    assert_eq!(thread.apply(cancelled), Applied::Replaced { record: RecordRef::Run(run_id.as_str().into()), order_changed: false, only_text_grew: false });
    assert_eq!(thread.runs.len(), 1);
    assert_eq!(thread.runs.get(run_id.as_str()).unwrap().restart_cancelled_background_work.as_ref().map(Vec::len), Some(1));
    assert_eq!(thread.record.updated_at.as_str(), "2026-05-01T00:00:00.000Z");
    let elsewhere = event("run.background-work-cancelled", json!({ "runId": "run-nowhere", "restartCancelledBackgroundWork": [] }));
    assert_eq!(thread.apply(elsewhere), Applied::Touched);

    // A pull request link's new state replaces the link, not the thread's record.
    let link = |title: &str| json!({ "host": "github.com", "repository": "o/r", "number": 7, "url": "https://github.com/o/r/pull/7",
        "source": "manual", "linkedAt": "2026-01-01T00:00:00.000Z", "snapshot": null, "stack": null, "repositoryWebUrl": title });
    assert_eq!(thread.apply(event("thread.pull-request-link-updated", link("https://github.com/o/r"))), Applied::Nothing, "no such link");
    let mut record = serde_json::to_value(&thread.record).unwrap();
    record["pullRequests"] = json!([link("https://github.com/o/r")]);
    assert_eq!(thread.apply(event("thread.pull-request-synced", record)), Applied::Thread);
    assert_eq!(thread.apply(event("thread.pull-request-link-updated", link("https://github.com/o/r2"))), Applied::Thread);
    assert_eq!(thread.record.pull_requests.as_ref().unwrap()[0].repository_web_url.as_deref(), Some("https://github.com/o/r2"));
    assert_eq!(thread.record.title, "Generated", "the thread's record is still the thread's");

    // An unknown event of a familiar family changes no record: its payload is
    // not taken for the thread, or for a run.
    let title = thread.record.title.clone();
    assert_eq!(thread.apply(event("thread.teleported", json!({ "id": THREAD, "title": "elsewhere" }))), Applied::Touched);
    assert_eq!(thread.apply(event("run.paused", json!({ "id": run_id, "status": "paused" }))), Applied::Touched);
    assert_eq!((thread.record.title.as_str(), thread.runs.len()), (title.as_str(), 1));

    // A record that does not fit its schema is refused, not held as it came.
    let run = find(&generator.checked.held, "runs", &run_id).unwrap().clone();
    let mut unfit = run.clone();
    unfit.as_object_mut().unwrap().remove("requestedAt");
    let error = serde_json::from_value::<DomainEvent>(json!({ "id": "e", "threadId": THREAD, "occurredAt": "t", "type": "run.updated", "payload": unfit })).unwrap_err();
    assert!(error.to_string().contains("requestedAt"), "{error}");
    assert!(StoredRecord::decode(RecordKind::Run, json!({ "id": "run-x" })).is_err());

    // A projection that lists a record twice holds its last version once, in
    // the first one's place.
    let mut projection = generator.checked.held.clone();
    let mut again = run.clone();
    again["status"] = json!("completed");
    projection["runs"] = json!([run, { "id": "run-other", "threadId": THREAD, "ordinal": 99, "providerInstanceId": "codex",
        "modelSelection": { "instanceId": "codex", "model": "m" }, "providerThreadId": null, "userMessageId": "m", "rootNodeId": null,
        "activeAttemptId": null, "status": "queued", "requestedAt": "t", "startedAt": null, "completedAt": null, "checkpointId": null,
        "contextHandoffId": null }, again]);
    let twice = Thread::from_projection(serde_json::from_value(projection).unwrap());
    assert_eq!(twice.runs.iter().map(|run| (run.id.as_str(), run.status.as_str())).collect::<Vec<_>>(), [(run_id.as_str(), "completed"), ("run-other", "queued")]);
}

/// Sends a new record of `kind` with `fields` set as given (`null` for a field to leave out).
fn send_with(generator: &mut Generator, kind: &str, fields: Value) {
    let (event_type, mut record) = generator.new_record(kind);
    for (field, value) in fields.as_object().unwrap() {
        match value {
            Value::Null => drop(record.as_object_mut().unwrap().remove(field)),
            value => record[field] = value.clone(),
        }
    }
    assert!(matches!(generator.send("a new record", event_type, record), Applied::Added { .. }));
}

/// Sends a new turn item at the end of the timeline and gives its id.
fn send_item(generator: &mut Generator, item_type: &str, run: Value, own: Value) -> String {
    match generator.send_item("a new record", item_type, run, json!("node-0"), "completed", own) {
        Applied::Added { record, .. } => record.id().to_owned(),
        other => panic!("a new item is added, not {other:?}"),
    }
}

/// What a timeline hides, rule by rule.
#[test]
fn hidden_turn_items() {
    let mut generator = Generator::new(2);
    for (id, ordinal, status) in [("r1", 1, "completed"), ("r2", 2, "rolled_back"), ("r3", 3, "queued"), ("r4", 4, "cancelled")] {
        send_with(&mut generator, "runs", json!({ "id": id, "threadId": THREAD, "ordinal": ordinal, "status": status, "rootNodeId": "node-0" }));
    }
    let from_user = |intent: &str| json!({ "createdBy": "user", "creationSource": "web", "messageId": "message-0", "inputIntent": intent, "text": "do it", "attachments": [] });
    let visible = |generator: &Generator| -> Vec<(u64, String)> {
        generator.checked.thread.visible_turn_items().iter().map(|row| (row.position, row.source_item_id.as_str().to_owned())).collect()
    };

    let asked = send_item(&mut generator, "user_message", json!("r1"), from_user("turn_start"));
    // An item of a run that was rolled back.
    send_item(&mut generator, "assistant_message", json!("r2"), json!({ "messageId": "message-0", "text": "undone", "streaming": false }));
    // The message of a queued turn whose run is still queued, or was cancelled.
    send_item(&mut generator, "user_message", json!("r3"), from_user("queued_turn"));
    send_item(&mut generator, "user_message", json!("r4"), from_user("queued_turn"));
    let started_anyway = send_item(&mut generator, "user_message", json!("r4"), from_user("turn_start"));
    let noted = send_item(&mut generator, "system_notice", Value::Null, json!({ "message": "noted" }));
    assert_eq!(visible(&generator), [(0, asked.clone()), (1, started_anyway.clone()), (2, noted.clone())]);
    assert_eq!(generator.checked.thread.shell().visible_item_count, 3);

    // The result of an interrupt nobody asked for, on an attempt that was
    // superseded; it shows once the timeline holds the request.
    send_with(&mut generator, "attempts", json!({ "runId": "r1", "rootNodeId": "node-0", "status": "superseded" }));
    let result = send_item(&mut generator, "run_interrupt_result", json!("r1"), json!({ "message": "stopped" }));
    assert_eq!(visible(&generator).len(), 3, "the result is hidden");
    assert!(!generator.checked.thread.shell().latest_run_stop_requested.unwrap());
    send_with(&mut generator, "runs", json!({ "id": "r5", "threadId": THREAD, "ordinal": 5, "status": "interrupted", "rootNodeId": "node-0" }));
    send_with(&mut generator, "attempts", json!({ "runId": "r5", "rootNodeId": "node-0", "status": "superseded" }));
    let request = send_item(&mut generator, "run_interrupt_request", json!("r5"), json!({ "message": "stop" }));
    let answered = send_item(&mut generator, "run_interrupt_result", json!("r5"), json!({ "message": "stopped" }));
    assert_eq!(visible(&generator)[3..], [(3, request), (4, answered)]);
    assert!(generator.checked.thread.turn_items.contains(result.as_str()));
    assert!(generator.checked.thread.shell().latest_run_stop_requested.unwrap());
}

/// What a shell says of a thread with a run that waits and requests pending.
#[test]
fn the_shell_of_a_waiting_thread() {
    let mut generator = Generator::new(3);
    let (started, sent) = ("2026-01-01T00:00:05.000Z", "2026-01-01T00:00:04.000Z");
    send_with(&mut generator, "runs", json!({ "id": "r1", "threadId": THREAD, "ordinal": 1, "status": "completed", "queueHeld": null }));
    send_with(&mut generator, "runs", json!({ "id": "r2", "threadId": THREAD, "ordinal": 2, "status": "waiting", "startedAt": started, "queueHeld": null, "workStartedAt": null }));
    send_with(&mut generator, "messages", json!({ "role": "user", "createdBy": "user", "streaming": false, "updatedAt": sent, "idleCompaction": null }));
    let shell = generator.checked.thread.shell();
    assert_eq!((shell.status.as_str(), shell.latest_run_id.as_ref().map(|run| run.as_str())), ("waiting", Some("r2")));
    assert_eq!(shell.active_run_id, None, "a run that waits is not active");
    assert_eq!(shell.activity_run_status, Some(Some(typed::ActivityRunStatus::Waiting)));
    assert_eq!(shell.activity_run_started_at.clone().flatten().as_ref().map(|at| at.as_str()), Some(started));
    assert_eq!(shell.latest_user_message_at.as_ref().map(|at| at.as_str()), Some(sent));
    assert_eq!((shell.pending_runtime_request.as_ref(), typed::shell_status(&shell)), (None, ShellStatus::Working));

    // Of the pending requests, one that blocks the agent comes before a
    // question to the user, though the question is newer.
    let live = json!({ "type": "live", "providerSessionId": "session-0" });
    send_with(&mut generator, "runtimeRequests", json!({ "id": "q1", "kind": "user_input", "status": "pending", "createdAt": "2026-01-01T00:00:09.000Z", "responseCapability": { "type": "message" } }));
    let question = generator.checked.thread.shell().pending_runtime_request.unwrap();
    assert_eq!((question.id.as_str(), question.blocking), ("q1", Some(false)));
    send_with(&mut generator, "runtimeRequests", json!({ "id": "q2", "kind": "command", "status": "pending", "createdAt": "2026-01-01T00:00:06.000Z", "responseCapability": live }));
    let shell = generator.checked.thread.shell();
    let pending = shell.pending_runtime_request.as_ref().unwrap();
    assert_eq!((pending.id.as_str(), pending.blocking), ("q2", Some(true)));
    assert_eq!(typed::shell_status(&shell), ShellStatus::Approval);
}

/// A change of status is announced when it deserves the user's attention.
#[test]
fn a_change_of_status_is_announced_when_it_matters() {
    use ShellStatus::{Approval, Completed, Failed, Idle, Working};
    assert_eq!(typed::notice(None, Approval), None, "the first status seen is where the thread starts");
    assert_eq!(typed::notice(Some(Working), Approval), Some("Needs your approval"));
    assert_eq!(typed::notice(Some(Working), Completed), Some("Finished"));
    assert_eq!(typed::notice(Some(Idle), Completed), None);
    assert_eq!(typed::notice(Some(Working), Failed), Some("Failed"));
    assert_eq!(typed::notice(Some(Failed), Failed), None);
    assert_eq!(typed::notice(Some(Approval), Working), None);
}

/// A measurement over a server's own database: the thread built from the
/// records it stores for each thread is checked as the threads above are.
/// `DOMOVOI_THREADS` names a file with one line per thread:
///
/// ```sh
/// sqlite3 <home>/userdata/domovoi.sqlite "SELECT json_object('threadId', t.id, 'thread', json(t.record), 'entities',
///   (SELECT json_group_array(json_object('kind', e.kind, 'record', json(e.record)))
///    FROM (SELECT kind, record FROM thread_entities WHERE thread_id = t.id ORDER BY first_sequence) e))
///   FROM threads t;" > threads.ndjson
/// DOMOVOI_THREADS=threads.ndjson cargo test -p domovoi-model --test typed_thread stored_threads -- --ignored --nocapture
/// ```
#[test]
#[ignore = "measurement over a server's own database; needs DOMOVOI_THREADS"]
fn stored_threads_follow_the_rules() {
    let path = std::env::var("DOMOVOI_THREADS").expect("DOMOVOI_THREADS names the exported threads");
    let text = std::fs::read_to_string(&path).expect("the exported threads");
    let (mut threads, mut records, mut longest) = (0, 0, 0);
    // Threads that cannot be built, each with why.
    let mut unbuilt: BTreeMap<String, (usize, String)> = BTreeMap::new();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let row: Value = serde_json::from_str(line).expect("a JSON line");
        let thread_id = row["threadId"].as_str().unwrap_or("?");
        let rows = row["entities"].as_array().map(Vec::as_slice).unwrap_or_default();
        threads += 1;
        records += rows.len();
        longest = longest.max(rows.len());
        let built = serde_json::from_value::<AppThread>(row["thread"].clone()).map_err(|error| format!("thread: {error}")).and_then(|record| {
            let stored: Result<Vec<StoredRecord>, String> = rows
                .iter()
                .filter_map(|stored| Some((RecordKind::of_collection(stored["kind"].as_str()?)?, &stored["record"])))
                .map(|(kind, record)| StoredRecord::decode(kind, record).map_err(|error| format!("{}: {error}", kind.collection())))
                .collect();
            Ok(Thread::from_records(record, stored?))
        });
        match built {
            Ok(thread) => Checked::new(thread).check(thread_id),
            Err(why) => {
                let reason: String = why.chars().take(160).collect();
                unbuilt.entry(reason).or_insert_with(|| (0, thread_id.to_owned())).0 += 1;
            }
        }
    }
    let failed: usize = unbuilt.values().map(|(count, _)| count).sum();
    println!("{threads} threads, {records} records (the longest thread has {longest})");
    println!("{} threads built and checked; {failed} that could not be built", threads - failed);
    for (reason, (count, example)) in &unbuilt {
        println!("    {count:>5}  {reason}  (e.g. {example})");
    }
}

/// A second measurement over a server's own database: every event it stored,
/// replayed thread by thread from each thread's `thread.created`, and checked
/// after every event as the tests above do. `DOMOVOI_EVENTS` names a file of the
/// thread events in the order they were stored:
///
/// ```sh
/// sqlite3 <home>/userdata/domovoi.sqlite "SELECT json_object('threadId', aggregate_id, 'event', json(payload))
///   FROM events WHERE aggregate_kind = 'thread' ORDER BY sequence;" > events.ndjson
/// DOMOVOI_EVENTS=events.ndjson cargo test --release -p domovoi-model --test typed_thread stored_events -- --ignored --nocapture
/// ```
#[test]
#[ignore = "measurement over a server's own database; needs DOMOVOI_EVENTS"]
fn stored_events_follow_the_rules() {
    use std::io::BufRead;
    let path = std::env::var("DOMOVOI_EVENTS").expect("DOMOVOI_EVENTS names the exported events");
    let file = std::io::BufReader::new(std::fs::File::open(&path).expect("the exported events"));
    let mut threads: BTreeMap<String, Checked> = BTreeMap::new();
    let (mut events, mut before_created) = (0, 0);
    let mut by_type: BTreeMap<String, usize> = BTreeMap::new();
    let mut applied_kinds: BTreeMap<String, usize> = BTreeMap::new();
    for line in file.lines() {
        let row: Value = serde_json::from_str(&line.expect("a line")).expect("a JSON line");
        let (thread, event) = (row["threadId"].as_str().expect("a thread id"), &row["event"]);
        let event_type = event["type"].as_str().expect("an event type");
        *by_type.entry(event_type.to_owned()).or_default() += 1;
        events += 1;
        if event_type == "thread.created" {
            let record: AppThread = serde_json::from_value(event["payload"].clone()).expect("the created thread decodes");
            let mut checked = Checked::new(Thread::new(record));
            checked.check("thread.created");
            threads.insert(thread.to_owned(), checked);
        } else if let Some(checked) = threads.get_mut(thread) {
            let applied = checked.apply(event);
            let name = applied_name(&applied);
            let only_text_grew = matches!(applied, Applied::Replaced { only_text_grew: true, .. });
            *applied_kinds.entry(if only_text_grew { format!("{name} (only text grew)") } else { name }).or_default() += 1;
        } else {
            before_created += 1;
        }
    }
    let checks: usize = threads.values().map(|checked| checked.checks).sum();
    println!("{events} events of {} threads replayed, {checks} checks, all passed", threads.len());
    println!("  events of a thread never created (not applied): {before_created}");
    println!("  by type: {by_type:?}");
    println!("  what they did: {applied_kinds:?}");
}
