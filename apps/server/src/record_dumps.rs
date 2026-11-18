//! What the parts of the server that edit stored records leave behind, written
//! out so two builds can be compared: startup recovery, the repair of old
//! background-agent results, session import, checkpoints and rollback.
//!
//! ```text
//! DOMOVOI_DUMP_DIR=/tmp/before cargo test -p domovoi-server record_dumps
//! ```
//!
//! Each test sets a store up (through the orchestrator and `Turn`, as a
//! running server would, or from events given as JSON), runs the code, and
//! writes `<DOMOVOI_DUMP_DIR>/<name>.json`: what each step answered, the events it
//! appended, and at the end every stored record. Times of the run and
//! generated ids are replaced by placeholders. Without `DOMOVOI_DUMP_DIR` the
//! tests run all the same and write nothing.

use crate::adapters::turn::{Plan, Turn, TurnEnd, WindowUse, Work};
use crate::command_differential::Normalizer;
use crate::db::Db;
use crate::orchestration::commands::{Effect, Orchestrator};
use crate::orchestration::effects::{ProviderError, ProviderHost};
use crate::orchestration::events::Origin;
use crate::store::Store;
use crate::test_records::{self, AT, with};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use domovoi_model::typed::{
    FileChangeDetail, PlanStep, PlanStepStatus, ProviderFailureClass, ProviderRequestKind, ProviderSessionStatus, ProviderThreadStatus, TurnItemStatus,
};
use tokio::sync::mpsc::UnboundedReceiver;

/// A store, and what each step did to it.
struct Dump {
    name: &'static str,
    store: Store,
    normalizer: Normalizer,
    last_sequence: i64,
    steps: Vec<Value>,
    /// Ids the test's data fixes that look generated, each with the name that stands for it.
    names: Vec<(&'static str, &'static str)>,
    /// A step's events are listed thread by thread, not in the order written:
    /// for code that works through threads in no fixed order (sessions as a folder lists them).
    by_thread: bool,
}

impl Dump {
    fn new(name: &'static str, store: Store) -> Self {
        Dump { name, store, normalizer: Normalizer::new(), last_sequence: 0, steps: vec![], names: vec![], by_thread: false }
    }

    /// `value` with the test's fixed ids named, then normalised.
    fn normalised(&mut self, value: &Value) -> Value {
        let mut text = value.to_string();
        for (id, name) in &self.names {
            text = text.replace(id, name);
        }
        self.normalizer.value(&serde_json::from_str(&text).expect("json"))
    }

    fn rows(&self, sql: &str, columns: usize) -> Vec<Vec<Value>> {
        self.store
            .db()
            .read(|c| {
                let mut st = c.prepare(sql)?;
                let rows = st.query_map([], |r| {
                    (0..columns)
                        .map(|i| {
                            let text: Option<String> = match r.get_ref(i)? {
                                rusqlite::types::ValueRef::Null => None,
                                rusqlite::types::ValueRef::Integer(n) => Some(n.to_string()),
                                other => Some(other.as_str()?.to_owned()),
                            };
                            Ok(text.map_or(Value::Null, |text| serde_json::from_str(&text).unwrap_or(Value::String(text))))
                        })
                        .collect::<rusqlite::Result<Vec<Value>>>()
                })?;
                rows.collect()
            })
            .expect("rows")
    }

    /// Notes a step: what it answered and the events appended since the last one.
    fn step(&mut self, step: &str, outcome: Value) {
        let appended = self.rows(
            &format!("SELECT sequence, aggregate_id, type, payload, command_id FROM events WHERE sequence > {} ORDER BY sequence", self.last_sequence),
            5,
        );
        let mut events = vec![];
        for row in appended {
            self.last_sequence = row[0].as_i64().expect("a sequence");
            let sequence = if self.by_thread { Value::Null } else { row[0].clone() };
            events.push(json!({ "sequence": sequence, "thread": row[1], "type": row[2], "commandId": row[4], "event": row[3] }));
        }
        if self.by_thread {
            // A stable sort: each thread's events stay in the order written.
            events.sort_by_key(|event| event["thread"].as_str().map(str::to_owned));
        }
        let step = self.normalised(&json!({ "step": step, "outcome": outcome, "events": events }));
        self.steps.push(step);
    }

    /// Waits until nothing has been appended for a moment (work spawned in the background is done).
    async fn settled(&self) {
        let mut seen = self.store.latest_sequence().expect("sequence");
        let mut quiet = 0;
        while quiet < 6 {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            let now = self.store.latest_sequence().expect("sequence");
            quiet = if now == seen { quiet + 1 } else { 0 };
            seen = now;
        }
    }

    /// Writes the dump: the steps, then everything the store holds.
    fn finish(mut self) -> Value {
        let threads = self.rows("SELECT id, record, deleted FROM threads ORDER BY id", 3);
        let records = self.rows("SELECT thread_id, kind, entity_id, record FROM thread_entities ORDER BY thread_id, kind, first_sequence, entity_id", 4);
        let receipts = self.rows("SELECT command_id, result FROM command_receipts ORDER BY rowid", 2);
        let outbox = self.rows("SELECT key, effect FROM effect_outbox ORDER BY rowid", 2);
        let imports = self.rows("SELECT thread_id, cwd FROM agent_session_imports ORDER BY thread_id", 2);
        let projects = self.rows("SELECT id, shell FROM projects ORDER BY rowid", 2);
        let end = json!({ "threads": threads, "records": records, "receipts": receipts, "outbox": outbox, "imports": imports, "projects": projects });
        let end = self.normalised(&end);
        let out = json!({ "steps": self.steps, "end": end });
        let events: usize = self.steps.iter().map(|s| s["events"].as_array().map_or(0, Vec::len)).sum();
        if let Ok(dir) = std::env::var("DOMOVOI_DUMP_DIR")
            && self.name != "self-check"
        {
            std::fs::create_dir_all(&dir).expect("the dump's folder");
            let path = Path::new(&dir).join(format!("{}.json", self.name));
            std::fs::write(&path, serde_json::to_string_pretty(&out).expect("json")).expect("the dump is written");
            println!("record dump {}: {} steps, {events} events, written to {}", self.name, self.steps.len(), path.display());
        }
        out
    }
}

/// What a call answered, as JSON, whichever way it went.
fn outcome<T: serde::Serialize, E: serde::Serialize>(result: Result<T, E>) -> Value {
    match result {
        Ok(answer) => json!({ "ok": answer }),
        Err(error) => json!({ "error": error }),
    }
}

/// A stored record as JSON, read where it is kept.
fn stored(store: &Store, thread_id: &str, kind: &str, id: &str) -> Value {
    let text: String = store
        .db()
        .read(|c| c.query_row("SELECT record FROM thread_entities WHERE thread_id = ?1 AND kind = ?2 AND entity_id = ?3", [thread_id, kind, id], |r| r.get(0)))
        .unwrap_or_else(|e| panic!("no {kind} {id} in {thread_id}: {e}"));
    serde_json::from_str(&text).expect("a stored record is JSON")
}

/// The ids of the thread's records of `kind`, in the order they first appeared.
fn ids(store: &Store, thread_id: &str, kind: &str) -> Vec<String> {
    store
        .db()
        .read(|c| {
            let mut st = c.prepare("SELECT entity_id FROM thread_entities WHERE thread_id = ?1 AND kind = ?2 ORDER BY first_sequence")?;
            let rows = st.query_map([thread_id, kind], |r| r.get::<_, String>(0))?;
            rows.collect()
        })
        .expect("ids")
}

fn append(store: &Store, thread_id: &str, event_type: &str, payload: Value) {
    test_records::append(store, thread_id, event_type, payload, &Origin::default()).expect("the test's event is stored");
}

fn project(id: &str, root: &Path) -> domovoi_contracts::shell::ProjectShell {
    domovoi_contracts::shell::ProjectShell {
        id: id.into(), title: id.into(), has_custom_title: false, workspace_root: root.to_string_lossy().into(),
        repository_identity: None, default_model_selection: None, default_thread_env_mode: None, auto_pull: false,
        favicon_path: None, project_icon: None, scripts: vec![], pinned_at: None, archived_at: None,
        created_at: "2030-01-01T00:00:00.000Z".into(), updated_at: "2030-01-01T00:00:00.000Z".into(),
    }
}

/// A fixed folder for the dump `name`, empty.
fn folder(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("domovoi-record-dump-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    dir
}

fn git(cwd: &Path, args: &[&str]) {
    let done = crate::process::output_blocking(crate::process::blocking_command("git").args(args).current_dir(cwd), crate::process::QUICK).expect("git");
    assert!(done.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&done.stderr));
}

fn run_id(thread_id: &str, ordinal: u64) -> String {
    crate::orchestration::commands::run_id(thread_id, ordinal)
}

async fn launch(o: &Orchestrator, thread_id: &str, project: &str, instance: &str, fields: Value) -> Value {
    let input = with(
        json!({ "commandId": format!("launch-{thread_id}"), "creationSource": "web", "threadId": thread_id, "projectId": project, "title": format!("Thread {thread_id}"),
            "generateTitle": false, "modelSelection": { "instanceId": instance, "model": "some-model" }, "runtimeMode": "approval-required",
            "interactionMode": "default", "workspaceStrategy": { "type": "root" },
            "initialMessage": { "messageId": format!("{thread_id}-m1"), "text": "hello", "attachments": [] } }),
        fields,
    );
    outcome(crate::rpc::shell::launch_thread(o, input).await.map(|launched| launched.thread_id))
}

async fn send(o: &Orchestrator, thread_id: &str, n: u64, text: &str) -> Value {
    let command = json!({ "type": "message.dispatch", "commandId": format!("send-{thread_id}-{n}"), "threadId": thread_id, "messageId": format!("{thread_id}-m{n}"),
        "text": text, "attachments": [], "createdBy": "user", "creationSource": "web", "dispatchMode": { "type": "queue_after_active" } });
    outcome(crate::rpc::shell::dispatch_command(o, command).await)
}

/// The effects queued so far.
fn effects(rx: &mut UnboundedReceiver<Effect>) -> Value {
    let mut out = vec![];
    while let Ok(effect) = rx.try_recv() {
        out.push(json!(effect));
    }
    json!(out)
}

/// A provider turn of the thread's run `ordinal` under way: a session, a
/// native thread and turn, and work of every kind left open.
fn turn_under_way(store: &Store, thread_id: &str, ordinal: u64) -> Turn {
    let mut turn = Turn::begin(store.clone(), thread_id, &run_id(thread_id, ordinal)).expect("the run and its provider thread are there");
    turn.session(ProviderSessionStatus::Ready, "/work", "some-model", None);
    turn.thread_status(ProviderThreadStatus::Active, Some(&format!("native-{thread_id}")));
    turn.turn_started(&format!("native-turn-{thread_id}-{ordinal}"));
    turn.token_usage(WindowUse { used_tokens: 10, max_tokens: Some(1000), input_tokens: 6, cached_input_tokens: 1, output_tokens: 4, reasoning_output_tokens: 2 });
    turn.item("done-1", Work::Command { input: "ls".into(), output: Some("a.txt".into()), exit_code: Some(0) }, TurnItemStatus::Completed);
    turn.assistant("reply-1", "So far", true, Some("commentary"));
    turn.item("think-1", Work::Reasoning { text: "thinking".into(), streaming: true }, TurnItemStatus::Running);
    turn.item("cmd-1", Work::Command { input: "make".into(), output: None, exit_code: None }, TurnItemStatus::Running);
    let changes = vec![FileChangeDetail { operation: "update".into(), path: "src/a.rs".into(), old_path: None, file_type: None, mime_type: None }];
    turn.item("edit-1", Work::FileChange { file_name: "a.rs".into(), changes, texts: Some(("a".into(), "b".into())), patch: None }, TurnItemStatus::Running);
    turn.item("tool-1", Work::Tool { name: "lookup".into(), input: json!({ "deep": [1, 2.5, null] }), output: None, viewed_image_path: None }, TurnItemStatus::Running);
    turn.item("search-1", Work::WebSearch { patterns: vec!["rust".into()] }, TurnItemStatus::Running);
    let step = PlanStep { id: "step-1".into(), text: "Look".into(), status: PlanStepStatus::Running, duration_anchor_at: None, duration_ms: None };
    turn.plan("todo-1", Plan::Todo { steps: vec![step], explanation: None }, false);
    turn.plan("plan-1", Plan::Proposed { markdown: "# Plan".into() }, false);
    turn.open_request("req-1", "cmd-1", ProviderRequestKind::Command, "Run make");
    let questions = serde_json::from_value(json!([{ "id": "db", "header": "Database", "question": "Which one?",
        "options": [{ "label": "Postgres", "description": "Relational" }], "multiSelect": false }]))
    .expect("the question");
    turn.open_user_input("req-2", "ask-1", questions);
    turn
}

/// Startup recovery: a store left mid-turn in each state it repairs, and the
/// states it leaves alone. (The turns that ended have one item each: a turn
/// closes several open items in no fixed order.)
#[tokio::test]
async fn record_dumps_recovery() {
    let dir = folder("recovery");
    let store = Store::new(Db::open(&dir.join("state.sqlite")).expect("db"));
    store.create_project(project("p", &dir), None).expect("project");
    let (o, mut rx) = Orchestrator::new(store.clone());
    let o = Arc::new(o);
    let mut d = Dump::new("recovery", store.clone());

    // Launched, the provider not started yet.
    launch(&o, "starting", "p", "codex", json!({})).await;
    // The same, still preparing.
    launch(&o, "preparing", "p", "codex", json!({})).await;
    let run = stored(&store, "preparing", "runs", &run_id("preparing", 1));
    append(&store, "preparing", "run.updated", with(run, json!({ "status": "preparing" })));
    // Mid-turn, with work of every kind open and a message queued behind it.
    for (thread, instance) in [("running", "codex"), ("running-claude", "claudeAgent")] {
        launch(&o, thread, "p", instance, json!({})).await;
        let _turn = turn_under_way(&store, thread, 1);
        send(&o, thread, 2, "next").await;
        // An item of a type this build does not know, a message of another run, a request already answered.
        append(&store, thread, "turn-item.updated", json!({ "id": "future-1", "threadId": thread, "runId": run_id(thread, 1), "ordinal": 1_000_900, "status": "running",
            "updatedAt": AT, "type": "a_type_of_a_newer_server", "streaming": true, "more": { "kept": [1, 2] } }));
        append(&store, thread, "message.updated", with(test_records::message(thread, "other-run", "assistant", "elsewhere", true), json!({ "runId": "run-of-another-thread" })));
        append(&store, thread, "runtime-request.updated", test_records::runtime_request("answered", "resolved"));
        append(&store, thread, "runtime-request.updated", test_records::runtime_request("unanswered", "pending"));
    }
    // The turn finished, its finalization did not; a message waits behind it.
    launch(&o, "waiting", "p", "codex", json!({})).await;
    turn(&store, "waiting", 1, "All done.", TurnEnd::Completed);
    send(&o, "waiting", 2, "next").await;
    // Settled one way or another: left alone.
    launch(&o, "interrupted", "p", "codex", json!({})).await;
    turn(&store, "interrupted", 1, "Cut short.", TurnEnd::Interrupted);
    launch(&o, "failed", "p", "codex", json!({})).await;
    turn(&store, "failed", 1, "Went wrong.", TurnEnd::Failed);
    // A deleted thread whose run was live.
    launch(&o, "deleted", "p", "codex", json!({})).await;
    outcome(crate::rpc::shell::dispatch_command(&o, json!({ "type": "thread.delete", "commandId": "del", "threadId": "deleted" })).await);
    d.step("setup", effects(&mut rx));

    let found = crate::orchestration::recovery::interrupt_orphaned_runs(&store);
    d.step("interrupt_orphaned_runs", json!({ "interrupted": found.interrupted, "unfinalized": found.unfinalized }));
    let again = crate::orchestration::recovery::interrupt_orphaned_runs(&store);
    d.step("interrupt_orphaned_runs again", json!({ "interrupted": again.interrupted, "unfinalized": again.unfinalized }));

    // The whole of startup on a store left the same way: finalization and the queues move on.
    let dir2 = dir.join("second");
    std::fs::create_dir_all(&dir2).expect("dir");
    let store2 = Store::new(Db::open(&dir2.join("state.sqlite")).expect("db"));
    store2.create_project(project("p", &dir2), None).expect("project");
    let (o2, mut rx2) = Orchestrator::new(store2.clone());
    let o2 = Arc::new(o2);
    let mut d2 = Dump::new("recovery-startup", store2.clone());
    launch(&o2, "running", "p", "codex", json!({})).await;
    let _turn = turn_under_way(&store2, "running", 1);
    send(&o2, "running", 2, "next").await;
    launch(&o2, "waiting", "p", "claudeAgent", json!({})).await;
    turn(&store2, "waiting", 1, "All done.", TurnEnd::Completed);
    send(&o2, "waiting", 2, "next").await;
    launch(&o2, "idle", "p", "codex", json!({})).await;
    turn(&store2, "idle", 1, "Cut short.", TurnEnd::Interrupted);
    d2.step("setup", effects(&mut rx2));
    crate::orchestration::recovery::recover(o2.clone());
    d2.settled().await;
    d2.step("recover", effects(&mut rx2));

    // A deliberate stop remembers the turns it cut off; the next start continues them.
    let cut_off = vec![("running".to_owned(), run_id("running", 1)), ("idle".to_owned(), run_id("idle", 1)), ("waiting".to_owned(), run_id("waiting", 1)),
        ("no-such-thread".to_owned(), "no-such-run".to_owned())];
    crate::orchestration::recovery::record_restart_continuation(&store2, &cut_off);
    let taken = crate::orchestration::recovery::take_restart_continuation(&store2);
    let taken_again = crate::orchestration::recovery::take_restart_continuation(&store2);
    d2.step("restart continuation taken", json!({ "taken": taken, "again": taken_again }));
    crate::orchestration::recovery::continue_after_restart(o2.clone(), taken);
    d2.settled().await;
    d2.step("continue_after_restart", effects(&mut rx2));

    let out = d.finish();
    let interrupted = out["steps"][1]["outcome"]["interrupted"].as_array().map_or(0, Vec::len);
    assert_eq!(interrupted, 4, "the live runs were interrupted: {}", out["steps"][1]["outcome"]);
    assert!(out["steps"][2]["events"].as_array().is_some_and(Vec::is_empty), "a second start records nothing");
    let out2 = d2.finish();
    assert!(out2["steps"][1]["events"].as_array().is_some_and(|e| e.len() > 10), "startup recorded what it settled");
    assert!(out2["steps"][3]["events"].as_array().is_some_and(|e| !e.is_empty()), "the cut-off turns were continued");
}

const RECEIPT: &str = "Async agent launched successfully. (This tool result is internal metadata.) agentId: a1";

/// The repair of background agents recorded the old way: with a report in
/// the provider log, without one, and one that needs no repair.
#[test]
fn record_dumps_repair() {
    let dir = folder("repair");
    let store = Store::new(Db::open(&dir.join("state.sqlite")).expect("db"));
    let mut d = Dump::new("repair", store.clone());
    let origin = Origin { driver: Some("claudeAgent".into()), provider_instance_id: Some("claudeAgent".into()), command_id: None };
    let put = |thread: &str, event_type: &str, payload: Value| {
        test_records::append(&store, thread, event_type, payload, &origin).expect("the test's event is stored");
    };
    put("t1", "thread.created", test_records::thread("t1", "p1"));
    for (tool, result, status, child) in [
        ("toolu_A", RECEIPT, "completed", true),
        ("toolu_B", RECEIPT, "completed", true),
        ("toolu_C", RECEIPT, "completed", false),
        ("toolu_D", RECEIPT, "running", true),
        ("toolu_E", "A real result, not a receipt.", "completed", true),
    ] {
        let node = format!("node:provider:claudeAgent:native-item:{tool}");
        let child_thread = format!("t1:subagent:{tool}");
        put("t1", "node.updated", with(test_records::node("t1", &node, "completed"), json!({ "kind": "subagent" })));
        put("t1", "subagent.updated", json!({
            "id": node, "threadId": "t1", "parentNodeId": "root", "origin": "provider_native", "createdBy": "agent",
            "driver": "claudeAgent", "providerInstanceId": "claudeAgent", "childThreadId": if child { json!(child_thread) } else { Value::Null },
            "nativeTaskRef": { "driver": "claudeAgent", "nativeId": tool, "strength": "strong" }, "prompt": "Map the shell",
            "title": "Map the shell", "status": status, "result": result, "completedAt": "2026-11-16T00:16:52.550Z", "updatedAt": AT,
        }));
        put("t1", "turn-item.updated", json!({
            "id": format!("turn-item:{tool}"), "threadId": "t1", "nodeId": node, "ordinal": 1, "updatedAt": AT, "type": "subagent",
            "subagentId": node, "origin": "provider_native", "driver": "claudeAgent", "providerInstanceId": "claudeAgent",
            "childThreadId": if child { json!(child_thread) } else { Value::Null }, "prompt": "Map the shell", "status": status, "result": result,
        }));
        // Another item of the same node, with no result of its own.
        put("t1", "turn-item.updated", with(test_records::assistant_item("t1", &format!("note:{tool}"), "m-none", 2, "a note", false), json!({ "nodeId": node })));
        if child {
            put(&child_thread, "thread.created", test_records::thread(&child_thread, "p1"));
            put(&child_thread, "message.updated", test_records::message(&child_thread, "m-prompt", "user", "Map the shell", false));
            put(&child_thread, "message.updated", test_records::message(&child_thread, "m-result", "assistant", result, false));
            put(&child_thread, "turn-item.updated", test_records::assistant_item(&child_thread, "i-result", "m-result", 1, result, false));
            put(&child_thread, "turn-item.updated", test_records::assistant_item(&child_thread, "i-other", "m-prompt", 2, "unrelated", false));
        }
    }
    d.step("setup", Value::Null);

    let logs = dir.join("logs");
    std::fs::create_dir_all(&logs).expect("dir");
    let notice = |tool: &str, status: &str, summary: &str, at: &str| {
        json!({ "t": at, "direction": "incoming", "payload": { "type": "system", "subtype": "task_notification", "task_id": "a1", "tool_use_id": tool, "status": status, "summary": summary } })
    };
    let lines = [
        json!({ "t": "2026-11-16T00:19:00Z", "payload": { "type": "system", "subtype": "task_progress", "tool_use_id": "toolu_A" } }),
        notice("toolu_A", "completed", "An earlier report.", "2026-11-16T00:18:00.000Z"),
        notice("toolu_A", "completed", "## Findings\nThe shell drives the core.", "2026-11-16T00:19:49.730Z"),
        notice("toolu_D", "failed", "It could not finish.", "2026-11-16T00:21:00.000Z"),
        notice("toolu_C", "stopped", "  ", "2026-11-16T00:22:00.000Z"),
        notice("toolu_Z", "completed", "Nobody asked.", "2026-11-16T00:23:00.000Z"),
    ];
    std::fs::write(logs.join("claude-s1.ndjson"), lines.iter().map(|l| format!("{l}\n")).collect::<String>()).expect("log");
    std::fs::write(logs.join("codex-app-server.ndjson"), format!("{}\n", notice("toolu_B", "completed", "Not Claude's log.", AT))).expect("log");

    let fixed = crate::adapters::repair::background_receipts(&store, &logs);
    d.step("background_receipts", json!(fixed));
    let again = crate::adapters::repair::background_receipts(&store, &logs);
    d.step("background_receipts again", json!(again));
    let out = d.finish();
    assert_eq!((fixed, again), (4, 0), "{}", out["steps"][1]);
}

const CLAUDE_SESSION: &str = "0b5d6a8e-1c2f-4a3b-9d4e-5f6a7b8c9d0e";
const CLAUDE_LONG_SESSION: &str = "2c3d4e5f-6a7b-4c8d-9e0f-1a2b3c4d5e6f";
const CODEX_SESSION: &str = "019a1b2c-3d4e-7f60-8a1b-2c3d4e5f6a7b";

const TERMINAL_SESSION: &str = "1c2d3e4f-5a6b-4c7d-8e9f-0a1b2c3d4e5f";

fn session_names() -> Vec<(&'static str, &'static str)> {
    vec![(CLAUDE_SESSION, "claude-session"), (CLAUDE_LONG_SESSION, "claude-long-session"), (CODEX_SESSION, "codex-session"), (TERMINAL_SESSION, "terminal-session")]
}

fn lines(records: &[Value]) -> String {
    records.iter().map(|r| format!("{r}\n")).collect()
}

fn claude_transcript(cwd: &str) -> String {
    lines(&[
        json!({ "type": "summary", "summary": "ignored" }),
        json!({ "type": "user", "cwd": cwd, "sessionId": CLAUDE_SESSION, "timestamp": "2026-11-13T10:00:00Z",
            "message": { "role": "user", "content": "<system-reminder>ctx</system-reminder>Where is add defined?\nSecond line" } }),
        json!({ "type": "assistant", "isSidechain": true, "message": { "role": "assistant", "content": [{ "type": "text", "text": "sidechain" }] } }),
        json!({ "type": "assistant", "timestamp": "2026-11-13T10:00:05Z",
            "message": { "role": "assistant", "model": "claude-haiku-5-5", "content": [
                { "type": "thinking", "thinking": "hidden" }, { "type": "text", "text": "In calc.py." }, { "type": "tool_use", "id": "t1" }] } }),
        json!({ "type": "user", "timestamp": "2026-11-13T10:01:00Z", "origin": { "kind": "task-notification" },
            "message": { "role": "user", "content": "<task-notification>done</task-notification>" } }),
        json!({ "type": "assistant", "message": { "role": "assistant", "model": "<synthetic>", "content": [{ "type": "text", "text": "API error" }] } }),
        json!({ "type": "ai-title", "aiTitle": "Find add" }),
        json!({ "type": "custom-title", "customTitle": "  My rename  " }),
    ])
}

fn codex_rollout(cwd: &str, subagent: bool) -> String {
    let source = if subagent { json!({ "subagent": { "parent": "x" } }) } else { json!("cli") };
    lines(&[
        json!({ "timestamp": "2026-11-13T09:00:00Z", "type": "session_meta", "payload": { "id": CODEX_SESSION, "cwd": cwd, "source": source } }),
        json!({ "type": "turn_context", "payload": { "cwd": cwd, "model": "gpt-5-codex" } }),
        json!({ "timestamp": "2026-11-13T09:00:01Z", "type": "response_item", "payload": { "type": "message", "role": "user",
            "content": [{ "type": "input_text", "text": "<environment_context>\n  <cwd>/x</cwd>\n</environment_context>" }] } }),
        json!({ "timestamp": "2026-11-13T09:00:02Z", "type": "response_item", "payload": { "type": "message", "role": "user",
            "content": [{ "type": "input_text", "text": "Fix the tests" }], "internal_chat_message_metadata_passthrough": { "turn_id": "turn-1" } } }),
        json!({ "timestamp": "2026-11-13T09:00:02Z", "type": "event_msg", "payload": { "type": "user_message", "message": "Fix the tests" } }),
        json!({ "type": "response_item", "payload": { "type": "reasoning", "summary": [], "content": null } }),
        json!({ "timestamp": "2026-11-13T09:00:09Z", "type": "response_item", "payload": { "type": "message", "role": "assistant",
            "content": [{ "type": "output_text", "text": "Done." }] } }),
    ])
}

fn claude_turn(session: &str, at_user: &str, at_reply: &str, text: &str) -> Vec<Value> {
    vec![
        json!({ "type": "user", "sessionId": session, "timestamp": at_user, "message": { "role": "user", "content": text } }),
        json!({ "type": "assistant", "timestamp": at_reply,
            "message": { "role": "assistant", "model": "claude-haiku-5-5", "content": [{ "type": "text", "text": format!("Re: {text}") }] } }),
    ]
}

fn append_lines(path: &Path, values: &[Value]) {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new().append(true).open(path).expect("the transcript");
    f.write_all(lines(values).as_bytes()).expect("written");
}

/// Session import: a Claude transcript, a long one, a Codex rollout; read
/// again unchanged, with more said in the terminal, and after Domovoi ran the thread.
#[test]
fn record_dumps_import() {
    use crate::agent_sessions::{ScanEnv, Source, import_recent};
    let root = folder("import");
    let home = root.join("home");
    let work = root.join("work").join("calc");
    std::fs::create_dir_all(work.join(".git")).expect("dir");
    std::fs::write(work.join(".git/config"), "[remote \"origin\"]\n\turl = git@github.com:acme/calc.git\n").expect("config");
    let cwd = work.to_string_lossy().into_owned();
    let store = Store::new(Db::open(&root.join("state.sqlite")).expect("db"));
    // The dump's clock starts before the transcripts are written: their times are this run's.
    let mut d = Dump::new("import", store.clone());
    (d.names, d.by_thread) = (session_names(), true);

    let claude_dir = home.join(".claude/projects/-work-calc");
    std::fs::create_dir_all(&claude_dir).expect("dir");
    let transcript = claude_dir.join(format!("{CLAUDE_SESSION}.jsonl"));
    std::fs::write(&transcript, claude_transcript(&cwd)).expect("transcript");
    let mut long = vec![json!({ "type": "user", "cwd": cwd, "sessionId": CLAUDE_LONG_SESSION, "timestamp": "2026-11-12T08:00:00Z", "message": { "content": "first" } })];
    long.extend((0..250).map(|i| json!({ "type": "assistant", "timestamp": "2026-11-12T08:00:01Z", "message": { "content": format!("reply {i}") } })));
    std::fs::write(claude_dir.join(format!("{CLAUDE_LONG_SESSION}.jsonl")), lines(&long)).expect("transcript");
    std::fs::write(claude_dir.join("not-a-session-id.jsonl"), claude_transcript(&cwd).replace(CLAUDE_SESSION, "not-a-session-id")).expect("transcript");
    let day = home.join(".codex/sessions/2026/11/13");
    std::fs::create_dir_all(&day).expect("dir");
    std::fs::write(day.join(format!("rollout-2026-11-13T09-00-00-{CODEX_SESSION}.jsonl")), codex_rollout(&cwd, false)).expect("rollout");
    std::fs::write(day.join("rollout-2026-11-13T09-30-00-019a1b2c-3d4e-7f60-8a1b-ffffffffffff.jsonl"), codex_rollout(&cwd, true)).expect("rollout");
    std::fs::write(home.join(".codex/session_index.jsonl"), format!("{}\n", json!({ "id": CODEX_SESSION, "thread_name": "Codex rename" }))).expect("index");
    let env = ScanEnv {
        homes: vec![(Source::Claude, "claudeAgent".into(), home.join(".claude")), (Source::Codex, "codex".into(), home.join(".codex"))],
        base_dir: root.join("domovoi-home"),
        worktrees_dir: root.join("domovoi-home/worktrees"),
        temp_dir: root.join("tmp"),
        home,
    };
    store.create_project(project("p1", &work), None).expect("project");
    store.create_project(project("p2", &root.join("work").join("other")), None).expect("project");
    d.step("setup", Value::Null);

    d.step("scan", outcome(crate::agent_sessions::scan(&env, &store)));
    d.step("import into a project that is not there", outcome(import_recent(&env, &store, "nope", None)));
    d.step("import with another root expected", outcome(import_recent(&env, &store, "p1", Some("/elsewhere"))));
    d.step("import", outcome(import_recent(&env, &store, "p1", Some(&format!("{cwd}/")))));
    d.step("import again", outcome(import_recent(&env, &store, "p1", None)));
    d.step("scan after", outcome(crate::agent_sessions::scan(&env, &store)));

    // More was said in the terminal.
    append_lines(&transcript, &claude_turn(CLAUDE_SESSION, "2026-11-13T11:00:00Z", "2026-11-13T11:00:04Z", "And mul?"));
    d.step("import after more was said", outcome(import_recent(&env, &store, "p1", None)));
    d.step("import again with nothing new", outcome(import_recent(&env, &store, "p1", None)));
    // The same thread, settled no more: its settled time is not moved.
    let claude_thread = format!("import:claudeAgent:{CLAUDE_SESSION}");
    d.step("unsettle", outcome(crate::rpc::shell::dispatch_command(&Orchestrator::new(store.clone()).0, json!({ "type": "thread.unsettle", "commandId": "u1", "threadId": claude_thread, "reason": "user" })).now_or_never().expect("a command does not wait")));
    append_lines(&transcript, &claude_turn(CLAUDE_SESSION, "2026-11-13T11:30:00Z", "2026-11-13T11:30:04Z", "And div?"));
    d.step("import after more was said to an unsettled thread", outcome(import_recent(&env, &store, "p1", None)));

    // Once Domovoi has run the thread, the terminal no longer feeds it.
    append(&store, &claude_thread, "run.created", test_records::run(&claude_thread, "r1", "completed"));
    append_lines(&transcript, &claude_turn(CLAUDE_SESSION, "2026-11-13T12:00:00Z", "2026-11-13T12:00:04Z", "From Domovoi"));
    d.step("import after Domovoi ran the thread", outcome(import_recent(&env, &store, "p1", None)));

    // The imported thread belongs to its project.
    d.step("import into another project", outcome(import_recent(&env, &store, "p2", None)));

    // The automatic import and the sync of new terminal sessions, on a fresh store.
    let store2 = Store::new(Db::open(&root.join("state2.sqlite")).expect("db"));
    let mut d2 = Dump::new("import-automatic", store2.clone());
    (d2.names, d2.by_thread) = (session_names(), true);
    let report = crate::agent_sync::auto_import(&env, &store2);
    d2.step("auto_import", json!({ "projects": report.projects, "sessions": report.sessions, "error": report.error }));
    let report = crate::agent_sync::auto_import(&env, &store2);
    d2.step("auto_import again", json!({ "projects": report.projects, "sessions": report.sessions, "error": report.error }));
    let mut seen = crate::agent_sync::Seen::default();
    d2.step("sync, first pass", json!(crate::agent_sync::sync(&env, &store2, &mut seen)));
    let folder_name: String = cwd.chars().map(|c| if c.is_ascii_alphanumeric() { c } else { '-' }).collect();
    let session_dir = env.home.join(".claude/projects").join(folder_name);
    std::fs::create_dir_all(&session_dir).expect("dir");
    let session = TERMINAL_SESSION;
    std::fs::write(session_dir.join(format!("{session}.jsonl")), lines(&claude_turn(session, "2026-11-14T09:00:00Z", "2026-11-14T09:00:03Z", "New from the terminal")).replacen("\"sessionId\"", &format!("\"cwd\":{},\"sessionId\"", json!(cwd)), 1)).expect("transcript");
    d2.step("sync, a new session", json!(crate::agent_sync::sync(&env, &store2, &mut seen)));
    d2.step("sync, nothing new", json!(crate::agent_sync::sync(&env, &store2, &mut seen)));

    let out = d.finish();
    assert_eq!(out["steps"][4]["outcome"]["ok"]["importedCount"], 3, "{}", out["steps"][4]["outcome"]);
    assert!(out["steps"][5]["events"].as_array().is_some_and(Vec::is_empty), "a second import writes nothing");
    assert!(out["steps"][7]["events"].as_array().is_some_and(|e| e.len() == 5), "two messages, their items, the thread: {}", out["steps"][7]["events"]);
    let out2 = d2.finish();
    assert_eq!(out2["steps"][0]["outcome"]["sessions"], 3, "{}", out2["steps"][0]["outcome"]);
    assert_eq!(out2["steps"][3]["outcome"], 1, "the new session was imported");
}

/// A provider that rewinds its conversation when asked, or refuses.
struct Rewinds {
    refuse: std::sync::atomic::AtomicBool,
    asked: std::sync::Mutex<Vec<String>>,
}

impl ProviderHost for Rewinds {
    async fn start_turn(&self, _: &str, _: &str) -> Result<(), ProviderError> {
        Ok(())
    }
    async fn respond(&self, _: &str, _: &str, _: &str, _: Option<crate::orchestration::commands::Answers>) -> Result<(), ProviderError> {
        Ok(())
    }
    async fn interrupt(&self, _: &str, _: &str) -> Result<(), ProviderError> {
        Ok(())
    }
    async fn runtime_mode_changed(&self, _: &str) {}
    async fn rollback_conversation(&self, thread_id: &str, checkpoint_id: &str) -> Result<(), ProviderError> {
        self.asked.lock().expect("lock").push(format!("{thread_id} {checkpoint_id}"));
        if self.refuse.load(std::sync::atomic::Ordering::SeqCst) { Err("the provider is gone".into()) } else { Ok(()) }
    }
    async fn steer(&self, _: &str, _: &str, _: &str) -> Result<(), ProviderError> {
        Ok(())
    }
    async fn stop_subagent(&self, _: &str, _: &str) -> Result<(), ProviderError> {
        Ok(())
    }
}

/// A turn of the thread's run `ordinal` that says `reply` and ends as `end`.
fn turn(store: &Store, thread_id: &str, ordinal: u64, reply: &str, end: TurnEnd) {
    let mut turn = Turn::begin(store.clone(), thread_id, &run_id(thread_id, ordinal)).expect("the run and its provider thread are there");
    turn.session(ProviderSessionStatus::Ready, "/work", "some-model", None);
    turn.thread_status(ProviderThreadStatus::Active, Some(&format!("native-{thread_id}")));
    turn.turn_started(&format!("native-turn-{thread_id}-{ordinal}"));
    turn.set_turn_cursor(&format!("cursor-{thread_id}-{ordinal}"));
    turn.assistant(&format!("reply-{ordinal}"), reply, false, None);
    turn.complete(end, (end == TurnEnd::Failed).then_some("The provider failed."));
}

/// Checkpoints and rollback: the baseline, a checkpoint per run with its
/// files, turn diffs, a failed run, finalization, a delegated task reporting
/// back, and rollbacks that work, restore files, and fail.
#[tokio::test]
async fn record_dumps_checkpoints_and_rollback() {
    use crate::orchestration::{checkpoints, effects, rollback};
    let dir = folder("checkpoints");
    let repo = dir.join("repo");
    std::fs::create_dir_all(&repo).expect("dir");
    git(&repo, &["init", "-q", "-b", "main"]);
    std::fs::write(repo.join("a.txt"), "one\n").expect("file");
    git(&repo, &["add", "-A"]);
    git(&repo, &["-c", "user.name=domovoi", "-c", "user.email=domovoi@example.invalid", "commit", "-q", "-m", "first"]);
    let plain = dir.join("plain");
    std::fs::create_dir_all(&plain).expect("dir");
    let store = Store::new(Db::open(&dir.join("state.sqlite")).expect("db"));
    store.create_project(project("g", &repo), None).expect("project");
    store.create_project(project("n", &plain), None).expect("project");
    let (o, mut rx) = Orchestrator::new(store.clone());
    let o = Arc::new(o);
    let mut d = Dump::new("checkpoints", store.clone());
    let host = Rewinds { refuse: false.into(), asked: Default::default() };
    let diff = |from: u64, to: u64, thread: &'static str| {
        let store = store.clone();
        async move { outcome(checkpoints::turn_diff(&store, thread, from, to, checkpoints::DiffCall::TurnDiff).await) }
    };

    d.step("launch", launch(&o, "t", "g", "claudeAgent", json!({})).await);
    checkpoints::ensure_baseline(&store, "t", &run_of(&store, "t", 1)).await;
    d.step("ensure_baseline", Value::Null);
    checkpoints::ensure_baseline(&store, "t", &run_of(&store, "t", 1)).await;
    d.step("ensure_baseline again", Value::Null);

    // Run 1 changes files and completes.
    std::fs::write(repo.join("a.txt"), "one\ntwo\n").expect("file");
    std::fs::write(repo.join("b.txt"), "new\n").expect("file");
    turn(&store, "t", 1, "Added two.", TurnEnd::Completed);
    d.step("turn 1", Value::Null);
    effects::finalize_run(&o, "t", &run_id("t", 1)).await;
    d.step("finalize_run 1", effects(&mut rx));
    d.step("turn_diff 0..1", diff(0, 1, "t").await);
    d.step("turn_diff 0..7", diff(0, 7, "t").await);
    d.step("turn_diff of a thread without checkpoints", diff(0, 1, "no-such-thread").await);

    // Run 2 fails before its provider starts; run 3 is queued behind it and starts when it is finalized.
    d.step("send 2", send(&o, "t", 2, "again").await);
    d.step("send 3", send(&o, "t", 3, "and again").await);
    effects::fail_run(&store, "t", &run_id("t", 2), "The provider did not start.", ProviderFailureClass::ProviderError);
    d.step("fail_run 2", Value::Null);
    effects::fail_run(&store, "t", "no-such-run", "Nothing to fail.", ProviderFailureClass::ProviderError);
    d.step("fail_run of no run", Value::Null);
    effects::finalize_run(&o, "t", &run_id("t", 2)).await;
    d.step("finalize_run 2", effects(&mut rx));

    // Run 3 removes a file and is interrupted.
    std::fs::remove_file(repo.join("b.txt")).expect("file");
    std::fs::write(repo.join("c.txt"), "third\n").expect("file");
    turn(&store, "t", 3, "Removed b.", TurnEnd::Interrupted);
    effects::finalize_run(&o, "t", &run_id("t", 3)).await;
    d.step("turn 3 and finalize_run 3", effects(&mut rx));
    effects::finalize_run(&o, "t", &run_id("t", 3)).await;
    d.step("finalize_run 3 again", effects(&mut rx));
    d.step("turn_diff 1..3", diff(1, 3, "t").await);

    // Rollbacks: an unknown checkpoint, a provider that refuses, the conversation only, files too.
    let scope = crate::orchestration::commands::scope_id("t");
    let first = checkpoints::checkpoint_id(&scope, 1);
    rollback::run(&o, &host, "t", "no-such-checkpoint", false, "rb-unknown").await;
    d.step("rollback to an unknown checkpoint", Value::Null);
    host.refuse.store(true, std::sync::atomic::Ordering::SeqCst);
    rollback::run(&o, &host, "t", &first, false, "rb-refused").await;
    d.step("rollback the provider refuses", Value::Null);
    host.refuse.store(false, std::sync::atomic::Ordering::SeqCst);
    rollback::run(&o, &host, "t", &first, false, "rb-conversation").await;
    d.step("rollback of the conversation", json!({ "b.txt": repo.join("b.txt").exists(), "c.txt": repo.join("c.txt").exists() }));
    d.step("turn_diff 1..3 after the rollback", diff(1, 3, "t").await);
    rollback::run(&o, &host, "t", &first, true, "rb-files").await;
    d.step("rollback with files", json!({ "a.txt": std::fs::read_to_string(repo.join("a.txt")).ok(), "b.txt": repo.join("b.txt").exists(), "c.txt": repo.join("c.txt").exists() }));
    let baseline = checkpoints::checkpoint_id(&scope, 0);
    rollback::run(&o, &host, "t", &baseline, true, "rb-baseline").await;
    d.step("rollback to the baseline", json!({ "a.txt": std::fs::read_to_string(repo.join("a.txt")).ok(), "b.txt": repo.join("b.txt").exists(), "asked": host.asked.lock().expect("lock").clone() }));

    // The run after a rollback gets the next checkpoint.
    d.step("send 4", send(&o, "t", 4, "once more").await);
    std::fs::write(repo.join("d.txt"), "fourth\n").expect("file");
    turn(&store, "t", 4, "Added d.", TurnEnd::Completed);
    effects::finalize_run(&o, "t", &run_id("t", 4)).await;
    d.step("turn 4 and finalize_run 4", effects(&mut rx));

    // A thread outside a git repository has no checkpoints: its run completes all the same.
    d.step("launch outside a repository", launch(&o, "plain", "n", "codex", json!({})).await);
    checkpoints::ensure_baseline(&store, "plain", &run_of(&store, "plain", 1)).await;
    turn(&store, "plain", 1, "Done.", TurnEnd::Completed);
    effects::finalize_run(&o, "plain", &run_id("plain", 1)).await;
    d.step("turn and finalize_run outside a repository", effects(&mut rx));
    d.step("turn_diff outside a repository", diff(0, 1, "plain").await);

    // A delegated task tells the thread that delegated it when it settles, once.
    let delegated = json!({ "createdBy": "agent", "creationSource": "mcp", "delegatedFrom": { "environmentId": "env-1", "threadId": "plain", "title": "Thread plain" } });
    d.step("launch a delegated task", launch(&o, "task", "n", "codex", delegated).await);
    turn(&store, "task", 1, "The task's report.", TurnEnd::Completed);
    effects::finalize_run(&o, "task", &run_id("task", 1)).await;
    d.step("finalize_run of the delegated task", effects(&mut rx));
    effects::finalize_run(&o, "task", &run_id("task", 1)).await;
    d.step("finalize_run of the delegated task again", effects(&mut rx));

    // The provider host's part of a Claude rollback: the conversation resumes at the target turn.
    let claude = crate::adapters::Host::new(o.clone(), dir.join("userdata"), dir.join("logs"));
    d.step("launch on claude", launch(&o, "c", "g", "claudeAgent", json!({})).await);
    checkpoints::ensure_baseline(&store, "c", &run_of(&store, "c", 1)).await;
    turn(&store, "c", 1, "One.", TurnEnd::Completed);
    effects::finalize_run(&o, "c", &run_id("c", 1)).await;
    send(&o, "c", 2, "two").await;
    turn(&store, "c", 2, "Two.", TurnEnd::Completed);
    effects::finalize_run(&o, "c", &run_id("c", 2)).await;
    d.step("two turns on claude", effects(&mut rx));
    let scope_c = crate::orchestration::commands::scope_id("c");
    d.step("host rollback to the first run", outcome(claude.rollback_conversation("c", &checkpoints::checkpoint_id(&scope_c, 1)).await.map_err(|e| e.to_string())));
    d.step("host rollback to the baseline", outcome(claude.rollback_conversation("c", &checkpoints::checkpoint_id(&scope_c, 0)).await.map_err(|e| e.to_string())));
    d.step("host rollback to the newest checkpoint", outcome(claude.rollback_conversation("c", &checkpoints::checkpoint_id(&scope_c, 2)).await.map_err(|e| e.to_string())));
    d.step("host rollback to an unknown checkpoint", outcome(claude.rollback_conversation("c", "no-such-checkpoint").await.map_err(|e| e.to_string())));
    d.step("host runtime mode changed", { claude.runtime_mode_changed("c").await; Value::Null });

    let out = d.finish();
    let step = |name: &str| out["steps"].as_array().into_iter().flatten().find(|s| s["step"] == name).unwrap_or_else(|| panic!("no step {name}")).clone();
    assert!(step("turn_diff 0..1")["outcome"]["ok"]["diff"].as_str().is_some_and(|d| d.contains("+two")), "{}", step("turn_diff 0..1"));
    assert_eq!(step("rollback with files")["outcome"], json!({ "a.txt": "one\ntwo\n", "b.txt": true, "c.txt": false }));
    assert_eq!(step("rollback to the baseline")["outcome"]["a.txt"], "one\n");
    assert!(step("finalize_run of the delegated task")["events"].as_array().is_some_and(|e| e.iter().any(|e| e["thread"] == "plain" && e["type"] == "message.updated")));
    assert!(step("host rollback to the first run")["events"].as_array().is_some_and(|e| e.len() == 1), "{}", step("host rollback to the first run"));
}

/// The thread's run `ordinal`, as the checkpoint functions take it.
fn run_of(store: &Store, thread_id: &str, ordinal: u64) -> domovoi_model::typed::Run {
    serde_json::from_value(stored(store, thread_id, "runs", &run_id(thread_id, ordinal))).expect("the run")
}

use futures::FutureExt as _;

/// A dump that cannot fail proves nothing: one field of one record changed
/// is a different dump.
#[test]
fn record_dumps_tell_one_changed_field() {
    let store = Store::new(Db::open_in_memory().expect("db"));
    append(&store, "t1", "thread.created", test_records::thread("t1", "p"));
    append(&store, "t1", "run.created", test_records::run("t1", "r1", "queued"));
    let mut d = Dump::new("self-check", store.clone());
    d.step("setup", Value::Null);
    let before = d.finish();
    let other = Store::new(Db::open_in_memory().expect("db"));
    append(&other, "t1", "thread.created", test_records::thread("t1", "p"));
    append(&other, "t1", "run.created", test_records::run("t1", "r1", "running"));
    let mut d = Dump::new("self-check", other.clone());
    d.step("setup", Value::Null);
    assert_ne!(before, d.finish());
    assert_eq!(ids(&store, "t1", "runs"), ["r1"]);
}
