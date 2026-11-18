//! Parity with t3code's own projection: start from the TypeScript server's
//! thread snapshot (compact ids and all), apply the events it streamed
//! afterwards, and at every sequence where it published a thread shell on the
//! shell stream, require the shell derived from our thread to agree with it.

use serde_json::Value;
use std::collections::BTreeMap;
use domovoi_model::typed::Thread;

/// Shell fields compared. `updatedAt`/`opens`/`lastVisitedAt` depend on visit
/// timing and are excluded; everything derived from runs/items/requests is in.
const FIELDS: &[&str] = &[
    "title", "status", "runtimeMode", "interactionMode", "providerInstanceId", "modelSelection",
    "latestRunId", "latestRunRequestedAt", "latestRunStartedAt", "latestRunCompletedAt",
    "latestRunStopRequested", "activeRunId", "activityRunStatus", "activityRunStartedAt",
    "activeProviderThreadId", "lastError", "lastErrorClass", "latestUserMessageAt",
    "latestUserAuthoredMessageAt", "hasActionableProposedPlan", "providerInstanceHistory",
    "itemCount", "visibleItemCount", "archivedAt", "deletedAt",
];

fn fixture() -> Vec<Value> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/golden/ts-session1/wire.ndjson");
    std::fs::read_to_string(path).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect()
}

struct Recorded {
    snapshot: Value,
    events: BTreeMap<u64, Value>,
    shells: BTreeMap<u64, Value>,
}

fn recorded(thread_id: &str) -> Recorded {
    let mut requests: BTreeMap<(i64, String), Value> = BTreeMap::new();
    let mut snapshot = None;
    let mut events = BTreeMap::new();
    let mut shells = BTreeMap::new();
    for rec in fixture() {
        if rec["kind"] != "ws" || !rec["frame"].is_object() {
            continue;
        }
        let f = &rec["frame"];
        let conn = rec["conn"].as_i64().unwrap_or(0);
        if f["_tag"] == "Request" {
            requests.insert((conn, f["id"].to_string()), f.clone());
            continue;
        }
        if f["_tag"] != "Chunk" {
            continue;
        }
        let Some(req) = requests.get(&(conn, f["requestId"].to_string())) else { continue };
        for item in f["values"].as_array().unwrap() {
            match (req["tag"].as_str(), item["kind"].as_str()) {
                (Some("orchestration.subscribeThread"), Some("snapshot")) if req["payload"]["threadId"] == thread_id => {
                    snapshot.get_or_insert_with(|| item.clone());
                }
                (Some("orchestration.subscribeThread"), Some("event")) if item["event"]["threadId"] == thread_id => {
                    events.insert(item["sequence"].as_u64().unwrap(), item["event"].clone());
                }
                (Some("orchestration.subscribeShell"), Some("thread.updated")) if item["thread"]["id"] == thread_id => {
                    shells.insert(item["sequence"].as_u64().unwrap(), item["thread"].clone());
                }
                _ => {}
            }
        }
    }
    Recorded { snapshot: snapshot.expect("thread snapshot recorded"), events, shells }
}

fn check_thread(thread_id: &str) -> (usize, Vec<String>) {
    let rec = recorded(thread_id);
    let mut thread = Thread::from_snapshot(serde_json::from_value(rec.snapshot.clone()).unwrap());
    let start = thread.snapshot_sequence;
    assert_eq!(Some(start), rec.snapshot["snapshotSequence"].as_u64());
    let mut compared = 0;
    let mut diffs = Vec::new();
    for (&seq, event) in rec.events.range(start + 1..) {
        thread.apply(serde_json::from_value(event.clone()).unwrap());
        let Some(ts_shell) = rec.shells.get(&seq) else { continue };
        // Only compare where we hold every event up to this sequence.
        if (start + 1..=seq).any(|s| !rec.events.contains_key(&s) && rec.shells.contains_key(&s)) {
            continue;
        }
        let ours = serde_json::to_value(thread.shell()).unwrap();
        compared += 1;
        for f in FIELDS {
            let a = ts_shell.get(*f).cloned().unwrap_or(Value::Null);
            let b = ours.get(*f).cloned().unwrap_or(Value::Null);
            if a != b {
                diffs.push(format!("seq {seq} {f}: t3code={a} domovoi={b}"));
            }
        }
    }
    (compared, diffs)
}

#[test]
fn shells_match_t3code_for_codex_thread() {
    let (compared, diffs) = check_thread("6421f1e6-14f6-4d7a-89b3-6e82dac38210");
    eprintln!("codex thread: {compared} shells compared");
    assert!(compared >= 5, "too few shells compared: {compared}");
    assert!(diffs.is_empty(), "{} mismatches over {compared} shells:\n{}", diffs.len(), diffs.join("\n"));
}

#[test]
fn shells_match_t3code_for_claude_threads() {
    for t in ["4454828a-013b-4976-8887-d9064f5ca464", "056c0574-d9dd-46ff-a008-9ea9779662be"] {
        let (compared, diffs) = check_thread(t);
        eprintln!("{t}: {compared} shells compared");
        assert!(compared >= 3, "{t}: too few shells compared: {compared}");
        assert!(diffs.is_empty(), "{t}: {} mismatches over {compared} shells:\n{}", diffs.len(), diffs.join("\n"));
    }
}
