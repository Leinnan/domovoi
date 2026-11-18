//! A fixed script of commands run through the orchestrator on a fresh store,
//! with everything it leaves behind written out: each command's answer, the
//! events it appended, the effects it queued, and at the end every stored
//! record and receipt. Two builds of the server behave alike if they write
//! the same file.
//!
//! ```text
//! DOMOVOI_DIFFERENTIAL_OUT=/tmp/after.json cargo test -p domovoi-server command_differential -- --ignored
//! ```
//!
//! Commands are given as JSON and decoded where the RPC edge decodes them
//! (`dispatch_json`, `launch_json`); setup between commands appends events as
//! a provider adapter would. Times of the run and generated ids are replaced
//! by placeholders, so two runs of one build write the same file.

use crate::db::Db;
use crate::orchestration::commands::{Effect, Orchestrator};
use crate::orchestration::events::Origin;
use crate::store::Store;
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tokio::sync::mpsc::UnboundedReceiver;

/// `orchestration.dispatchCommand`, as the RPC edge runs it.
async fn dispatch_json(o: &Orchestrator, command: Value) -> Result<Value, Value> {
    crate::rpc::shell::dispatch_command(o, command).await.map(|answer| json!(answer)).map_err(|failure| json!(failure))
}

/// `orchestration.launchThread`, as the RPC edge runs it.
async fn launch_json(o: &Orchestrator, input: Value) -> Result<Value, Value> {
    crate::rpc::shell::launch_thread(o, input).await.map(|answer| json!(answer)).map_err(|failure| json!(failure))
}

/// Replaces what differs from run to run: generated ids, generated branch
/// names and the worktree folders named after them, and times of this run.
pub(crate) struct Normalizer {
    uuid: regex::Regex,
    time: regex::Regex,
    branch: regex::Regex,
    folder_hash: regex::Regex,
    uuids: HashMap<String, usize>,
    started_ms: i64,
}

impl Normalizer {
    pub(crate) fn new() -> Self {
        Normalizer {
            uuid: regex::Regex::new("[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}").expect("regex"),
            time: regex::Regex::new(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d{3}Z").expect("regex"),
            branch: regex::Regex::new("t3([/-])[0-9a-f]{8}").expect("regex"),
            folder_hash: regex::Regex::new("-[0-9a-f]{16}").expect("regex"),
            uuids: HashMap::new(),
            started_ms: domovoi_contracts::time_fmt::now_millis() - 5_000,
        }
    }

    fn of_this_run(&self, ms: i64) -> bool {
        ms >= self.started_ms && ms <= domovoi_contracts::time_fmt::now_millis() + 5_000
    }

    fn text(&mut self, text: &str) -> String {
        let mut out = String::new();
        let mut rest = 0;
        for found in self.uuid.find_iter(text) {
            out.push_str(&text[rest..found.start()]);
            let next = self.uuids.len() + 1;
            let n = *self.uuids.entry(found.as_str().to_owned()).or_insert(next);
            out.push_str(&format!("<uuid#{n}>"));
            rest = found.end();
        }
        out.push_str(&text[rest..]);
        let started = self.started_ms;
        let out = self.time.replace_all(&out, |found: &regex::Captures| {
            let time = &found[0];
            match domovoi_contracts::time_fmt::millis_from_iso(time) {
                Some(ms) if ms >= started && ms <= domovoi_contracts::time_fmt::now_millis() + 5_000 => "<now>".to_owned(),
                _ => time.to_owned(),
            }
        });
        let out = self.branch.replace_all(&out, "t3$1<generated>");
        if out.contains("<generated>") { self.folder_hash.replace_all(&out, "-<hash>").into_owned() } else { out.into_owned() }
    }

    pub(crate) fn value(&mut self, value: &Value) -> Value {
        match value {
            Value::String(text) => Value::String(self.text(text)),
            Value::Number(n) => match n.as_i64() {
                Some(ms) if self.of_this_run(ms) => json!("<now-ms>"),
                _ => value.clone(),
            },
            Value::Array(items) => Value::Array(items.iter().map(|item| self.value(item)).collect()),
            Value::Object(fields) => {
                let mut sorted: Vec<(&String, &Value)> = fields.iter().collect();
                sorted.sort_by(|a, b| a.0.cmp(b.0));
                Value::Object(sorted.into_iter().map(|(key, value)| (self.text(key), self.value(value))).collect())
            }
            other => other.clone(),
        }
    }
}

struct Harness {
    o: Orchestrator,
    effects: UnboundedReceiver<Effect>,
    store: Store,
    normalizer: Normalizer,
    last_sequence: i64,
    steps: Vec<Value>,
}

impl Harness {
    /// What the last step left behind: events appended and effects queued.
    fn record(&mut self, step: &str, input: Value, outcome: Value) {
        let last = self.last_sequence;
        let rows: Vec<(i64, String, String, String, Option<String>)> = self
            .store
            .db()
            .read(|c| {
                let mut st = c.prepare("SELECT sequence, aggregate_id, type, payload, command_id FROM events WHERE sequence > ?1 ORDER BY sequence")?;
                let rows = st.query_map([last], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?;
                rows.collect()
            })
            .expect("events");
        let mut events = vec![];
        for (sequence, aggregate, event_type, payload, command_id) in rows {
            self.last_sequence = sequence;
            let payload: Value = serde_json::from_str(&payload).expect("event JSON");
            events.push(json!({ "sequence": sequence, "aggregate": aggregate, "type": event_type, "commandId": command_id, "event": payload }));
        }
        let mut effects = vec![];
        while let Ok(effect) = self.effects.try_recv() {
            effects.push(serde_json::to_value(&effect).expect("effect"));
        }
        let step = json!({ "step": step, "input": input, "outcome": outcome, "events": events, "effects": effects });
        let step = self.normalizer.value(&step);
        self.steps.push(step);
    }

    async fn dispatch(&mut self, command: Value) {
        let outcome = match dispatch_json(&self.o, command.clone()).await {
            Ok(result) => json!({ "ok": result }),
            Err(error) => json!({ "error": error }),
        };
        self.record("dispatch", command, outcome);
    }

    async fn launch(&mut self, input: Value) -> Option<String> {
        let result = launch_json(&self.o, input.clone()).await;
        let thread_id = result.as_ref().ok().and_then(|r| r["threadId"].as_str().map(str::to_owned));
        let outcome = match result {
            Ok(result) => json!({ "ok": result }),
            Err(error) => json!({ "error": error }),
        };
        self.record("launch", input, outcome);
        thread_id
    }

    /// Appends an event as a provider adapter would.
    fn append(&mut self, thread_id: &str, event_type: &str, payload: Value) {
        crate::test_records::append(&self.store, thread_id, event_type, payload.clone(), &Origin::default()).expect("setup event");
        self.record("append", json!({ "threadId": thread_id, "type": event_type }), Value::Null);
    }

    /// The thread's stored records as JSON, by collection.
    fn entities(&self, thread_id: &str) -> Map<String, Value> {
        domovoi_model::typed::RecordKind::ALL
            .into_iter()
            .map(|kind| (kind.collection().to_owned(), Value::Array(crate::test_records::stored_of_kind(&self.store, thread_id, kind))))
            .collect()
    }

    /// Marks the thread's run `run_id` as running a provider turn.
    fn run_turn(&mut self, thread_id: &str, run_id: &str) {
        let e = self.entities(thread_id);
        let mut run = e["runs"].as_array().into_iter().flatten().find(|r| r["id"] == run_id).expect("run").clone();
        run["status"] = json!("running");
        self.append(thread_id, "run.updated", run.clone());
        self.append(thread_id, "provider-turn.updated", json!({ "id": format!("pturn-{run_id}"), "providerThreadId": run["providerThreadId"],
            "nodeId": run["rootNodeId"], "runAttemptId": run["activeAttemptId"], "nativeTurnRef": null, "ordinal": run["ordinal"],
            "status": "running", "startedAt": "2030-01-01T00:00:00.000Z", "completedAt": null }));
    }

    /// Finishes the thread's run `run_id` as a provider would: a native
    /// thread and turn, the agent's reply, status completed.
    fn finish_run(&mut self, thread_id: &str, run_id: &str, reply: &str) {
        let e = self.entities(thread_id);
        let mut run = e["runs"].as_array().into_iter().flatten().find(|r| r["id"] == run_id).expect("run").clone();
        let mut pt = e["providerThreads"].as_array().into_iter().flatten().find(|p| p["id"] == run["providerThreadId"]).expect("provider thread").clone();
        let driver = pt["driver"].clone();
        pt["nativeThreadRef"] = json!({ "driver": driver, "nativeId": format!("native-{thread_id}"), "strength": "strong" });
        self.append(thread_id, "provider-thread.updated", pt);
        let turn_id = format!("pturn-{run_id}");
        self.append(thread_id, "provider-turn.updated", json!({ "id": turn_id, "providerThreadId": run["providerThreadId"], "nodeId": run["rootNodeId"],
            "runAttemptId": run["activeAttemptId"], "nativeTurnRef": { "driver": driver, "nativeId": format!("turn-{}", run["ordinal"]), "strength": "strong" },
            "ordinal": run["ordinal"], "status": "completed", "startedAt": "2030-01-01T00:00:00.000Z", "completedAt": "2030-01-01T00:01:00.000Z",
            "tokenUsage": { "usedTokens": 1200, "maxTokens": 200000, "updatedAt": "2030-01-01T00:01:00.000Z" } }));
        let mut attempt = e["attempts"].as_array().into_iter().flatten().find(|a| a["id"] == run["activeAttemptId"]).expect("attempt").clone();
        attempt["providerTurnId"] = json!(turn_id);
        self.append(thread_id, "run-attempt.updated", attempt);
        self.append(thread_id, "message.updated", json!({ "createdBy": "agent", "creationSource": "provider", "id": format!("reply-{run_id}"),
            "threadId": thread_id, "runId": run_id, "nodeId": run["rootNodeId"], "role": "assistant", "text": reply, "attachments": [],
            "streaming": false, "createdAt": "2030-01-01T00:00:30.000Z", "updatedAt": "2030-01-01T00:00:30.000Z" }));
        run["status"] = json!("completed");
        self.append(thread_id, "run.updated", run);
    }

    fn subagent(&mut self, thread_id: &str, id: &str, status: &str, native_id: Option<&str>) {
        self.append(thread_id, "subagent.updated", json!({ "id": id, "threadId": thread_id, "runId": null, "parentNodeId": "node-parent",
            "origin": "provider_native", "createdBy": "agent", "driver": "claudeAgent", "providerInstanceId": "claudeAgent", "providerThreadId": null,
            "childThreadId": null, "nativeTaskRef": native_id.map(|n| json!({ "driver": "claudeAgent", "nativeId": n, "strength": "strong" })),
            "prompt": "look into it", "title": "Explore", "model": null, "status": status, "result": null,
            "startedAt": "2030-01-01T00:00:00.000Z", "completedAt": null, "updatedAt": "2030-01-01T00:00:00.000Z" }));
    }

    fn checkpoint(&mut self, thread_id: &str, id: &str, run_id: &str) {
        let scope = crate::orchestration::commands::scope_id(thread_id);
        self.append(thread_id, "checkpoint.captured", json!({ "id": id, "threadId": thread_id, "scopeId": scope, "runId": run_id,
            "nodeId": crate::orchestration::commands::root_node_id(run_id), "parentCheckpointId": null, "ordinalWithinScope": 1, "appRunOrdinal": 1,
            "ref": format!("refs/t3/checkpoints/{id}"), "status": "ready", "files": [], "capturedAt": "2030-01-01T00:02:00.000Z" }));
    }

    fn plan(&mut self, thread_id: &str, id: &str) {
        self.append(thread_id, "plan.updated", json!({ "kind": "proposed_plan", "id": id, "threadId": thread_id, "runId": null,
            "nodeId": "node-plan", "status": "active", "markdown": "# Plan\n\n1. Do it." }));
    }

    /// Everything the store holds at the end.
    fn finish(mut self) -> Value {
        let query = |sql: &str, columns: usize| -> Vec<Vec<Value>> {
            self.store
                .db()
                .read(|c| {
                    let mut st = c.prepare(sql)?;
                    let rows = st.query_map([], |r| {
                        (0..columns)
                            .map(|i| {
                                let text: Option<String> = r.get(i)?;
                                Ok(text.map_or(Value::Null, |text| serde_json::from_str(&text).unwrap_or(Value::String(text))))
                            })
                            .collect::<rusqlite::Result<Vec<Value>>>()
                    })?;
                    rows.collect()
                })
                .expect("rows")
        };
        let threads = query("SELECT id, record FROM threads ORDER BY rowid", 2);
        let records = query("SELECT thread_id, kind, entity_id, record FROM thread_entities ORDER BY thread_id, kind, first_sequence, entity_id", 4);
        let receipts = query("SELECT command_id, result FROM command_receipts ORDER BY rowid", 2);
        let outbox = query("SELECT key, effect FROM effect_outbox ORDER BY rowid", 2);
        let events = query("SELECT type, payload FROM events ORDER BY sequence", 2);
        let end = json!({ "threads": threads, "records": records, "receipts": receipts, "outbox": outbox, "events": events });
        let end = self.normalizer.value(&end);
        json!({ "steps": self.steps, "end": end })
    }
}

fn project(id: &str, root: &Path) -> domovoi_contracts::shell::ProjectShell {
    domovoi_contracts::shell::ProjectShell {
        id: id.into(), title: id.into(), has_custom_title: false, workspace_root: root.to_string_lossy().into(),
        repository_identity: None, default_model_selection: None, default_thread_env_mode: None, auto_pull: false,
        favicon_path: None, project_icon: None, scripts: vec![], pinned_at: None, archived_at: None,
        created_at: "2030-01-01T00:00:00.000Z".into(), updated_at: "2030-01-01T00:00:00.000Z".into(),
    }
}

fn git(cwd: &Path, args: &[&str]) {
    let done = crate::process::output_blocking(crate::process::blocking_command("git").args(args).current_dir(cwd), crate::process::QUICK).expect("git");
    assert!(done.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&done.stderr));
}

const CODEX: &str = r#"{ "instanceId": "codex", "model": "gpt-6-luna" }"#;
const CLAUDE: &str = r#"{ "instanceId": "claudeAgent", "model": "claude-haiku-5-5" }"#;

fn model(text: &str) -> Value {
    serde_json::from_str(text).expect("model selection")
}

fn launch(command_id: &str, thread_id: Option<&str>, project: &str, strategy: Value, message: Option<Value>) -> Value {
    let mut input = json!({ "commandId": command_id, "creationSource": "web", "projectId": project, "title": format!("Thread of {command_id}"),
        "generateTitle": false, "modelSelection": model(CODEX), "runtimeMode": "approval-required", "interactionMode": "default",
        "workspaceStrategy": strategy });
    if let Some(id) = thread_id {
        input["threadId"] = json!(id);
    }
    if let Some(message) = message {
        input["initialMessage"] = message;
    }
    input
}

fn send(command_id: &str, thread_id: &str, message_id: &str, text: &str, mode: Value) -> Value {
    json!({ "type": "message.dispatch", "commandId": command_id, "threadId": thread_id, "messageId": message_id, "text": text,
        "attachments": [], "createdBy": "user", "creationSource": "web", "dispatchMode": mode })
}

fn with(mut command: Value, fields: Value) -> Value {
    for (key, value) in fields.as_object().into_iter().flatten() {
        command[key.as_str()] = value.clone();
    }
    command
}

fn queue() -> Value {
    json!({ "type": "queue_after_active" })
}

fn run_id(thread_id: &str, ordinal: u64) -> String {
    crate::orchestration::commands::run_id(thread_id, ordinal)
}

#[tokio::test]
#[ignore = "writes the differential file; run with --ignored and DOMOVOI_DIFFERENTIAL_OUT"]
async fn command_differential() {
    // A fixed place, so the worktree folders named after it are the same in every run.
    let dir: PathBuf = std::env::temp_dir().join("domovoi-command-differential");
    let _ = std::fs::remove_dir_all(&dir);
    for sub in ["root", "other", "third", "repo", "state"] {
        std::fs::create_dir_all(dir.join(sub)).expect("dir");
    }
    let repo = dir.join("repo");
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["-c", "user.name=domovoi", "-c", "user.email=domovoi@example.invalid", "commit", "-q", "--allow-empty", "-m", "first"]);
    let _ = crate::vcs_ops::WORKTREES_DIR.set(dir.join("worktrees"));
    let other = dir.join("other").to_string_lossy().into_owned();
    let third = dir.join("third").to_string_lossy().into_owned();

    let store = Store::new(Db::open(&dir.join("state").join("state.sqlite")).expect("db"));
    store.create_project(project("p", &dir.join("root")), None).expect("project");
    store.create_project(project("p2", &dir.join("other")), None).expect("project");
    store.create_project(project("g", &repo), None).expect("project");
    let (o, effects) = Orchestrator::new(store.clone());
    let mut h = Harness { o, effects, store, normalizer: Normalizer::new(), last_sequence: 0, steps: vec![] };
    h.record("setup", Value::Null, Value::Null);

    // ---- launches
    let first = json!({ "messageId": "t1-m1", "text": "hello", "attachments": [] });
    h.launch(launch("l-t1", Some("t1"), "p", json!({ "type": "root" }), Some(first.clone()))).await;
    h.launch(launch("l-t1", Some("t1"), "p", json!({ "type": "root" }), Some(first.clone()))).await;
    h.launch(launch("l-t1-again", Some("t1"), "p", json!({ "type": "root" }), Some(first.clone()))).await;
    h.launch(launch("l-nowhere", Some("t-nowhere"), "no-such-project", json!({ "type": "root" }), None)).await;
    let mut no_command_id = launch("x", Some("t-none"), "p", json!({ "type": "root" }), None);
    no_command_id.as_object_mut().expect("object").remove("commandId");
    h.launch(no_command_id).await;
    let t2 = h.launch(launch("l-t2", None, "p", json!({ "type": "root", "branch": "main" }), None)).await.expect("t2");
    let image = json!({ "type": "image", "id": "att-1", "name": "shot.png", "mimeType": "image/png", "sizeBytes": 12 });
    h.launch(with(
        launch("l-t3", Some("t3"), "p", json!({ "type": "root" }), Some(json!({ "text": "Build the search bar", "attachments": [image.clone()] }))),
        json!({ "generateTitle": true, "runtimeMode": "full-access", "interactionMode": "plan",
            "modelSelection": { "instanceId": "codex", "model": "gpt-6-luna", "options": [{ "id": "effort", "value": "high" }, { "id": "fast", "value": true }] } }),
    ))
    .await;
    h.launch(with(
        launch("l-t4", Some("t4"), "p", json!({ "type": "root" }), Some(json!({ "text": "A delegated task", "attachments": [] }))),
        json!({ "createdBy": "agent", "creationSource": "mcp", "delegatedFrom": { "environmentId": "env-1", "threadId": "t1", "title": "First" } }),
    ))
    .await;
    h.launch(launch("l-t5", Some("t5"), "p", json!({ "type": "existing_worktree", "worktreePath": other, "branch": "feature/y" }),
        Some(json!({ "messageId": "t5-m1", "text": "in a worktree", "attachments": [] })))).await;
    h.launch(launch("l-t6", Some("t6"), "p", json!({ "type": "existing_worktree", "worktreePath": third }), None)).await;
    h.launch(launch("l-t7", Some("t7"), "p", json!({ "type": "existing_worktree", "worktreePath": dir.join("missing").to_string_lossy() }), None)).await;
    h.launch(launch("l-t8", Some("t8"), "g", json!({ "type": "worktree", "baseRef": "HEAD", "branch": "feature/x" }),
        Some(json!({ "messageId": "t8-m1", "text": "on a branch", "attachments": [] })))).await;
    h.launch(launch("l-t9", Some("t9"), "g", json!({ "type": "worktree", "baseRef": "HEAD" }),
        Some(json!({ "messageId": "t9-m1", "text": "on a generated branch", "attachments": [] })))).await;
    h.launch(launch("l-t10", Some("t10"), "g", json!({ "type": "worktree", "baseRef": "HEAD", "startFromOrigin": false }), None)).await;
    h.launch(launch("l-t11", Some("t11"), "g", json!({ "type": "worktree", "baseRef": "no-such-ref", "branch": "feature/z" }), None)).await;

    // ---- the thread's own fields
    let cmd = |ty: &str, id: &str, fields: Value| with(json!({ "type": ty, "commandId": id, "threadId": "t1" }), fields);
    h.dispatch(cmd("thread.visit", "v1", json!({ "visitedAt": "2099-01-01T00:00:00.000Z", "opened": true, "searchQuery": " Hello " }))).await;
    h.dispatch(cmd("thread.visit", "v2", json!({ "visitedAt": "2001-01-01T00:00:00.000Z", "opened": false }))).await;
    h.dispatch(cmd("thread.visit", "v3", json!({ "visitedAt": "2099-01-02T00:00:00.000Z", "opened": true, "searchQuery": "other" }))).await;
    h.dispatch(cmd("thread.visit", "v4", json!({ "visitedAt": "2099-01-02T00:00:00.000Z" }))).await;
    h.dispatch(cmd("thread.runtime-mode.set", "rm1", json!({ "runtimeMode": "auto-accept-edits" }))).await;
    h.dispatch(cmd("thread.interaction-mode.set", "im1", json!({ "interactionMode": "plan" }))).await;
    h.dispatch(cmd("thread.metadata.update", "md1", json!({ "title": "Renamed", "branch": "b1", "worktreePath": null }))).await;
    h.dispatch(cmd("thread.metadata.update", "md2", json!({ "branch": null }))).await;
    h.dispatch(cmd("thread.metadata.update", "md3", json!({ "regenerateTitle": true }))).await;
    h.dispatch(cmd("thread.title.regeneration.complete", "tr1", json!({ "requestId": "someone-else", "title": "Stale" }))).await;
    h.dispatch(cmd("thread.title.regeneration.complete", "tr2", json!({ "requestId": "md3", "title": "Generated", "outcome": { "status": "renamed" } }))).await;
    h.dispatch(cmd("thread.metadata.update", "md4", json!({ "regenerateTitle": true, "title": "Meanwhile" }))).await;
    h.dispatch(cmd("thread.title.regeneration.complete", "tr3", json!({ "requestId": "md4", "outcome": { "status": "failed", "message": "no model" } }))).await;
    h.dispatch(cmd("thread.metadata.update", "md5", json!({ "regenerateTitle": true }))).await;
    h.dispatch(cmd("thread.title.regeneration.complete", "tr4", json!({ "requestId": "md5", "title": "Third" }))).await;
    h.dispatch(cmd("thread.model-selection.set", "ms1", json!({ "modelSelection": { "instanceId": "codex", "model": "gpt-6-sol" } }))).await;
    h.dispatch(cmd("provider.switch", "ps1", json!({ "modelSelection": model(CLAUDE) }))).await;
    h.dispatch(cmd("thread.model-selection.set", "ms2", json!({ "modelSelection": model(CODEX) }))).await;
    h.dispatch(cmd("provider.switch", "ps2", json!({ "modelSelection": model(CODEX) }))).await;
    h.dispatch(cmd("thread.archive", "ar1", json!({}))).await;
    h.dispatch(cmd("thread.unarchive", "ar2", json!({}))).await;
    h.dispatch(cmd("thread.pin", "pin1", json!({ "orderKey": "a5" }))).await;
    h.dispatch(cmd("thread.pin.reorder", "pin2", json!({ "orderKey": "a0" }))).await;
    h.dispatch(cmd("thread.unpin", "pin3", json!({}))).await;
    h.dispatch(cmd("thread.pin", "pin4", json!({}))).await;
    h.dispatch(cmd("thread.active.reorder", "act1", json!({ "orderKey": "b0" }))).await;
    h.dispatch(cmd("thread.tag.set", "tag1", json!({ "tag": "bug", "tagged": true }))).await;
    h.dispatch(cmd("thread.tag.set", "tag2", json!({ "tag": "  ui  ", "tagged": true }))).await;
    h.dispatch(cmd("thread.tag.set", "tag3", json!({ "tag": "bug", "tagged": false }))).await;
    h.dispatch(cmd("thread.tag.set", "tag4", json!({ "tag": "ui", "tagged": true }))).await;
    h.dispatch(cmd("thread.tag.set", "tag5", json!({ "tag": "a\nb", "tagged": true }))).await;
    h.dispatch(cmd("thread.tag.set", "tag6", json!({ "tag": "x".repeat(65), "tagged": true }))).await;
    h.dispatch(cmd("thread.tag.set", "tag7", json!({ "tag": "   ", "tagged": true }))).await;
    h.dispatch(cmd("thread.tag.set", "tag8", json!({ "tag": "absent", "tagged": false }))).await;
    h.dispatch(cmd("thread.auto-settle.set", "as1", json!({ "enabled": false }))).await;
    h.dispatch(cmd("thread.auto-settle.set", "as2", json!({ "enabled": false }))).await;
    h.dispatch(cmd("thread.auto-settle.set", "as3", json!({ "enabled": true }))).await;
    h.dispatch(cmd("thread.auto-settle.set", "as4", json!({ "enabled": true }))).await;
    h.dispatch(cmd("thread.snooze", "sn0", json!({ "snoozedUntil": "later" }))).await;
    h.dispatch(cmd("thread.snooze", "sn1", json!({ "snoozedUntil": "2099-06-01T00:00:00.000Z" }))).await;
    h.dispatch(cmd("thread.unsnooze", "sn2", json!({ "reason": "user" }))).await;
    h.dispatch(cmd("thread.unsnooze", "sn3", json!({ "reason": "user" }))).await;
    h.dispatch(cmd("thread.mark-unread", "mu1", json!({}))).await;
    h.dispatch(cmd("thread.settle", "st1", json!({ "settledAt": "2031-01-01T00:00:00.000Z", "expectedLatestRunId": run_id("t1", 1) }))).await;
    h.dispatch(cmd("thread.unsettle", "st2", json!({ "reason": "user" }))).await;
    h.dispatch(cmd("thread.auto-settle", "st3", json!({ "snapshotAt": "2031-01-01T00:00:00.000Z" }))).await;

    // ---- messages to a busy thread, and its queue
    h.dispatch(send("s2", "t1", "t1-m2", "two", queue())).await;
    h.dispatch(with(send("s3", "t1", "t1-m3", "three", queue()), json!({ "modelSelection": { "instanceId": "codex", "model": "gpt-6-sol" }, "titleSeed": "Ignored seed" }))).await;
    h.dispatch(with(send("s4", "t1", "t1-m4", "four", queue()), json!({ "modelSelection": model(CLAUDE), "attachments": [image.clone()],
        "context": { "version": 1, "records": [{ "kind": "file", "path": "src/a.rs" }] } }))).await;
    h.dispatch(send("s5", "t1", "t1-m5", "five", json!({ "type": "steer_active", "targetRunId": run_id("t1", 1) }))).await;
    h.dispatch(send("s2", "t1", "t1-m2", "two", queue())).await;
    h.dispatch(send("s6", "t1", "t1-m6", "six", json!({ "type": "start_immediately" }))).await;
    h.dispatch(cmd("queued-run.reorder", "qr1", json!({ "runId": run_id("t1", 4), "beforeRunId": run_id("t1", 2) }))).await;
    h.dispatch(cmd("queued-run.reorder", "qr2", json!({ "runId": run_id("t1", 4), "beforeRunId": null }))).await;
    h.dispatch(cmd("queued-run.reorder", "qr3", json!({ "runId": run_id("t1", 4), "beforeRunId": run_id("t1", 1) }))).await;
    h.dispatch(cmd("queued-run.reorder", "qr4", json!({ "runId": run_id("t1", 99), "beforeRunId": null }))).await;
    h.dispatch(cmd("queued-run.edit", "qe1", json!({ "runId": run_id("t1", 3), "text": "THREE", "attachments": [image.clone()],
        "context": { "version": 1, "records": [] } }))).await;
    h.dispatch(cmd("queued-run.edit", "qe2", json!({ "runId": run_id("t1", 4), "text": "FOUR" }))).await;
    h.dispatch(cmd("queued-run.edit", "qe3", json!({ "runId": run_id("t1", 4), "text": "four again", "attachments": [] }))).await;
    h.dispatch(cmd("queued-run.edit", "qe4", json!({ "runId": run_id("t1", 1), "text": "not queued" }))).await;
    h.dispatch(cmd("queued-run.cancel", "qc1", json!({ "runId": run_id("t1", 2) }))).await;
    h.dispatch(cmd("queued-run.cancel", "qc2", json!({ "runId": run_id("t1", 2) }))).await;
    h.dispatch(cmd("queued-run.start", "qs1", json!({ "runId": run_id("t1", 5) }))).await;
    h.dispatch(cmd("run.interrupt", "ri1", json!({ "runId": run_id("t1", 1), "holdQueue": true, "reason": "stop" }))).await;
    h.dispatch(cmd("queue.resume", "qz1", json!({}))).await;
    h.dispatch(cmd("thread.stop", "ts1", json!({ "reason": "user" }))).await;
    h.dispatch(cmd("run.interrupt", "ri2", json!({ "runId": run_id("t1", 1), "holdQueue": true }))).await;
    h.dispatch(with(send("s7", "t1", "t1-m7", "from an agent", queue()), json!({ "createdBy": "agent", "creationSource": "mcp", "senderThreadId": "t4" }))).await;
    h.dispatch(cmd("queued-run.start", "qs2", json!({ "runId": run_id("t1", 3) }))).await;
    h.dispatch(send("s8", "t1", "t1-m8", "from the user", queue())).await;

    // ---- steering a running turn
    h.run_turn("t1", &run_id("t1", 1));
    h.dispatch(send("s9", "t1", "t1-m9", "also do this", json!({ "type": "steer_active", "targetRunId": run_id("t1", 1) }))).await;
    h.dispatch(with(send("s10", "t1", "t1-m10", "and this", json!({ "type": "steer_active", "targetRunId": run_id("t1", 1) })),
        json!({ "attachments": [image.clone()], "createdBy": "agent", "creationSource": "mcp" }))).await;
    h.o.requeue_steer("t1", "t1-m9").await;
    h.record("requeue_steer", json!("t1-m9"), Value::Null);
    h.o.requeue_steer("t1", "no-such-message").await;
    h.record("requeue_steer", json!("no-such-message"), Value::Null);
    let third_run = run_id("t1", 3);
    h.dispatch(cmd("queued-message.promote-to-steer", "pr1", json!({ "queuedRunId": third_run, "targetRunId": run_id("t1", 1) }))).await;
    h.dispatch(cmd("queued-message.promote-to-steer", "pr2", json!({ "queuedRunId": run_id("t1", 4), "targetRunId": run_id("t1", 5) }))).await;
    h.dispatch(cmd("queued-message.promote-to-steer", "pr3", json!({ "queuedRunId": run_id("t1", 99), "targetRunId": run_id("t1", 1) }))).await;
    h.dispatch(send("s11", "t1", "t1-m11", "instead", json!({ "type": "restart_active", "targetRunId": run_id("t1", 1) }))).await;
    h.dispatch(with(send("s12", "t1", "t1-m12", "continue", json!({ "type": "restart_active", "targetRunId": run_id("t1", 4) })),
        json!({ "restartContinuationOfRunId": run_id("t1", 1) }))).await;

    // ---- subagents, runtime requests, checkpoints
    h.dispatch(cmd("subagent.stop", "sa0", json!({ "runId": run_id("t1", 1), "subagentId": "sub-none" }))).await;
    h.subagent("t1", "sub-1", "running", Some("task-1"));
    h.subagent("t1", "sub-2", "completed", Some("task-2"));
    h.subagent("t1", "sub-3", "waiting", None);
    h.dispatch(cmd("subagent.stop", "sa1", json!({ "runId": run_id("t1", 1), "subagentId": "sub-1" }))).await;
    h.dispatch(cmd("subagent.stop", "sa2", json!({ "runId": run_id("t1", 1), "subagentId": "sub-2" }))).await;
    h.dispatch(cmd("subagent.stop", "sa3", json!({ "runId": run_id("t1", 1), "subagentId": "sub-3" }))).await;
    h.dispatch(cmd("runtime-request.respond", "rr0", json!({ "requestId": "req-none", "decision": "accept" }))).await;
    h.append("t1", "runtime-request.updated", crate::test_records::runtime_request("req-1", "pending"));
    h.append("t1", "runtime-request.updated", crate::test_records::runtime_request("req-2", "resolved"));
    h.dispatch(cmd("runtime-request.respond", "rr1", json!({ "requestId": "req-1", "decision": "acceptForSession", "feedback": "fine" }))).await;
    h.dispatch(cmd("runtime-request.respond", "rr2", json!({ "requestId": "req-1", "answers": { "q1": ["a", "b"], "q2": "free text" } }))).await;
    h.dispatch(cmd("runtime-request.respond", "rr3", json!({ "requestId": "req-1", "decision": "decline", "answers": { "q1": { "answers": ["a"] } } }))).await;
    h.dispatch(cmd("runtime-request.respond", "rr4", json!({ "requestId": "req-1" }))).await;
    h.dispatch(cmd("thread.user-input.dismiss", "rr5", json!({ "requestId": "req-1" }))).await;
    h.dispatch(cmd("runtime-request.respond", "rr6", json!({ "requestId": "req-2", "decision": "accept" }))).await;
    h.dispatch(cmd("checkpoint.rollback", "cr0", json!({ "scopeId": "scope-x", "checkpointId": "cp-none" }))).await;
    h.checkpoint("t1", "cp-1", &run_id("t1", 1));
    h.dispatch(cmd("checkpoint.rollback", "cr1", json!({ "scopeId": crate::orchestration::commands::scope_id("t1"), "checkpointId": "cp-1" }))).await;

    // ---- the run settles and the queue moves on
    h.finish_run("t1", &run_id("t1", 1), "Added the search bar.");
    let started = h.o.start_next_queued("t1").await.expect("start");
    h.record("start_next_queued", json!("t1"), json!(started));
    let again = h.o.start_next_queued("t1").await.expect("start");
    h.record("start_next_queued", json!("t1"), json!(again));

    // ---- a queued run started by hand on an idle thread, and one held
    h.dispatch(send("u1", "t6", "t6-m1", "first", queue())).await;
    h.dispatch(send("u2", "t6", "t6-m2", "second", queue())).await;
    h.dispatch(send("u3", "t6", "t6-m3", "third", queue())).await;
    h.dispatch(with(json!({ "type": "run.interrupt", "commandId": "u4", "threadId": "t6" }), json!({ "runId": run_id("t6", 1), "holdQueue": true }))).await;
    h.finish_run("t6", &run_id("t6", 1), "Done.");
    let held = h.o.start_next_queued("t6").await.expect("start");
    h.record("start_next_queued", json!("t6"), json!(held));
    h.dispatch(json!({ "type": "queued-run.start", "commandId": "u5", "threadId": "t6", "runId": run_id("t6", 3) })).await;
    h.finish_run("t6", &run_id("t6", 3), "Done again.");
    h.dispatch(json!({ "type": "queue.resume", "commandId": "u6", "threadId": "t6" })).await;
    h.dispatch(json!({ "type": "queue.resume", "commandId": "u7", "threadId": t2 })).await;

    // ---- rollback
    h.finish_run("t3", &run_id("t3", 1), "Search bar built.");
    h.checkpoint("t3", "cp-3", &run_id("t3", 1));
    let scope3 = crate::orchestration::commands::scope_id("t3");
    h.dispatch(json!({ "type": "checkpoint.rollback", "commandId": "cr2", "threadId": "t3", "scopeId": scope3, "checkpointId": "cp-3", "restoreFiles": true })).await;
    h.dispatch(json!({ "type": "checkpoint.rollback", "commandId": "cr3", "threadId": "t3", "scopeId": scope3, "checkpointId": "cp-3", "restoreFiles": false })).await;
    h.dispatch(json!({ "type": "checkpoint.rollback", "commandId": "cr4", "threadId": "t3", "scopeId": scope3, "checkpointId": "cp-3" })).await;
    h.finish_run("t5", &run_id("t5", 1), "Worked in the worktree.");
    h.checkpoint("t5", "cp-5", &run_id("t5", 1));
    h.dispatch(json!({ "type": "checkpoint.rollback", "commandId": "cr5", "threadId": "t5", "scopeId": crate::orchestration::commands::scope_id("t5"),
        "checkpointId": "cp-5", "restoreFiles": true })).await;

    // ---- implementing a plan
    let plan_ref = |thread: &str, plan: &str| json!({ "sourcePlanRef": { "threadId": thread, "planId": plan } });
    h.dispatch(with(send("pl0", "t3", "t3-m2", "Implement the plan.", queue()), plan_ref("t3", "plan-none"))).await;
    h.plan("t3", "plan-1");
    h.plan("t4", "plan-2");
    h.dispatch(with(send("pl1", "t3", "t3-m2", "Implement the plan.", json!({ "type": "steer_active", "targetRunId": run_id("t3", 1) })), plan_ref("t3", "plan-1"))).await;
    h.dispatch(with(send("pl2", "t3", "t3-m3", "Implement the plan.", queue()), plan_ref("t3", "plan-1"))).await;
    h.dispatch(with(send("pl3", "t3", "t3-m4", "Implement their plan.", queue()), plan_ref("t4", "plan-2"))).await;

    // ---- forks, handoffs and merging back
    let fork = |ty: &str, id: &str, source: &str, target: &str, fields: Value| {
        with(json!({ "type": ty, "commandId": id, "sourceThreadId": source, "targetThreadId": target,
            "sourcePoint": { "type": "latest_stable" }, "createdBy": "user", "creationSource": "web" }), fields)
    };
    h.dispatch(fork("thread.fork", "f1", "t1", "f1", json!({}))).await;
    h.dispatch(fork("thread.fork", "f1-again", "t1", "f1", json!({}))).await;
    h.dispatch(fork("thread.fork", "f2", "t1", "f2", json!({ "sourcePoint": { "type": "run", "runId": run_id("t1", 1) }, "title": "Side",
        "sideChat": true, "createdAt": "2031-05-05T00:00:00.000Z", "createdBy": "agent", "creationSource": "mcp" }))).await;
    let active = h.entities("t1")["runs"].as_array().into_iter().flatten().find(|r| r["status"] == "starting").and_then(|r| r["id"].as_str()).expect("an active run").to_owned();
    h.dispatch(fork("thread.fork", "f3", "t1", "f3", json!({ "sourcePoint": { "type": "run", "runId": active } }))).await;
    h.dispatch(fork("thread.fork", "f4", &t2, "f4", json!({}))).await;
    h.dispatch(fork("thread.fork", "f5", "t-unknown", "f5", json!({}))).await;
    h.dispatch(fork("thread.fork", "f6", "t1", "f6", json!({ "sourcePoint": { "type": "checkpoint", "checkpointId": "cp-none" } }))).await;
    h.dispatch(fork("thread.fork", "f7", "t1", "f7", json!({ "sourcePoint": { "type": "run", "runId": "run-none" } }))).await;
    h.dispatch(fork("thread.fork", "f8", "t3", "f8", json!({}))).await;
    // The fork continues on another provider (as text), on the same one (natively).
    h.dispatch(with(send("ff1", "f1", "f1-m1", "carry on", queue()), json!({ "modelSelection": model(CLAUDE) }))).await;
    h.dispatch(with(send("ff2", "f2", "f2-m1", "carry on here", queue()), json!({ "modelSelection": model(CODEX) }))).await;
    h.dispatch(send("ff3", "f2", "f2-m2", "and more", queue())).await;
    h.dispatch(send("ff8", "f8", "f8-m1", "the rolled back thread's fork", queue())).await;
    h.finish_run("f1", &run_id("f1", 1), "Tried the other approach; it works.");
    h.dispatch(fork("thread.merge_back", "mb0", "t1", "f1", json!({}))).await;
    h.dispatch(fork("thread.merge_back", "mb1", "f1", "t1", json!({ "createdAt": "2031-06-06T00:00:00.000Z" }))).await;
    h.dispatch(fork("thread.merge_back", "mb2", "f1", "t-unknown", json!({}))).await;
    h.dispatch(send("mb3", "t1", "t1-m13", "use what the fork found", queue())).await;
    // A thread switched to another provider hands its conversation over.
    h.dispatch(json!({ "type": "provider.switch", "commandId": "sw1", "threadId": "t5", "modelSelection": model(CLAUDE) })).await;
    h.dispatch(send("sw2", "t5", "t5-m2", "go on", queue())).await;
    h.finish_run("t5", &run_id("t5", 2), "Went on.");
    h.dispatch(send("sw3", "t5", "t5-m3", "and on", queue())).await;

    // ---- moving a thread to the project that owns its folder
    h.dispatch(json!({ "type": "thread.project.adopt-folder", "commandId": "ad0", "threadId": "t5", "projectId": "no-such-project" })).await;
    h.dispatch(json!({ "type": "thread.project.adopt-folder", "commandId": "ad1", "threadId": "t4", "projectId": "p2" })).await;
    h.dispatch(json!({ "type": "thread.project.adopt-folder", "commandId": "ad2", "threadId": "t5", "projectId": "p2" })).await;
    h.dispatch(json!({ "type": "thread.project.adopt-folder", "commandId": "ad3", "threadId": "t4", "projectId": "p" })).await;

    // ---- snoozes that run out
    h.dispatch(json!({ "type": "thread.snooze", "commandId": "wk1", "threadId": t2, "snoozedUntil": "2001-01-01T00:00:00.000Z" })).await;
    h.dispatch(json!({ "type": "thread.snooze", "commandId": "wk2", "threadId": "t4", "snoozedUntil": "2099-01-01T00:00:00.000Z" })).await;
    let woke = h.o.wake_snoozed();
    h.record("wake_snoozed", Value::Null, json!(woke));
    let woke = h.o.wake_snoozed();
    h.record("wake_snoozed", Value::Null, json!(woke));

    // ---- deleting a thread with work in flight, and what a deleted thread refuses
    h.dispatch(cmd("thread.delete", "del1", json!({}))).await;
    for (i, ty) in ["thread.visit", "thread.archive", "thread.unarchive", "thread.pin", "thread.unpin", "thread.mark-unread", "thread.settle",
        "thread.unsettle", "thread.unsnooze", "thread.delete", "queue.resume", "thread.stop"].into_iter().enumerate()
    {
        h.dispatch(cmd(ty, &format!("dead{i}"), json!({ "visitedAt": "2099-01-03T00:00:00.000Z", "reason": "user" }))).await;
    }
    h.dispatch(send("dead-send", "t1", "t1-m14", "anyone there", queue())).await;
    h.dispatch(cmd("queued-run.cancel", "dead-cancel", json!({ "runId": run_id("t1", 4) }))).await;
    h.dispatch(cmd("thread.runtime-mode.set", "dead-mode", json!({ "runtimeMode": "full-access" }))).await;
    h.dispatch(cmd("checkpoint.rollback", "dead-rollback", json!({ "scopeId": crate::orchestration::commands::scope_id("t1"), "checkpointId": "cp-1" }))).await;
    h.dispatch(fork("thread.fork", "dead-fork", "t1", "f9", json!({}))).await;
    let started = h.o.start_next_queued("t1").await.expect("start");
    h.record("start_next_queued", json!("t1"), json!(started));
    h.dispatch(json!({ "type": "thread.delete", "commandId": "del2", "threadId": t2, "onlyIfUnused": true })).await;

    // ---- threads that do not exist
    let nowhere = |ty: &str, id: &str, fields: Value| with(json!({ "type": ty, "commandId": id, "threadId": "t-unknown" }), fields);
    h.dispatch(nowhere("thread.visit", "un1", json!({ "visitedAt": "2099-01-03T00:00:00.000Z" }))).await;
    h.dispatch(send("un2", "t-unknown", "un-m1", "hello?", queue())).await;
    h.dispatch(nowhere("run.interrupt", "un3", json!({ "runId": "r" }))).await;
    h.dispatch(nowhere("thread.stop", "un4", json!({}))).await;
    h.dispatch(nowhere("runtime-request.respond", "un5", json!({ "requestId": "req-1", "decision": "accept" }))).await;
    h.dispatch(nowhere("thread.user-input.dismiss", "un6", json!({ "requestId": "req-1" }))).await;
    h.dispatch(nowhere("checkpoint.rollback", "un7", json!({ "scopeId": "s", "checkpointId": "c" }))).await;
    h.dispatch(nowhere("queued-run.start", "un8", json!({ "runId": "r" }))).await;
    h.dispatch(nowhere("queue.resume", "un9", json!({}))).await;
    h.dispatch(nowhere("subagent.stop", "un10", json!({ "runId": "r", "subagentId": "s" }))).await;
    h.dispatch(nowhere("thread.snooze", "un11", json!({ "snoozedUntil": "2099-06-01T00:00:00.000Z" }))).await;
    h.dispatch(nowhere("thread.tag.set", "un12", json!({ "tag": "bug", "tagged": true }))).await;
    h.dispatch(nowhere("thread.auto-settle.set", "un13", json!({ "enabled": true }))).await;
    h.dispatch(nowhere("thread.title.regeneration.complete", "un14", json!({ "requestId": "x" }))).await;
    h.dispatch(nowhere("thread.project.adopt-folder", "un15", json!({ "projectId": "p" }))).await;
    h.dispatch(nowhere("thread.model-selection.set", "un16", json!({ "modelSelection": model(CODEX) }))).await;
    h.dispatch(nowhere("queued-message.promote-to-steer", "un17", json!({ "queuedRunId": "a", "targetRunId": "b" }))).await;
    h.dispatch(nowhere("queued-run.edit", "un18", json!({ "runId": "r", "text": "x" }))).await;
    h.dispatch(nowhere("queued-run.reorder", "un19", json!({ "runId": "r", "beforeRunId": null }))).await;

    // ---- what Domovoi does not do, and what is not a command
    h.dispatch(json!({ "type": "prepared-run.release", "commandId": "ns1", "threadId": "t3", "runId": "r" })).await;
    h.dispatch(json!({ "type": "thread.guest-share.close", "commandId": "ns2", "threadId": "t3" })).await;
    h.dispatch(json!({ "type": "notification.delivery.accept", "commandId": "ns3", "threadId": "t3", "messageId": "m" })).await;
    h.dispatch(json!({ "type": "some.future-command", "commandId": "ns4", "threadId": "t3", "anything": { "goes": [1, 2, 3] } })).await;
    h.dispatch(json!({ "type": "some.future-command", "commandId": "ns5" })).await;
    h.dispatch(json!({ "type": "thread.visit", "threadId": "t3", "visitedAt": "2099-01-03T00:00:00.000Z" })).await;
    h.dispatch(json!({ "commandId": "ns6", "threadId": "t3" })).await;
    h.dispatch(json!({ "type": "thread.archive", "commandId": "ar1", "threadId": "t3" })).await;

    let out = h.finish();
    let commands = out["steps"].as_array().map_or(0, |s| s.iter().filter(|s| s["step"] == "dispatch" || s["step"] == "launch").count());
    let events = out["end"]["events"].as_array().map_or(0, Vec::len);
    let path = std::env::var("DOMOVOI_DIFFERENTIAL_OUT").unwrap_or_else(|_| dir.join("differential.json").to_string_lossy().into_owned());
    std::fs::write(&path, serde_json::to_string_pretty(&out).expect("json")).expect("write");
    println!("command differential: {commands} commands, {events} events, written to {path}");
}
