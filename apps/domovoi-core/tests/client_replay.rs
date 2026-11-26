//! Drives the Crux core the way a shell would (connect, socket opened, select a
//! thread) and feeds it the frames the TypeScript server actually sent in a
//! recorded session, re-addressed to the core's own request ids. The resulting
//! view model must agree with what t3code itself reported at the end.

use crux_core::App;
use crux_http::protocol::{HttpRequest, HttpResponse, HttpResult};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use domovoi_core::local::{LocalOperation, LocalOutput};
use domovoi_core::socket::{SocketEvent, SocketOperation};
use domovoi_core::store::{StoreOperation, StoreOutput};
use domovoi_core::tunnel::{TunnelOperation, TunnelOutput};
use domovoi_core::app::{ProviderInstanceEdit, ScheduledTaskEdit};
use domovoi_core::app::QuestionAnswer;
use domovoi_core::sidebar::Grouping;
use domovoi_core::{Effect, Event, Model, DomovoiApp};

mod common;

const THREAD: &str = "t1";

/// Records as a server sends them, for the threads these tests make up: a
/// test writes the fields it is about, and the rest of what the schema
/// requires is filled in here.
mod records {
    use serde_json::{Map, Value, json};
    use domovoi_model::typed::{DomainEvent, ProjectShell, PullRequestListEntry, ServerProvider, Thread, ThreadShell, ThreadStreamItem};

    /// Gives `record` every field of `defaults` it lacks, objects within objects too.
    fn fill(record: &mut Value, defaults: Value) {
        let (Value::Object(record), Value::Object(defaults)) = (record, defaults) else { return };
        for (key, default) in defaults {
            match record.get_mut(&key) {
                Some(held) => fill(held, default),
                None => {
                    record.insert(key, default);
                }
            }
        }
    }

    /// Gives every record of `list`, if the list is there, the fields of `defaults` it lacks.
    fn each(list: Option<&mut Value>, defaults: impl Fn(usize) -> Value) {
        for (n, record) in list.and_then(Value::as_array_mut).into_iter().flatten().enumerate() {
            fill(record, defaults(n));
        }
    }

    fn model_selection() -> Value {
        json!({ "instanceId": "codex", "model": "model-1" })
    }

    fn native_ref() -> Value {
        json!({ "driver": "claudeAgent", "nativeId": null, "strength": "strong" })
    }

    /// A thread as the thread list holds it (its shell), from the fields a
    /// test is about. A `null` where the schema wants a record counts as not given.
    pub fn shell_json(mut record: Value) -> Value {
        if let Some(fields) = record.as_object_mut() {
            fields.retain(|key, value| !(key == "lineage" && value.is_null()));
        }
        complete("thread", &mut record);
        fill(&mut record, json!({
            "latestRunId": null, "activeRunId": null, "status": "idle", "pendingRuntimeRequest": null, "latestVisibleMessage": null,
            "latestUserMessageAt": null, "hasActionableProposedPlan": false, "itemCount": 0, "visibleItemCount": 0,
            "settledOverride": null, "settledAt": null,
        }));
        if let Some(pending) = record.get_mut("pendingRuntimeRequest").filter(|pending| pending.is_object()) {
            fill(pending, json!({ "createdAt": "1" }));
        }
        record
    }

    pub fn shell(record: Value) -> ThreadShell {
        serde_json::from_value(shell_json(record)).expect("the shell fits the schema")
    }

    /// A project as the thread list holds it, from the fields a test is about.
    pub fn project_json(mut record: Value) -> Value {
        fill(&mut record, json!({ "title": "Project", "workspaceRoot": "/w", "defaultModelSelection": null, "scripts": [], "createdAt": "1", "updatedAt": "1" }));
        record
    }

    pub fn project(record: Value) -> ProjectShell {
        serde_json::from_value(project_json(record)).expect("the project fits the schema")
    }

    /// A provider as the server reports one, from the fields a test is about
    /// (its driver is named as its instance unless the test says).
    pub fn provider_json(mut record: Value) -> Value {
        let instance = record["instanceId"].clone();
        fill(&mut record, json!({
            "driver": instance, "enabled": true, "installed": true, "version": null, "status": "ready", "auth": { "status": "authenticated" },
            "checkedAt": "1", "models": [],
        }));
        each(record.get_mut("models"), |_| json!({ "isCustom": false, "capabilities": null }));
        for model in record.get_mut("models").and_then(Value::as_array_mut).into_iter().flatten() {
            each(model.get_mut("capabilities").and_then(|capabilities| capabilities.get_mut("optionDescriptors")), |_| json!({ "label": "Option" }));
        }
        record
    }

    /// The server's config as its stream first sends it, with these
    /// providers (each from the fields a test is about) and settings.
    pub fn config_snapshot(providers: Value, settings: Value) -> Value {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../crates/domovoi-model/tests/samples/subscribeServerConfig.domovoi-server.json");
        let sample: Value = serde_json::from_str(&std::fs::read_to_string(path).expect("the sample config stream")).expect("JSON");
        let mut snapshot = sample.as_array().and_then(|items| items.iter().find(|item| item["type"] == "snapshot")).expect("a config snapshot").clone();
        snapshot["config"]["providers"] = providers.as_array().map(|providers| providers.iter().cloned().map(provider_json).collect()).unwrap_or_default();
        snapshot["config"]["settings"] = settings;
        snapshot["config"]["keybindings"] = json!([]);
        snapshot
    }

    /// The server's welcome, describing itself with the fields a test is about.
    pub fn welcome(mut environment: Value) -> Value {
        fill(&mut environment, json!({
            "environmentId": "srv", "label": "Server", "platform": { "os": "darwin", "arch": "arm64" }, "serverVersion": "0.0.0", "capabilities": {},
        }));
        json!({ "version": 1, "sequence": 1, "type": "welcome", "payload": { "environment": environment, "cwd": "/w", "projectName": "w" } })
    }

    /// The thread list as its stream first sends it: projects and threads,
    /// each from the fields a test is about.
    pub fn shell_snapshot(sequence: u64, projects: Value, threads: Value) -> Value {
        let projects: Vec<Value> = projects.as_array().into_iter().flatten().cloned().map(project_json).collect();
        let threads: Vec<Value> = threads.as_array().into_iter().flatten().cloned().map(shell_json).collect();
        json!({ "kind": "snapshot", "snapshot": { "schemaVersion": 2, "snapshotSequence": sequence, "projects": projects, "threads": threads, "archivedThreads": [] } })
    }

    /// A thread of the list changing, from the fields a test is about.
    pub fn thread_updated(thread: Value) -> Value {
        json!({ "kind": "thread.updated", "sequence": 1, "location": "active", "thread": shell_json(thread) })
    }

    pub fn provider(record: Value) -> ServerProvider {
        serde_json::from_value(provider_json(record)).expect("the provider fits the schema")
    }

    /// A pull request as the server lists one, from the fields a test is about.
    pub fn pull_request_json(mut record: Value) -> Value {
        fill(&mut record, json!({
            "provider": "github", "host": "github.com", "projectTitle": "", "url": "", "author": null, "headBranch": "", "baseBranch": "",
            "state": "open", "isDraft": false, "mergeability": "unknown", "additions": 0, "deletions": 0, "createdAt": "1", "updatedAt": "1",
            "viewerReviewRequested": false, "labels": [],
        }));
        record
    }

    pub fn pull_request(record: Value) -> PullRequestListEntry {
        serde_json::from_value(pull_request_json(record)).expect("the pull request fits the schema")
    }

    /// Completes a record of the projection's collection `collection`
    /// (`thread` for the thread's own). Threads here are `t1` unless they say.
    pub fn complete(collection: &str, record: &mut Value) {
        match collection {
            "thread" => {
                let id = record["id"].as_str().unwrap_or("t1").to_owned();
                fill(record, json!({
                    "createdBy": "user", "creationSource": "web", "id": id, "projectId": "p1", "title": "", "providerInstanceId": "codex",
                    "modelSelection": model_selection(), "runtimeMode": "full-access", "interactionMode": "default", "branch": null,
                    "worktreePath": null, "activeProviderThreadId": null,
                    "lineage": { "parentThreadId": null, "relationshipToParent": null, "rootThreadId": id }, "forkedFrom": null,
                    "createdAt": "1", "updatedAt": "1", "archivedAt": null, "deletedAt": null,
                }));
            }
            "runs" => fill(record, json!({
                "threadId": "t1", "ordinal": 0, "providerInstanceId": "codex", "modelSelection": model_selection(), "providerThreadId": null,
                "userMessageId": "", "rootNodeId": null, "activeAttemptId": null, "status": "completed", "requestedAt": "1", "startedAt": null,
                "completedAt": null, "checkpointId": null, "contextHandoffId": null,
            })),
            "nodes" => fill(record, json!({
                "threadId": "t1", "runId": null, "parentNodeId": null, "rootNodeId": "root", "kind": "assistant_message", "status": "running",
                "countsForRun": true, "providerThreadId": null, "providerTurnId": null, "nativeItemRef": null, "runtimeRequestId": null,
                "checkpointScopeId": null, "startedAt": null, "completedAt": null,
            })),
            "subagents" => {
                if record["nativeTaskRef"].is_object() {
                    fill(&mut record["nativeTaskRef"], native_ref());
                }
                fill(record, json!({
                    "threadId": "t1", "runId": null, "parentNodeId": "root", "origin": "provider_native", "createdBy": "agent", "driver": "claudeAgent",
                    "providerInstanceId": "claudeAgent", "providerThreadId": null, "childThreadId": null, "nativeTaskRef": null, "prompt": "",
                    "title": null, "model": null, "status": "running", "result": null, "startedAt": null, "completedAt": null, "updatedAt": "1",
                }));
            }
            "providerThreads" => fill(record, json!({
                "driver": "codex", "providerInstanceId": "codex", "providerSessionId": null, "appThreadId": "t1", "ownerNodeId": null,
                "nativeThreadRef": null, "nativeConversationHeadRef": null, "status": "idle", "firstRunOrdinal": null, "lastRunOrdinal": null,
                "handoffIds": [], "forkedFrom": null, "createdAt": "1", "updatedAt": "1",
            })),
            "runtimeRequests" => fill(record, json!({
                "nodeId": "root", "providerTurnId": null, "nativeRequestRef": null, "kind": "command", "status": "pending",
                "responseCapability": { "type": "message" }, "createdAt": "1", "resolvedAt": null,
            })),
            "messages" => fill(record, json!({
                "createdBy": "user", "creationSource": "web", "threadId": "t1", "runId": null, "nodeId": null, "role": "user", "text": "",
                "attachments": [], "streaming": false, "createdAt": "1", "updatedAt": "1",
            })),
            "plans" => {
                let of_kind = if record["kind"] == "todo_list" { json!({ "steps": [] }) } else { json!({ "markdown": "" }) };
                fill(record, json!({ "threadId": "t1", "runId": null, "nodeId": "root", "status": "active" }));
                fill(record, of_kind);
            }
            "turnItems" => {
                if record["nativeItemRef"].is_object() {
                    fill(&mut record["nativeItemRef"], native_ref());
                }
                fill(record, json!({
                    "threadId": "t1", "runId": null, "nodeId": null, "providerThreadId": null, "providerTurnId": null, "nativeItemRef": null,
                    "parentItemId": null, "ordinal": 0, "status": "completed", "title": null, "startedAt": null, "completedAt": null, "updatedAt": "1",
                }));
                let item_type = record["type"].as_str().unwrap_or_default().to_owned();
                let of_type = match item_type.as_str() {
                    "user_message" => json!({ "createdBy": "user", "creationSource": "web", "inputIntent": "turn_start", "text": "", "attachments": [] }),
                    "assistant_message" | "reasoning" => json!({ "text": "", "streaming": false }),
                    "command_execution" => json!({ "input": "" }),
                    "file_change" => {
                        each(record.get_mut("changes"), |_| json!({ "operation": "update" }));
                        json!({ "fileName": "" })
                    }
                    "dynamic_tool" => json!({ "toolName": null }),
                    "user_input_request" => json!({ "questions": [] }),
                    "approval_request" => json!({ "requestKind": "command" }),
                    "checkpoint" => json!({ "scopeId": "scope", "files": [] }),
                    "error" => json!({ "failure": { "class": "provider_error", "message": "", "code": null, "retryable": null } }),
                    "todo_list" => {
                        each(record.get_mut("steps"), |n| json!({ "id": format!("step-{}", n + 1) }));
                        json!({ "planId": "plan", "steps": [] })
                    }
                    "proposed_plan" => json!({ "markdown": "", "streaming": false }),
                    "subagent" => json!({
                        "origin": "provider_native", "driver": "claudeAgent", "providerInstanceId": "claudeAgent", "childThreadId": null,
                        "prompt": "", "result": null,
                    }),
                    _ => json!({}),
                };
                fill(record, of_type);
            }
            _ => {}
        }
    }

    /// A thread's snapshot as its stream delivers it, from the parts of a
    /// projection a test gives.
    pub fn snapshot(sequence: u64, mut projection: Value) -> Value {
        let parts = projection.as_object_mut().expect("a projection is an object");
        for kind in domovoi_model::typed::RecordKind::ALL {
            let list = parts.entry(kind.collection()).or_insert_with(|| json!([]));
            for record in list.as_array_mut().into_iter().flatten() {
                complete(kind.collection(), record);
            }
        }
        complete("thread", parts.entry("thread").or_insert_with(|| json!({})));
        let updated_at = parts["thread"]["updatedAt"].clone();
        fill(&mut projection, json!({ "updatedAt": updated_at }));
        json!({ "kind": "snapshot", "snapshotSequence": sequence, "projection": projection })
    }

    /// The thread a client holds after that snapshot.
    pub fn thread(projection: Value) -> Thread {
        match ThreadStreamItem::from_value(snapshot(1, projection)).expect("the snapshot fits the schema") {
            ThreadStreamItem::Snapshot(snapshot) => Thread::from_snapshot(*snapshot),
            _ => unreachable!("a snapshot"),
        }
    }

    /// The collection an event of type `event_type` carries a record of.
    fn collection(event_type: &str) -> &'static str {
        match event_type.rsplit_once('.').map_or(event_type, |(of, _)| of) {
            "run" => "runs",
            "node" => "nodes",
            "message" => "messages",
            "turn-item" => "turnItems",
            "runtime-request" => "runtimeRequests",
            "provider-thread" => "providerThreads",
            "subagent" => "subagents",
            _ if event_type.starts_with("thread.") => "thread",
            other => panic!("no builder for `{other}` events"),
        }
    }

    /// An event as the server records it: `payload` completed as the record
    /// its type carries, in the envelope every event has.
    pub fn event(event_type: &str, occurred_at: &str, mut payload: Value) -> Value {
        complete(collection(event_type), &mut payload);
        let mut event = Map::new();
        event.insert("id".into(), json!(format!("event-{event_type}-{occurred_at}")));
        event.insert("threadId".into(), json!("t1"));
        event.insert("occurredAt".into(), json!(occurred_at));
        event.insert("type".into(), json!(event_type));
        event.insert("payload".into(), payload);
        Value::Object(event)
    }

    /// That event at its place in a thread's stream.
    pub fn stream_event(sequence: u64, event_type: &str, occurred_at: &str, payload: Value) -> Value {
        json!({ "kind": "event", "sequence": sequence, "event": event(event_type, occurred_at, payload) })
    }

    /// That event, to apply to a thread held.
    pub fn domain_event(event_type: &str, occurred_at: &str, payload: Value) -> DomainEvent {
        DomainEvent::from_value(event(event_type, occurred_at, payload)).expect("the event fits the schema")
    }
}

/// What a server of this app's protocol version answers a request for a socket ticket.
fn ticket_answer(ticket: &str) -> Value {
    json!({ "ticket": ticket, "expiresAt": "2026-11-13T00:00:00.000Z", "protocol": domovoi_model::PROTOCOL_VERSION })
}

fn fixture() -> Vec<Value> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/native/session1/wire.ndjson");
    std::fs::read_to_string(path).unwrap().lines().map(|l| serde_json::from_str(l).unwrap()).collect()
}

/// The recorded thread `t1` (a scripted Codex turn with every kind of item).
fn recorded() -> (Vec<Value>, Vec<Value>, Value) {
    recorded_of(THREAD)
}

/// The recorded thread whose second reply streams 20,000 characters.
const STREAMED_THREAD: &str = "t2";

/// (shell stream items, `thread`'s snapshot + events and appended text, the thread's last shell as the server sent it)
fn recorded_of(thread: &str) -> (Vec<Value>, Vec<Value>, Value) {
    let mut reqs: BTreeMap<(i64, String), Value> = BTreeMap::new();
    let (mut shell, mut thread_items, mut last_shell) = (vec![], BTreeMap::new(), Value::Null);
    let mut snapshot = None;
    for rec in fixture() {
        let f = &rec["frame"];
        if rec["kind"] != "ws" || !f.is_object() {
            continue;
        }
        let conn = rec["conn"].as_i64().unwrap_or(0);
        if f["_tag"] == "Request" {
            reqs.insert((conn, f["id"].to_string()), f.clone());
            continue;
        }
        let Some(req) = reqs.get(&(conn, f["requestId"].to_string())) else { continue };
        if f["_tag"] != "Chunk" {
            continue;
        }
        for v in f["values"].as_array().unwrap() {
            match req["tag"].as_str() {
                Some("orchestration.subscribeShell") => {
                    if v["kind"] == "thread.updated" && v["thread"]["id"] == thread {
                        last_shell = v["thread"].clone();
                    }
                    shell.push(v.clone());
                }
                Some("orchestration.subscribeThread") if req["payload"]["threadId"] == thread => {
                    if v["kind"] == "snapshot" {
                        snapshot.get_or_insert_with(|| v.clone());
                    } else if v["kind"] == "event" || v["kind"] == "append" {
                        thread_items.insert(v["sequence"].as_u64().unwrap(), v.clone());
                    }
                }
                _ => {}
            }
        }
    }
    let mut items = vec![snapshot.expect("snapshot")];
    items.extend(thread_items.into_values());
    (shell, items, last_shell)
}

/// The `Send` frames among `effects`, parsed.
fn sent_requests(effects: &[Effect]) -> Vec<Value> {
    effects
        .iter()
        .filter_map(|e| match e {
            Effect::Socket(req) => match &req.operation {
                SocketOperation::Send { text } => serde_json::from_str::<Value>(text).ok(),
                _ => None,
            },
            _ => None,
        })
        .collect()
}

/// `run`, for where a local `run` hides it.
fn run_event(model: &mut Model, event: Event) -> Vec<Effect> {
    run(model, event)
}

/// Runs one event and returns the effects it requested.
fn run(model: &mut Model, event: Event) -> Vec<Effect> {
    let mut cmd = DomovoiApp.update(event, model);
    read_by_the_server(cmd.effects().collect())
}

/// `effects`, once every command and launch among them is one the server
/// reads as it was written (`common::read_by_the_server`).
fn read_by_the_server(effects: Vec<Effect>) -> Vec<Effect> {
    for request in sent_requests(&effects) {
        if let Err(why) = common::read_by_the_server(&request) {
            panic!("a request the server would not read as sent: {why}");
        }
    }
    effects
}

fn chunk(id: &str, values: &[Value]) -> SocketEvent {
    SocketEvent::Message { text: json!({ "_tag": "Chunk", "requestId": id, "values": values }).to_string() }
}

fn exit_ok(id: &str, value: Value) -> SocketEvent {
    SocketEvent::Message { text: json!({ "_tag": "Exit", "requestId": id, "exit": { "_tag": "Success", "value": value } }).to_string() }
}

/// A socket event of the current connection attempt.
fn sock(model: &mut Model, event: SocketEvent) -> Vec<Effect> {
    let attempt = model.attempt;
    run(model, Event::Socket { attempt, event })
}

/// An output of the current tunnel.
fn tun(model: &mut Model, out: TunnelOutput) -> Vec<Effect> {
    let tunnel_gen = model.tunnel_gen;
    run(model, Event::Tunnel { tunnel_gen, out })
}

/// A shell double: a key-value store and an HTTP server (by URL path).
#[derive(Default)]
struct Shell {
    store: HashMap<String, String>,
    routes: HashMap<String, (u16, Value)>,
    requests: Vec<HttpRequest>,
    /// When set, the commands and launches the server would not read as
    /// sent are listed here; otherwise one is a failure where it is sent.
    refused: Option<Vec<String>>,
    /// The descriptor of the server on this machine, as its file's text.
    descriptor: Option<String>,
    /// What writing a request file gives: its id and secret (`None`: it cannot be written).
    local_request: Option<(String, String)>,
    /// What the core asked about this machine's server.
    local_asked: Vec<LocalOperation>,
}

impl Shell {
    fn paired() -> Self {
        let mut shell = Shell::default();
        shell.routes.insert("/oauth/token".into(), (200, json!({ "access_token": "tok-1", "issued_token_type": "urn:ietf:params:oauth:token-type:access_token", "token_type": "Bearer", "expires_in": 100, "scope": "orchestration:read" })));
        shell.routes.insert("/api/auth/websocket-ticket/rpc".into(), (200, ticket_answer("tick-1")));
        shell
    }

    /// Runs `event` and answers store and HTTP requests, feeding resulting
    /// events back, until only other effects remain; returns those.
    fn drive(&mut self, model: &mut Model, event: Event) -> Vec<Effect> {
        let mut queue = VecDeque::from([event]);
        let mut out = vec![];
        while let Some(event) = queue.pop_front() {
            let mut cmd = DomovoiApp.update(event, model);
            loop {
                let effects: Vec<Effect> = cmd.effects().collect();
                let answered = effects.iter().any(|e| matches!(e, Effect::Store(_) | Effect::Http(_) | Effect::Local(_)));
                for effect in effects {
                    match effect {
                        Effect::Local(mut req) => {
                            self.local_asked.push(req.operation.clone());
                            let out = match (&req.operation, &self.local_request) {
                                (LocalOperation::Watch, _) => LocalOutput::Descriptor { text: self.descriptor.clone() },
                                (LocalOperation::Request, Some((id, secret))) => LocalOutput::Request { id: id.clone(), secret: secret.clone() },
                                (LocalOperation::Request, None) => LocalOutput::Unavailable,
                            };
                            req.resolve(out).unwrap();
                        }
                        Effect::Store(mut req) => {
                            let value = match &req.operation {
                                StoreOperation::Load { key } => self.store.get(key).cloned(),
                                StoreOperation::Save { key, value } => {
                                    self.store.insert(key.clone(), value.clone());
                                    None
                                }
                                StoreOperation::Delete { key } => {
                                    self.store.remove(key);
                                    None
                                }
                            };
                            req.resolve(StoreOutput::Value { value }).unwrap();
                        }
                        Effect::Http(mut req) => {
                            let path = req.operation.url.splitn(4, '/').nth(3).map(|p| format!("/{p}")).unwrap_or_default();
                            let (status, body) = self.routes.get(&path).cloned().unwrap_or((404, json!({})));
                            self.requests.push(req.operation.clone());
                            req.resolve(HttpResult::Ok(HttpResponse::status(status).json(body).build())).unwrap();
                        }
                        other => out.push(other),
                    }
                }
                queue.extend(cmd.events());
                if !answered {
                    break;
                }
            }
        }
        match &mut self.refused {
            Some(refused) => {
                refused.extend(sent_requests(&out).iter().filter_map(|request| common::read_by_the_server(request).err()));
                out
            }
            None => read_by_the_server(out),
        }
    }

    fn drive_sock(&mut self, model: &mut Model, event: SocketEvent) -> Vec<Effect> {
        let attempt = model.attempt;
        self.drive(model, Event::Socket { attempt, event })
    }

    fn drive_tunnel(&mut self, model: &mut Model, out: TunnelOutput) -> Vec<Effect> {
        let tunnel_gen = model.tunnel_gen;
        self.drive(model, Event::Tunnel { tunnel_gen, out })
    }
}

fn socket_url(effects: &[Effect]) -> Option<String> {
    effects.iter().find_map(|e| match e {
        Effect::Socket(r) => match &r.operation {
            SocketOperation::Open { url } => Some(url.clone()),
            _ => None,
        },
        _ => None,
    })
}

fn tunnel_start(effects: &mut [Effect]) -> Option<&mut crux_core::Request<TunnelOperation>> {
    effects.iter_mut().find_map(|e| match e {
        Effect::Tunnel(r) if matches!(r.operation, TunnelOperation::Start { .. }) => Some(r),
        _ => None,
    })
}

fn connected() -> (Model, Vec<Value>) {
    let mut shell = Shell::paired();
    let mut model = Model::default();
    let effects = shell.drive(&mut model, Event::Connect { server_url: "http://127.0.0.1:1".into(), pairing_code: "CODE".into() });
    let url = socket_url(&effects).expect("socket opened");
    assert!(url.contains("wsTicket=tick-1"), "{url}");
    let sent = sent_requests(&sock(&mut model, SocketEvent::Opened));
    (model, sent)
}

#[test]
fn core_reaches_the_servers_state_from_recorded_frames() {
    let (mut model, sent) = connected();
    let shell_id = sent.iter().find(|r| r["tag"] == "orchestration.subscribeShell").expect("shell subscription")["id"].as_str().unwrap().to_owned();
    assert!(sent.iter().any(|r| r["tag"] == "subscribeServerConfig"));

    let (shell, thread_items, ts_shell) = recorded();
    for v in &shell {
        // Every chunk is acknowledged.
        let sent = sent_requests(&sock(&mut model, chunk(&shell_id, std::slice::from_ref(v))));
        assert!(sent.iter().any(|r| r["_tag"] == "Ack"));
    }
    model.sidebar_layout.grouping = Grouping::Project;
    let view = DomovoiApp.view(&model);
    assert!(view.sidebar.projects.iter().any(|p| p.title == "w"));

    // Select the thread: the core subscribes and records the visit.
    let sent = sent_requests(&run(&mut model, Event::SelectThread { thread_id: THREAD.into() }));
    let thread_sub = sent.iter().find(|r| r["tag"] == "orchestration.subscribeThread").expect("thread subscription");
    assert_eq!(thread_sub["payload"]["threadId"], THREAD);
    assert!(sent.iter().any(|r| r["payload"]["type"] == "thread.visit"));
    let thread_id = thread_sub["id"].as_str().unwrap().to_owned();
    for v in &thread_items {
        let _ = sock(&mut model, chunk(&thread_id, std::slice::from_ref(v)));
    }

    let view = DomovoiApp.view(&model);
    let listed = view.sidebar.projects.iter().flat_map(|p| p.threads.iter()).chain(view.sidebar.settled.iter()).find(|t| t.id == THREAD).expect("thread listed");
    let ts_status = domovoi_core::replies::shell_status(&serde_json::from_value(ts_shell.clone()).expect("the server's shell decodes"));
    assert_eq!(listed.status, ts_status.as_str(), "thread list status matches the server's shell ({})", ts_shell["status"]);
    let detail = view.thread.expect("thread page");
    assert_eq!(view.route, "thread");
    assert_eq!(detail.items.len() as u64, ts_shell["visibleItemCount"].as_u64().unwrap(), "visible items match the server's count");
    assert_eq!(detail.items.first().map(|i| i.kind.as_str()), Some("user"));
    assert!(detail.items.iter().any(|i| i.kind == "approval"), "approval cards are shown");
    let cp = detail.items.iter().find(|i| i.kind == "checkpoint").expect("checkpoint row");
    assert!(cp.checkpoint.is_some(), "checkpoint rows link their checkpoint");
    assert!(!detail.checkpoints.is_empty());
    assert!(detail.items[0].text.starts_with("ALL of it"), "user text comes from the message entity");
    assert!(detail.items.iter().any(|i| i.kind == "assistant" && !i.blocks.is_empty()), "assistant markdown is split into blocks");
    assert!(detail.pending_requests.is_empty(), "approvals were all answered");
    assert!(!detail.working);
}

/// A server of another protocol version is not connected to: the app says
/// which side to update and asks again later. One that names no version is
/// from before the versions were compared.
#[test]
fn a_server_of_another_protocol_version_is_named_not_connected_to() {
    let mine = domovoi_model::PROTOCOL_VERSION;
    for (theirs, advice) in [
        (json!(mine + 1), "This app is too old for this server. Update Domovoi on this device."),
        (json!(mine - 1), "This server is older than this app. Update domovoi-server on the machine it runs on."),
        (Value::Null, "This server is older than this app. Update domovoi-server on the machine it runs on."),
    ] {
        let mut shell = Shell::paired();
        let mut answer = json!({ "ticket": "tick-1", "expiresAt": "2026-11-13T00:00:00.000Z" });
        if !theirs.is_null() {
            answer["protocol"] = theirs.clone();
        }
        shell.routes.insert("/api/auth/websocket-ticket/rpc".into(), (200, answer));
        let mut model = Model::default();
        let _ = shell.drive(&mut model, Event::Launch { surface: "mobile".into(), device_label: "Test Phone".into(), device_os: "iOS".into() });
        let effects = shell.drive(&mut model, Event::Connect { server_url: "http://h:1/".into(), pairing_code: "CODE".into() });
        assert_eq!(socket_url(&effects), None, "protocol {theirs}: no socket is opened");
        let connection = DomovoiApp.view(&model).connection;
        assert_eq!(connection.error.as_deref(), Some(advice), "protocol {theirs}");
        assert_eq!(connection.state, "reconnecting", "protocol {theirs}: the app asks again, the server may be updated meanwhile");
        assert!(connection.reconnect_in_seconds > 0, "protocol {theirs}");
        assert!(!connection.server_changed && !connection.needs_pairing, "protocol {theirs}: the device stays paired");
    }
}

/// A server on this machine is not asked to pack its frames: nothing is
/// between the two but memory.
#[test]
fn a_server_on_this_machine_is_not_asked_for_packed_frames() {
    let mut shell = Shell::paired();
    let mut model = Model::default();
    let _ = shell.drive(&mut model, Event::Launch { surface: "desktop".into(), device_label: "Test Mac".into(), device_os: "macOS".into() });
    let effects = shell.drive(&mut model, Event::Connect { server_url: "http://127.0.0.1:13773/".into(), pairing_code: "CODE".into() });
    assert!(socket_url(&effects).unwrap().starts_with(&format!("ws://127.0.0.1:13773/ws?protocol={}&wsTicket=tick-1", domovoi_model::PROTOCOL_VERSION)));
}

#[test]
fn pairing_exchanges_the_code_for_a_saved_token() {
    let mut shell = Shell::paired();
    let mut model = Model::default();
    let _ = shell.drive(&mut model, Event::Launch { surface: "mobile".into(), device_label: "Test Phone".into(), device_os: "iOS".into() });
    let effects = shell.drive(&mut model, Event::Connect { server_url: "http://h:1/".into(), pairing_code: "CODE".into() });
    let exchange = &shell.requests[0];
    assert_eq!((exchange.url.as_str(), exchange.method.as_str()), ("http://h:1/oauth/token", "POST"));
    let body = String::from_utf8(exchange.body.clone()).unwrap();
    for part in ["grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Atoken-exchange", "subject_token=CODE", "client_device_type=mobile", "client_label=Test+Phone", "client_os=iOS"] {
        assert!(body.contains(part), "{body} lacks {part}");
    }
    // The ticket request carries the token; the socket URL the ticket.
    let ticket = &shell.requests[1];
    assert!(ticket.headers.iter().any(|h| h.name.eq_ignore_ascii_case("authorization") && h.value == "Bearer tok-1"));
    // A server across a network is asked for packed frames.
    assert!(socket_url(&effects).unwrap().starts_with(&format!("ws://h:1/ws?protocol={}&pack=1&wsTicket=tick-1", domovoi_model::PROTOCOL_VERSION)));
    // Saved: the environment, which one is active, and its token.
    let env_id = model.active_env.clone().unwrap();
    assert_eq!(shell.store.get(&format!("token:{env_id}")).map(String::as_str), Some("tok-1"));
    assert_eq!(shell.store.get("activeEnvironment"), Some(&env_id));
    assert!(shell.store["environments"].contains("http://h:1"));

    // A fresh launch restores it all and connects without a code.
    let mut model = Model::default();
    shell.requests.clear();
    let effects = shell.drive(&mut model, Event::Launch { surface: "mobile".into(), device_label: String::new(), device_os: String::new() });
    assert!(socket_url(&effects).is_some(), "reconnects at launch");
    assert_eq!(shell.requests.len(), 1, "only a ticket, no pairing");
}

#[test]
fn a_rejected_code_or_revoked_token_asks_to_pair_again() {
    let mut shell = Shell::paired();
    shell.routes.insert("/oauth/token".into(), (401, json!({ "_tag": "EnvironmentAuthInvalidError" })));
    let mut model = Model::default();
    let _ = shell.drive(&mut model, Event::Connect { server_url: "http://h:1".into(), pairing_code: "BAD".into() });
    let view = DomovoiApp.view(&model);
    assert_eq!(view.connection.state, "failed");
    assert!(view.connection.error.unwrap().contains("rejected"));

    // A saved token the server no longer accepts is forgotten.
    let mut shell = Shell::paired();
    let mut model = Model::default();
    let _ = shell.drive(&mut model, Event::Connect { server_url: "http://h:1".into(), pairing_code: "CODE".into() });
    shell.routes.insert("/api/auth/websocket-ticket/rpc".into(), (401, json!({})));
    let _ = sock(&mut model, SocketEvent::Opened);
    let _ = sock(&mut model, SocketEvent::Closed { reason: "server restarted".into() });
    let _ = shell.drive(&mut model, Event::Reconnect);
    let view = DomovoiApp.view(&model);
    assert_eq!(view.connection.state, "failed");
    assert!(view.connection.needs_pairing);
    assert!(!shell.store.keys().any(|k| k.starts_with("token:")));
}

#[test]
fn remote_environments_connect_through_the_tunnel() {
    let mut shell = Shell::paired();
    let mut model = Model::default();
    let link = "domovoi://connect?ticket=endpointabc&code=CODE&name=Studio+Mac";
    let mut effects = shell.drive(&mut model, Event::ConnectRemote { link: link.into() });
    assert_eq!(DomovoiApp.view(&model).connection.state, "pairing");
    assert!(shell.requests.is_empty(), "nothing is sent before the tunnel is up");
    let start = tunnel_start(&mut effects).expect("tunnel started");
    assert_eq!(start.operation, TunnelOperation::Start { ticket: "endpointabc".into() });

    // The tunnel comes up on a loopback port: pair and open the socket there.
    let effects = shell.drive_tunnel(&mut model, TunnelOutput::Started { port: 4242 });
    assert_eq!(shell.requests[0].url, "http://127.0.0.1:4242/oauth/token");
    assert!(socket_url(&effects).unwrap().starts_with("ws://127.0.0.1:4242/ws?"));
    let _ = sock(&mut model, SocketEvent::Opened);
    let _ = tun(&mut model, TunnelOutput::Status { connected: true, path: "relay".into(), rtt_ms: 80, error: String::new(), listening: true });
    let view = DomovoiApp.view(&model);
    assert_eq!(view.connection.state, "connected");
    assert!(view.connection.remote);
    assert_eq!((view.connection.path.as_str(), view.connection.rtt_ms), ("relay", 80));
    assert_eq!(view.connection.environments[0].label, "Studio Mac");

    // The same server with a newer ticket updates the saved environment.
    let _ = shell.drive(&mut model, Event::ConnectRemote { link: "domovoi://connect?ticket=endpointabc&name=Studio".into() });
    assert_eq!(model.environments.len(), 1);
}

#[test]
fn a_failed_tunnel_retries_and_switching_tears_down() {
    let mut shell = Shell::paired();
    let mut model = Model::default();
    let _ = shell.drive(&mut model, Event::ConnectRemote { link: "domovoi://connect?ticket=endpointabc&code=CODE".into() });
    let _ = shell.drive_tunnel(&mut model, TunnelOutput::Started { port: 4242 });
    let _ = sock(&mut model, SocketEvent::Opened);

    // The connection drops and the tunnel cannot redial: back off and retry.
    let _ = sock(&mut model, SocketEvent::Closed { reason: "lost".into() });
    let _ = tun(&mut model, TunnelOutput::Failed { reason: "no route".into() });
    let view = DomovoiApp.view(&model);
    assert_eq!(view.connection.state, "reconnecting");
    assert!(view.connection.reconnect_in_seconds > 0);
    let mut effects = shell.drive(&mut model, Event::Reconnect);
    assert!(tunnel_start(&mut effects).is_some(), "a failed tunnel is started again");

    // Switching to a local environment stops the tunnel and forgets the server's data.
    model.projects = vec![records::project(json!({ "id": "p-remote" }))];
    let effects = shell.drive(&mut model, Event::Connect { server_url: "http://h:1".into(), pairing_code: "CODE2".into() });
    assert!(effects.iter().any(|e| matches!(e, Effect::Tunnel(r) if r.operation == TunnelOperation::Stop)));
    assert!(model.projects.is_empty());
    assert_eq!(model.environments.len(), 2);
    assert!(socket_url(&effects).unwrap().starts_with("ws://h:1/"));

    // Forgetting the active environment disconnects and deletes its token.
    let active = model.active_env.clone().unwrap();
    let _ = shell.drive(&mut model, Event::ForgetEnvironment { id: active.clone() });
    assert_eq!(DomovoiApp.view(&model).connection.state, "disconnected");
    assert!(!shell.store.contains_key(&format!("token:{active}")));
}

#[test]
fn backgrounding_closes_the_socket_and_resume_reconnects() {
    let (mut model, _) = connected();
    let effects = run(&mut model, Event::Suspend);
    assert!(effects.iter().any(|e| matches!(e, Effect::Socket(r) if r.operation == SocketOperation::Close)));
    let _ = sock(&mut model, SocketEvent::Closed { reason: String::new() });
    assert_eq!(DomovoiApp.view(&model).connection.reconnect_in_seconds, 0, "no retries while in the background");
    assert!(run(&mut model, Event::Reconnect).is_empty());
    let mut shell = Shell::paired();
    let effects = shell.drive(&mut model, Event::Resume);
    assert!(socket_url(&effects).is_some());
}

fn providers_snapshot(model: &mut Model, sent: &[Value]) {
    let config_id = sent.iter().find(|r| r["tag"] == "subscribeServerConfig").unwrap()["id"].as_str().unwrap().to_owned();
    let providers = json!([{ "instanceId": "claudeAgent", "displayName": "Claude", "status": "ready", "enabled": true, "models": [
        { "slug": "claude-haiku-5-5", "name": "Haiku 5.5", "isDefault": true, "capabilities": { "optionDescriptors": [
            { "id": "effort", "type": "select", "options": [{ "id": "low", "label": "Low" }, { "id": "medium", "label": "Medium", "isDefault": true }] }] } }] }]);
    let _ = sock(model, chunk(&config_id, &[records::config_snapshot(providers, json!({ "defaultRuntimeMode": "approval-required" }))]));
}

#[test]
fn composing_and_launching_a_thread() {
    let (mut model, sent) = connected();
    model.selected_project = Some("p1".into());
    model.route = "new-thread".into();

    // No provider yet: sending reports instead of launching.
    let _ = run(&mut model, Event::SetDraft { text: "hi".into() });
    let effects = run(&mut model, Event::Send);
    assert!(sent_requests(&effects).iter().all(|r| r["tag"] != "orchestration.launchThread"));
    assert!(DomovoiApp.view(&model).toast.unwrap().message.contains("No provider"));

    providers_snapshot(&mut model, &sent);
    let view = DomovoiApp.view(&model);
    assert_eq!(view.composer.selected_model.as_ref().map(|m| m.model.as_str()), Some("claude-haiku-5-5"));
    assert_eq!(view.composer.selected_effort.as_deref(), Some("medium"));
    assert_eq!(view.composer.runtime_mode, "approval-required", "new threads start from the server's default");
    assert!(view.composer.can_send);

    let _ = run(&mut model, Event::SelectEffort { value: "low".into() });
    let _ = run(&mut model, Event::SetWorkspaceMode { mode: "worktree".into() });
    let _ = run(&mut model, Event::SetBaseBranch { branch: "main".into() });
    let sent = sent_requests(&run(&mut model, Event::Send));
    let launch = sent.iter().find(|r| r["tag"] == "orchestration.launchThread").expect("launch");
    let p = &launch["payload"];
    assert_eq!(p["projectId"], "p1");
    assert_eq!(p["initialMessage"]["text"], "hi");
    assert_eq!(p["modelSelection"], json!({ "instanceId": "claudeAgent", "model": "claude-haiku-5-5", "options": [{ "id": "effort", "value": "low" }] }));
    assert_eq!(p["workspaceStrategy"], json!({ "type": "worktree", "baseRef": "main" }));
    assert_eq!(DomovoiApp.view(&model).composer.draft, "", "the draft clears once sent");

    // Shown at once, before the server answers: open, listed, its message on its way.
    let tid = p["threadId"].as_str().unwrap().to_owned();
    assert!(sent.iter().all(|r| r["tag"] != "orchestration.subscribeThread"), "nothing to subscribe to before it is made");
    let view = DomovoiApp.view(&model);
    assert_eq!(view.route, "thread");
    let page = view.thread.expect("the new thread's page, at once");
    assert_eq!((page.id.as_str(), page.title.as_str(), page.loading), (tid.as_str(), "hi", false));
    assert_eq!(page.pending_sends.iter().map(|p| p.text.as_str()).collect::<Vec<_>>(), ["hi"]);
    assert_eq!(page.working_activity.and_then(|activity| activity.kind).as_deref(), Some("starting"));
    assert_eq!(view.sidebar.surface_threads.first().map(|r| (r.id.as_str(), r.status_label.as_str())), Some((tid.as_str(), "Starting")));
    let running = view.sidebar.sections.iter().find(|s| s.kind == "running").expect("listed under Running, made for it");
    assert_eq!((running.count, running.rows.first().map(|r| r.id.as_str())), (1, Some(tid.as_str())));

    // The launch result opens the new thread's stream.
    let sent = sent_requests(&sock(&mut model, exit_ok(launch["id"].as_str().unwrap(), json!({ "threadId": tid }))));
    assert!(sent.iter().any(|r| r["tag"] == "orchestration.subscribeThread" && r["payload"]["threadId"] == tid.as_str()));
    assert_eq!(model.selected_thread.as_deref(), Some(tid.as_str()));
    let page = DomovoiApp.view(&model).thread.expect("page");
    assert!(!page.loading && page.pending_sends.len() == 1, "its message stays shown until the thread's own copy comes");
}

/// Two providers whose models offer different efforts.
fn two_effort_providers(model: &mut Model, sent: &[Value]) {
    let config_id = sent.iter().find(|r| r["tag"] == "subscribeServerConfig").unwrap()["id"].as_str().unwrap().to_owned();
    let effort = |ids: &[&str], default: &str| json!({ "optionDescriptors": [{ "id": "effort", "type": "select",
        "options": ids.iter().map(|id| json!({ "id": id, "label": id, "isDefault": *id == default })).collect::<Vec<_>>() }] });
    let providers = json!([
        { "instanceId": "codex", "displayName": "Codex", "status": "ready", "enabled": true, "models": [
            { "slug": "gpt-6", "name": "GPT-6", "isDefault": true, "capabilities": effort(&["low", "medium", "high", "xhigh"], "medium") }] },
        { "instanceId": "claudeAgent", "displayName": "Claude", "status": "ready", "enabled": true, "models": [
            { "slug": "claude-haiku-5-5", "name": "Haiku 5.5", "isDefault": true, "capabilities": effort(&["low", "medium", "high"], "high") }] }]);
    let _ = sock(model, chunk(&config_id, &[records::config_snapshot(providers, json!({ "defaultRuntimeMode": "approval-required" }))]));
}

#[test]
fn a_model_switch_never_carries_an_effort_the_new_model_lacks() {
    let (mut model, sent) = connected();
    two_effort_providers(&mut model, &sent);
    model.threads = vec![records::shell(json!({ "id": "t1", "projectId": "p1",
        "modelSelection": { "instanceId": "codex", "model": "gpt-6", "options": [{ "id": "effort", "value": "xhigh" }] } }))];
    model.selected_thread = Some("t1".into());
    model.route = "thread".into();
    assert_eq!(DomovoiApp.view(&model).composer.selected_effort.as_deref(), Some("xhigh"), "the thread's own effort");

    let sent = sent_requests(&run(&mut model, Event::SelectModel { instance_id: "claudeAgent".into(), model: "claude-haiku-5-5".into() }));
    let set = sent.iter().find(|r| r["payload"]["type"] == "thread.model-selection.set").expect("the model changes");
    assert_eq!(set["payload"]["modelSelection"]["options"], json!([{ "id": "effort", "value": "high" }]), "Claude's default, not Codex's xhigh");
}

#[test]
fn an_effort_picked_in_one_thread_stays_in_it() {
    let (mut model, sent) = connected();
    two_effort_providers(&mut model, &sent);
    let on_claude = json!({ "instanceId": "claudeAgent", "model": "claude-haiku-5-5" });
    model.threads = vec![
        records::shell(json!({ "id": "t1", "projectId": "p1", "modelSelection": on_claude })),
        records::shell(json!({ "id": "t2", "projectId": "p1", "modelSelection": on_claude })),
    ];
    model.selected_thread = Some("t1".into());
    let _ = run(&mut model, Event::SelectEffort { value: "low".into() });
    assert_eq!(DomovoiApp.view(&model).composer.selected_effort.as_deref(), Some("low"));
    model.selected_thread = Some("t2".into());
    assert_eq!(DomovoiApp.view(&model).composer.selected_effort.as_deref(), Some("high"), "t2 keeps the model's default");
    model.selected_thread = Some("t1".into());
    assert_eq!(DomovoiApp.view(&model).composer.selected_effort.as_deref(), Some("low"));
}

#[test]
fn a_dropped_socket_mid_upload_lets_the_message_be_sent_again() {
    let (mut model, _) = connected();
    model.selected_thread = Some("t1".into());
    model.route = "thread".into();
    let _ = run(&mut model, Event::AddAttachment { name: "shot.png".into(), mime_type: "image/png".into(), size_bytes: 3, data_url: "data:image/png;base64,AAAA".into() });
    let _ = run(&mut model, Event::SetDraft { text: "see this".into() });
    let sent = sent_requests(&run(&mut model, Event::Send));
    assert!(sent.iter().any(|r| r["tag"] == "assets.persistChatAttachments"));
    assert!(DomovoiApp.view(&model).composer.sending);

    let _ = sock(&mut model, SocketEvent::Closed { reason: "gone".into() });
    let view = DomovoiApp.view(&model);
    assert!(!view.composer.sending, "nothing will finish that send");
    assert_eq!(view.composer.draft, "see this", "the message is still there to send");
    assert!(view.composer.attachments.iter().all(|a| !a.uploading));
}

#[test]
fn an_upload_that_finishes_after_switching_threads_sends_where_it_was_asked() {
    let (mut model, _) = connected();
    model.selected_thread = Some("t1".into());
    model.route = "thread".into();
    let _ = run(&mut model, Event::AddAttachment { name: "shot.png".into(), mime_type: "image/png".into(), size_bytes: 3, data_url: "data:image/png;base64,AAAA".into() });
    let _ = run(&mut model, Event::SetDraft { text: "for t1".into() });
    let sent = sent_requests(&run(&mut model, Event::Send));
    let upload = sent.iter().find(|r| r["tag"] == "assets.persistChatAttachments").expect("the upload").clone();

    // Another thread opens, with its own draft, before the upload is done.
    model.selected_thread = Some("t2".into());
    let _ = run(&mut model, Event::SetDraft { text: "t2's draft".into() });
    let sent = sent_requests(&sock(&mut model, exit_ok(upload["id"].as_str().unwrap(),
        json!({ "attachments": [{ "type": "image", "id": "t1-new", "name": "shot.png", "mimeType": "image/png", "sizeBytes": 3 }] }))));
    let dispatch = sent.iter().find(|r| r["payload"]["type"] == "message.dispatch").expect("the message goes");
    assert_eq!(dispatch["payload"]["threadId"], "t1");
    assert_eq!(dispatch["payload"]["text"], "for t1");
    assert_eq!(DomovoiApp.view(&model).composer.draft, "t2's draft", "the open thread's draft is untouched");
}

#[test]
fn thread_intents_become_commands() {
    let (mut model, _) = connected();
    model.selected_thread = Some("t1".into());
    model.route = "thread".into();
    let _ = run(&mut model, Event::SetDraft { text: "next".into() });
    let sent = sent_requests(&run(&mut model, Event::Send));
    assert_eq!(sent[0]["payload"]["type"], "message.dispatch");
    assert_eq!(sent[0]["payload"]["text"], "next");

    // An answer goes to the thread holding the request, and only to it.
    assert!(sent_requests(&run(&mut model, Event::Respond { request_id: "rq".into(), decision: "accept".into() })).is_empty(), "no thread holds it");
    model.thread = Some(records::thread(json!({ "runtimeRequests": [{ "id": "rq" }] })));
    let sent = sent_requests(&run(&mut model, Event::Respond { request_id: "rq".into(), decision: "acceptForSession".into() }));
    assert_eq!((sent[0]["payload"]["type"].as_str(), sent[0]["payload"]["decision"].as_str()), (Some("runtime-request.respond"), Some("acceptForSession")));
    assert_eq!(sent[0]["payload"]["threadId"], "t1");

    let sent = sent_requests(&run(&mut model, Event::RenameThread { thread_id: "t1".into(), title: " New name ".into() }));
    assert_eq!((sent[0]["payload"]["type"].as_str(), sent[0]["payload"]["title"].as_str()), (Some("thread.metadata.update"), Some("New name")));
    let sent = sent_requests(&run(&mut model, Event::SettleThread { thread_id: "t1".into(), settled: true }));
    assert_eq!(sent[0]["payload"]["type"], "thread.settle");
    let sent = sent_requests(&run(&mut model, Event::SetRuntimeMode { mode: "full-access".into() }));
    assert_eq!(sent[0]["payload"]["runtimeMode"], "full-access");
}

/// A secret the agent asks for shows as a card with what it asked; its
/// answer goes once as `secrets.answerRequest` (t3code's payload) and is
/// kept nowhere: not in the model, the outbox or the store, nor sent again
/// after the socket drops.
#[test]
fn secret_requests_are_answered_once_and_kept_nowhere() {
    let (mut model, _) = connected();
    model.selected_thread = Some("t1".into());
    model.route = "thread".into();
    let asked = |status: &str| records::thread(json!({ "turnItems": [{
        "id": "s1", "type": "secret_request", "status": "running", "label": "OpenAI API key", "reason": "To call the API in tests.",
        "url": "https://platform.openai.com/api-keys", "placeholder": "sk-...", "delivery": "file", "secretStatus": status,
    }] }));
    model.thread = Some(asked("pending"));
    let page = DomovoiApp.view(&model).thread.expect("the thread");
    assert_eq!(page.secret_requests.len(), 1);
    let card = &page.secret_requests[0];
    assert_eq!((card.item_id.as_str(), card.label.as_str(), card.placeholder.as_str(), card.delivery.as_str()), ("s1", "OpenAI API key", "sk-...", "file"));
    assert_eq!(card.url.as_deref(), Some("https://platform.openai.com/api-keys"));
    assert!(!card.sending && card.hint.contains("file"));

    // Nothing typed, nothing sent; an item that isn't a pending secret request, nothing.
    assert!(sent_requests(&run(&mut model, Event::AnswerSecret { item_id: "s1".into(), value: "  ".into() })).is_empty());
    assert!(sent_requests(&run(&mut model, Event::AnswerSecret { item_id: "other".into(), value: "x".into() })).is_empty());

    let effects = run(&mut model, Event::AnswerSecret { item_id: "s1".into(), value: "sk-test-123".into() });
    let sent = sent_requests(&effects);
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["tag"], "secrets.answerRequest");
    assert_eq!(sent[0]["payload"], json!({ "threadId": "t1", "turnItemId": "s1", "answer": { "type": "save", "secret": "sk-test-123" } }));
    assert!(effects.iter().all(|e| !matches!(e, Effect::Store(_))), "nothing is saved");
    assert!(model.composer.outbox.is_empty(), "no outbox holds it");
    assert!(DomovoiApp.view(&model).thread.expect("the thread").secret_requests[0].sending);
    // Tapped again while it goes: not sent twice.
    assert!(sent_requests(&run(&mut model, Event::AnswerSecret { item_id: "s1".into(), value: "sk-test-123".into() })).is_empty());

    // The socket drops before the answer: nothing is sent again on reconnect.
    let _ = sock(&mut model, SocketEvent::Closed { reason: "gone".into() });
    assert!(!DomovoiApp.view(&model).thread.expect("the thread").secret_requests[0].sending, "the card can be answered anew");
    let effects = run(&mut model, Event::AnswerSecret { item_id: "s1".into(), value: "sk-test-123".into() });
    assert!(sent_requests(&effects).is_empty(), "not sent without a connection");
    assert!(model.toast.as_ref().is_some_and(|t| t.kind == "error"));

    // Connected again: a decline, then the server's answer.
    let (mut model, _) = connected();
    model.selected_thread = Some("t1".into());
    model.thread = Some(asked("pending"));
    let effects = run(&mut model, Event::DeclineSecret { item_id: "s1".into() });
    let sent = sent_requests(&effects);
    assert_eq!(sent[0]["payload"]["answer"], json!({ "type": "decline" }));
    let id = sent[0]["id"].as_str().expect("an id").to_owned();
    let _ = sock(&mut model, exit_ok(&id, Value::Null));
    assert!(model.secrets_answering.is_empty());
    // Saved: the card goes and the row says so.
    model.thread = Some(asked("saved"));
    let page = DomovoiApp.view(&model).thread.expect("the thread");
    assert!(page.secret_requests.is_empty());
    assert_eq!(page.items[0].title, "Secret saved: OpenAI API key");

    // A refusal names its reason (`SecretRequestError` has no message).
    model.thread = Some(asked("pending"));
    let sent = sent_requests(&run(&mut model, Event::AnswerSecret { item_id: "s1".into(), value: "v".into() }));
    let id = sent[0]["id"].as_str().expect("an id").to_owned();
    let failure = json!({ "_tag": "Exit", "requestId": id, "exit": { "_tag": "Failure", "cause": [{ "_tag": "Fail", "error": { "_tag": "SecretRequestError", "reason": "agent_stopped" } }] } });
    let _ = sock(&mut model, SocketEvent::Message { text: failure.to_string() });
    assert_eq!(model.toast.as_ref().map(|t| t.message.as_str()), Some("Secret: the agent stopped waiting for it."));
    assert!(!DomovoiApp.view(&model).thread.expect("the thread").secret_requests[0].sending);
}

/// The pull requests and issues linked to the open thread, for its header:
/// a stack's dismissed pull requests are left out.
#[test]
fn linked_pull_requests_and_issues_show_on_the_thread() {
    let (mut model, _) = connected();
    model.threads = vec![records::shell(json!({ "id": "t1", "pullRequests": [
        { "host": "github.com", "repository": "o/r", "number": 12, "url": "https://github.com/o/r/pull/12", "source": "agent", "linkedAt": "1",
          "snapshot": { "state": "open", "title": "Fix login", "headBranch": "fix", "baseBranch": "main", "isDraft": true, "updatedAt": null, "syncedAt": "1" }, "stack": null },
        { "host": "github.com", "repository": "o/r", "number": 9, "url": "https://github.com/o/r/pull/9", "source": "stack-dismissed", "linkedAt": "1", "snapshot": null, "stack": null },
    ], "issues": [
        { "tracker": "linear", "host": "linear.app", "scope": "ENG", "key": "abc", "projectId": "p1", "url": "https://linear.app/x/issue/ENG-123",
          "source": "agent", "linkedAt": "1", "displayKey": "ENG-123", "title": "Login fails" },
    ] }))];
    model.selected_thread = Some("t1".into());
    model.thread = Some(records::thread(json!({})));
    let links = DomovoiApp.view(&model).thread.expect("the thread").links;
    let shown: Vec<_> = links.iter().map(|l| (l.kind.as_str(), l.label.as_str(), l.title.as_str(), l.state.as_str(), l.url.as_str())).collect();
    assert_eq!(shown, [
        ("pull_request", "#12", "Fix login", "draft", "https://github.com/o/r/pull/12"),
        ("issue", "ENG-123", "Login fails", "", "https://linear.app/x/issue/ENG-123"),
    ]);
    assert_eq!(DomovoiApp.view(&model).sidebar.surface_threads[0].pull_request, "#12");
}

#[test]
fn mentions_search_files_and_complete_the_draft() {
    let (mut model, _) = connected();
    model.selected_project = Some("p1".into());
    model.projects = vec![records::project(json!({ "id": "p1", "title": "calc", "workspaceRoot": "/w/calc" }))];
    let sent = sent_requests(&run(&mut model, Event::SetDraft { text: "fix @cal".into() }));
    let search = sent.iter().find(|r| r["tag"] == "projects.searchEntries").expect("mention search");
    assert_eq!((search["payload"]["cwd"].as_str(), search["payload"]["query"].as_str()), (Some("/w/calc"), Some("cal")));
    let _ = sock(&mut model, exit_ok(search["id"].as_str().unwrap(), json!({ "entries": [{ "path": "src/calc.py", "kind": "file" }], "truncated": false })));
    let view = DomovoiApp.view(&model);
    assert_eq!(view.composer.mention.as_ref().unwrap().suggestions[0].name, "calc.py");
    let rev = view.composer.draft_revision;
    let _ = run(&mut model, Event::PickMention { path: String::new() });
    let view = DomovoiApp.view(&model);
    assert_eq!(view.composer.draft, "fix @src/calc.py ");
    assert!(view.composer.draft_revision > rev && view.composer.mention.is_none());
}

/// The git panel lists the worktrees and the project's scripts; a script
/// runs in a new terminal once it is up; a file is saved back.
#[test]
fn worktrees_scripts_and_saved_files() {
    let (mut model, _) = connected();
    working_thread(&mut model);
    model.projects = vec![records::project(json!({ "id": "p1", "title": "App", "workspaceRoot": "/w",
        "scripts": [{ "id": "test", "name": "Test", "command": "cargo test", "icon": "test", "runOnWorktreeCreate": false }] }))];
    model.panel.git.cwd = "/w".into();
    model.composer.branches_cwd = "/w".into();
    let sent = sent_requests(&run(&mut model, Event::RefreshGit));
    let refs = sent.iter().find(|r| r["tag"] == "vcs.listRefs").expect("the branches");
    let _ = sock(&mut model, exit_ok(refs["id"].as_str().unwrap(), json!({ "refs": [
        { "name": "main", "current": true, "isDefault": true, "worktreePath": "/w" },
        { "name": "feature", "current": false, "isDefault": false, "worktreePath": "/w-feature" },
        { "name": "old", "current": false, "isDefault": false, "worktreePath": null },
    ], "isRepo": true, "hasPrimaryRemote": false, "nextCursor": null, "totalCount": 3 })));
    let git = DomovoiApp.view(&model).panel.git;
    assert_eq!(git.worktrees.iter().map(|w| (w.branch.as_str(), w.path.as_str())).collect::<Vec<_>>(), [("feature", "/w-feature")], "the project's own folder is no extra worktree");
    assert_eq!(git.scripts.iter().map(|s| (s.id.as_str(), s.icon.as_str())).collect::<Vec<_>>(), [("test", "test")]);

    let sent = sent_requests(&run(&mut model, Event::CreateWorktree { base: "main".into(), branch: "fix".into() }));
    assert_eq!((sent[0]["tag"].as_str(), sent[0]["payload"]["refName"].as_str(), sent[0]["payload"]["newRefName"].as_str()), (Some("vcs.createWorktree"), Some("main"), Some("fix")));
    let sent = sent_requests(&run(&mut model, Event::RemoveWorktree { path: "/w-feature".into(), force: false }));
    assert_eq!((sent[0]["tag"].as_str(), sent[0]["payload"]["path"].as_str()), (Some("vcs.removeWorktree"), Some("/w-feature")));

    let sent = sent_requests(&run(&mut model, Event::RunScript { id: "test".into() }));
    let attach = sent.iter().find(|r| r["tag"] == "terminal.attach").expect("a terminal for the script");
    assert!(!sent.iter().any(|r| r["tag"] == "terminal.write"), "nothing typed before the terminal is up");
    let view = DomovoiApp.view(&model);
    assert_eq!(view.panel.terminal.tabs.last().unwrap().label, "Test");
    let effects = sock(&mut model, SocketEvent::Message { text: json!({ "_tag": "Chunk", "requestId": attach["id"], "values": [{ "type": "snapshot", "threadId": "t1", "terminalId": attach["payload"]["terminalId"],
        "snapshot": { "threadId": "t1", "terminalId": attach["payload"]["terminalId"], "cwd": "/w", "worktreePath": null, "status": "running", "pid": 1, "history": "", "exitCode": null, "exitSignal": null, "updatedAt": "2026-11-01T00:00:00.000Z" } }] }).to_string() });
    let typed = sent_requests(&effects);
    assert_eq!(typed.iter().find(|r| r["tag"] == "terminal.write").map(|r| r["payload"]["data"].clone()), Some(json!("cargo test\r")));

    model.panel.files.root = "/w".into();
    let sent = sent_requests(&run(&mut model, Event::SaveFile { path: "README.md".into(), text: "# App\n".into() }));
    assert_eq!((sent[0]["tag"].as_str(), sent[0]["payload"]["contents"].as_str()), (Some("projects.writeFile"), Some("# App\n")));
    assert!(sent.iter().any(|r| r["tag"] == "projects.readFile"), "then shown again");
}

/// The palette goes to a file, or searches the project's files for a line.
#[test]
fn the_palette_goes_to_a_file_and_searches_the_project() {
    let (mut model, _) = connected();
    model.selected_project = Some("p1".into());
    model.projects = vec![records::project(json!({ "id": "p1", "title": "calc", "workspaceRoot": "/w/calc" }))];
    let _ = run(&mut model, Event::OpenPalette);
    let _ = run(&mut model, Event::RunPaletteItem { id: "action:go-to-file".into() });
    let palette = DomovoiApp.view(&model).palette.unwrap();
    assert_eq!((palette.mode.as_str(), palette.placeholder.as_str()), ("files", "Go to file…"));
    let sent = sent_requests(&run(&mut model, Event::SetPaletteQuery { query: "calc".into() }));
    let search = sent.iter().find(|r| r["tag"] == "projects.searchEntries").expect("a file search");
    let _ = sock(&mut model, exit_ok(search["id"].as_str().unwrap(), json!({ "entries": [{ "path": "src/calc.py", "kind": "file" }, { "path": "src", "kind": "directory" }], "truncated": false })));
    let items = DomovoiApp.view(&model).palette.unwrap().items;
    assert_eq!(items.iter().map(|i| (i.id.as_str(), i.title.as_str(), i.subtitle.as_str())).collect::<Vec<_>>(), [("file:src/calc.py", "calc.py", "src")]);

    let _ = run(&mut model, Event::SetPaletteMode { mode: "content".into() });
    let sent = sent_requests(&run(&mut model, Event::SetPaletteQuery { query: "def add".into() }));
    let search = sent.iter().find(|r| r["tag"] == "projects.searchContents").expect("a content search");
    assert_eq!(search["payload"]["query"], "def add");
    let _ = sock(&mut model, exit_ok(search["id"].as_str().unwrap(), json!({ "matches": [{ "path": "src/calc.py", "lineNumber": 3, "lineContent": "  def add(a, b):", "matchRanges": [{ "start": 2, "end": 9 }] }], "truncated": false })));
    let items = DomovoiApp.view(&model).palette.unwrap().items;
    assert_eq!((items[0].title.as_str(), items[0].subtitle.as_str()), ("def add(a, b):", "src/calc.py:3"));
    let sent = sent_requests(&run(&mut model, Event::RunPaletteItem { id: String::new() }));
    assert!(DomovoiApp.view(&model).palette.is_none());
    assert!(sent.iter().any(|r| r["tag"] == "projects.readFile" && r["payload"]["relativePath"] == "src/calc.py"), "the file opens: {sent:?}");
}

#[test]
fn a_dismissed_mention_stays_closed_until_a_new_at() {
    let (mut model, _) = connected();
    model.selected_project = Some("p1".into());
    model.projects = vec![records::project(json!({ "id": "p1", "title": "calc", "workspaceRoot": "/w/calc" }))];
    let _ = run(&mut model, Event::SetDraft { text: "fix @cal".into() });
    assert!(DomovoiApp.view(&model).composer.mention.is_some());
    let _ = run(&mut model, Event::DismissMention);
    let view = DomovoiApp.view(&model);
    assert!(view.composer.mention.is_none());
    assert_eq!(view.composer.draft, "fix @cal", "the draft is left as typed");
    // Typing on after the dismissed `@` searches nothing.
    let sent = sent_requests(&run(&mut model, Event::SetDraft { text: "fix @calc".into() }));
    assert!(!sent.iter().any(|r| r["tag"] == "projects.searchEntries"));
    assert!(DomovoiApp.view(&model).composer.mention.is_none());
    // A new `@` opens the list again.
    let sent = sent_requests(&run(&mut model, Event::SetDraft { text: "fix @calc and @ma".into() }));
    assert!(sent.iter().any(|r| r["tag"] == "projects.searchEntries"));
    assert!(DomovoiApp.view(&model).composer.mention.is_some());
}

#[test]
fn terminals_render_streamed_output() {
    let (mut model, _) = connected();
    model.selected_thread = Some("t1".into());
    model.threads = vec![records::shell(json!({ "id": "t1", "projectId": "p1", "worktreePath": "/w" }))];
    let sent = sent_requests(&run(&mut model, Event::ShowPanel { tab: "terminal".into() }));
    let attach = sent.iter().find(|r| r["tag"] == "terminal.attach").expect("terminal attach");
    assert_eq!(attach["payload"]["cwd"], "/w");
    let id = attach["id"].as_str().unwrap().to_owned();
    let snapshot = json!({ "threadId": "t1", "terminalId": "term-1", "cwd": "/w", "worktreePath": null, "status": "running", "pid": 1, "history": "", "exitCode": null, "exitSignal": null, "label": "Terminal 1", "updatedAt": "1", "rows": 5, "cols": 30, "sequence": 0 });
    let _ = sock(&mut model, chunk(&id, &[json!({ "type": "snapshot", "snapshot": snapshot }), json!({ "type": "output", "threadId": "t1", "terminalId": "term-1", "sequence": 1, "data": "hello\r\n" })]));
    let screen = DomovoiApp.view(&model).panel.terminal.active.expect("screen");
    assert_eq!(screen.lines[0].spans[0].text, "hello");
    let sent = sent_requests(&run(&mut model, Event::TerminalKey { key: "char".into(), text: "c".into(), ctrl: true, alt: false }));
    assert_eq!((sent[0]["tag"].as_str(), sent[0]["payload"]["data"].as_str()), (Some("terminal.write"), Some("\u{3}")));
}

#[test]
fn reconnects_after_the_socket_drops() {
    let (mut model, _) = connected();
    let _ = sock(&mut model, SocketEvent::Closed { reason: "lost".into() });
    let view = DomovoiApp.view(&model);
    assert_eq!(view.connection.state, "reconnecting");
    assert!((2..=3).contains(&view.connection.reconnect_in_seconds), "2 s, give or take a quarter: {}", view.connection.reconnect_in_seconds);
    let mut shell = Shell::paired();
    let effects = shell.drive(&mut model, Event::Reconnect);
    assert!(socket_url(&effects).is_some(), "a new ticket, then the socket");
    let _ = run(&mut model, Event::Disconnect);
    assert!(run(&mut model, Event::NetworkChanged).is_empty(), "no automatic reconnect after a manual disconnect");
    let retry_gen = model.retry_gen;
    assert!(run(&mut model, Event::RetryTimer { retry_gen }).is_empty());
    // The user's Connect does.
    assert!(!run(&mut model, Event::Reconnect).is_empty());
    assert_eq!(DomovoiApp.view(&model).connection.state, "reconnecting");
}

/// A UnifiedPush endpoint (Android) registers with the server in use, at
/// once when connected and again on each connection; an empty one is forgotten.
#[test]
fn push_endpoints_register_with_the_server_in_use() {
    let mut shell = Shell::paired();
    shell.routes.insert("/api/domovoi/push/webpush".into(), (200, json!({ "registered": true })));
    let mut model = Model::default();
    let _ = shell.drive(&mut model, Event::PushEndpoint { endpoint: "https://ntfy.example/upX".into(), p256dh: "BKey".into(), auth: "Auth".into() });
    assert!(shell.requests.iter().all(|r| !r.url.ends_with("/api/domovoi/push/webpush")), "not before a connection");
    let _ = shell.drive(&mut model, Event::Connect { server_url: "http://h:1".into(), pairing_code: "CODE".into() });
    let sent = sent_requests(&sock(&mut model, SocketEvent::Opened));
    let lifecycle = sent.iter().find(|r| r["tag"] == "subscribeServerLifecycle").unwrap()["id"].as_str().unwrap().to_owned();
    let _ = shell.drive_sock(&mut model, chunk(&lifecycle, &[records::welcome(json!({ "environmentId": "srv-1", "label": "Mac" }))]));
    let reg = shell.requests.iter().find(|r| r.url == "http://h:1/api/domovoi/push/webpush").expect("registered once connected");
    assert_eq!(serde_json::from_slice::<Value>(&reg.body).unwrap(), json!({ "endpoint": "https://ntfy.example/upX", "p256dh": "BKey", "auth": "Auth" }));
    let before = shell.requests.len();
    let _ = shell.drive(&mut model, Event::PushEndpoint { endpoint: String::new(), p256dh: String::new(), auth: String::new() });
    assert!(model.push_endpoint.is_none());
    assert_eq!(shell.requests.len(), before, "nothing to register");
}

#[test]
fn push_tokens_register_and_notifications_open_their_server() {
    let mut shell = Shell::paired();
    shell.routes.insert("/api/domovoi/push/devices".into(), (200, json!({ "registered": true })));
    let mut model = Model::default();
    let _ = shell.drive(&mut model, Event::Connect { server_url: "http://h:1".into(), pairing_code: "CODE".into() });
    let sent = sent_requests(&sock(&mut model, SocketEvent::Opened));
    let home = model.active_env.clone().unwrap();

    // A token arriving while connected registers at once, with the bearer token.
    let token = "ab".repeat(32);
    let _ = shell.drive(&mut model, Event::PushToken { token: token.clone(), environment: "sandbox".into() });
    let reg = shell.requests.iter().find(|r| r.url == "http://h:1/api/domovoi/push/devices").expect("registered");
    assert_eq!(serde_json::from_slice::<Value>(&reg.body).unwrap(), json!({ "token": token, "environment": "sandbox" }));

    // The welcome names the server; it is saved with the environment and the token registered again.
    let lifecycle = sent.iter().find(|r| r["tag"] == "subscribeServerLifecycle").unwrap()["id"].as_str().unwrap().to_owned();
    let _ = shell.drive_sock(&mut model, chunk(&lifecycle, &[records::welcome(json!({ "environmentId": "srv-1", "label": "Mac" }))]));
    assert!(shell.store["environments"].contains("srv-1"));
    assert_eq!(shell.requests.iter().filter(|r| r.url.ends_with("/api/domovoi/push/devices")).count(), 2);

    // On another server, opening a push from srv-1 switches back and opens the thread.
    let _ = shell.drive(&mut model, Event::Connect { server_url: "http://other:2".into(), pairing_code: "CODE2".into() });
    assert_ne!(model.active_env.as_deref(), Some(home.as_str()));
    let effects = shell.drive(&mut model, Event::OpenNotification { environment_id: "srv-1".into(), thread_id: "t9".into() });
    assert_eq!(model.active_env.as_deref(), Some(home.as_str()));
    assert!(socket_url(&effects).unwrap().starts_with("ws://h:1/"));
    let sent = sent_requests(&sock(&mut model, SocketEvent::Opened));
    assert!(sent.iter().any(|r| r["tag"] == "orchestration.subscribeThread" && r["payload"]["threadId"] == "t9"));
}

#[test]
fn a_notification_opened_from_the_background_loads_its_thread() {
    let mut shell = Shell::paired();
    let mut model = Model::default();
    let _ = shell.drive(&mut model, Event::Connect { server_url: "http://h:1".into(), pairing_code: "CODE".into() });
    let sent = sent_requests(&sock(&mut model, SocketEvent::Opened));
    let lifecycle = sent.iter().find(|r| r["tag"] == "subscribeServerLifecycle").unwrap()["id"].as_str().unwrap().to_owned();
    let _ = shell.drive_sock(&mut model, chunk(&lifecycle, &[records::welcome(json!({ "environmentId": "srv-1" }))]));
    let _ = run(&mut model, Event::Suspend);
    let _ = sock(&mut model, SocketEvent::Closed { reason: String::new() });
    // iOS delivers the tap and the return to the foreground in either order.
    for order in [true, false] {
        let mut m = Model::default();
        std::mem::swap(&mut m, &mut model);
        let mut model2 = m;
        let mut effects = vec![];
        if order {
            effects.extend(shell.drive(&mut model2, Event::OpenNotification { environment_id: "srv-1".into(), thread_id: "t9".into() }));
            effects.extend(shell.drive(&mut model2, Event::Resume));
        } else {
            effects.extend(shell.drive(&mut model2, Event::Resume));
            effects.extend(shell.drive(&mut model2, Event::OpenNotification { environment_id: "srv-1".into(), thread_id: "t9".into() }));
        }
        assert!(socket_url(&effects).is_some(), "order {order}: reconnects");
        let sent = sent_requests(&sock(&mut model2, SocketEvent::Opened));
        assert!(sent.iter().any(|r| r["tag"] == "orchestration.subscribeThread" && r["payload"]["threadId"] == "t9"), "order {order}: {sent:?}");
        // Reset for the other order.
        let _ = run(&mut model2, Event::Suspend);
        let _ = sock(&mut model2, SocketEvent::Closed { reason: String::new() });
        model2.selected_thread = None;
        model = model2;
    }
}

// ---- held replies: a shell that answers later, or never ----

/// Runs `event` and keeps the command, so its requests can be answered later.
fn start(model: &mut Model, event: Event) -> (crux_core::Command<Effect, Event>, Vec<Effect>) {
    let mut cmd = DomovoiApp.update(event, model);
    let effects = read_by_the_server(cmd.effects().collect());
    (cmd, effects)
}

/// Takes the HTTP request whose URL ends with `path` out of `effects`.
fn http_to(effects: &mut Vec<Effect>, path: &str) -> Option<crux_core::Request<HttpRequest>> {
    let i = effects.iter().position(|e| matches!(e, Effect::Http(r) if r.operation.url.ends_with(path)))?;
    match effects.swap_remove(i) {
        Effect::Http(r) => Some(r),
        _ => None,
    }
}

/// Takes the tunnel start out of `effects`.
fn take_tunnel(effects: &mut Vec<Effect>) -> Option<crux_core::Request<TunnelOperation>> {
    let i = effects.iter().position(|e| matches!(e, Effect::Tunnel(r) if matches!(r.operation, TunnelOperation::Start { .. })))?;
    match effects.swap_remove(i) {
        Effect::Tunnel(r) => Some(r),
        _ => None,
    }
}

/// Answers a held HTTP request and feeds what it leads to through `shell`.
fn answer(shell: &mut Shell, model: &mut Model, cmd: &mut crux_core::Command<Effect, Event>, req: &mut crux_core::Request<HttpRequest>, status: u16, body: Value) -> Vec<Effect> {
    req.resolve(HttpResult::Ok(HttpResponse::status(status).json(body).build())).unwrap();
    let mut out = vec![];
    for event in cmd.events().collect::<Vec<_>>() {
        out.extend(shell.drive(model, event));
    }
    out
}

fn connected_to(shell: &mut Shell, model: &mut Model, url: &str, code: &str) {
    let effects = shell.drive(model, Event::Connect { server_url: url.into(), pairing_code: code.into() });
    assert!(socket_url(&effects).is_some(), "connected to {url}");
    let _ = sock(model, SocketEvent::Opened);
}

#[test]
fn a_late_reply_from_the_previous_server_changes_nothing() {
    let mut shell = Shell::paired();
    let mut model = Model::default();
    connected_to(&mut shell, &mut model, "http://a:1", "CODE");
    let a = model.active_env.clone().unwrap();
    // A drops; its retry asks for a ticket that is slow to come back.
    let _ = sock(&mut model, SocketEvent::Closed { reason: "lost".into() });
    let (mut cmd, mut effects) = start(&mut model, Event::Reconnect);
    let mut slow = http_to(&mut effects, "/websocket-ticket/rpc").expect("ticket requested");

    // Meanwhile the user picks server B, which connects.
    shell.routes.insert("/oauth/token".into(), (200, json!({ "access_token": "tok-b", "issued_token_type": "urn:ietf:params:oauth:token-type:access_token", "token_type": "Bearer", "expires_in": 100, "scope": "orchestration:read" })));
    connected_to(&mut shell, &mut model, "http://b:2", "CODE2");
    let b = model.active_env.clone().unwrap();
    assert_ne!(a, b);

    // A's answer arrives at last, refusing the token: B's sign-in is untouched.
    let effects = answer(&mut shell, &mut model, &mut cmd, &mut slow, 401, json!({}));
    assert!(effects.is_empty(), "{effects:?}");
    assert_eq!(shell.store.get(&format!("token:{b}")).map(String::as_str), Some("tok-b"));
    assert_eq!(shell.store.get(&format!("token:{a}")).map(String::as_str), Some("tok-1"), "nor is A's");
    assert_eq!(DomovoiApp.view(&model).connection.state, "connected");
}

#[test]
fn a_late_ticket_opens_no_socket_and_a_late_tunnel_is_ignored() {
    let mut shell = Shell::paired();
    let mut model = Model::default();
    connected_to(&mut shell, &mut model, "http://a:1", "CODE");
    let _ = sock(&mut model, SocketEvent::Closed { reason: "lost".into() });
    let (mut cmd, mut effects) = start(&mut model, Event::Reconnect);
    let mut slow = http_to(&mut effects, "/websocket-ticket/rpc").unwrap();
    // The user forgets the server before the ticket arrives.
    let active = model.active_env.clone().unwrap();
    let _ = shell.drive(&mut model, Event::ForgetEnvironment { id: active });
    let effects = answer(&mut shell, &mut model, &mut cmd, &mut slow, 200, json!({ "ticket": "late" }));
    assert!(socket_url(&effects).is_none(), "no socket to nowhere");

    // A remote server's tunnel comes up after the user switched to a local one.
    let (mut cmd, mut effects) = start(&mut model, Event::ConnectRemote { link: "domovoi://connect?ticket=endpointabc&code=C".into() });
    let mut tunnel = take_tunnel(&mut effects).unwrap();
    connected_to(&mut shell, &mut model, "http://b:2", "CODE2");
    tunnel.resolve(TunnelOutput::Started { port: 4242 }).unwrap();
    for event in cmd.events().collect::<Vec<_>>() {
        assert!(shell.drive(&mut model, event).is_empty());
    }
    assert_eq!(model.server_url, "http://b:2", "the old tunnel's port is not used");
    assert!(!shell.requests.iter().any(|r| r.url.contains("127.0.0.1:4242")), "no request went through it");
}

#[test]
fn reconnect_attempts_never_overlap() {
    let mut shell = Shell::paired();
    let mut model = Model::default();
    connected_to(&mut shell, &mut model, "http://a:1", "CODE");
    let _ = sock(&mut model, SocketEvent::Closed { reason: "lost".into() });
    let (_cmd, effects) = start(&mut model, Event::Reconnect);
    assert!(effects.iter().any(|e| matches!(e, Effect::Http(_))));
    // The timer fires again, the network changes, the user asks: all wait.
    let retry_gen = model.retry_gen;
    assert!(run(&mut model, Event::RetryTimer { retry_gen }).is_empty());
    assert!(run(&mut model, Event::Reconnect).is_empty());
    assert!(run(&mut model, Event::NetworkChanged).is_empty());
    let view = DomovoiApp.view(&model);
    assert_eq!(view.connection.reconnect_in_seconds, 0, "no countdown while trying");
    assert!(view.connection.attempting);
}

#[test]
fn retries_back_off_and_reset_only_once_the_server_answers() {
    let mut shell = Shell::paired();
    let mut model = Model::default();
    connected_to(&mut shell, &mut model, "http://a:1", "CODE");
    let _ = sock(&mut model, SocketEvent::Closed { reason: "lost".into() });
    shell.routes.insert("/api/auth/websocket-ticket/rpc".into(), (503, json!({})));
    let mut delays = vec![DomovoiApp.view(&model).connection.reconnect_in_seconds];
    for _ in 0..5 {
        let retry_gen = model.retry_gen;
        let _ = shell.drive(&mut model, Event::RetryTimer { retry_gen });
        delays.push(DomovoiApp.view(&model).connection.reconnect_in_seconds);
    }
    for (delay, base) in delays.iter().zip([2u32, 4, 8, 16, 30, 30]) {
        let (low, high) = ((base as f64 * 0.75).round() as u32, (base as f64 * 1.25).round() as u32);
        assert!((low..=high).contains(delay), "{delays:?}");
    }
    // A server that takes the socket and drops it at once keeps backing off.
    shell.routes.insert("/api/auth/websocket-ticket/rpc".into(), (200, ticket_answer("t")));
    let retry_gen = model.retry_gen;
    let _ = shell.drive(&mut model, Event::RetryTimer { retry_gen });
    let _ = sock(&mut model, SocketEvent::Opened);
    let _ = sock(&mut model, SocketEvent::Closed { reason: "dropped".into() });
    assert!(DomovoiApp.view(&model).connection.reconnect_in_seconds >= 23, "still backing off");
    // Once it sends its welcome, the next drop starts over at 2 s.
    let retry_gen = model.retry_gen;
    let _ = shell.drive(&mut model, Event::RetryTimer { retry_gen });
    let sent = sent_requests(&sock(&mut model, SocketEvent::Opened));
    let lifecycle = sent.iter().find(|r| r["tag"] == "subscribeServerLifecycle").unwrap()["id"].as_str().unwrap().to_owned();
    let _ = shell.drive_sock(&mut model, chunk(&lifecycle, &[records::welcome(json!({ "environmentId": "srv" }))]));
    let _ = sock(&mut model, SocketEvent::Closed { reason: "lost".into() });
    assert!((2..=3).contains(&DomovoiApp.view(&model).connection.reconnect_in_seconds));
}

#[test]
fn a_server_that_refuses_this_device_for_good_is_not_retried() {
    let mut shell = Shell::paired();
    let mut model = Model::default();
    connected_to(&mut shell, &mut model, "http://a:1", "CODE");
    let _ = sock(&mut model, SocketEvent::Closed { reason: "lost".into() });
    shell.routes.insert("/api/auth/websocket-ticket/rpc".into(), (404, json!({ "error": "not_found" })));
    let _ = shell.drive(&mut model, Event::Reconnect);
    let view = DomovoiApp.view(&model);
    assert_eq!(view.connection.state, "failed");
    assert!(view.connection.server_changed && view.connection.needs_pairing);
    assert!(view.connection.error.unwrap().contains("pair it again"));
    assert_eq!(view.connection.reconnect_in_seconds, 0);
    assert!(run(&mut model, Event::Reconnect).is_empty(), "no retry loop");
    // Pairing again clears it.
    shell.routes.insert("/api/auth/websocket-ticket/rpc".into(), (200, ticket_answer("t")));
    connected_to(&mut shell, &mut model, "http://a:1", "NEWCODE");
    assert!(!DomovoiApp.view(&model).connection.server_changed);
}

#[test]
fn going_to_the_background_mid_connect_abandons_the_attempt() {
    let mut shell = Shell::paired();
    let mut model = Model::default();
    connected_to(&mut shell, &mut model, "http://a:1", "CODE");
    let _ = sock(&mut model, SocketEvent::Closed { reason: "lost".into() });
    let (mut cmd, mut effects) = start(&mut model, Event::Reconnect);
    let mut slow = http_to(&mut effects, "/websocket-ticket/rpc").unwrap();
    let _ = run(&mut model, Event::Suspend);
    let effects = answer(&mut shell, &mut model, &mut cmd, &mut slow, 200, json!({ "ticket": "late" }));
    assert!(socket_url(&effects).is_none(), "no socket opened in the background");
    let effects = shell.drive(&mut model, Event::Resume);
    assert!(socket_url(&effects).is_some(), "a fresh attempt on return");

    // Backgrounded while pairing: the sign-in the code bought is kept.
    let mut model = Model::default();
    let (mut cmd, mut effects) = start(&mut model, Event::Connect { server_url: "http://b:2".into(), pairing_code: "CODE".into() });
    let mut exchange = http_to(&mut effects, "/oauth/token").unwrap();
    let _ = run(&mut model, Event::Suspend);
    let _ = answer(&mut shell, &mut model, &mut cmd, &mut exchange, 200, json!({ "access_token": "tok-late", "issued_token_type": "urn:ietf:params:oauth:token-type:access_token", "token_type": "Bearer", "expires_in": 100, "scope": "orchestration:read" }));
    let env = model.active_env.clone().unwrap();
    assert_eq!(shell.store.get(&format!("token:{env}")).map(String::as_str), Some("tok-late"));
    let effects = shell.drive(&mut model, Event::Resume);
    assert!(socket_url(&effects).is_some(), "connects with it on return");
}

#[test]
fn a_used_code_for_a_paired_server_keeps_the_saved_sign_in() {
    let mut shell = Shell::paired();
    let mut model = Model::default();
    connected_to(&mut shell, &mut model, "http://a:1", "CODE");
    // The same QR code scanned again: the server refuses it, the device carries on.
    shell.routes.insert("/oauth/token".into(), (401, json!({})));
    let effects = shell.drive(&mut model, Event::Connect { server_url: "http://a:1".into(), pairing_code: "CODE".into() });
    assert!(socket_url(&effects).is_some(), "connected with the saved token");
    assert!(DomovoiApp.view(&model).toast.unwrap().message.contains("still signed in"));
    // A server this device never paired with still says the code was rejected.
    let _ = shell.drive(&mut model, Event::Connect { server_url: "http://c:3".into(), pairing_code: "BAD".into() });
    assert!(DomovoiApp.view(&model).connection.error.unwrap().contains("rejected"));
}

#[test]
fn forgetting_a_connected_server_signs_this_device_out_there() {
    let mut shell = Shell::paired();
    shell.routes.insert("/api/domovoi/session/end".into(), (200, json!({ "signedOut": true })));
    let mut model = Model::default();
    connected_to(&mut shell, &mut model, "http://a:1", "CODE");
    let id = model.active_env.clone().unwrap();
    let _ = shell.drive(&mut model, Event::ForgetEnvironment { id: id.clone() });
    let end = shell.requests.iter().find(|r| r.url == "http://a:1/api/domovoi/session/end").expect("signed out first");
    assert!(end.headers.iter().any(|h| h.name.eq_ignore_ascii_case("authorization") && h.value == "Bearer tok-1"));
    assert!(model.environments.is_empty() && model.active_env.is_none());
    assert!(!shell.store.contains_key(&format!("token:{id}")));
    assert_eq!(DomovoiApp.view(&model).connection.state, "disconnected");
    // Unreachable (here: a 404 from an older server), it is forgotten all the same.
    let mut shell = Shell::paired();
    let mut model = Model::default();
    connected_to(&mut shell, &mut model, "http://a:1", "CODE");
    let id = model.active_env.clone().unwrap();
    let _ = shell.drive(&mut model, Event::ForgetEnvironment { id });
    assert!(model.environments.is_empty());
}

#[test]
fn notifications_open_the_right_server_or_nothing() {
    let mut shell = Shell::paired();
    let mut model = Model::default();
    connected_to(&mut shell, &mut model, "http://a:1", "CODE");
    let home = model.active_env.clone().unwrap();
    // A push from a server this device forgot: nothing happens.
    assert!(run(&mut model, Event::OpenNotification { environment_id: "gone".into(), thread_id: "t1".into() }).is_empty());
    assert_eq!(model.active_env.as_deref(), Some(home.as_str()));

    // One server saved twice, by address and by link: the link works from anywhere.
    model.environments[0].server_id = "srv".into();
    model.environments.push(domovoi_core::model::Environment { id: "remote-entry".into(), label: "Mac".into(), ticket: "endpointabc".into(), server_id: "srv".into(), ..Default::default() });
    shell.store.insert("token:remote-entry".into(), "tok-r".into());
    connected_to(&mut shell, &mut model, "http://other:2", "CODE2");
    let effects = shell.drive(&mut model, Event::OpenNotification { environment_id: "srv".into(), thread_id: "t2".into() });
    assert_eq!(model.active_env.as_deref(), Some("remote-entry"));
    assert!(effects.iter().any(|e| matches!(e, Effect::Tunnel(r) if matches!(r.operation, TunnelOperation::Start { .. }))));

    // A tap for the server being connected to does not start over.
    let effects = run(&mut model, Event::OpenNotification { environment_id: "srv".into(), thread_id: "t3".into() });
    assert!(!effects.iter().any(|e| matches!(e, Effect::Tunnel(r) if r.operation == TunnelOperation::Stop)), "no teardown");
    assert_eq!(model.selected_thread.as_deref(), Some("t3"));
}

#[test]
fn links_and_taps_at_a_cold_start_wait_for_the_saved_servers() {
    let mut shell = Shell::paired();
    let mut model = Model::default();
    connected_to(&mut shell, &mut model, "http://a:1", "CODE");
    let saved = model.active_env.clone().unwrap();
    // A fresh process: the link that launched it arrives before the saved list loads.
    let mut model = Model::default();
    let (mut cmd, mut effects) = start(&mut model, Event::Launch { surface: "mobile".into(), device_label: String::new(), device_os: String::new() });
    assert!(run(&mut model, Event::ConnectRemote { link: "domovoi://connect?ticket=endpointnew&code=C&name=New".into() }).is_empty(), "deferred");
    let Some(Effect::Store(load)) = effects.iter_mut().find(|e| matches!(e, Effect::Store(_))) else { panic!("loads environments") };
    load.resolve(StoreOutput::Value { value: shell.store.get("environments").cloned() }).unwrap();
    let mut queue: VecDeque<Event> = cmd.events().collect();
    while let Some(event) = queue.pop_front() {
        let _ = shell.drive(&mut model, event);
    }
    // Both servers are there, and the link's server is the one connecting.
    assert_eq!(model.environments.len(), 2, "{:?}", model.environments);
    assert!(model.environments.iter().any(|e| e.id == saved));
    assert_eq!(model.active_environment().unwrap().label, "New");
    // It is saved once this device has signed in to it, not before.
    assert!(!shell.store["environments"].contains("endpointnew") && shell.store["environments"].contains("http://a:1"));
    assert_eq!(shell.store["activeEnvironment"], saved);
    let _ = shell.drive_tunnel(&mut model, TunnelOutput::Started { port: 4000 });
    assert!(shell.store["environments"].contains("endpointnew") && shell.store["environments"].contains("http://a:1"));
    assert_eq!(Some(&shell.store["activeEnvironment"]), model.active_env.as_ref());
}

#[test]
fn servers_can_be_renamed_and_reordered() {
    let mut shell = Shell::paired();
    let mut model = Model::default();
    connected_to(&mut shell, &mut model, "http://a:1", "CODE");
    connected_to(&mut shell, &mut model, "http://b:2", "CODE2");
    let (a, b) = (model.environments[0].id.clone(), model.environments[1].id.clone());
    let _ = shell.drive(&mut model, Event::RenameEnvironment { id: a.clone(), label: "  Studio Mac ".into() });
    let _ = shell.drive(&mut model, Event::RenameEnvironment { id: a.clone(), label: "   ".into() });
    assert_eq!(model.environments[0].label, "Studio Mac");
    let _ = shell.drive(&mut model, Event::MoveEnvironment { id: b.clone(), delta: -5 });
    assert_eq!(model.environments.iter().map(|e| e.id.as_str()).collect::<Vec<_>>(), [b.as_str(), a.as_str()]);
    assert!(shell.store["environments"].find(&b).unwrap() < shell.store["environments"].find(&a).unwrap(), "saved in the new order");
}

#[test]
fn a_newer_entry_for_the_same_server_replaces_the_old_one() {
    let mut shell = Shell::paired();
    let mut model = Model::default();
    model.environments.push(domovoi_core::model::Environment { id: "old".into(), label: "Mac".into(), ticket: "endpointold".into(), server_id: "srv".into(), ..Default::default() });
    shell.store.insert("token:old".into(), "tok-old".into());
    // Its key changed (remote access reset): a new link, same server.
    let _ = shell.drive(&mut model, Event::ConnectRemote { link: "domovoi://connect?ticket=endpointnew&code=C".into() });
    let _ = shell.drive_tunnel(&mut model, TunnelOutput::Started { port: 4242 });
    let sent = sent_requests(&sock(&mut model, SocketEvent::Opened));
    let lifecycle = sent.iter().find(|r| r["tag"] == "subscribeServerLifecycle").unwrap()["id"].as_str().unwrap().to_owned();
    let _ = shell.drive_sock(&mut model, chunk(&lifecycle, &[records::welcome(json!({ "environmentId": "srv" }))]));
    assert_eq!(model.environments.len(), 1);
    assert_eq!(model.environments[0].ticket, "endpointnew");
    assert!(!shell.store.contains_key("token:old"));
}

#[test]
fn a_tunnel_that_lost_its_port_is_started_again() {
    let mut shell = Shell::paired();
    let mut model = Model::default();
    let _ = shell.drive(&mut model, Event::ConnectRemote { link: "domovoi://connect?ticket=endpointabc&code=C".into() });
    let _ = shell.drive_tunnel(&mut model, TunnelOutput::Started { port: 4242 });
    let _ = sock(&mut model, SocketEvent::Opened);
    // iOS took the listening socket away while the app was suspended.
    let effects = tun(&mut model, TunnelOutput::Status { connected: true, path: "direct".into(), rtt_ms: 5, error: String::new(), listening: false });
    assert!(effects.iter().any(|e| matches!(e, Effect::Socket(r) if r.operation == SocketOperation::Close)));
    assert_eq!(DomovoiApp.view(&model).connection.state, "reconnecting");
    let mut effects = shell.drive(&mut model, Event::Reconnect);
    assert!(tunnel_start(&mut effects).is_some(), "a new tunnel");
}

fn exit_fail(id: &str, error: Value) -> SocketEvent {
    SocketEvent::Message {
        text: json!({ "_tag": "Exit", "requestId": id, "exit": { "_tag": "Failure", "cause": [{ "_tag": "Fail", "error": error }] } }).to_string(),
    }
}

#[test]
fn the_import_picker_scans_then_imports_each_folder() {
    let (mut model, _) = connected();
    model.now_ms = 1_794_700_000_000; // 2026-11-14
    model.shown_ms = model.now_ms;
    let sent = sent_requests(&run(&mut model, Event::ScanAgentSessions));
    assert_eq!(sent[0]["tag"], "agentSessions.scan");
    assert!(DomovoiApp.view(&model).agent_sessions.scanning);
    let candidates = json!({ "candidates": [
        { "path": "/w/calc", "title": "calc", "sources": ["claudeAgent", "codex"], "threadCount": 3, "lastActiveAt": "2026-11-13T10:00:00.000Z", "alreadyImported": false, "git": null },
        { "path": "/w/old", "title": "old", "sources": ["claudeAgent"], "threadCount": 0, "lastActiveAt": "2026-08-01T10:00:00.000Z", "alreadyImported": false, "git": null },
        { "path": "/w/site", "title": "site", "sources": ["codex"], "threadCount": 1, "lastActiveAt": "2026-11-10T10:00:00.000Z", "alreadyImported": true, "projectId": "p-site", "git": null },
    ], "scannedAt": "2026-11-14T00:00:00.000Z" });
    let _ = sock(&mut model, exit_ok(sent[0]["id"].as_str().unwrap(), candidates));
    let view = DomovoiApp.view(&model).agent_sessions;
    assert!(view.scanned && !view.scanning);
    assert_eq!(view.available, 1, "only calc has something new to import");
    let calc = &view.candidates[0];
    assert_eq!((calc.sources.as_str(), calc.detail.as_str(), calc.recent), ("Claude Code · Codex", "3 sessions · 1d ago", true));
    assert!(!view.candidates[1].recent && view.candidates[2].already_imported);

    // calc gets a project first; site has one already.
    let sent = sent_requests(&run(&mut model, Event::ImportAgentSessions { paths: vec!["/w/calc".into(), "/w/site".into()] }));
    assert_eq!((sent[0]["tag"].as_str(), sent[0]["payload"]["type"].as_str()), (Some("projects.mutate"), Some("project.create")));
    assert_eq!((sent[0]["payload"]["workspaceRoot"].as_str(), sent[0]["payload"]["title"].as_str()), (Some("/w/calc"), Some("calc")));
    let project_id = sent[0]["payload"]["projectId"].as_str().unwrap().to_owned();
    assert!(DomovoiApp.view(&model).agent_sessions.importing);
    let sent = sent_requests(&sock(&mut model, exit_ok(sent[0]["id"].as_str().unwrap(), json!({ "sequence": 1 }))));
    assert_eq!(sent[0]["tag"], "agentSessions.import");
    assert_eq!(sent[0]["payload"], json!({ "projectId": project_id, "expectedWorkspaceRoot": "/w/calc" }));
    let sent = sent_requests(&sock(&mut model, exit_ok(sent[0]["id"].as_str().unwrap(), json!({ "importedCount": 3, "skippedCount": 0 }))));
    assert_eq!(sent[0]["payload"], json!({ "projectId": "p-site", "expectedWorkspaceRoot": "/w/site" }));
    let failed = exit_fail(sent[0]["id"].as_str().unwrap(), json!({ "_tag": "AgentSessionScanError", "operation": "read-projects", "cause": "disk on fire" }));
    let sent = sent_requests(&sock(&mut model, failed));
    assert_eq!(sent[0]["tag"], "agentSessions.scan", "the picker refreshes after an import");
    let view = DomovoiApp.view(&model);
    assert!(!view.agent_sessions.importing);
    assert_eq!(view.toast.unwrap().message, "Imported 3 sessions from 2 folders. Not imported: site (disk on fire)");
    assert_ne!(view.route, "new-thread", "an import does not open the composer");
}

#[test]
fn the_folder_browser_walks_the_servers_folders() {
    let (mut model, _) = connected();
    let sent = sent_requests(&run(&mut model, Event::BrowseFolders { path: String::new() }));
    assert_eq!((sent[0]["tag"].as_str(), sent[0]["payload"]["partialPath"].as_str()), (Some("filesystem.browse"), Some("~/")));
    let home = json!({ "parentPath": "/Users/me", "entries": [{ "name": "code", "fullPath": "/Users/me/code" }] });
    let first = sent[0]["id"].as_str().unwrap().to_owned();
    // A newer listing wins over a late one.
    let sent = sent_requests(&run(&mut model, Event::BrowseFolders { path: "/".into() }));
    assert_eq!(sent[0]["payload"]["partialPath"], "/");
    let _ = sock(&mut model, exit_ok(&first, home.clone()));
    assert!(DomovoiApp.view(&model).folder_browser.loading, "the late listing is ignored");
    let _ = sock(&mut model, exit_ok(sent[0]["id"].as_str().unwrap(), json!({ "parentPath": "/", "entries": [{ "name": "Users", "fullPath": "/Users" }] })));
    let view = DomovoiApp.view(&model).folder_browser;
    assert_eq!((view.path.as_str(), view.parent_path.as_str(), view.entries.len()), ("/", "", 1));

    let sent = sent_requests(&run(&mut model, Event::BrowseFolders { path: "/Users/me/".into() }));
    assert_eq!(sent[0]["payload"]["partialPath"], "/Users/me/");
    let _ = sock(&mut model, exit_ok(sent[0]["id"].as_str().unwrap(), home));
    model.projects = vec![records::project(json!({ "id": "p1", "title": "me", "workspaceRoot": "/Users/me" }))];
    let view = DomovoiApp.view(&model).folder_browser;
    assert_eq!((view.path.as_str(), view.parent_path.as_str()), ("/Users/me", "/Users"));
    assert_eq!(view.entries[0].full_path, "/Users/me/code");
    assert!(view.is_project);

    let sent = sent_requests(&run(&mut model, Event::BrowseFolders { path: "/nope".into() }));
    let missing = json!({ "_tag": "FilesystemBrowseError", "partialPath": "/nope/", "message": "Directory does not exist.", "pathMissing": true });
    let _ = sock(&mut model, exit_fail(sent[0]["id"].as_str().unwrap(), missing));
    assert_eq!(DomovoiApp.view(&model).folder_browser.error.as_deref(), Some("Directory does not exist."));
}

#[test]
fn imported_sessions_show_inside_their_project() {
    let (mut model, _) = connected();
    model.now_ms = 1_794_700_000_000;
    model.shown_ms = model.now_ms;
    model.projects = vec![records::project(json!({ "id": "p1", "title": "calc", "workspaceRoot": "/w/calc" }))];
    let thread = |id: &str, updated: &str, settled: bool| {
        records::shell(json!({ "id": id, "projectId": "p1", "title": id, "updatedAt": updated, "deletedAt": null, "archivedAt": null,
            "settledOverride": if settled { "settled" } else { "" }, "settledAt": if settled { json!(updated) } else { json!(null) } }))
    };
    model.threads = (0..7)
        .map(|i| thread(&format!("import:claudeAgent:s{i}"), &format!("2026-11-0{}T10:00:00.000Z", i + 1), true))
        .chain([thread("import:codex:c1", "2026-11-09T10:00:00.000Z", true), thread("t5", "2026-11-12T10:00:00.000Z", false)])
        .collect();
    model.sidebar_layout.grouping = Grouping::Project;
    let p = &DomovoiApp.view(&model).sidebar.projects[0];
    assert_eq!(p.threads.len(), 1);
    assert_eq!((p.threads[0].id.as_str(), p.threads[0].source.as_str()), ("t5", ""));
    assert_eq!((p.earlier_count, p.earlier.len()), (8, 5));
    assert_eq!((p.earlier[0].id.as_str(), p.earlier[0].source.as_str()), ("import:codex:c1", "Codex"));
    assert_eq!(p.earlier[1].source, "Claude Code");
    let _ = run(&mut model, Event::ShowAllThreads { project_id: "p1".into() });
    assert_eq!(DomovoiApp.view(&model).sidebar.projects[0].earlier.len(), 8);
}

/// A thread whose only turn failed before the provider started.
fn failed_thread(model: &mut Model) {
    model.selected_thread = Some("t1".into());
    model.route = "thread".into();
    model.threads = vec![records::shell(json!({ "id": "t1", "projectId": "p1", "title": "Explore", "modelSelection": { "instanceId": "claudeAgent", "model": "claude-haiku-5-5" } }))];
    let item = |id: &str, ty: &str, extra: Value| {
        let mut v = json!({ "id": id, "threadId": "t1", "runId": "r1", "type": ty, "status": "completed" });
        v.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        v
    };
    model.thread = Some(records::thread(json!({
        "thread": { "id": "t1" },
        "runs": [{ "id": "r1", "status": "failed", "ordinal": 1 }],
        "messages": [{ "id": "m1", "role": "user", "text": "Explore the passwords" }],
        "turnItems": [
            item("i1", "user_message", json!({ "messageId": "m1" })),
            item("i2", "error", json!({ "failure": { "message": "Claude Code isn't installed on the server" } })),
            item("i3", "checkpoint", json!({ "checkpointId": "c1", "files": [] })),
        ],
    })));
}

#[test]
fn a_provider_that_cannot_run_blocks_sending_with_its_reason() {
    let (mut model, sent) = connected();
    providers_snapshot(&mut model, &sent);
    let config_id = sent.iter().find(|r| r["tag"] == "subscribeServerConfig").unwrap()["id"].as_str().unwrap().to_owned();
    let broken = json!([{ "instanceId": "claudeAgent", "displayName": "Claude", "status": "error", "enabled": true, "installed": false,
        "message": "Claude Code isn't installed on the server.", "models": [{ "slug": "claude-haiku-5-5", "name": "Haiku 5.5", "isDefault": true }] }]);
    let broken: Vec<Value> = broken.as_array().unwrap().iter().cloned().map(records::provider_json).collect();
    let _ = sock(&mut model, chunk(&config_id, &[json!({ "version": 1, "type": "providerStatuses", "payload": { "providers": broken } })]));
    failed_thread(&mut model);
    let _ = run(&mut model, Event::SetDraft { text: "again".into() });
    let view = DomovoiApp.view(&model);
    assert_eq!(view.composer.blocked_reason, "Claude Code isn't installed on the server.");
    assert!(!view.composer.can_send);
    let effects = run(&mut model, Event::Send);
    assert!(sent_requests(&effects).iter().all(|r| r["tag"] != "orchestration.dispatchCommand"), "nothing is sent");
    assert_eq!(DomovoiApp.view(&model).toast.unwrap().message, "Claude Code isn't installed on the server.");
    assert_eq!(DomovoiApp.view(&model).composer.draft, "again", "the draft is kept");

    // Check Again asks the server to look for the tool again.
    let sent = sent_requests(&run(&mut model, Event::RefreshProviders));
    assert!(sent.iter().any(|r| r["tag"] == "server.refreshProviders"));
}

#[test]
fn a_failed_turn_hides_its_empty_change_card_and_can_be_retried() {
    let (mut model, sent) = connected();
    providers_snapshot(&mut model, &sent);
    failed_thread(&mut model);
    let page = DomovoiApp.view(&model).thread.unwrap();
    let kinds: Vec<&str> = page.items.iter().map(|i| i.kind.as_str()).collect();
    assert_eq!(kinds, ["user", "error"], "no \"No file changes\" card for a turn that failed");
    assert!(page.items[1].can_retry);
    assert!(!page.items[0].can_retry);

    let sent = sent_requests(&run(&mut model, Event::Retry));
    let dispatch = sent.iter().find(|r| r["payload"]["type"] == "message.dispatch").expect("the message is sent again");
    assert_eq!(dispatch["payload"]["text"], "Explore the passwords");
    assert_eq!(dispatch["payload"]["threadId"], "t1");
}

#[test]
fn a_launch_cut_off_by_a_dropped_connection_is_sent_again_on_reconnect() {
    let (mut model, sent) = connected();
    providers_snapshot(&mut model, &sent);
    model.selected_project = Some("p1".into());
    model.route = "new-thread".into();
    let _ = run(&mut model, Event::SetDraft { text: "Explore the passwords".into() });
    let sent = sent_requests(&run(&mut model, Event::Send));
    let first = sent.iter().find(|r| r["tag"] == "orchestration.launchThread").expect("launch").clone();
    assert!(DomovoiApp.view(&model).composer.sending);

    // The phone loses the server before it answers.
    let _ = sock(&mut model, SocketEvent::Closed { reason: "lost".into() });
    assert!(DomovoiApp.view(&model).composer.sending, "still on its way");

    // Back: the same launch, same command id, so the server can't run it twice.
    let sent = sent_requests(&sock(&mut model, SocketEvent::Opened));
    let again = sent.iter().find(|r| r["tag"] == "orchestration.launchThread").expect("sent again");
    assert_eq!(again["payload"]["commandId"], first["payload"]["commandId"]);
    let tid = again["payload"]["threadId"].as_str().unwrap().to_owned();
    let _ = sock(&mut model, exit_ok(again["id"].as_str().unwrap(), json!({ "threadId": tid })));
    assert_eq!(model.selected_thread.as_deref(), Some(tid.as_str()));
    assert!(!DomovoiApp.view(&model).composer.sending);
    assert!(model.composer.outbox.is_empty());
}

#[test]
fn a_send_the_server_never_answers_comes_back_to_the_composer() {
    let (mut model, sent) = connected();
    providers_snapshot(&mut model, &sent);
    model.selected_project = Some("p1".into());
    model.route = "new-thread".into();
    let _ = run(&mut model, Event::Tick { now_ms: 1_000 });
    let _ = run(&mut model, Event::SetDraft { text: "Explore the passwords".into() });
    let _ = run(&mut model, Event::Send);
    assert_eq!(DomovoiApp.view(&model).composer.draft, "");

    let _ = run(&mut model, Event::Tick { now_ms: 30_000 });
    assert!(DomovoiApp.view(&model).composer.sending, "not yet");
    let _ = run(&mut model, Event::Tick { now_ms: 1_000 + domovoi_core::update::SEND_TIMEOUT_MS });
    let view = DomovoiApp.view(&model);
    assert!(!view.composer.sending);
    assert_eq!(view.composer.draft, "Explore the passwords", "the text is back");
    assert!(view.toast.unwrap().message.contains("didn't answer"));
}

#[test]
fn a_launch_the_server_refuses_gives_the_text_back() {
    let (mut model, sent) = connected();
    providers_snapshot(&mut model, &sent);
    model.selected_project = Some("p1".into());
    model.route = "new-thread".into();
    let _ = run(&mut model, Event::SetDraft { text: "Explore the passwords".into() });
    let sent = sent_requests(&run(&mut model, Event::Send));
    let launch = sent.iter().find(|r| r["tag"] == "orchestration.launchThread").unwrap();
    let failure = SocketEvent::Message { text: json!({ "_tag": "Exit", "requestId": launch["id"], "exit": { "_tag": "Failure", "cause": [{ "_tag": "Fail", "error": { "_tag": "OrchestrationV2ThreadLaunchError", "message": "Unknown project." } }] } }).to_string() };
    assert_eq!(DomovoiApp.view(&model).route, "thread", "shown while it was being started");
    let _ = sock(&mut model, failure);
    let view = DomovoiApp.view(&model);
    assert_eq!(view.route, "new-thread", "back where it was typed");
    assert!(view.sidebar.surface_threads.iter().all(|r| r.status_label != "Starting"), "no longer listed");
    assert!(!view.composer.sending);
    assert_eq!(view.composer.draft, "Explore the passwords");
    assert!(view.toast.unwrap().message.contains("Unknown project."));
}

#[test]
fn a_fast_failure_waits_its_backoff_and_old_timers_do_nothing() {
    let mut shell = Shell::paired();
    let mut model = Model::default();
    connected_to(&mut shell, &mut model, "http://a:1", "CODE");
    let _ = sock(&mut model, SocketEvent::Closed { reason: "lost".into() });
    let first = model.retry_gen;
    shell.routes.insert("/api/auth/websocket-ticket/rpc".into(), (503, json!({})));
    // The timer fires; the attempt fails at once and schedules the next retry.
    let _ = shell.drive(&mut model, Event::RetryTimer { retry_gen: first });
    let view = DomovoiApp.view(&model);
    assert!(view.connection.retry_gen > first, "a new retry, which the shell times afresh");
    assert!((3..=5).contains(&view.connection.reconnect_in_seconds), "{}", view.connection.reconnect_in_seconds);
    // A timer left over from the first retry does nothing.
    assert!(shell.drive(&mut model, Event::RetryTimer { retry_gen: first }).is_empty());
    // Offline, the current timer waits too; back online, it goes at once.
    let _ = run(&mut model, Event::NetworkStatus { online: false });
    assert!(DomovoiApp.view(&model).connection.offline);
    let retry_gen = model.retry_gen;
    assert!(shell.drive(&mut model, Event::RetryTimer { retry_gen }).is_empty());
    let _ = shell.drive(&mut model, Event::NetworkStatus { online: true });
    assert!(model.retry_gen > retry_gen, "an attempt went (and failed again, so a new retry)");
}

#[test]
fn a_refusal_without_the_servers_own_error_is_retried_a_few_times() {
    let mut shell = Shell::paired();
    let mut model = Model::default();
    connected_to(&mut shell, &mut model, "http://a:1", "CODE");
    let _ = sock(&mut model, SocketEvent::Closed { reason: "lost".into() });
    // A captive portal's 403 page.
    shell.routes.insert("/api/auth/websocket-ticket/rpc".into(), (403, json!(null)));
    let _ = shell.drive(&mut model, Event::Reconnect);
    assert_eq!(DomovoiApp.view(&model).connection.state, "reconnecting");
    let retry_gen = model.retry_gen;
    let _ = shell.drive(&mut model, Event::RetryTimer { retry_gen });
    assert_eq!(DomovoiApp.view(&model).connection.state, "reconnecting");
    let retry_gen = model.retry_gen;
    let _ = shell.drive(&mut model, Event::RetryTimer { retry_gen });
    let view = DomovoiApp.view(&model);
    assert_eq!(view.connection.state, "failed", "the third time it means it");
    assert!(view.connection.server_changed);
}

#[test]
fn a_reconnect_resumes_the_open_thread_and_the_shell_instead_of_reloading() {
    let (mut model, sent) = connected();
    let shell_id = sent.iter().find(|r| r["tag"] == "orchestration.subscribeShell").unwrap()["id"].as_str().unwrap().to_owned();
    let _ = sock(&mut model, chunk(&shell_id, &[records::shell_snapshot(40, json!([{ "id": "p1", "title": "P", "workspaceRoot": "/p" }]), json!([{ "id": "t1", "projectId": "p1", "title": "T" }]))]));
    let sent = sent_requests(&run(&mut model, Event::SelectThread { thread_id: "t1".into() }));
    let sub = sent.iter().find(|r| r["tag"] == "orchestration.subscribeThread").unwrap();
    assert!(sub["payload"].get("afterSequence").is_none(), "first open: a snapshot");
    let tid = sub["id"].as_str().unwrap().to_owned();
    let snap = records::snapshot(41, json!({ "thread": { "id": "t1", "projectId": "p1", "title": "T" }, "messages": [], "turnItems": [] }));
    let _ = sock(&mut model, chunk(&tid, &[snap, records::stream_event(45, "thread.metadata-updated", "2026-01-01T00:00:00Z", json!({ "id": "t1", "projectId": "p1", "title": "T2" }))]));

    let _ = sock(&mut model, SocketEvent::Closed { reason: "lost".into() });
    assert!(DomovoiApp.view(&model).thread.unwrap().title == "T" || model.thread.is_some(), "the transcript stays while reconnecting");
    let sent = sent_requests(&sock(&mut model, SocketEvent::Opened));
    let sub = sent.iter().find(|r| r["tag"] == "orchestration.subscribeThread").unwrap();
    assert_eq!(sub["payload"]["afterSequence"], 45, "only what happened since");
    let shell = sent.iter().find(|r| r["tag"] == "orchestration.subscribeShell").unwrap();
    assert_eq!(shell["payload"]["afterSequence"], 40);
}

/// A thread opened again shows at once, as it was left, with its rows; its
/// stream resumes after what it had, with no loading state meanwhile.
#[test]
fn a_thread_opened_again_shows_at_once_and_resumes_its_stream() {
    let (mut model, sent) = connected();
    let shell_id = sent.iter().find(|r| r["tag"] == "orchestration.subscribeShell").unwrap()["id"].as_str().unwrap().to_owned();
    let threads = json!([{ "id": "t1", "projectId": "p1", "title": "One" }, { "id": "t2", "projectId": "p1", "title": "Two" }]);
    let _ = sock(&mut model, chunk(&shell_id, &[records::shell_snapshot(40, json!([{ "id": "p1", "title": "P", "workspaceRoot": "/p" }]), threads)]));
    let open = |model: &mut Model, id: &str| {
        let sent = sent_requests(&run(model, Event::SelectThread { thread_id: id.into() }));
        sent.iter().find(|r| r["tag"] == "orchestration.subscribeThread").expect("subscription").clone()
    };
    let reply = |id: &str, text: &str| json!({ "id": format!("i-{id}"), "type": "assistant_message", "messageId": format!("m-{id}"), "text": text, "status": "completed", "ordinal": 1 });
    let message = |id: &str, text: &str| json!({ "id": format!("m-{id}"), "role": "assistant", "text": text });
    // t1, first open: a snapshot.
    let sub = open(&mut model, "t1");
    assert!(sub["payload"].get("afterSequence").is_none());
    let snap = records::snapshot(41, json!({ "thread": { "id": "t1", "projectId": "p1", "title": "One" }, "messages": [message("t1", "first reply")], "turnItems": [reply("t1", "first reply")] }));
    let _ = sock(&mut model, chunk(sub["id"].as_str().unwrap(), &[snap]));
    let first = DomovoiApp.view(&model).thread.expect("t1 shown");
    assert!(first.items.iter().any(|r| r.text.contains("first reply")));
    // t2, then t1 again.
    let sub = open(&mut model, "t2");
    let snap = records::snapshot(42, json!({ "thread": { "id": "t2", "projectId": "p1", "title": "Two" }, "messages": [], "turnItems": [] }));
    let _ = sock(&mut model, chunk(sub["id"].as_str().unwrap(), &[snap]));
    let sub = open(&mut model, "t1");
    assert_eq!(sub["payload"]["afterSequence"], 41, "only what happened since it was left");
    let again = DomovoiApp.view(&model).thread.expect("t1 shown before any reply");
    assert!(!again.loading, "nothing to wait for: it is on screen");
    assert_eq!(again.items, first.items, "as it was left");
    assert_eq!(model.item_cache.lock().unwrap().len(), first.items.len(), "with the rows built for it");
    // Starting a new thread keeps the one left too.
    let _ = run(&mut model, Event::NewThread { project_id: None });
    assert!(model.recent_threads.iter().any(|r| r.id == "t1"));
}

#[test]
fn earlier_pages_merge_without_replacing_live_rows_or_leaking_across_switches() {
    let (mut model, _) = connected();
    model.supports_thread_windows = true;
    let requests = sent_requests(&run(&mut model, Event::SelectThread { thread_id: "t1".into() }));
    let subscription = requests.iter().find(|r| r["tag"] == "orchestration.subscribeThreadWindow").unwrap();
    let sub = subscription["id"].as_str().unwrap();
    let full = thread_with("t1", 3, 41);
    let mut latest = full.clone();
    latest["historyBefore"] = json!("41:i-t1-3");
    latest["projection"]["turnItems"] = json!([full["projection"]["turnItems"][2]]);
    let _ = sock(&mut model, chunk(sub, &[latest]));
    assert!(DomovoiApp.view(&model).thread.unwrap().has_earlier);
    assert_eq!(rows_of(&model, "t1"), ["t1 says 3"]);

    let older = sent_requests(&run(&mut model, Event::LoadEarlier));
    let request = older.iter().find(|r| r["tag"] == "orchestration.getThreadHistory").unwrap();
    assert_eq!(request["payload"]["before"], "41:i-t1-3");
    let details = |request: &Value, items: &[Value], before: Value| {
        exit_ok(request["id"].as_str().unwrap(), json!({ "snapshotSequence": 41, "historyBefore": before, "items": items }))
    };
    let _ = sock(&mut model, chunk(sub, &said("t1", 4, 42)));
    let items = full["projection"]["turnItems"].as_array().unwrap();
    let _ = sock(&mut model, details(request, &items[1..], json!("40:i-t1-2")));
    assert_eq!(rows_of(&model, "t1"), ["t1 says 2", "t1 says 3", "t1 says 4"]);
    assert_eq!(model.thread_sequence, 43, "history cannot rewind the live cursor");

    let next = sent_requests(&run(&mut model, Event::LoadEarlier));
    let request = next.iter().find(|r| r["tag"] == "orchestration.getThreadHistory").unwrap();
    let _ = run(&mut model, Event::SelectThread { thread_id: "t2".into() });
    let _ = run(&mut model, Event::SelectThread { thread_id: "t1".into() });
    let _ = sock(&mut model, details(request, &items[..1], Value::Null));
    assert_eq!(rows_of(&model, "t1"), ["t1 says 2", "t1 says 3", "t1 says 4"]);
    assert!(DomovoiApp.view(&model).thread.unwrap().has_earlier);
}

/// A stream item of thread `tid`'s: `item` with its event's envelope naming `tid`.
fn of_thread(tid: &str, mut item: Value) -> Value {
    item["event"]["threadId"] = json!(tid);
    item
}

/// What `tid`'s agent said as its `n`th reply: the message and its row, as events.
fn said(tid: &str, n: u64, sequence: u64) -> [Value; 2] {
    let text = format!("{tid} says {n}");
    let message = records::stream_event(sequence, "message.updated", "2026-11-23T13:00:00.000Z", json!({ "id": format!("m-{tid}-{n}"), "threadId": tid, "role": "assistant", "text": text }));
    let row = records::stream_event(sequence + 1, "turn-item.updated", "2026-11-23T13:00:00.000Z",
        json!({ "id": format!("i-{tid}-{n}"), "threadId": tid, "type": "assistant_message", "messageId": format!("m-{tid}-{n}"), "text": text, "status": "completed", "ordinal": n }));
    [of_thread(tid, message), of_thread(tid, row)]
}

/// `tid` as its stream's snapshot sends it, with its first `replies` replies.
fn thread_with(tid: &str, replies: u64, sequence: u64) -> Value {
    let messages: Vec<Value> = (1..=replies).map(|n| json!({ "id": format!("m-{tid}-{n}"), "threadId": tid, "role": "assistant", "text": format!("{tid} says {n}") })).collect();
    let items: Vec<Value> = (1..=replies)
        .map(|n| json!({ "id": format!("i-{tid}-{n}"), "threadId": tid, "type": "assistant_message", "messageId": format!("m-{tid}-{n}"), "text": format!("{tid} says {n}"), "status": "completed", "ordinal": n }))
        .collect();
    records::snapshot(sequence, json!({ "thread": { "id": tid, "projectId": "p1", "title": tid }, "messages": messages, "turnItems": items }))
}

/// The rows on the open thread's page that came from `other`'s agent.
fn rows_of(model: &Model, other: &str) -> Vec<String> {
    let Some(page) = DomovoiApp.view(model).thread else { return vec![] };
    page.items.iter().map(|r| r.text.clone()).filter(|t| t.contains(&format!("{other} says"))).collect()
}

/// The iPhone incident: two threads of one project run at once; one is open
/// when the connection drops, and the other is opened from its notification
/// (or Live Activity) before the app is connected again. The page opened is
/// that thread's alone: it waits for its own snapshot, its stream is not
/// resumed from the other's cursor, and neither thread's records take in the
/// other's, then or after switching back.
#[test]
fn a_thread_opened_from_a_notification_while_reconnecting_shows_only_its_own_output() {
    let (mut model, sent) = connected();
    let shell_id = sent.iter().find(|r| r["tag"] == "orchestration.subscribeShell").unwrap()["id"].as_str().unwrap().to_owned();
    let threads = json!([{ "id": "t1", "projectId": "p1", "title": "One" }, { "id": "t2", "projectId": "p1", "title": "Two" }]);
    let _ = sock(&mut model, chunk(&shell_id, &[records::shell_snapshot(40, json!([{ "id": "p1", "title": "P", "workspaceRoot": "/p" }]), threads)]));
    let sent = sent_requests(&run(&mut model, Event::SelectThread { thread_id: "t1".into() }));
    let sub = sent.iter().find(|r| r["tag"] == "orchestration.subscribeThread").unwrap()["id"].as_str().unwrap().to_owned();
    let mut frames = vec![thread_with("t1", 1, 41)];
    frames.extend(said("t1", 2, 60));
    let _ = sock(&mut model, chunk(&sub, &frames));
    assert_eq!(rows_of(&model, "t1").len(), 2);

    // The connection drops; t2's notification is tapped before it is back.
    let _ = sock(&mut model, SocketEvent::Closed { reason: "lost".into() });
    let _ = run(&mut model, Event::OpenNotification { environment_id: String::new(), thread_id: "t2".into() });
    let page = DomovoiApp.view(&model).thread.expect("t2's page");
    assert_eq!(page.id, "t2");
    assert!(page.loading, "it waits for its own records");
    assert!(page.items.is_empty() && rows_of(&model, "t1").is_empty(), "none of t1's output is shown as t2's");

    // Back: t2 is loaded fresh, not resumed after t1's last event (61).
    let sent = sent_requests(&sock(&mut model, SocketEvent::Opened));
    let sub = sent.iter().find(|r| r["tag"] == "orchestration.subscribeThread").expect("t2's stream");
    assert_eq!(sub["payload"]["threadId"], "t2");
    assert!(sub["payload"].get("afterSequence").is_none(), "a snapshot of t2: {sub}");
    let sub = sub["id"].as_str().unwrap().to_owned();
    let mut frames = vec![thread_with("t2", 1, 70)];
    // An event of t1's on t2's stream (it never should be) changes nothing.
    frames.extend(said("t1", 3, 71));
    frames.extend(said("t2", 2, 73));
    let _ = sock(&mut model, chunk(&sub, &frames));
    assert_eq!(rows_of(&model, "t2").len(), 2);
    assert!(rows_of(&model, "t1").is_empty());
    assert_eq!(model.misrouted, 2, "both of t1's events were left out");

    // Each thread, opened again, is its own.
    let sent = sent_requests(&run(&mut model, Event::SelectThread { thread_id: "t1".into() }));
    let sub = sent.iter().find(|r| r["tag"] == "orchestration.subscribeThread").unwrap();
    assert_eq!(sub["payload"]["afterSequence"], 61, "t1 resumes from its own cursor");
    assert_eq!((rows_of(&model, "t1").len(), rows_of(&model, "t2").len()), (2, 0));
    let _ = run(&mut model, Event::SelectThread { thread_id: "t2".into() });
    assert_eq!((rows_of(&model, "t2").len(), rows_of(&model, "t1").len()), (2, 0));
    assert!(model.recent_threads.iter().all(|r| r.thread.record.id.as_str() == r.id), "each kept under its own id");
}

/// Records held for one thread are never resumed into, or shown as, another's,
/// however selections, taps, drops, reconnects and the server's answers
/// (late ones on streams since closed among them) interleave.
#[test]
fn two_threads_never_mix_however_their_streams_interleave() {
    for seed in 0..64u64 {
        let mut random = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        let mut roll = |n: u64| {
            random = random.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (random >> 33) % n
        };
        let (mut model, sent) = connected();
        let shell_id = sent.iter().find(|r| r["tag"] == "orchestration.subscribeShell").unwrap()["id"].as_str().unwrap().to_owned();
        let threads = json!([{ "id": "t1", "projectId": "p1", "title": "One" }, { "id": "t2", "projectId": "p1", "title": "Two" }]);
        let _ = sock(&mut model, chunk(&shell_id, &[records::shell_snapshot(40, json!([{ "id": "p1", "title": "P", "workspaceRoot": "/p" }]), threads)]));
        // The server: every stream opened (by request id) and its thread, and what each thread said.
        let mut streams: Vec<(String, String)> = vec![];
        let mut replies = [0u64, 0u64];
        let mut sequence = 100u64;
        let mut connected = true;
        let mut log = vec![];
        for _ in 0..40 {
            let tid = ["t1", "t2"][roll(2) as usize];
            let effects = match roll(6) {
                0 => { log.push(format!("select {tid}")); run(&mut model, Event::SelectThread { thread_id: tid.into() }) }
                1 => { log.push(format!("notification {tid}")); run(&mut model, Event::OpenNotification { environment_id: String::new(), thread_id: tid.into() }) }
                2 if connected => { log.push("drop".into()); connected = false; sock(&mut model, SocketEvent::Closed { reason: "lost".into() }) }
                2 => { log.push("back".into()); connected = true; sock(&mut model, SocketEvent::Opened) }
                // A thread's agent says something; the server sends it on every stream of that thread.
                _ if connected && !streams.is_empty() => {
                    let (id, of) = streams[roll(streams.len() as u64) as usize].clone();
                    let at = if of == "t1" { 0 } else { 1 };
                    sequence += 2;
                    let frames: Vec<Value> = if roll(3) == 0 {
                        log.push(format!("snapshot of {of} on {id}"));
                        vec![thread_with(&of, replies[at], sequence)]
                    } else {
                        replies[at] += 1;
                        log.push(format!("{of} says {} on {id}", replies[at]));
                        said(&of, replies[at], sequence).to_vec()
                    };
                    sock(&mut model, chunk(&id, &frames))
                }
                _ => continue,
            };
            for request in sent_requests(&effects) {
                if request["tag"] == "orchestration.subscribeThread" {
                    streams.push((request["id"].as_str().unwrap().to_owned(), request["payload"]["threadId"].as_str().unwrap().to_owned()));
                }
            }
            let shown = DomovoiApp.view(&model).thread;
            if let Some(page) = shown {
                let other = if page.id == "t1" { "t2" } else { "t1" };
                assert!(rows_of(&model, other).is_empty(), "seed {seed}: {other}'s output on {}'s page after {log:?}", page.id);
            }
            for kept in &model.recent_threads {
                assert_eq!(kept.thread.record.id.as_str(), kept.id, "seed {seed}: kept under another's id after {log:?}");
                let other = if kept.id == "t1" { "t2" } else { "t1" };
                assert!(!kept.thread.messages.iter().any(|m| m.text.contains(&format!("{other} says"))), "seed {seed}: {} took in {other}'s after {log:?}", kept.id);
            }
        }
    }
}

#[test]
fn commands_made_while_disconnected_go_once_the_server_is_back() {
    let (mut model, sent) = connected();
    providers_snapshot(&mut model, &sent);
    failed_thread(&mut model);
    let _ = sock(&mut model, SocketEvent::Closed { reason: "lost".into() });
    // Stop, and a message, while the connection is down.
    assert!(sent_requests(&run(&mut model, Event::Interrupt)).is_empty());
    let _ = run(&mut model, Event::SetDraft { text: "and then?".into() });
    assert!(DomovoiApp.view(&model).composer.can_send, "sending queues it");
    let _ = run(&mut model, Event::Send);
    let page = DomovoiApp.view(&model).thread.unwrap();
    assert_eq!(page.pending_sends.len(), 1);
    assert_eq!(page.pending_sends[0].state, "waiting");
    assert!(DomovoiApp.view(&model).composer.send_hint.contains("once the server is back"));
    let sent = sent_requests(&sock(&mut model, SocketEvent::Opened));
    let dispatched: Vec<&str> = sent.iter().filter(|r| r["tag"] == "orchestration.dispatchCommand").map(|r| r["payload"]["type"].as_str().unwrap()).collect();
    assert!(dispatched.contains(&"thread.stop") && dispatched.contains(&"message.dispatch"), "{dispatched:?}");
}

#[test]
fn a_waiting_message_can_be_taken_back() {
    let (mut model, sent) = connected();
    providers_snapshot(&mut model, &sent);
    failed_thread(&mut model);
    let _ = sock(&mut model, SocketEvent::Closed { reason: "lost".into() });
    let _ = run(&mut model, Event::SetDraft { text: "never mind".into() });
    let _ = run(&mut model, Event::Send);
    let id = DomovoiApp.view(&model).thread.unwrap().pending_sends[0].id.clone();
    let _ = run(&mut model, Event::CancelSend { id });
    assert!(DomovoiApp.view(&model).thread.unwrap().pending_sends.is_empty());
    assert_eq!(DomovoiApp.view(&model).composer.draft, "never mind");
}

#[test]
fn a_drop_unsticks_whatever_was_waiting_on_an_answer() {
    let (mut model, _) = connected();
    let _ = run(&mut model, Event::ScanAgentSessions);
    let _ = run(&mut model, Event::BrowseFolders { path: "/".into() });
    assert!(DomovoiApp.view(&model).agent_sessions.scanning);
    let _ = sock(&mut model, SocketEvent::Closed { reason: "lost".into() });
    let view = DomovoiApp.view(&model);
    assert!(!view.agent_sessions.scanning);
    assert!(view.agent_sessions.error.is_some());
    assert!(!view.folder_browser.loading);
}

#[test]
fn a_bad_link_while_connected_keeps_the_connection() {
    let (mut model, _) = connected();
    let _ = run(&mut model, Event::ConnectRemote { link: "not a link".into() });
    let view = DomovoiApp.view(&model);
    assert_eq!(view.connection.state, "connected");
    assert!(view.toast.unwrap().message.contains("not a Domovoi pairing link"));
}

#[test]
fn drafts_wait_for_their_server_while_another_is_active() {
    let mut shell = Shell::paired();
    let mut model = Model::default();
    connected_to(&mut shell, &mut model, "http://a:1", "CODE");
    model.selected_project = Some("p1".into());
    let _ = run(&mut model, Event::SetDraft { text: "half a thought".into() });
    let first = model.active_env.clone().unwrap();
    connected_to(&mut shell, &mut model, "http://b:2", "CODE2");
    assert_eq!(DomovoiApp.view(&model).composer.draft, "");
    let _ = shell.drive(&mut model, Event::SelectEnvironment { id: first });
    model.selected_project = Some("p1".into());
    assert_eq!(DomovoiApp.view(&model).composer.draft, "half a thought");
}

/// A long thread with markdown, edits and command output.
fn long_thread(model: &mut Model, turns: usize) {
    model.selected_thread = Some("t1".into());
    model.route = "thread".into();
    model.threads = vec![records::shell(json!({ "id": "t1", "projectId": "p1", "title": "Long" }))];
    let mut messages = vec![];
    let mut items = vec![];
    let md = "## Plan\n\nSome **bold** text with `code` and a [link](http://x).\n\n- one\n- two\n\n```rust\nfn main() { println!(\"hi\"); }\n```\n";
    for t in 0..turns {
        let (u, a) = (format!("mu{t}"), format!("ma{t}"));
        messages.push(json!({ "id": u, "role": "user", "text": format!("question {t}"), "updatedAt": "1" }));
        messages.push(json!({ "id": a, "role": "assistant", "text": md.repeat(4), "updatedAt": "1" }));
        let base = (t * 4) as u64;
        items.push(json!({ "id": format!("iu{t}"), "threadId": "t1", "runId": format!("r{t}"), "type": "user_message", "messageId": u, "ordinal": base, "status": "completed", "updatedAt": "1" }));
        items.push(json!({ "id": format!("ic{t}"), "threadId": "t1", "runId": format!("r{t}"), "type": "command_execution", "input": "make", "output": "a\n".repeat(400), "ordinal": base + 1, "status": "completed", "updatedAt": "1" }));
        items.push(json!({ "id": format!("ie{t}"), "threadId": "t1", "runId": format!("r{t}"), "type": "file_change", "fileName": "a.rs", "oldStr": "x\n".repeat(150), "newStr": "y\n".repeat(150), "ordinal": base + 2, "status": "completed", "updatedAt": "1" }));
        items.push(json!({ "id": format!("ia{t}"), "threadId": "t1", "runId": format!("r{t}"), "type": "assistant_message", "messageId": a, "ordinal": base + 3, "status": "completed", "updatedAt": "1" }));
    }
    model.thread = Some(records::thread(json!({ "thread": { "id": "t1" }, "runs": [], "messages": messages, "turnItems": items })));
}

#[test]
fn streaming_into_a_long_thread_only_rebuilds_the_row_that_changed() {
    let (mut model, _) = connected();
    long_thread(&mut model, 125); // 500 items
    let first = std::time::Instant::now();
    let page = DomovoiApp.view(&model).thread.unwrap();
    let cold = first.elapsed();
    assert_eq!(page.items.len(), 500);
    let command = page.items.iter().find(|i| i.kind == "command").unwrap();
    assert!(command.truncated && command.output.as_ref().unwrap().lines().count() <= 201, "long output shows its end");

    // Stream 100 updates into the last message.
    let mut text = String::new();
    let started = std::time::Instant::now();
    for n in 0..100 {
        text.push_str("more words ");
        let event = records::domain_event("message.updated", "2", json!({ "id": "ma124", "role": "assistant", "text": text, "streaming": true, "updatedAt": format!("2.{n}") }));
        model.thread.as_mut().unwrap().apply(event);
        let page = DomovoiApp.view(&model).thread.unwrap();
        assert!(page.items.last().unwrap().text.ends_with("more words "));
    }
    let per_update = started.elapsed() / 100;
    println!("cold render {cold:?}, per streamed update {per_update:?}");
    assert!(per_update * 3 < cold, "a streamed update costs far less than building every row ({per_update:?} vs {cold:?})");

    // Expanding shows the whole output.
    let id = command.id.clone();
    let _ = run(&mut model, Event::ExpandItem { id: id.clone() });
    let page = DomovoiApp.view(&model).thread.unwrap();
    let command = page.items.iter().find(|i| i.id == id).unwrap();
    assert!(!command.truncated && command.output.as_ref().unwrap().lines().count() == 400);
}

#[test]
fn a_turns_steps_fold_into_a_summary_and_finished_turns_show_their_time() {
    let (mut model, _) = connected();
    model.selected_thread = Some("t1".into());
    model.route = "thread".into();
    model.threads = vec![records::shell(json!({ "id": "t1", "projectId": "p1", "title": "T" }))];
    let item = |id: &str, ord: u64, ty: &str, extra: Value| {
        let mut v = json!({ "id": id, "threadId": "t1", "runId": "r1", "type": ty, "status": "completed", "ordinal": ord });
        v.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        v
    };
    model.thread = Some(records::thread(json!({
        "thread": { "id": "t1" },
        "runs": [{ "id": "r1", "status": "completed", "ordinal": 1, "startedAt": "2026-01-01T10:00:00.000Z", "completedAt": "2026-01-01T10:00:42.000Z" }],
        "messages": [{ "id": "m1", "role": "user", "text": "go" }, { "id": "m2", "role": "assistant", "text": "done" }],
        "turnItems": [
            item("i1", 1, "user_message", json!({ "messageId": "m1" })),
            item("i2", 2, "command_execution", json!({ "input": "make" })),
            item("i3", 3, "command_execution", json!({ "input": "pwd" })),
            item("i4", 4, "file_change", json!({ "fileName": "a.rs", "oldStr": "a", "newStr": "b" })),
            item("i5", 5, "dynamic_tool", json!({ "toolName": "TodoWrite", "input": { "todos": [{ "content": "Read the code", "status": "completed" }, { "content": "Fix it", "status": "in_progress" }] } })),
            item("i6", 6, "assistant_message", json!({ "messageId": "m2" })),
        ],
    })));
    let page = DomovoiApp.view(&model).thread.unwrap();
    let rows: Vec<(&str, &str)> = page.items.iter().map(|i| (i.kind.as_str(), i.group.as_str())).collect();
    assert_eq!(rows, [("user", ""), ("command", "i2"), ("command", "i2"), ("file_change", "i2"), ("todo", ""), ("assistant", "")]);
    assert_eq!(page.items[1].group_summary, "Edited 1 file (+1 −1) and ran 2 commands");
    let todo = &page.items[4];
    assert_eq!(todo.todos.len(), 2);
    assert_eq!(todo.todos[1].status, "in_progress");
    let last = page.items.last().unwrap();
    assert_eq!(last.run_duration_ms, 42_000);
    assert!(last.run_started_ms > 0);
    assert_eq!(page.items[0].run_duration_ms, 0, "only the turn's last row");

    // Edit & Resend puts the text in the composer.
    let _ = run(&mut model, Event::EditMessage { text: "go again".into() });
    assert_eq!(DomovoiApp.view(&model).composer.draft, "go again");
}

#[test]
fn working_activity_follows_the_current_run_and_its_visible_steps() {
    let (mut model, _) = connected();
    model.selected_thread = Some("t1".into());
    model.route = "thread".into();
    let shell = |active: Value, pending: Value| records::shell(json!({
        "id": "t1", "projectId": "p1", "title": "T", "status": "running",
        "activeRunId": active, "pendingRuntimeRequest": pending,
    }));
    let mut projection = json!({
        "thread": { "id": "t1" },
        "runs": [
            { "id": "r1", "ordinal": 1, "status": "completed", "startedAt": "2026-11-01T00:00:00.000Z" },
            { "id": "r2", "ordinal": 2, "status": "running", "startedAt": "2026-11-01T00:00:01.000Z" },
            { "id": "r3", "ordinal": 3, "status": "queued" },
        ],
        "turnItems": [
            { "id": "old", "runId": "r1", "ordinal": 1, "type": "command_execution", "input": "pwd", "status": "running" },
            { "id": "live", "runId": "r2", "ordinal": 2, "type": "command_execution", "input": "make", "status": "running" },
            { "id": "done", "runId": "r2", "ordinal": 3, "type": "dynamic_tool", "toolName": "List", "status": "completed" },
        ],
    });
    let page = |model: &mut Model, projection: &Value| {
        model.thread = Some(records::thread(projection.clone()));
        DomovoiApp.view(model).thread.unwrap()
    };
    let activity = |kind: Option<&str>, step: Option<&str>| {
        Some(domovoi_core::view::WorkingActivityView { kind: kind.map(str::to_owned), step_item_id: step.map(str::to_owned) })
    };
    model.threads = vec![shell(json!("r2"), Value::Null)];
    assert_eq!(page(&mut model, &projection).working_activity, activity(Some("command"), Some("live")));

    projection["turnItems"][1]["status"] = json!("completed");
    projection["turnItems"].as_array_mut().unwrap().push(json!({
        "id": "reply", "runId": "r2", "ordinal": 4, "type": "assistant_message",
        "messageId": "reply-message", "text": "Here is what I found", "streaming": true, "status": "running",
    }));
    let writing = page(&mut model, &projection);
    assert!(writing.working_since_ms > 0);
    assert_eq!(writing.working_activity, activity(Some("assistant"), Some("done")));

    model.threads = vec![shell(json!("r2"), json!({ "id": "request", "kind": "command" }))];
    let approval = page(&mut model, &projection);
    assert_eq!(approval.status, "approval");
    assert_eq!(approval.working_activity, None);

    model.threads = vec![shell(Value::Null, Value::Null)];
    projection["runs"][1]["status"] = json!("waiting");
    projection["turnItems"][3]["streaming"] = json!(false);
    projection["turnItems"][3]["status"] = json!("completed");
    assert_eq!(page(&mut model, &projection).working_activity, activity(None, Some("done")));

    model.threads = vec![shell(json!("r3"), Value::Null)];
    projection["runs"][1]["status"] = json!("completed");
    projection["runs"][2]["status"] = json!("starting");
    assert_eq!(page(&mut model, &projection).working_activity, activity(Some("starting"), None));
    projection["runs"][2]["status"] = json!("running");
    let handoff = page(&mut model, &projection);
    assert_eq!(handoff.working_activity, None);
    assert_eq!(handoff.working_since_ms, 0);
}

#[test]
fn toasts_wait_their_turn_and_archive_can_be_undone() {
    let (mut model, _) = connected();
    model.threads = vec![records::shell(json!({ "id": "t9", "projectId": "p1", "title": "Old work" }))];
    let _ = run(&mut model, Event::ArchiveThread { thread_id: "t9".into(), archived: true });
    let toast = DomovoiApp.view(&model).toast.unwrap();
    assert_eq!(toast.message, "Archived “Old work”.");
    assert_eq!(toast.action_label, "Undo");
    // Another toast doesn't replace it; it waits.
    model.toast("error", "Something else");
    assert_eq!(DomovoiApp.view(&model).toast.unwrap().queued, 1);
    let sent = sent_requests(&run(&mut model, Event::ToastAction));
    assert!(sent.iter().any(|r| r["payload"]["type"] == "thread.unarchive" && r["payload"]["threadId"] == "t9"), "Undo unarchives");
    assert_eq!(DomovoiApp.view(&model).toast.unwrap().message, "Something else", "then the next toast shows");
    let _ = run(&mut model, Event::DismissToast);
    assert!(DomovoiApp.view(&model).toast.is_none());
}

#[test]
fn drafts_and_the_open_thread_survive_the_app_being_closed() {
    let mut shell = Shell::paired();
    let mut model = Model::default();
    connected_to(&mut shell, &mut model, "http://a:1", "CODE");
    model.threads = vec![records::shell(json!({ "id": "t1", "projectId": "p1", "title": "T" }))];
    let _ = shell.drive(&mut model, Event::SelectThread { thread_id: "t1".into() });
    let _ = shell.drive(&mut model, Event::SetDraft { text: "unfinished thought".into() });
    // iOS suspends the app (and may end it): the drafts are saved.
    let _ = shell.drive(&mut model, Event::Suspend);
    let saved = shell.store.iter().find(|(k, _)| k.starts_with("drafts:")).map(|(_, v)| v.clone()).expect("saved");
    assert!(saved.contains("unfinished thought") && saved.contains("t1"), "{saved}");

    // A fresh start: the thread opens and the draft is back.
    let mut fresh = Model::default();
    let _ = shell.drive(&mut fresh, Event::Launch { surface: "mobile".into(), device_label: "Phone".into(), device_os: "iOS".into() });
    assert_eq!(fresh.selected_thread.as_deref(), Some("t1"));
    assert_eq!(fresh.composer.drafts.get("t1").map(String::as_str), Some("unfinished thought"));
}

#[test]
fn pull_to_refresh_resyncs_a_live_connection_from_where_it_was() {
    let (mut model, sent) = connected();
    let shell_id = sent.iter().find(|r| r["tag"] == "orchestration.subscribeShell").unwrap()["id"].as_str().unwrap().to_owned();
    let (shell, thread_items, _) = recorded();
    for v in &shell {
        let _ = sock(&mut model, chunk(&shell_id, std::slice::from_ref(v)));
    }
    let sent = sent_requests(&run(&mut model, Event::SelectThread { thread_id: THREAD.into() }));
    let thread_id = sent.iter().find(|r| r["tag"] == "orchestration.subscribeThread").unwrap()["id"].as_str().unwrap().to_owned();
    for v in &thread_items {
        let _ = sock(&mut model, chunk(&thread_id, std::slice::from_ref(v)));
    }

    let sent = sent_requests(&run(&mut model, Event::Resync));
    // The old streams stop, and new ones pick up after the last item seen.
    assert!(sent.iter().any(|r| r["_tag"] == "Interrupt" && r["requestId"] == json!(shell_id)), "{sent:?}");
    assert!(sent.iter().any(|r| r["_tag"] == "Interrupt" && r["requestId"] == json!(thread_id)), "{sent:?}");
    let shell_sub = sent.iter().find(|r| r["tag"] == "orchestration.subscribeShell").expect("shell again");
    assert!(shell_sub["payload"]["afterSequence"].is_u64(), "{shell_sub}");
    let thread_sub = sent.iter().find(|r| r["tag"] == "orchestration.subscribeThread").expect("thread again");
    assert_eq!(thread_sub["payload"]["threadId"], THREAD);
    assert!(thread_sub["payload"]["afterSequence"].as_u64().is_some_and(|s| s > 0), "{thread_sub}");
    assert_eq!(DomovoiApp.view(&model).connection.state, "connected", "the connection is kept");

    // Not connected, it reconnects instead.
    let _ = sock(&mut model, SocketEvent::Closed { reason: "lost".into() });
    let mut shell = Shell::paired();
    let effects = shell.drive(&mut model, Event::Resync);
    assert!(socket_url(&effects).is_some(), "a new ticket, then the socket");
}

#[test]
fn a_question_shows_its_options_and_is_answered_with_them() {
    let (mut model, _) = connected();
    model.selected_thread = Some("t1".into());
    model.route = "thread".into();
    model.threads = vec![records::shell(json!({ "id": "t1", "projectId": "p1", "title": "T" }))];
    let questions = json!([
        { "id": "Which database?", "header": "DB", "question": "Which database?", "multiSelect": false, "allowCustomAnswer": false, "required": true,
          "options": [{ "label": "Postgres", "description": "Relational" }, { "label": "SQLite", "description": "A file", "value": "sqlite-value" }] },
        { "id": "Which features?", "header": "Features", "question": "Which features?", "multiSelect": true, "required": false,
          "options": [{ "label": "Auth", "description": "Sign in" }] },
    ]);
    let thread = |status: &str, answers: Value| records::thread(json!({
        "thread": { "id": "t1" }, "runs": [], "messages": [],
        "turnItems": [{ "id": "iq", "threadId": "t1", "runId": "r1", "type": "user_input_request", "requestId": "rq", "questions": questions,
            "status": if status == "pending" { "waiting" } else { "completed" }, "ordinal": 1, "updatedAt": status }],
        "runtimeRequests": [{ "id": "rq", "kind": "user_input", "status": status, "answers": answers }],
    }));
    model.thread = Some(thread("pending", Value::Null));
    let page = DomovoiApp.view(&model).thread.unwrap();
    let request = page.pending_requests.first().expect("the question is pending");
    assert_eq!(request.kind, "user_input");
    assert_eq!(request.questions.len(), 2);
    assert_eq!(request.questions[0].header, "DB");
    assert_eq!(request.questions[0].options[1].label, "SQLite");
    assert_eq!(request.questions[0].options[1].value, "sqlite-value");
    assert!(!request.questions[0].allow_custom_answer);
    assert!(request.questions[0].required);
    assert!(request.questions[1].multi_select);
    assert!(!request.questions[1].required);
    assert!(request.questions[1].allow_custom_answer);
    let row = page.items.iter().find(|i| i.kind == "question").expect("a question row");
    assert_eq!(row.title, "Which database?\nWhich features?");

    let sent = sent_requests(&run(&mut model, Event::RespondUserInput { request_id: "rq".into(), answers: vec![
        QuestionAnswer { question_id: "Which database?".into(), values: vec!["sqlite-value".into()] },
        QuestionAnswer { question_id: "Which features?".into(), values: vec![] },
    ] }));
    let cmd = &sent[0]["payload"];
    assert_eq!(cmd["type"], "runtime-request.respond");
    assert_eq!(cmd["answers"], json!({ "Which database?": ["sqlite-value"] }), "unanswered questions are left out");

    let sent = sent_requests(&run(&mut model, Event::DismissUserInput { request_id: "rq".into() }));
    assert_eq!((sent[0]["payload"]["type"].as_str(), sent[0]["payload"]["requestId"].as_str()), (Some("thread.user-input.dismiss"), Some("rq")));

    // Answered: the row shows the answers.
    model.thread = Some(thread("resolved", json!({ "Which database?": ["sqlite-value"], "Which features?": ["Auth"] })));
    let page = DomovoiApp.view(&model).thread.unwrap();
    assert!(page.pending_requests.is_empty());
    assert_eq!(page.items.iter().find(|i| i.kind == "question").unwrap().text, "SQLite · Auth");
}

#[test]
fn an_edit_approval_shows_the_change_it_would_make() {
    let (mut model, _) = connected();
    model.selected_thread = Some("t1".into());
    model.route = "thread".into();
    model.threads = vec![records::shell(json!({ "id": "t1", "projectId": "p1", "title": "T" }))];
    let native = json!({ "driver": "claudeAgent", "nativeId": "toolu_1" });
    model.thread = Some(records::thread(json!({
        "thread": { "id": "t1" }, "runs": [], "messages": [],
        "turnItems": [
            { "id": "edit", "threadId": "t1", "runId": "r1", "type": "file_change", "nativeItemRef": native, "status": "waiting", "ordinal": 1,
              "changes": [{ "path": "src/main.rs" }], "oldStr": "let a = 1;\n", "newStr": "let a = 2;\n", "updatedAt": "1" },
            { "id": "ask", "threadId": "t1", "runId": "r1", "type": "approval_request", "nativeItemRef": native, "requestId": "rq",
              "requestKind": "file-change", "prompt": "Edit src/main.rs", "status": "waiting", "ordinal": 2, "updatedAt": "1" },
        ],
        "runtimeRequests": [{ "id": "rq", "kind": "file-change", "status": "pending" }],
    })));
    let page = DomovoiApp.view(&model).thread.unwrap();
    let request = page.pending_requests.first().expect("the approval is pending");
    assert_eq!(request.files.len(), 1, "the edit's diff rides along");
    assert_eq!(request.files[0].path, "src/main.rs");
    let lines: Vec<_> = request.files[0].hunks.iter().flat_map(|h| h.lines.iter()).map(|l| (l.kind.as_str(), l.text.as_str())).collect();
    assert!(lines.contains(&("del", "let a = 1;")) && lines.contains(&("add", "let a = 2;")), "{lines:?}");
}

#[test]
fn allowing_for_the_thread_is_offered_where_the_server_keeps_it_and_can_be_forgotten() {
    let (mut model, _) = connected();
    model.selected_thread = Some("t1".into());
    model.route = "thread".into();
    model.threads = vec![records::shell(json!({ "id": "t1", "projectId": "p1", "title": "T" }))];
    let labels = |model: &mut Model, driver: &str| {
        model.thread = Some(records::thread(json!({
            "thread": { "id": "t1" }, "runs": [], "messages": [], "turnItems": [],
            "runtimeRequests": [{ "id": "rq", "kind": "permission", "status": "pending",
                "nativeRequestRef": { "driver": driver, "nativeId": "toolu_1", "strength": "strong" } }],
        })));
        DomovoiApp.view(model).thread.unwrap().pending_requests[0].session_label.clone()
    };
    assert_eq!(labels(&mut model, "claudeAgent"), "Allow for Thread");
    assert_eq!(labels(&mut model, "antigravity"), "Allow for Thread");
    assert_eq!(labels(&mut model, "codex"), "Allow for Session", "Codex keeps a grant for its session only");

    let sent = sent_requests(&run(&mut model, Event::ForgetThreadPermissions { thread_id: "t1".into() }));
    let forget = sent.iter().find(|r| r["tag"] == "orchestration.forgetThreadPermissions").expect("the forget request");
    assert_eq!(forget["payload"], json!({ "threadId": "t1" }));
    let _ = sock(&mut model, exit_ok(forget["id"].as_str().unwrap(), json!({ "forgotten": 2 })));
    assert_eq!(DomovoiApp.view(&model).toast.unwrap().message, "Forgot 2 allowed tools: its agent asks again from its next start.");

    // A scheduled task's thread, not the one open, gets other permissions.
    let sent = sent_requests(&run(&mut model, Event::SetThreadRuntimeMode { thread_id: "t9".into(), mode: "full-access".into() }));
    let command = &sent.iter().find(|r| r["tag"] == "orchestration.dispatchCommand").expect("a command")["payload"];
    assert_eq!((command["type"].as_str(), command["threadId"].as_str(), command["runtimeMode"].as_str()), (Some("thread.runtime-mode.set"), Some("t9"), Some("full-access")));
}

/// A client with thread `t1` selected and its stream open; returns the
/// stream's request id.
fn open_stream(model: &mut Model) -> String {
    model.threads = vec![records::shell(json!({ "id": "t1", "projectId": "p1", "title": "T" }))];
    let sent = sent_requests(&run(model, Event::SelectThread { thread_id: "t1".into() }));
    sent.iter().find(|r| r["tag"] == "orchestration.subscribeThread").expect("thread subscription")["id"].as_str().unwrap().to_owned()
}

/// The `afterSequence` the thread's stream is asked for again with after the connection dropped.
fn resumed_after(model: &mut Model) -> Value {
    let _ = sock(model, SocketEvent::Closed { reason: "lost".into() });
    let sent = sent_requests(&sock(model, SocketEvent::Opened));
    sent.iter().find(|r| r["tag"] == "orchestration.subscribeThread").expect("thread subscription")["payload"]["afterSequence"].clone()
}

/// A reply as it streams: its turn item first, then the text each flush
/// added. The row grows, and nothing else of the page is built again. Text
/// the thread already holds (a snapshot taken after it was written) is not
/// added twice; text that does not follow what is held starts the stream over.
#[test]
fn a_streamed_reply_grows_by_the_text_it_is_sent() {
    let (mut model, _) = connected();
    let stream = open_stream(&mut model);
    let reply = |text: &str| json!({ "id": "reply", "type": "assistant_message", "messageId": "m-reply", "text": text, "streaming": true, "status": "running", "ordinal": 2 });
    let snapshot = records::snapshot(1, json!({ "turnItems": [{ "id": "ask", "type": "user_message", "messageId": "m-ask", "text": "hello", "ordinal": 1 }] }));
    let _ = sock(&mut model, chunk(&stream, &[snapshot, records::stream_event(2, "turn-item.updated", "2", reply("Hel"))]));
    let text = |model: &Model| DomovoiApp.view(model).thread.unwrap().items.iter().find(|i| i.id == "reply").map(|i| (i.text.clone(), i.streaming));
    assert_eq!(text(&model), Some(("Hel".into(), true)));

    let append = |sequence: u64, at: usize, text: &str| json!({ "kind": "append", "sequence": sequence, "threadId": "t1", "itemId": "reply", "at": at, "text": text, "updatedAt": "3" });
    let sent = sent_requests(&sock(&mut model, chunk(&stream, &[append(3, 3, "lo, wör"), append(4, "Hello, wör".len(), "ld")])));
    assert!(sent.iter().any(|r| r["_tag"] == "Ack"), "the chunk is acknowledged");
    assert_eq!(text(&model), Some(("Hello, wörld".into(), true)));
    assert_eq!(model.thread_sequence, 4, "the stream resumes after the last text added");

    // Sent again (the snapshot already held it): nothing changes.
    let _ = sock(&mut model, chunk(&stream, &[append(4, "Hello, wör".len(), "ld")]));
    assert_eq!(text(&model), Some(("Hello, wörld".into(), true)));

    // The whole reply ends it, like any other item's new version.
    let mut whole = reply("Hello, wörld.");
    whole["streaming"] = json!(false);
    whole["status"] = json!("completed");
    let _ = sock(&mut model, chunk(&stream, &[records::stream_event(5, "turn-item.updated", "4", whole)]));
    assert_eq!(text(&model), Some(("Hello, wörld.".into(), false)));

    // Text that does not follow what is held: the thread is asked for again, from a snapshot.
    let sent = sent_requests(&sock(&mut model, chunk(&stream, &[append(6, 99, "lost")])));
    assert!(sent.iter().any(|r| r["_tag"] == "Interrupt" && r["requestId"] == json!(stream)), "{sent:?}");
    let again = sent.iter().find(|r| r["tag"] == "orchestration.subscribeThread").expect("the thread is subscribed to again");
    assert!(again["payload"].get("afterSequence").is_none(), "from a snapshot: {again}");
}

#[test]
fn a_codex_patch_shows_as_the_change_an_approval_would_make() {
    let (mut model, _) = connected();
    let stream = open_stream(&mut model);
    let native = json!({ "driver": "codex", "nativeId": "call_1" });
    let patch = "--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1 +1 @@\n-let a = 1;\n+let a = 2;\n";
    // The server's record of a Codex edit: the whole patch.
    let edit = |id: &str, ordinal: u64| json!({ "id": id, "type": "file_change", "nativeItemRef": native, "status": "waiting", "ordinal": ordinal,
        "fileName": "main.rs", "changes": [{ "path": "src/main.rs" }], "diffStr": patch });
    let snapshot = records::snapshot(1, json!({
        "turnItems": [
            edit("edit", 1),
            { "id": "ask", "type": "approval_request", "nativeItemRef": native, "requestId": "rq", "requestKind": "file-change", "status": "waiting", "ordinal": 2 },
        ],
        "runtimeRequests": [{ "id": "rq", "kind": "file-change", "status": "pending" }],
    }));
    let _ = sock(&mut model, chunk(&stream, &[snapshot, records::stream_event(2, "turn-item.updated", "2", edit("later", 3))]));
    let page = DomovoiApp.view(&model).thread.unwrap();
    let lines = |file: &domovoi_core::view::DiffFileView| file.hunks.iter().flat_map(|h| h.lines.iter()).map(|l| (l.kind.clone(), l.text.clone())).collect::<Vec<_>>();
    let wanted = [("del".to_owned(), "let a = 1;".to_owned()), ("add".to_owned(), "let a = 2;".to_owned())];
    for id in ["edit", "later"] {
        let row = page.items.iter().find(|i| i.id == id).unwrap();
        assert_eq!(lines(row.diff.as_ref().expect("the row shows the patch")), wanted, "{id}");
    }
    let request = page.pending_requests.first().expect("the approval is pending");
    assert_eq!(request.files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(), ["src/main.rs", "src/main.rs"], "both edits of the native item");
    assert_eq!(lines(&request.files[0]), wanted);
}

/// An item of a thread's stream this app cannot read (here: records without
/// a field the schema requires) is left out. The thread shows what could be
/// read, the user is told once that it may be incomplete, and the stream
/// goes on from after the item.
#[test]
fn an_unreadable_stream_item_is_left_out_and_the_user_is_told() {
    let (mut model, _) = connected();
    let stream = open_stream(&mut model);
    let snapshot = records::snapshot(4, json!({
        "messages": [{ "id": "m1", "role": "user", "text": "hello" }],
        "turnItems": [{ "id": "iu", "type": "user_message", "messageId": "m1", "ordinal": 1 }],
    }));
    let _ = sock(&mut model, chunk(&stream, &[snapshot, json!({ "kind": "synchronized" })]));
    let kinds = |model: &Model| DomovoiApp.view(model).thread.unwrap().items.iter().map(|i| i.kind.clone()).collect::<Vec<_>>();
    assert_eq!(kinds(&model), ["user"]);
    assert!(DomovoiApp.view(&model).toast.is_none());

    // A command without its `input`, between a reply's start and its thinking, in one frame.
    let unreadable = |sequence: u64| {
        let mut event = records::stream_event(sequence, "turn-item.updated", "2", json!({ "id": "ic", "type": "command_execution", "ordinal": 3 }));
        event["event"]["payload"].as_object_mut().unwrap().remove("input");
        event
    };
    let before = records::stream_event(5, "turn-item.updated", "2", json!({ "id": "ir", "type": "reasoning", "ordinal": 2, "text": "Hm." }));
    let after = records::stream_event(7, "turn-item.updated", "2", json!({ "id": "ia", "type": "assistant_message", "messageId": "m2", "ordinal": 4, "text": "Hi." }));
    let _ = sock(&mut model, chunk(&stream, &[before, unreadable(6), after]));
    assert_eq!(kinds(&model), ["user", "reasoning", "assistant"], "everything that could be read, and no row made up for what could not");
    let toast = DomovoiApp.view(&model).toast.expect("the user is told");
    assert_eq!(toast.kind, "error");
    assert!(toast.message.contains("may be incomplete") && toast.message.contains("missing field `input`"), "{}", toast.message);

    // Told once for a thread, however many more such items come.
    let _ = run(&mut model, Event::DismissToast);
    let _ = sock(&mut model, chunk(&stream, &[unreadable(8), json!({ "kind": "event", "sequence": 9 }), json!({ "kind": "hologram" })]));
    assert!(DomovoiApp.view(&model).toast.is_none());
    assert_eq!(model.thread_gaps, 4);
    assert_eq!(kinds(&model), ["user", "reasoning", "assistant"]);
    // The stream is resumed from after the last item received, read or not.
    assert_eq!(resumed_after(&mut model), 9);

    // A snapshot starts the thread over: whole again, and a later gap is told again.
    let stream = model.rpc.id_of(&domovoi_core::rpc::Route::Thread("t1".into())).expect("the stream is open again");
    let _ = sock(&mut model, chunk(&stream, &[records::snapshot(20, json!({})), unreadable(21)]));
    assert_eq!(model.thread_gaps, 1);
    assert!(DomovoiApp.view(&model).toast.is_some_and(|toast| toast.message.contains("may be incomplete")));
}

/// A snapshot this app cannot read opens no thread: the page stays as it is
/// while loading, the user is told why, and nothing of the stream is taken
/// for the thread.
#[test]
fn a_thread_this_app_cannot_read_is_not_opened_and_the_user_is_told() {
    let (mut model, _) = connected();
    let stream = open_stream(&mut model);
    let mut snapshot = records::snapshot(4, json!({ "messages": [{ "id": "m1", "role": "user", "text": "hello" }] }));
    snapshot["projection"]["messages"][0].as_object_mut().unwrap().remove("role");
    let event = records::stream_event(5, "message.updated", "2", json!({ "id": "m2", "role": "user", "text": "more" }));
    let _ = sock(&mut model, chunk(&stream, &[snapshot, json!({ "kind": "synchronized" }), event]));
    assert!(model.thread.is_none());
    let view = DomovoiApp.view(&model);
    let page = view.thread.expect("the page of the selected thread");
    assert!(page.loading && page.items.is_empty(), "no thread is shown rather than a wrong one");
    let toast = view.toast.expect("the user is told");
    assert!(toast.kind == "error" && toast.message.contains("can't read this thread") && toast.message.contains("role"), "{}", toast.message);
    // With no thread held, a reconnect asks for all of it again.
    assert_eq!(resumed_after(&mut model), Value::Null);
}

/// A row is built again whenever a record it shows was replaced, whatever
/// changed in it: here a command's line and a message's text, each with the
/// time, status and length it had.
#[test]
fn a_replaced_record_always_rebuilds_its_row() {
    let (mut model, _) = connected();
    model.selected_thread = Some("t1".into());
    model.route = "thread".into();
    model.threads = vec![records::shell(json!({ "id": "t1", "projectId": "p1", "title": "T" }))];
    let command = |input: &str| json!({ "id": "ic", "type": "command_execution", "ordinal": 2, "input": input, "output": "ok\n", "updatedAt": "5" });
    let message = |text: &str| json!({ "id": "m1", "role": "user", "text": text, "updatedAt": "5" });
    model.thread = Some(records::thread(json!({
        "messages": [message("first")],
        "turnItems": [{ "id": "iu", "type": "user_message", "messageId": "m1", "ordinal": 1 }, command("make")],
    })));
    let shown = |model: &Model| {
        let page = DomovoiApp.view(model).thread.unwrap();
        (page.items[0].text.clone(), page.items[1].title.clone())
    };
    assert_eq!(shown(&model), ("first".into(), "make".into()));
    let thread = model.thread.as_mut().unwrap();
    thread.apply(records::domain_event("turn-item.updated", "6", command("pwd")));
    thread.apply(records::domain_event("message.updated", "6", message("fifth")));
    assert_eq!(shown(&model), ("fifth".into(), "pwd".into()));
    // Records that did not change keep their rows: the same views again.
    let (again, once_more) = (DomovoiApp.view(&model).thread.unwrap(), DomovoiApp.view(&model).thread.unwrap());
    assert_eq!(again, once_more);
    assert!(std::sync::Arc::ptr_eq(again.items[1].output.as_ref().unwrap(), once_more.items[1].output.as_ref().unwrap()), "the row was kept, not built twice");
}

/// The open thread is in the offline copy as its projection, the form the
/// server sends: a copy written by this version is read back, and so is one
/// an earlier version wrote (the records as they came, a Codex patch still
/// under `diff`). A copy whose thread this app cannot read brings the thread
/// list back without it.
#[test]
fn the_open_thread_comes_back_from_the_offline_copy_if_it_can_be_read() {
    let mut shell = Shell::paired();
    let mut model = Model::default();
    connected_to(&mut shell, &mut model, "http://a:1", "CODE");
    model.threads = vec![records::shell(json!({ "id": "t1", "projectId": "p1", "title": "T" }))];
    let _ = shell.drive(&mut model, Event::SelectThread { thread_id: "t1".into() });
    model.thread = Some(records::thread(json!({
        "messages": [{ "id": "m1", "role": "user", "text": "hello" }],
        "turnItems": [{ "id": "iu", "type": "user_message", "messageId": "m1", "ordinal": 1 }, { "id": "ic", "type": "command_execution", "ordinal": 2, "input": "make" }],
    })));
    model.cache_dirty = true;
    let _ = shell.drive(&mut model, Event::Suspend);
    let env = model.active_env.clone().unwrap();
    let saved = shell.store.get(&format!("cache:shell:{env}")).cloned().expect("the copy was saved");
    let copy: Value = serde_json::from_str(&saved).unwrap();
    assert_eq!(copy["version"], 1);
    let parts: Vec<&str> = copy["thread"].as_object().expect("the open thread is in the copy").keys().map(String::as_str).collect();
    assert_eq!(parts, [
        "thread", "runs", "attempts", "nodes", "subagents", "providerSessions", "providerThreads", "providerTurns", "runtimeRequests", "messages",
        "plans", "turnItems", "checkpointScopes", "checkpoints", "contextHandoffs", "contextTransfers", "updatedAt",
    ]);

    // A launch without the server, with the thread selected as the saved drafts say.
    let restore = |saved: String| {
        let mut fresh = Model::default();
        fresh.active_env = Some(env.clone());
        fresh.selected_thread = Some("t1".into());
        fresh.route = "thread".into();
        let _ = run(&mut fresh, Event::CacheLoaded { environment_id: env.clone(), value: Some(saved) });
        fresh
    };
    let fresh = restore(saved);
    let page = DomovoiApp.view(&fresh).thread.unwrap();
    assert_eq!(page.items.iter().map(|i| (i.kind.as_str(), i.text.as_str(), i.title.as_str())).collect::<Vec<_>>(), [("user", "hello", ""), ("command", "", "make")]);
    assert!(!page.loading);

    // A thread that is not one this app can read, and one that is another thread's.
    let mut unreadable = copy.clone();
    unreadable["thread"]["turnItems"][1].as_object_mut().unwrap().remove("input");
    let mut not_a_thread = copy.clone();
    not_a_thread["thread"] = json!("gone");
    let mut another = copy.clone();
    another["thread"]["thread"]["id"] = json!("t2");
    for saved in [unreadable, not_a_thread, another] {
        let fresh = restore(saved.to_string());
        assert!(fresh.thread.is_none());
        let view = DomovoiApp.view(&fresh);
        assert!(view.connection.showing_saved && view.toast.is_none(), "the list is back, and nothing is said of the thread");
        assert_eq!(fresh.threads.len(), 1);
        assert!(view.thread.is_some_and(|page| page.loading && page.items.is_empty()));
    }
}

#[test]
fn a_slash_lists_the_providers_commands_and_completes_one() {
    let (mut model, sent) = connected();
    providers_snapshot(&mut model, &sent);
    let commands = json!([{ "name": "review" }, { "name": "compact" }, { "name": "security-review", "description": "Look for holes" }]);
    model.providers[0].slash_commands = Some(serde_json::from_value(commands).expect("slash commands as the server lists them"));
    model.selected_project = Some("p1".into());
    model.projects = vec![records::project(json!({ "id": "p1", "title": "calc", "workspaceRoot": "/w/calc" }))];

    let _ = run(&mut model, Event::SetDraft { text: "/rev".into() });
    let names: Vec<String> = DomovoiApp.view(&model).composer.slash_commands.iter().map(|c| c.name.clone()).collect();
    assert_eq!(names, ["review", "security-review"], "names starting with the word come first");
    let _ = run(&mut model, Event::SetDraft { text: "/".into() });
    let names: Vec<String> = DomovoiApp.view(&model).composer.slash_commands.iter().map(|c| c.name.clone()).collect();
    assert_eq!(names, ["model", "plan", "review", "compact", "security-review"], "the app's own first (a new thread has nothing to fork or clear)");
    let _ = run(&mut model, Event::SetDraft { text: "fix /rev".into() });
    assert!(DomovoiApp.view(&model).composer.slash_commands.is_empty(), "only at the start of the draft");

    let rev = DomovoiApp.view(&model).composer.draft_revision;
    let _ = run(&mut model, Event::PickSlashCommand { name: "security-review".into() });
    let view = DomovoiApp.view(&model);
    assert_eq!(view.composer.draft, "/security-review ");
    assert!(view.composer.draft_revision > rev, "the shell reloads the draft");
    assert!(view.composer.slash_commands.is_empty(), "a picked command closes the list");
}

#[test]
fn a_messages_images_load_through_signed_urls() {
    let (mut model, _) = connected();
    let sent = sent_requests(&run(&mut model, Event::SelectThread { thread_id: "t1".into() }));
    let sub = sent.iter().find(|r| r["tag"] == "orchestration.subscribeThread").unwrap()["id"].as_str().unwrap().to_owned();
    let snapshot = records::snapshot(1, json!({
        "thread": { "id": "t1" }, "runs": [],
        "messages": [{ "id": "m1", "role": "user", "text": "look", "attachments": [
            { "type": "image", "id": "t1-abc", "name": "shot.png", "mimeType": "image/png", "sizeBytes": 3 },
            { "type": "file", "id": "t1-doc", "name": "notes.md", "mimeType": "text/markdown", "sizeBytes": 3 }] }],
        "turnItems": [{ "id": "iu", "threadId": "t1", "runId": "r1", "type": "user_message", "messageId": "m1", "ordinal": 1, "status": "completed" }],
    }));
    let sent = sent_requests(&sock(&mut model, chunk(&sub, &[snapshot.clone()])));
    let asks: Vec<&Value> = sent.iter().filter(|r| r["tag"] == "assets.createUrl").collect();
    assert_eq!(asks.len(), 1, "one URL, for the image only");
    assert_eq!(asks[0]["payload"]["resource"], json!({ "_tag": "attachment", "attachmentId": "t1-abc", "fileName": "shot.png", "mimeType": "image/png", "disposition": "inline" }));
    let page = DomovoiApp.view(&model).thread.unwrap();
    assert!(page.items[0].images.is_empty(), "no image before its URL");
    assert_eq!(page.items[0].attachments, ["shot.png", "notes.md"]);

    // A second snapshot doesn't ask again while one is on its way.
    assert!(!sent_requests(&sock(&mut model, chunk(&sub, &[snapshot]))).iter().any(|r| r["tag"] == "assets.createUrl"));

    let id = asks[0]["id"].as_str().unwrap().to_owned();
    let _ = sock(&mut model, exit_ok(&id, json!({ "relativeUrl": "/api/assets/attachment/tok", "expiresAt": 1 })));
    let page = DomovoiApp.view(&model).thread.unwrap();
    assert_eq!(page.items[0].images.len(), 1);
    assert_eq!(page.items[0].images[0].url, "http://127.0.0.1:1/api/assets/attachment/tok");
    assert_eq!(page.items[0].images[0].name, "shot.png");
}

/// Images an agent looked at (a Claude `Read` of a PNG, Codex's
/// `view_image`) show on their step and on its folded group, through the
/// server's `media-file` links.
#[test]
fn images_the_agent_viewed_show_on_their_steps() {
    let (mut model, _) = connected();
    let sent = sent_requests(&run(&mut model, Event::SelectThread { thread_id: "t1".into() }));
    let sub = sent.iter().find(|r| r["tag"] == "orchestration.subscribeThread").unwrap()["id"].as_str().unwrap().to_owned();
    let tool = |id: &str, ord: u64, body: Value| {
        let mut v = json!({ "id": id, "threadId": "t1", "runId": "r1", "type": "dynamic_tool", "ordinal": ord, "status": "completed" });
        v.as_object_mut().unwrap().extend(body.as_object().unwrap().clone());
        v
    };
    let snapshot = records::snapshot(1, json!({
        "thread": { "id": "t1" }, "runs": [],
        "turnItems": [
            tool("read", 1, json!({ "toolName": "Read", "input": { "file_path": "/tmp/shot.png" }, "output": "" })),
            tool("view", 2, json!({ "toolName": "view_image", "input": { "path": "/tmp/b.jpg" }, "viewedImagePath": "/tmp/b.jpg" })),
            tool("text", 3, json!({ "toolName": "Read", "input": { "file_path": "/tmp/notes.md" }, "output": "1\thi" })),
        ],
    }));
    let sent = sent_requests(&sock(&mut model, chunk(&sub, &[snapshot])));
    let asks: Vec<&Value> = sent.iter().filter(|r| r["tag"] == "assets.createUrl").collect();
    assert_eq!(asks.iter().map(|a| a["payload"]["resource"].clone()).collect::<Vec<_>>(), [
        json!({ "_tag": "media-file", "threadId": "t1", "path": "/tmp/shot.png" }),
        json!({ "_tag": "media-file", "threadId": "t1", "path": "/tmp/b.jpg" }),
    ], "the images only");
    let page = DomovoiApp.view(&model).thread.unwrap();
    let lines: Vec<(&str, &str, &str, bool)> = page.items.iter().map(|i| (i.icon.as_str(), i.verb.as_str(), i.title.as_str(), i.expandable)).collect();
    assert_eq!(lines, [("image", "Viewed", "shot.png", false), ("image", "Viewed", "b.jpg", false), ("read", "Read", "notes.md", true)]);
    assert_eq!(page.items[0].group_summary, "Read 1 file and viewed 2 images");
    assert!(page.items[0].images.is_empty() && page.items[0].group_images.is_empty(), "no image before its URL");

    let id = asks[0]["id"].as_str().unwrap().to_owned();
    let expires_at = model.now_ms + 300_000;
    let _ = sock(&mut model, exit_ok(&id, json!({ "relativeUrl": "/api/assets/project-image/tok", "expiresAt": expires_at })));
    let page = DomovoiApp.view(&model).thread.unwrap();
    assert_eq!(page.items[0].images.iter().map(|i| (i.name.as_str(), i.url.as_str())).collect::<Vec<_>>(), [("shot.png", "http://127.0.0.1:1/api/assets/project-image/tok")]);
    assert_eq!(page.items[0].group_images.len(), 1, "the folded group shows it too");
    let sent = sent_requests(&run(&mut model, Event::Tick { now_ms: expires_at }));
    let renewed = sent.iter().find(|r| r["tag"] == "assets.createUrl").expect("the expired preview is renewed");
    assert_eq!(renewed["payload"]["resource"]["path"], "/tmp/shot.png");
    let _ = sock(&mut model, exit_ok(renewed["id"].as_str().unwrap(), json!({
        "relativeUrl": "/api/assets/none/project-favicon-missing",
        "expiresAt": expires_at + 300_000,
    })));
    let page = DomovoiApp.view(&model).thread.unwrap();
    assert_eq!(page.items[0].badge, "Image no longer available");
    assert!(page.items[0].images.is_empty());
    assert!(!page.items[0].expandable);
}

#[test]
fn snoozed_threads_wait_apart_and_unread_ones_can_be_listed_alone() {
    let (mut model, _) = connected();
    model.now_ms = domovoi_core::millis_from_iso("2026-11-15T12:00:00.000Z");
    model.shown_ms = model.now_ms;
    model.projects = vec![records::project(json!({ "id": "p1", "title": "calc", "workspaceRoot": "/w/calc" }))];
    let thread = |id: &str, extra: Value| {
        let mut t = json!({ "id": id, "projectId": "p1", "title": id, "updatedAt": "2026-11-15T11:00:00.000Z", "status": "completed", "lastVisitedAt": "2026-11-15T11:30:00.000Z" });
        t.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        records::shell(t)
    };
    model.threads = vec![
        thread("read", json!({})),
        thread("unread", json!({ "lastVisitedAt": "2026-11-15T10:00:00.000Z" })),
        thread("later", json!({ "snoozedUntil": "2026-11-15T15:00:00.000Z" })),
        thread("soon", json!({ "snoozedUntil": "2026-11-15T13:00:00.000Z" })),
        thread("woke", json!({ "snoozedUntil": "2026-11-15T09:00:00.000Z" })),
    ];
    model.sidebar_layout.grouping = Grouping::Project;
    let view = DomovoiApp.view(&model);
    let listed: Vec<&str> = view.sidebar.projects[0].threads.iter().map(|t| t.id.as_str()).collect();
    assert!(listed.contains(&"read") && listed.contains(&"woke") && !listed.contains(&"later"), "{listed:?}");
    let snoozed: Vec<&str> = view.sidebar.snoozed.iter().map(|t| t.id.as_str()).collect();
    assert_eq!(snoozed, ["soon", "later"], "the next to wake first");
    assert_eq!(view.sidebar.snoozed[0].snoozed_until_ms, domovoi_core::millis_from_iso("2026-11-15T13:00:00.000Z"));

    let _ = run(&mut model, Event::SetUnreadOnly { on: true });
    let view = DomovoiApp.view(&model);
    let listed: Vec<&str> = view.sidebar.projects.iter().flat_map(|p| p.threads.iter()).map(|t| t.id.as_str()).collect();
    assert_eq!(listed, ["unread"]);
    assert!(view.sidebar.unread_only && view.sidebar.snoozed.is_empty());
    let _ = run(&mut model, Event::SetUnreadOnly { on: false });

    let until = domovoi_core::millis_from_iso("2026-11-16T08:00:00.000Z");
    let sent = sent_requests(&run(&mut model, Event::SnoozeThread { thread_id: "read".into(), until_ms: until }));
    assert_eq!((sent[0]["payload"]["type"].as_str(), sent[0]["payload"]["snoozedUntil"].as_str()), (Some("thread.snooze"), Some("2026-11-16T08:00:00.000Z")));
    assert_eq!(DomovoiApp.view(&model).toast.unwrap().action_label, "Undo");
    let sent = sent_requests(&run(&mut model, Event::ToastAction));
    assert_eq!(sent[0]["payload"]["type"], "thread.unsnooze", "Undo wakes it");
    let sent = sent_requests(&run(&mut model, Event::SnoozeThread { thread_id: "read".into(), until_ms: 0 }));
    assert_eq!(sent[0]["payload"]["type"], "thread.unsnooze");
    let sent = sent_requests(&run(&mut model, Event::MarkThreadUnread { thread_id: "read".into() }));
    assert_eq!(sent[0]["payload"]["type"], "thread.mark-unread");
}

#[test]
fn a_scheduled_task_can_choose_any_thread_of_its_project_whatever_the_list_layout() {
    let (mut model, _) = connected();
    model.now_ms = domovoi_core::millis_from_iso("2026-11-15T12:00:00.000Z");
    model.shown_ms = model.now_ms;
    model.projects = vec![
        records::project(json!({ "id": "p1", "title": "calc", "workspaceRoot": "/w/calc" })),
        records::project(json!({ "id": "p2", "title": "web", "workspaceRoot": "/w/web" })),
    ];
    let thread = |id: &str, project: &str, updated: &str, extra: Value| {
        let mut t = json!({ "id": id, "projectId": project, "title": format!("{id} title"), "updatedAt": updated, "status": "completed" });
        t.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        records::shell(t)
    };
    model.threads = vec![
        thread("older", "p1", "2026-11-14T11:00:00.000Z", json!({})),
        thread("newer", "p1", "2026-11-15T11:00:00.000Z", json!({})),
        thread("snoozed", "p1", "2026-11-15T10:00:00.000Z", json!({ "snoozedUntil": "2026-11-16T09:00:00.000Z" })),
        thread("elsewhere", "p2", "2026-11-15T09:00:00.000Z", json!({})),
    ];
    model.archived = vec![thread("shelved", "p1", "2026-11-13T10:00:00.000Z", json!({ "archivedAt": "2026-11-14T10:00:00.000Z" }))];
    let task = |id: &str, thread: Option<&str>| serde_json::from_value(scheduled_task_json(id, thread)).expect("a scheduled task");
    model.pages.scheduled = vec![task("bound", Some("shelved")), task("fresh", None)];
    assert_eq!(model.sidebar_layout.grouping, Grouping::Status, "the default layout, whose projects carry no threads");
    assert!(DomovoiApp.view(&model).settings.scheduled_thread_choices.is_empty(), "not read until the scheduled tasks are open");
    let _ = run(&mut model, Event::LoadSettings { section: "scheduled".into() });

    let view = DomovoiApp.view(&model);
    let choices: Vec<(&str, &str, &str)> =
        view.settings.scheduled_thread_choices.iter().map(|c| (c.id.as_str(), c.project_id.as_str(), c.detail.as_str())).collect();
    assert_eq!(choices, [("newer", "p1", "1h"), ("snoozed", "p1", "2h"), ("elsewhere", "p2", "3h"), ("older", "p1", "1d"), ("shelved", "p1", "archived")]);
    let titles: Vec<(&str, &str)> = view.settings.scheduled_tasks.iter().map(|t| (t.name.as_str(), t.thread_title.as_str())).collect();
    assert_eq!(titles, [("bound", "shelved title"), ("fresh", "")]);
}

/// A scheduled task as the server sends it: hourly, in `p1`, bound to `thread` when given.
fn scheduled_task_json(id: &str, thread: Option<&str>) -> Value {
    json!({
        "id": id, "title": id, "prompt": "Run the tests", "enabled": true, "schedule": { "type": "interval", "everyMs": 3_600_000 },
        "projectId": "p1", "threadId": thread, "threadMode": if thread.is_some() { "continue" } else { "new_thread" },
        "workspaceStrategy": { "type": "root" }, "modelSelection": { "instanceId": "codex", "model": "gpt-6" },
        "runtimeMode": "full-access", "interactionMode": "default", "createdBy": "user", "creationSource": "web",
        "createdAt": "2026-11-01T00:00:00.000Z", "updatedAt": "2026-11-01T00:00:00.000Z", "nextRunAt": null, "lastRunAt": null,
        "lastRunStatus": "never", "lastRunError": null, "runCount": 0
    })
}

#[test]
fn a_scheduled_task_shows_its_runs_and_a_run_by_hand_offers_its_thread() {
    let (mut model, _) = connected();
    model.now_ms = domovoi_core::millis_from_iso("2026-11-15T12:00:00.000Z");
    model.shown_ms = model.now_ms;
    model.projects = vec![records::project(json!({ "id": "p1", "title": "calc", "workspaceRoot": "/w/calc" }))];
    model.threads = vec![records::shell(json!({ "id": "t1", "projectId": "p1", "title": "Nightly thread", "updatedAt": "2026-11-15T11:00:00.000Z", "status": "completed" }))];
    let request = |effects: &[Effect], tag: &str| sent_requests(effects).into_iter().find(|r| r["tag"] == tag).unwrap_or_else(|| panic!("no {tag}"));

    // The list, then every task's latest run with it.
    let subscribe = request(&run(&mut model, Event::LoadSettings { section: "scheduled".into() }), "scheduledTasks.subscribe");
    let list = json!({ "tasks": [scheduled_task_json("bound", Some("t1"))], "timeZone": "Europe/Warsaw" });
    let latest = request(&sock(&mut model, chunk(subscribe["id"].as_str().unwrap(), &[list])), "scheduledTasks.listRuns");
    assert_eq!(latest["payload"], json!({}));
    let queued = json!({ "runs": [{ "taskId": "bound", "startedAt": "2026-11-15T11:58:00.000Z", "trigger": "scheduled", "threadId": "t1", "status": "queued", "error": null }] });
    let _ = sock(&mut model, exit_ok(latest["id"].as_str().unwrap(), queued));
    let view = DomovoiApp.view(&model);
    let row = &view.settings.scheduled_tasks[0];
    assert_eq!((row.run_state.as_str(), row.last_thread_id.as_str()), ("Waiting in its thread", "t1"));

    // One task's runs.
    let runs = request(&run(&mut model, Event::LoadScheduledRuns { id: "bound".into() }), "scheduledTasks.listRuns");
    assert_eq!(runs["payload"], json!({ "id": "bound" }));
    let history = json!({ "runs": [
        { "taskId": "bound", "startedAt": "2026-11-15T11:58:00.000Z", "trigger": "manual", "threadId": "t1", "status": "completed", "error": null },
        { "taskId": "bound", "startedAt": "2026-11-15T10:00:00.000Z", "trigger": "scheduled", "threadId": null, "status": "failed", "error": "No provider is ready." }
    ] });
    let _ = sock(&mut model, exit_ok(runs["id"].as_str().unwrap(), history));
    let view = DomovoiApp.view(&model);
    assert_eq!(view.settings.scheduled_runs_task, "bound");
    let shown: Vec<(&str, &str, &str, &str)> =
        view.settings.scheduled_runs.iter().map(|r| (r.trigger.as_str(), r.status.as_str(), r.error.as_str(), r.thread_title.as_str())).collect();
    assert_eq!(shown, [("Run now", "Finished", "", "Nightly thread"), ("On schedule", "Failed", "No provider is ready.", "")]);
    assert_eq!(view.settings.scheduled_runs[0].started, "2m · 2026-11-15 11:58");

    // Run by hand: the toast offers the thread it went to.
    let ran = request(&run(&mut model, Event::RunScheduledNow { id: "bound".into() }), "scheduledTasks.runNow");
    let started = request(&sock(&mut model, exit_ok(ran["id"].as_str().unwrap(), json!({ "task": scheduled_task_json("bound", Some("t1")) }))), "scheduledTasks.listRuns");
    assert_eq!(started["payload"], json!({ "id": "bound", "limit": 1 }));
    let one = json!({ "runs": [{ "taskId": "bound", "startedAt": "2026-11-15T12:00:00.000Z", "trigger": "manual", "threadId": "t1", "status": "queued", "error": null }] });
    let _ = sock(&mut model, exit_ok(started["id"].as_str().unwrap(), one));
    let toast = DomovoiApp.view(&model).toast.expect("a toast");
    assert_eq!((toast.message.as_str(), toast.action_label.as_str()), ("Started “bound”.", "Open Thread"));
    let _ = run(&mut model, Event::ToastAction);
    assert_eq!(DomovoiApp.view(&model).sidebar.selected_thread_id.as_deref(), Some("t1"));
}

/// Rows of a status section, by kind (empty when it is not listed or collapsed).
fn section_rows<'a>(view: &'a domovoi_core::view::ViewModel, kind: &str) -> Vec<&'a str> {
    view.sidebar.sections.iter().find(|s| s.kind == kind).map(|s| s.rows.iter().map(|t| t.id.as_str()).collect()).unwrap_or_default()
}

fn section_count(view: &domovoi_core::view::ViewModel, kind: &str) -> u32 {
    view.sidebar.sections.iter().find(|s| s.kind == kind).map_or(0, |s| s.count)
}

#[test]
fn threads_are_grouped_by_what_they_need_from_you() {
    let (mut model, _) = connected();
    model.now_ms = domovoi_core::millis_from_iso("2026-11-15T12:00:00.000Z");
    model.shown_ms = model.now_ms;
    model.projects = vec![records::project(json!({ "id": "p1", "title": "calc", "workspaceRoot": "/w/calc" }))];
    model.providers = vec![];
    let thread = |id: &str, extra: Value| {
        let mut t = json!({ "id": id, "projectId": "p1", "title": id, "updatedAt": "2026-11-15T11:00:00.000Z", "status": "completed", "lastVisitedAt": "2026-11-15T11:30:00.000Z",
            "modelSelection": { "instanceId": "claudeAgent", "model": "claude-opus" } });
        t.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        records::shell(t)
    };
    model.threads = vec![
        thread("read", json!({})),
        thread("unread", json!({ "lastVisitedAt": "2026-11-15T10:00:00.000Z", "updatedAt": "2026-11-15T11:10:00.000Z" })),
        thread("asks", json!({ "pendingRuntimeRequest": { "id": "r1", "kind": "user_input", "createdAt": "2026-11-15T11:00:00.000Z" } })),
        thread("approve", json!({ "pendingRuntimeRequest": { "id": "r2", "kind": "command", "createdAt": "2026-11-15T11:00:00.000Z" }, "updatedAt": "2026-11-15T10:00:00.000Z" })),
        thread("busy", json!({ "status": "running", "activeRunId": "run-1", "activityRunStatus": "running", "activityRunStartedAt": "2026-11-15T11:54:00.000Z" })),
        thread("planning", json!({ "status": "running", "activeRunId": "run-2", "activityRunStatus": "running", "activityRunStartedAt": "2026-11-15T11:54:00.000Z",
            "updatedAt": "2026-11-15T10:00:00.000Z", "interactionMode": "plan" })),
        thread("pinned", json!({ "pinnedAt": "2026-11-15T09:00:00.000Z" })),
        thread("settled-unread", json!({ "lastVisitedAt": null, "settledAt": "2026-11-15T11:00:00.000Z", "settledOverride": "settled" })),
        thread("later", json!({ "snoozedUntil": "2026-11-15T15:00:00.000Z" })),
        thread("broke", json!({ "status": "failed", "lastVisitedAt": null })),
    ];
    model.archived = vec![thread("old", json!({ "archivedAt": "2026-11-14T10:00:00.000Z" }))];
    let view = DomovoiApp.view(&model);
    assert_eq!(view.sidebar.grouping, "status", "grouped by status unless asked otherwise");
    assert!(view.sidebar.projects.iter().all(|p| p.title == "calc" && p.threads.is_empty()), "the projects, for pickers, without their threads");
    let kinds: Vec<&str> = view.sidebar.sections.iter().map(|s| s.kind.as_str()).collect();
    assert_eq!(kinds, ["inbox", "running", "inactive", "archived"]);
    assert_eq!(view.sidebar.sections[2].title, "Inactive");
    // Waiting on you first, then pinned, then the rest by latest change.
    assert_eq!(section_rows(&view, "inbox"), ["asks", "approve", "pinned", "unread", "broke"]);
    assert_eq!(section_rows(&view, "running"), ["busy", "planning"]);
    // Inactive and Archived start collapsed: counted, no rows.
    assert_eq!((section_count(&view, "inactive"), section_rows(&view, "inactive").len()), (3, 0));
    assert_eq!((section_count(&view, "archived"), section_rows(&view, "archived").len()), (1, 0));

    let inbox = &view.sidebar.sections[0].rows;
    let label = |id: &str| inbox.iter().find(|t| t.id == id).unwrap().status_label.clone();
    assert_eq!((label("asks"), label("approve"), label("broke"), label("unread")), ("Question".into(), "Needs you".into(), "Failed".into(), String::new()));
    assert_eq!(view.sidebar.sections[1].rows[0].status_label, "Working 6m");
    let planning = &view.sidebar.sections[1].rows[1];
    assert_eq!((planning.status_label.as_str(), planning.plan_mode), ("Planning 6m", true), "a run in plan mode says so");
    assert!(!view.sidebar.sections[1].rows[0].plan_mode);
    let row = |id: &str| inbox.iter().find(|t| t.id == id).unwrap();
    assert!(row("asks").question && !row("approve").question, "a question is told from an approval");
    let busy = &view.sidebar.sections[1].rows[0];
    assert_eq!(busy.working_since_ms, domovoi_core::millis_from_iso("2026-11-15T11:54:00.000Z"), "for the Live Activity's timer");
    assert_eq!(row("broke").working_since_ms, 0, "nothing under way");
    assert_eq!((inbox[0].project_title.as_str(), inbox[0].model_label.as_str()), ("calc", "claude-opus"), "the model's slug when no provider names it");

    // Expanding a section lists it, and is saved on this device.
    let effects = run(&mut model, Event::ToggleSection { section: "inactive".into() });
    let saved = effects.iter().find_map(|e| match e {
        Effect::Store(req) => match &req.operation {
            StoreOperation::Save { key, value } if key == "sidebarLayout" => Some(value.clone()),
            _ => None,
        },
        _ => None,
    });
    assert_eq!(
        serde_json::from_str::<Value>(&saved.expect("saved layout")).unwrap(),
        json!({ "grouping": "status", "collapsedSections": ["archived"], "collapsedProjects": [] })
    );
    let view = DomovoiApp.view(&model);
    assert_eq!(section_rows(&view, "inactive"), ["read", "settled-unread", "later"]);
    assert!(view.sidebar.surface_threads.iter().take(5).map(|t| t.id.as_str()).eq(["asks", "approve", "busy", "planning", "unread"]), "the glance list ranks what needs you first");

    // Unread-only leaves just the Inbox; a search lists every section with matches, open.
    let _ = run(&mut model, Event::SetUnreadOnly { on: true });
    let kinds: Vec<String> = DomovoiApp.view(&model).sidebar.sections.iter().map(|s| s.kind.clone()).collect();
    assert_eq!(kinds, ["inbox"]);
    let _ = run(&mut model, Event::SetUnreadOnly { on: false });
    model.search.query = "old".into();
    let view = DomovoiApp.view(&model);
    assert_eq!(view.sidebar.sections.iter().map(|s| (s.kind.as_str(), s.expanded)).collect::<Vec<_>>(), [("archived", true)]);
    assert_eq!(section_rows(&view, "archived"), ["old"]);
    model.search.query.clear();

    // A section with no threads is left out, the Inbox too; unread-only with nothing new lists none.
    let inbox: HashSet<&str> = ["asks", "approve", "pinned", "unread", "broke"].into();
    let all = std::mem::take(&mut model.threads);
    model.threads = all.iter().filter(|t| !inbox.contains(t.id.as_str())).cloned().collect();
    let kinds: Vec<String> = DomovoiApp.view(&model).sidebar.sections.iter().map(|s| s.kind.clone()).collect();
    assert_eq!(kinds, ["running", "inactive", "archived"]);
    let _ = run(&mut model, Event::SetUnreadOnly { on: true });
    assert!(DomovoiApp.view(&model).sidebar.sections.is_empty());
    let _ = run(&mut model, Event::SetUnreadOnly { on: false });
    model.threads = all;

    // Grouped by project again: the old layout, and no sections.
    let _ = run(&mut model, Event::SetSidebarGrouping { grouping: "project".into() });
    let view = DomovoiApp.view(&model);
    assert!(view.sidebar.sections.is_empty() && view.sidebar.projects[0].title == "calc" && !view.sidebar.surface_threads.is_empty());
}

#[test]
fn inactive_section_can_show_all_rows() {
    let (mut model, _) = connected();
    let limit = domovoi_core::sidebar::ROWS_PER_SECTION;
    model.threads = (0..=limit)
        .map(|id| records::shell(json!({
            "id": format!("idle-{id}"),
            "projectId": "p1",
            "title": "Idle thread",
            "updatedAt": "2026-11-15T11:00:00.000Z",
            "status": "completed",
            "lastVisitedAt": "2026-11-15T11:30:00.000Z",
        })))
        .collect();
    let _ = run(&mut model, Event::ToggleSection { section: "inactive".into() });
    let view = DomovoiApp.view(&model);
    let inactive = view.sidebar.sections.iter().find(|section| section.kind == "inactive").unwrap();
    assert_eq!((inactive.rows.len(), inactive.hidden_count), (limit, 1));

    let _ = run(&mut model, Event::ShowAllInSection { section: "inactive".into() });
    let view = DomovoiApp.view(&model);
    let inactive = view.sidebar.sections.iter().find(|section| section.kind == "inactive").unwrap();
    assert_eq!((inactive.rows.len(), inactive.hidden_count), (limit + 1, 0));
}

#[test]
fn a_thread_opened_from_the_inbox_stays_there_until_you_leave_it() {
    let (mut model, _) = connected();
    model.now_ms = domovoi_core::millis_from_iso("2026-11-15T12:00:00.000Z");
    model.shown_ms = model.now_ms;
    model.projects = vec![records::project(json!({ "id": "p1", "title": "calc", "workspaceRoot": "/w/calc" }))];
    let thread = |id: &str, extra: Value| {
        let mut t = json!({ "id": id, "projectId": "p1", "title": id, "updatedAt": "2026-11-15T11:00:00.000Z", "status": "completed", "lastVisitedAt": "2026-11-15T10:00:00.000Z" });
        t.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        records::shell(t)
    };
    model.threads = vec![thread("done", json!({})), thread("other", json!({ "lastVisitedAt": "2026-11-15T11:30:00.000Z" }))];
    let _ = run(&mut model, Event::ToggleSection { section: "inactive".into() });
    assert_eq!(section_rows(&DomovoiApp.view(&model), "inbox"), ["done"], "finished since you looked");

    // Opened, it is read, yet its row stays put while it is open.
    let _ = run(&mut model, Event::SelectThread { thread_id: "done".into() });
    let view = DomovoiApp.view(&model);
    assert_eq!(section_rows(&view, "inbox"), ["done"]);
    assert!(!view.sidebar.sections[0].rows[0].unread);
    model.threads[0] = thread("done", json!({ "lastVisitedAt": "2026-11-15T11:59:00.000Z" }));

    // Opening another thread lets the read one move to Inactive.
    let _ = run(&mut model, Event::SelectThread { thread_id: "other".into() });
    let view = DomovoiApp.view(&model);
    assert!(section_rows(&view, "inbox").is_empty());
    let mut inactive = section_rows(&view, "inactive");
    inactive.sort_unstable();
    assert_eq!(inactive, ["done", "other"]);
}

#[test]
fn the_list_layout_is_kept_on_the_device_across_launches() {
    let mut shell = Shell::paired();
    let mut model = Model::default();
    let _ = shell.drive(&mut model, Event::Launch { surface: "mobile".into(), device_label: String::new(), device_os: String::new() });
    assert_eq!(DomovoiApp.view(&model).sidebar.grouping, "status");
    let _ = shell.drive(&mut model, Event::SetSidebarGrouping { grouping: "project".into() });
    let _ = shell.drive(&mut model, Event::ToggleProject { project_id: "p1".into() });

    let mut model = Model::default();
    let _ = shell.drive(&mut model, Event::Launch { surface: "mobile".into(), device_label: String::new(), device_os: String::new() });
    assert_eq!(model.sidebar_layout.grouping, Grouping::Project);
    assert!(model.sidebar_layout.collapsed_projects.contains("p1"));

    // Keep the saved section name readable by clients from before the rename.
    let layout = json!({ "grouping": "project", "collapsedSections": ["hibernating"], "collapsedProjects": ["p1"] });
    shell.store.insert("sidebarLayout".into(), layout.to_string());
    let mut model = Model::default();
    let _ = shell.drive(&mut model, Event::Launch { surface: "mobile".into(), device_label: String::new(), device_os: String::new() });
    assert_eq!(model.sidebar_layout.grouping, Grouping::Project);
    assert_eq!(model.sidebar_layout.collapsed_sections, [domovoi_core::sidebar::Section::Inactive].into());
    let _ = shell.drive(&mut model, Event::ToggleSection { section: "inactive".into() });
    assert!(model.sidebar_layout.collapsed_sections.is_empty());
    let _ = shell.drive(&mut model, Event::ToggleSection { section: "inactive".into() });
    assert_eq!(serde_json::from_str::<Value>(&shell.store["sidebarLayout"]).unwrap(), layout);

    // A layout this version cannot read leaves the default.
    shell.store.insert("sidebarLayout".into(), "{\"grouping\":\"sideways\"}".into());
    let mut model = Model::default();
    let _ = shell.drive(&mut model, Event::Launch { surface: "mobile".into(), device_label: String::new(), device_os: String::new() });
    assert_eq!(model.sidebar_layout, domovoi_core::sidebar::SidebarLayout::default());
}

#[test]
fn an_offline_launch_shows_the_saved_list_until_the_server_answers() {
    let mut shell = Shell::paired();
    let mut model = Model::default();
    let effects = shell.drive(&mut model, Event::Connect { server_url: "http://127.0.0.1:1".into(), pairing_code: "CODE".into() });
    let _ = socket_url(&effects).expect("socket opened");
    let sent = sent_requests(&sock(&mut model, SocketEvent::Opened));
    let shell_id = sent.iter().find(|r| r["tag"] == "orchestration.subscribeShell").unwrap()["id"].as_str().unwrap().to_owned();
    let snapshot = records::shell_snapshot(1, json!([{ "id": "p1", "title": "calc", "workspaceRoot": "/w/calc" }]), json!([{ "id": "t1", "projectId": "p1", "title": "Saved one", "updatedAt": "1" }]));
    let _ = sock(&mut model, chunk(&shell_id, &[snapshot.clone()]));
    assert!(!DomovoiApp.view(&model).connection.showing_saved);

    // Going to the background saves the list.
    let _ = shell.drive(&mut model, Event::Suspend);
    let env = model.active_env.clone().unwrap();
    let saved = shell.store.get(&format!("cache:shell:{env}")).cloned().expect("the list was saved");

    // A new launch with no server: the saved list shows, marked as such.
    let mut fresh = Model::default();
    fresh.active_env = Some(env.clone());
    let _ = run(&mut fresh, Event::CacheLoaded { environment_id: env.clone(), value: Some(saved.clone()) });
    fresh.sidebar_layout.grouping = Grouping::Project;
    let view = DomovoiApp.view(&fresh);
    assert!(view.connection.showing_saved);
    assert_eq!(view.sidebar.projects[0].threads[0].title, "Saved one");
    // Another server's copy, or one arriving after live data, is ignored.
    let mut other = Model::default();
    other.active_env = Some("elsewhere".into());
    let _ = run(&mut other, Event::CacheLoaded { environment_id: env, value: Some(saved) });
    assert!(other.threads.is_empty());
    // The server's own list replaces it.
    fresh.connection = domovoi_core::model::Connection::Connected;
    let _ = domovoi_core::replies::handle(&mut fresh, domovoi_core::rpc::Reply::Chunk { id: "x".into(), route: domovoi_core::rpc::Route::Shell, values: vec![snapshot] });
    assert!(!DomovoiApp.view(&fresh).connection.showing_saved);
}

#[test]
fn an_approval_is_answered_from_its_notification() {
    let (mut model, _) = connected();
    let server = model.environment.as_ref().map(|environment| environment.environment_id.as_str().to_owned()).unwrap_or_default();
    let sent = sent_requests(&run(&mut model, Event::RespondFromNotification { environment_id: server, thread_id: "t9".into(), request_id: "rq".into(), decision: "accept".into() }));
    let cmd = &sent.iter().find(|r| r["tag"] == "orchestration.dispatchCommand").expect("sent")["payload"];
    assert_eq!((cmd["type"].as_str(), cmd["threadId"].as_str(), cmd["requestId"].as_str(), cmd["decision"].as_str()), (Some("runtime-request.respond"), Some("t9"), Some("rq"), Some("accept")));
    // Another server's approval is left for the app (it would switch servers unseen).
    let sent = sent_requests(&run(&mut model, Event::RespondFromNotification { environment_id: "other-server".into(), thread_id: "t9".into(), request_id: "rq".into(), decision: "accept".into() }));
    assert!(sent.is_empty());
}

#[test]
fn a_live_activity_answers_its_threads_pending_approval() {
    let (mut model, _) = connected();
    let server = model.environment.as_ref().map(|environment| environment.environment_id.as_str().to_owned()).unwrap_or_default();
    model.threads = vec![
        records::shell(json!({ "id": "t1", "projectId": "p1", "title": "T", "pendingRuntimeRequest": { "id": "rq", "kind": "command" } })),
        records::shell(json!({ "id": "t2", "projectId": "p1", "title": "Q", "pendingRuntimeRequest": { "id": "rq2", "kind": "user_input" } })),
        records::shell(json!({ "id": "t3", "projectId": "p1", "title": "Idle" })),
    ];
    let respond = |thread: &str| Event::RespondToThread { environment_id: server.clone(), thread_id: thread.into(), decision: "accept".into() };
    let sent = sent_requests(&run(&mut model, respond("t1")));
    let cmd = &sent.iter().find(|r| r["tag"] == "orchestration.dispatchCommand").expect("sent")["payload"];
    assert_eq!((cmd["requestId"].as_str(), cmd["decision"].as_str()), (Some("rq"), Some("accept")));
    assert!(sent_requests(&run(&mut model, respond("t2"))).is_empty(), "a question needs the app");
    assert!(sent_requests(&run(&mut model, respond("t3"))).is_empty(), "nothing waiting, nothing sent");
}

#[test]
fn a_question_is_answered_from_its_notification() {
    let (mut model, _) = connected();
    let server = model.environment.as_ref().map(|environment| environment.environment_id.as_str().to_owned()).unwrap_or_default();
    let answer = |server: &str, text: &str| Event::AnswerFromNotification { environment_id: server.into(), thread_id: "t9".into(), request_id: "rq".into(), question_id: "name".into(), text: text.into() };
    let sent = sent_requests(&run(&mut model, answer(&server, "  feature/search ")));
    let cmd = &sent.iter().find(|r| r["tag"] == "orchestration.dispatchCommand").expect("sent")["payload"];
    assert_eq!((cmd["type"].as_str(), cmd["requestId"].as_str()), (Some("runtime-request.respond"), Some("rq")));
    assert_eq!(cmd["answers"], json!({ "name": ["feature/search"] }));
    assert!(sent_requests(&run(&mut model, answer(&server, "   "))).is_empty(), "an empty reply answers nothing");
    assert!(sent_requests(&run(&mut model, answer("other-server", "x"))).is_empty(), "another server's is left for the app");
}

#[test]
fn pull_requests_merge_review_and_start_a_thread_in_their_worktree() {
    let (mut model, _) = connected();
    model.projects = vec![records::project(json!({ "id": "p1", "title": "calc", "workspaceRoot": "/w/calc" }))];
    model.pages.prs = vec![records::pull_request(json!({ "projectId": "p1", "repository": "o/calc", "number": 12, "title": "Fix parser", "state": "open", "isDraft": false }))];
    let key = "p1#o/calc#12".to_owned();

    let sent = sent_requests(&run(&mut model, Event::PullRequestAction { key: key.clone(), action: "merge".into(), merge_method: "squash".into() }));
    let call = sent.iter().find(|r| r["tag"] == "pullRequests.runAction").expect("runAction");
    assert_eq!(call["payload"], json!({ "action": "merge", "mergeMethod": "squash", "projectId": "p1", "repository": "o/calc", "number": 12 }));
    assert!(DomovoiApp.view(&model).pull_requests.busy);
    let sent = sent_requests(&sock(&mut model, exit_ok(call["id"].as_str().unwrap(), json!({}))));
    assert!(!DomovoiApp.view(&model).pull_requests.busy);
    assert_eq!(DomovoiApp.view(&model).toast.unwrap().message, "Merged.");
    assert!(sent.iter().any(|r| r["tag"] == "pullRequests.list"), "the list refreshes");

    let sent = sent_requests(&run(&mut model, Event::ReviewPullRequest { key: key.clone(), verdict: "approve".into(), body: " ok ".into() }));
    assert_eq!(sent[0]["payload"]["verdict"], "approve");
    assert_eq!(sent[0]["payload"]["body"], "ok");
    let sent = sent_requests(&run(&mut model, Event::CommentOnPullRequest { key: key.clone(), body: "nice".into() }));
    assert_eq!((sent[0]["tag"].as_str(), sent[0]["payload"]["body"].as_str()), (Some("pullRequests.comment"), Some("nice")));

    // A thread from the pull request goes into the worktree it was checked out in.
    let sent = sent_requests(&run(&mut model, Event::StartPullRequestThread { key: key.clone() }));
    let prep = sent.iter().find(|r| r["tag"] == "git.preparePullRequestThread").expect("prepare");
    assert_eq!(prep["payload"], json!({ "cwd": "/w/calc", "reference": "12", "mode": "worktree" }));
    let pull_request = json!({ "number": 12, "title": "Fix parser", "url": "https://example.test/o/calc/pull/12", "baseBranch": "main", "headBranch": "fix-parser", "state": "open" });
    let _ = sock(&mut model, exit_ok(prep["id"].as_str().unwrap(), json!({ "pullRequest": pull_request, "worktreePath": "/wt/pr-12", "branch": "fix-parser" })));
    let view = DomovoiApp.view(&model);
    assert_eq!(view.route, "new-thread");
    assert_eq!(view.composer.pr_worktree, "PR #12 · fix-parser");
}

#[test]
fn a_quoted_line_is_added_to_the_draft_as_its_own_paragraph() {
    let (mut model, _) = connected();
    let _ = run(&mut model, Event::SetDraft { text: "Why is this here?  ".into() });
    let rev = DomovoiApp.view(&model).composer.draft_revision;
    let _ = run(&mut model, Event::AppendToDraft { text: "`src/a.rs:3`".into() });
    let view = DomovoiApp.view(&model);
    assert_eq!(view.composer.draft, "Why is this here?\n\n`src/a.rs:3`");
    assert!(view.composer.draft_revision > rev, "the composer reloads it");
}

/// A Claude thread mid-turn: a message queued behind it, a subagent at work
/// in its own thread, a proposed plan from an earlier turn and the context
/// window it reported.
fn working_thread(model: &mut Model) {
    model.providers = vec![records::provider(json!({ "instanceId": "claudeAgent", "driver": "claudeAgent", "displayName": "Claude", "status": "ready", "enabled": true,
        "models": [{ "slug": "claude-haiku-5-5", "name": "Haiku 5.5", "isDefault": true }] }))];
    let shell = json!({ "id": "t1", "projectId": "p1", "title": "Search", "providerInstanceId": "claudeAgent",
        "modelSelection": { "instanceId": "claudeAgent", "model": "claude-haiku-5-5" }, "status": "running",
        "activeRunId": "r2", "activityRunStatus": "running", "pendingRuntimeRequest": null, "lineage": null, "updatedAt": "2026-11-01T00:00:02.000Z" });
    let child = json!({ "id": "c1", "projectId": "p1", "title": "Find callers", "status": "idle", "updatedAt": "2026-11-01T00:00:02.000Z",
        "lineage": { "parentThreadId": "t1", "relationshipToParent": "subagent", "rootThreadId": "t1" } });
    model.projects = vec![records::project(json!({ "id": "p1", "title": "App", "workspaceRoot": "/w" }))];
    model.threads = vec![records::shell(shell), records::shell(child)];
    let projection = json!({
        "thread": { "id": "t1", "activeProviderThreadId": "pt1" },
        "runs": [
            { "id": "r1", "ordinal": 1, "status": "completed", "startedAt": "2026-11-01T00:00:00.000Z" },
            { "id": "r2", "ordinal": 2, "status": "running", "startedAt": "2026-11-01T00:00:01.000Z" },
            { "id": "r4", "ordinal": 4, "status": "queued", "queuePosition": 2, "userMessageId": "m4" },
            { "id": "r3", "ordinal": 3, "status": "queued", "queuePosition": 1, "userMessageId": "m3" },
        ],
        "messages": [
            { "id": "m3", "runId": "r3", "role": "user", "text": "then add tests" },
            { "id": "m4", "runId": "r4", "role": "user", "text": "and docs" },
        ],
        "providerThreads": [{ "id": "pt1", "contextUsage": { "usedTokens": 172_000, "maxTokens": 200_000 },
            "pendingBackgroundTasks": [{ "kind": "subagent", "taskId": "task-1", "childThreadId": "c1", "awaited": true }] }],
        "plans": [{ "id": "plan1", "kind": "proposed_plan", "status": "active" }],
        "subagents": [{ "id": "node-sub", "status": "running", "nativeTaskRef": { "nativeId": "toolu_1" }, "childThreadId": "c1", "progress": "Running in the background" }],
        "turnItems": [
            { "id": "i-plan", "runId": "r1", "ordinal": 1, "type": "proposed_plan", "planId": "plan1", "markdown": "1. Do it", "status": "completed" },
            { "id": "i-todo", "runId": "r2", "ordinal": 2, "type": "todo_list", "status": "completed",
              "steps": [{ "text": "Read", "status": "completed" }, { "text": "Write", "status": "running" }] },
            { "id": "i-todo-tool", "runId": "r2", "ordinal": 3, "type": "dynamic_tool", "toolName": "TodoWrite", "status": "completed" },
            { "id": "i-sub", "runId": "r2", "ordinal": 4, "type": "subagent", "subagentId": "node-sub", "title": "Find callers",
              "prompt": "Find every caller", "childThreadId": "c1", "status": "running" },
        ],
    });
    model.thread = Some(records::thread(projection));
    model.selected_thread = Some("t1".into());
    model.selected_project = Some("p1".into());
    model.route = "thread".into();
}

#[test]
fn a_working_thread_shows_its_queue_subagents_plan_and_context() {
    let (mut model, _) = connected();
    working_thread(&mut model);
    model.sidebar_layout.grouping = Grouping::Project;
    let view = DomovoiApp.view(&model);
    let page = view.thread.unwrap();

    let queued: Vec<&str> = page.queued.iter().map(|q| q.text.as_str()).collect();
    assert_eq!(queued, ["then add tests", "and docs"], "in queue order");
    assert!(page.queued[0].first && page.queued[1].last && page.queued[0].can_steer);
    let context = page.context.unwrap();
    assert_eq!((context.percent, context.label.as_str(), context.high), (86, "172k of 200k tokens", true));

    let kinds: Vec<&str> = page.items.iter().map(|i| i.kind.as_str()).collect();
    assert_eq!(kinds, ["plan", "todo", "subagent"], "the TodoWrite call is shown as its list only");
    let todo = &page.items[1];
    assert_eq!(todo.todos.iter().map(|t| t.status.as_str()).collect::<Vec<_>>(), ["completed", "in_progress"]);
    let sub = &page.items[2];
    assert_eq!((sub.child_thread_id.as_str(), sub.can_stop), ("c1", true));
    assert_eq!(sub.progress, "Running in the background");
    assert_eq!(page.background, "Waiting for a background agent");
    assert_eq!(page.working_activity, None);
    assert!(!page.items[0].can_implement, "not while a turn is under way");

    // A subagent's thread is reached from its row, not the thread list.
    let listed: Vec<&str> = view.sidebar.projects.iter().flat_map(|p| p.threads.iter().map(|t| t.id.as_str())).collect();
    assert_eq!(listed, ["t1"]);
    assert!(view.composer.can_steer == false, "nothing to send yet");
    let _ = run(&mut model, Event::SetDraft { text: "also check the API".into() });
    assert!(DomovoiApp.view(&model).composer.can_steer);
}

/// What t3code shows about a turn beyond its rows: the context window in
/// detail, the account's limits by the composer, a to-do strip, compaction
/// dividers, a dimmed superseded plan, and "Plan ready" in the list.
#[test]
fn a_thread_shows_its_context_limits_tasks_and_plans_in_detail() {
    let (mut model, _) = connected();
    working_thread(&mut model);
    model.providers = vec![records::provider(json!({ "instanceId": "claudeAgent", "driver": "claudeAgent", "displayName": "Claude", "status": "ready", "enabled": true,
        "models": [{ "slug": "claude-haiku-5-5", "name": "Haiku 5.5", "isDefault": true }],
        "slashCommands": [{ "name": "compact", "description": "Compact the conversation" }],
        "usageLimits": { "checkedAt": "2026-11-01T00:00:00.000Z", "windows": [
            { "id": "s", "kind": "session", "label": "Session", "usedPercent": 40, "resetsAt": "2026-11-01T03:00:00.000Z", "windowDurationMins": 300 },
            { "id": "w", "kind": "weekly", "label": "Weekly", "usedPercent": 92.4, "resetsAt": "2026-11-03T00:00:00.000Z", "windowDurationMins": 10_080 },
        ] } }))];
    let mut projection = json!({
        "thread": { "id": "t1", "activeProviderThreadId": "pt1" },
        "runs": [
            { "id": "r1", "ordinal": 1, "status": "completed", "startedAt": "2026-11-01T00:00:00.000Z" },
            { "id": "r2", "ordinal": 2, "status": "running", "startedAt": "2026-11-01T00:00:01.000Z" },
        ],
        "providerThreads": [{ "id": "pt1", "contextUsage": { "usedTokens": 182_000, "maxTokens": 200_000, "inputTokens": 120_000,
            "cachedInputTokens": 50_000, "outputTokens": 12_000, "autoCompactThreshold": 160_000, "cost": { "amount": 1.239, "currency": "USD" } } }],
        "plans": [{ "id": "plan0", "kind": "proposed_plan", "status": "superseded" }, { "id": "plan1", "kind": "proposed_plan", "status": "active" }],
        "turnItems": [
            { "id": "i-old", "runId": "r1", "ordinal": 1, "type": "proposed_plan", "planId": "plan0", "markdown": "# First try", "status": "completed" },
            { "id": "i-plan", "runId": "r1", "ordinal": 2, "type": "proposed_plan", "planId": "plan1", "markdown": "# Ship it\n1. Do it", "status": "completed" },
            { "id": "i-compact", "runId": "r2", "ordinal": 3, "type": "compaction", "beforeTokenCount": 180_000, "afterTokenCount": 22_000,
              "summary": "Earlier: read the code.", "status": "completed" },
            { "id": "i-todo", "runId": "r2", "ordinal": 4, "type": "todo_list", "status": "completed",
              "steps": [{ "text": "Read", "status": "completed" }, { "text": "Write", "status": "running" }, { "text": "Test", "status": "pending" }] },
        ],
    });
    model.thread = Some(records::thread(projection.take()));
    model.now_ms = domovoi_core::app::millis_from_iso("2026-11-01T00:00:00.000Z");
    model.shown_ms = model.now_ms;
    let view = DomovoiApp.view(&model);
    let page = view.thread.unwrap();

    let context = page.context.unwrap();
    assert_eq!((context.percent, context.high, context.critical, context.compact_at_percent), (91, true, true, 80));
    let breakdown: Vec<String> = context.breakdown.iter().map(|l| format!("{} {}", l.label, l.value)).collect();
    assert_eq!(breakdown, ["Input 120k", "Cached 50k", "Output 12k"]);
    assert_eq!(context.cost, "$1.24");
    assert!(!context.can_compact, "not while a turn is under way");

    // The header meter: the context and the thread's provider limits in one place.
    let usage = page.usage.as_ref().unwrap();
    assert_eq!(usage.context.as_ref().map(|c| c.percent), Some(91));
    let limits = usage.limits.as_ref().unwrap();
    assert_eq!(limits.provider, "Claude");
    let windows: Vec<(&str, u32, u32, &str)> = limits.windows.iter().map(|w| (w.label.as_str(), w.percent, w.elapsed_percent, w.pace.as_str())).collect();
    assert_eq!(windows, [("Weekly", 92, 71, "ahead"), ("Session", 40, 40, "on")], "fullest first, each with its pace");
    assert_eq!(limits.windows[0].resets_at, "2026-11-03T00:00:00.000Z");
    assert_eq!(usage.fullest.as_ref().map(|w| w.label.as_str()), Some("Weekly"));
    assert_eq!(usage.limit_alert, "danger");
    assert_eq!((usage.limits_notice.as_str(), usage.limits_checked.as_str()), ("", "now"));

    let compaction = page.items.iter().find(|i| i.id == "i-compact").unwrap();
    assert_eq!(compaction.title, "Context compacted · 180k → 22k tokens");
    assert_eq!(compaction.text, "Earlier: read the code.");

    let plans: Vec<(bool, bool)> = page.items.iter().filter(|i| i.kind == "plan").map(|i| (i.superseded, i.can_implement)).collect();
    assert_eq!(plans, [(true, false), (false, false)], "the old plan dimmed; none carried out mid-turn");

    let tasks = page.tasks.unwrap();
    assert_eq!((tasks.done, tasks.total, tasks.label.as_str()), (1, 3, "Tasks 1/3 · Write"));
    assert_eq!(page.items.iter().find(|i| i.id == "i-todo").unwrap().progress, "1/3 · Write");

    assert_eq!(view.composer.limits.iter().map(|w| w.percent).collect::<Vec<_>>(), [92, 40], "fullest first");
    assert_eq!(view.composer.limits_chip, "Weekly 92% · resets in 2d");

    // Once the turn is over: compact now, and the list says a plan waits.
    let mut shell = records::shell_json(json!({ "id": "t1", "projectId": "p1", "title": "Search", "providerInstanceId": "claudeAgent",
        "modelSelection": { "instanceId": "claudeAgent", "model": "claude-haiku-5-5" }, "status": "idle", "hasActionableProposedPlan": true,
        "updatedAt": "2026-11-01T00:00:02.000Z" }));
    model.threads[0] = serde_json::from_value(shell.take()).unwrap();
    model.sidebar_layout.grouping = Grouping::Project;
    let view = DomovoiApp.view(&model);
    assert!(view.thread.as_ref().unwrap().context.as_ref().unwrap().can_compact);
    let page = view.thread.as_ref().unwrap();
    assert_eq!(page.tasks.as_ref().unwrap().label, "Tasks 1/3 · Write");
    assert!(page.plan.as_ref().unwrap().can_implement);
    let row = view.sidebar.sections.iter().flat_map(|s| s.rows.iter()).chain(view.sidebar.projects.iter().flat_map(|p| p.threads.iter())).find(|t| t.id == "t1").unwrap();
    assert!(row.plan_ready);
    assert_eq!(row.status_label, "Plan ready");

    let sent = sent_requests(&run(&mut model, Event::Compact));
    assert_eq!(sent[0]["payload"]["type"], "message.dispatch");
    assert_eq!(sent[0]["payload"]["text"], "/compact");

    let sent = sent_requests(&run(&mut model, Event::SavePlan { item_id: "i-plan".into(), path: String::new() }));
    assert_eq!(sent[0]["tag"], "projects.writeFile");
    assert_eq!((sent[0]["payload"]["relativePath"].as_str(), sent[0]["payload"]["contents"].as_str()), (Some("plans/ship-it.md"), Some("# Ship it\n1. Do it")));
}

/// The header meter shows the limits of the provider the thread runs on,
/// even before its context is known, and says when they can't be read.
#[test]
fn work_progress_recovers_saved_plans_and_checks_the_current_run() {
    let (mut model, _) = connected();
    working_thread(&mut model);
    let project = json!({
        "runs": [
            { "id": "r1", "ordinal": 1, "status": "completed" },
            { "id": "r2", "ordinal": 2, "status": "running" }
        ],
        "plans": [
            { "id": "saved", "runId": "r1", "nodeId": "exit-node", "kind": "proposed_plan",
              "status": "active", "markdown": "# Saved Claude proposal" },
            { "id": "checklist", "runId": "r1", "nodeId": "todo-node", "kind": "todo_list", "status": "active",
              "steps": [{ "id": "1", "text": "Verify", "status": "running" }] }
        ],
        "turnItems": [{
            "id": "replaced-plan", "runId": "r1", "nodeId": "exit-node", "ordinal": 3, "type": "dynamic_tool",
            "toolName": "ExitPlanMode", "status": "failed"
        }]
    });
    model.thread = Some(records::thread(project.clone()));
    model.threads[0] = serde_json::from_value(records::shell_json(json!({
        "id": "t1", "status": "working", "activeRunId": "r2", "hasActionableProposedPlan": true
    }))).unwrap();
    let page = DomovoiApp.view(&model).thread.unwrap();
    let plan = page.plan.unwrap();
    assert_eq!(plan.plan_id, "saved");
    assert!(!plan.blocks.is_empty());
    assert!(!plan.can_implement);
    let tasks = page.tasks.unwrap();
    assert!(tasks.previous_run);
    assert_eq!(tasks.todos[0].text, "Verify");

    let mut revised = project.clone();
    revised["plans"].as_array_mut().unwrap().push(json!({
        "id": "draft", "runId": "r2", "kind": "proposed_plan", "status": "draft", "markdown": "# Revision"
    }));
    revised["turnItems"].as_array_mut().unwrap().push(json!({
        "id": "reset", "runId": "r2", "ordinal": 4, "type": "todo_list", "steps": [], "status": "completed"
    }));
    model.thread = Some(records::thread(revised));
    let page = DomovoiApp.view(&model).thread.unwrap();
    assert_eq!(page.plan.unwrap().label, "Plan draft");
    assert!(page.tasks.is_none(), "the latest checklist was explicitly cleared");

    model.thread = Some(records::thread(project));
    model.threads[0] = serde_json::from_value(records::shell_json(json!({
        "id": "t1", "status": "idle", "hasActionableProposedPlan": true
    }))).unwrap();
    let plan = DomovoiApp.view(&model).thread.unwrap().plan.unwrap();
    assert!(plan.can_implement);
    let sent = sent_requests(&run(&mut model, Event::ImplementPlan { plan_id: plan.plan_id }));
    assert_eq!(sent.last().unwrap()["payload"]["sourcePlanRef"]["planId"], "saved");
}

#[test]
fn the_header_meter_shows_the_thread_providers_limits() {
    let (mut model, _) = connected();
    working_thread(&mut model);
    let codex = json!({ "instanceId": "codex", "driver": "codex", "displayName": "Codex", "status": "ready", "enabled": true,
        "models": [{ "slug": "gpt-6-luna", "name": "Luna", "isDefault": true }],
        "usageLimits": { "checkedAt": "2026-11-01T00:00:00.000Z", "windows": [{ "id": "w", "kind": "weekly", "label": "Weekly", "usedPercent": 99 }] } });
    let claude = |limits: Value| records::provider(json!({ "instanceId": "claudeAgent", "driver": "claudeAgent", "displayName": "Claude", "status": "ready", "enabled": true,
        "models": [{ "slug": "claude-haiku-5-5", "name": "Haiku 5.5", "isDefault": true }], "usageLimits": limits }));
    model.providers = vec![records::provider(codex), claude(json!({ "checkedAt": "2026-10-31T23:55:00.000Z", "windows": [
        { "id": "s", "kind": "session", "label": "Session", "usedPercent": 78 },
    ] }))];
    model.now_ms = domovoi_core::app::millis_from_iso("2026-11-01T00:00:00.000Z");
    model.shown_ms = model.now_ms;
    let thread = |context: Value| {
        records::thread(json!({ "thread": { "id": "t1", "activeProviderThreadId": "pt1" },
            "runs": [{ "id": "r2", "ordinal": 1, "status": "running", "startedAt": "2026-11-01T00:00:00.000Z" }],
            "providerThreads": [{ "id": "pt1", "contextUsage": context }] }))
    };
    // No context reported yet.
    model.thread = Some(thread(Value::Null));

    let page = DomovoiApp.view(&model).thread.unwrap();
    assert!(page.context.is_none());
    let usage = page.usage.unwrap();
    assert!(usage.context.is_none(), "limits alone still show the meter");
    let limits = usage.limits.unwrap();
    assert_eq!((limits.provider.as_str(), limits.windows.len()), ("Claude", 1), "the thread's provider, not the fullest one");
    assert_eq!((limits.windows[0].elapsed_percent, limits.windows[0].pace.as_str()), (0, ""), "no pace without the window's length");
    assert_eq!((usage.limit_alert.as_str(), usage.limits_checked.as_str()), ("caution", "5m"));

    // Unreadable limits are said, under the provider's name.
    model.providers[1] = claude(json!({ "checkedAt": "2026-11-01T00:00:00.000Z", "windows": [], "unavailable": { "reason": "probeFailed" } }));
    assert!(DomovoiApp.view(&model).thread.unwrap().usage.is_none(), "nothing to show without context or windows");
    model.thread = Some(thread(json!({ "usedTokens": 1000, "maxTokens": 200_000 })));
    let usage = DomovoiApp.view(&model).thread.unwrap().usage.unwrap();
    assert_eq!(usage.limits.as_ref().map(|l| (l.provider.as_str(), l.windows.len())), Some(("Claude", 0)));
    assert_eq!((usage.limits_notice.as_str(), usage.limit_alert.as_str()), ("Could not read limits.", ""));

    // A window past its reset is open again, whatever the last reading said.
    model.providers[1] = claude(json!({ "checkedAt": "2026-10-31T18:00:00.000Z", "windows": [
        { "id": "five_hour", "kind": "session", "label": "Session", "usedPercent": 100, "resetsAt": "2026-10-31T22:00:00.000Z", "windowDurationMins": 300 },
        { "id": "seven_day", "kind": "weekly", "label": "Weekly", "usedPercent": 40, "resetsAt": "2026-11-05T00:00:00.000Z", "windowDurationMins": 10080 },
    ] }));
    let usage = DomovoiApp.view(&model).thread.unwrap().usage.unwrap();
    let windows = usage.limits.unwrap().windows;
    let session = windows.iter().find(|w| w.label == "Session").unwrap();
    assert_eq!((session.percent, session.resets.as_str(), session.pace.as_str(), session.high), (0, "", "", false));
    assert_eq!(windows[0].label, "Weekly", "the fullest open window leads");
    assert_eq!(usage.limit_alert, "", "a used-up window that has reset warns of nothing");
}

/// A finished turn forks from its reply; a subagent waiting on the user is
/// a banner in its parent; a turn lists the files it changed.
#[test]
fn turns_fork_from_a_reply_list_their_files_and_surface_waiting_subagents() {
    let (mut model, _) = connected();
    working_thread(&mut model);
    // The subagent's thread waits for an approval.
    let mut child = records::shell_json(json!({ "id": "c1", "projectId": "p1", "title": "Find callers", "status": "running", "updatedAt": "2026-11-01T00:00:02.000Z",
        "lineage": { "parentThreadId": "t1", "relationshipToParent": "subagent", "rootThreadId": "t1" },
        "pendingRuntimeRequest": { "id": "rq", "kind": "approval", "createdAt": "2026-11-01T00:00:02.000Z" } }));
    model.threads[1] = serde_json::from_value(child.take()).unwrap();
    let mut projection = json!({
        "thread": { "id": "t1" },
        "runs": [{ "id": "r1", "ordinal": 1, "status": "completed", "startedAt": "2026-11-01T00:00:00.000Z", "completedAt": "2026-11-01T00:00:05.000Z" }],
        "messages": [{ "id": "m1", "runId": "r1", "role": "assistant", "text": "Done." }],
        "checkpoints": [{
            "id": "cp1", "threadId": "t1", "scopeId": "s", "runId": "r1", "nodeId": "root", "parentCheckpointId": null, "ordinalWithinScope": 1,
            "appRunOrdinal": 1, "ref": "refs/domovoi/cp1", "status": "ready", "capturedAt": "2026-11-01T00:00:04.000Z",
            "files": [{ "path": "src/a.rs", "kind": "modified", "additions": 3, "deletions": 1 }] }],
        "turnItems": [
            { "id": "i-a", "runId": "r1", "ordinal": 1, "type": "assistant_message", "messageId": "m1", "text": "Done.", "status": "completed" },
            { "id": "i-cp", "runId": "r1", "ordinal": 2, "type": "checkpoint", "checkpointId": "cp1", "files": [], "status": "completed" },
            { "id": "i-secret", "runId": "r1", "ordinal": 3, "type": "secret_request", "label": "NPM token", "reason": "To publish", "secretStatus": "cancelled", "status": "completed" },
        ],
    });
    model.thread = Some(records::thread(projection.take()));
    let page = DomovoiApp.view(&model).thread.unwrap();

    let reply = page.items.iter().find(|i| i.id == "i-a").unwrap();
    assert!(reply.can_fork_here);
    let checkpoint = page.items.iter().find(|i| i.id == "i-cp").unwrap().checkpoint.clone().unwrap();
    assert_eq!((checkpoint.files[0].path.as_str(), checkpoint.files[0].insertions, checkpoint.diff_source.as_str()), ("src/a.rs", 3, "turn:1"));
    let secret = page.items.iter().find(|i| i.id == "i-secret").unwrap();
    assert_eq!((secret.title.as_str(), secret.text.as_str()), ("Secret no longer needed: NPM token", "To publish"), "a cancelled request says so");
    assert!(page.secret_requests.is_empty(), "and has no card to answer");
    assert_eq!(page.waiting_children.iter().map(|c| (c.thread_id.as_str(), c.relation.as_str())).collect::<Vec<_>>(), [("c1", "subagent")]);

    let sent = sent_requests(&run(&mut model, Event::ForkFromItem { item_id: "i-a".into() }));
    assert_eq!(sent[0]["payload"]["type"], "thread.fork");
    assert_eq!(sent[0]["payload"]["sourcePoint"], json!({ "type": "run", "runId": "r1" }));
}

/// The composer's own commands, skills, model options, earlier prompts and
/// the stash.
#[test]
fn the_composer_offers_commands_skills_options_history_and_a_stash() {
    let (mut model, _) = connected();
    working_thread(&mut model);
    model.providers = vec![records::provider(json!({ "instanceId": "claudeAgent", "driver": "claudeAgent", "displayName": "Claude", "status": "ready", "enabled": true,
        "models": [
            { "slug": "claude-haiku-5-5", "name": "Haiku 5.5", "isDefault": true, "capabilities": { "optionDescriptors": [
                { "type": "boolean", "id": "fastMode", "label": "Fast mode" },
                { "type": "select", "id": "contextWindow", "label": "Context window", "options": [{ "id": "200k", "label": "200k", "isDefault": true }, { "id": "1m", "label": "1M" }] },
            ] } },
            { "slug": "claude-opus-5-5", "name": "Opus 5.5" },
        ],
        "slashCommands": [{ "name": "compact", "description": "Compact" }, { "name": "clear", "description": "provider's" }, { "name": "review", "input": { "hint": "[pr]" } }],
        "skills": [
            { "name": "deploy", "path": "/s/deploy", "enabled": true, "shortDescription": "Ship it" },
            { "name": "hidden", "path": "/s/h", "enabled": true, "userInvocable": false },
            { "name": "debug", "path": "/s/d", "enabled": false },
        ] }))];

    let _ = run(&mut model, Event::SetDraft { text: "/".into() });
    let view = DomovoiApp.view(&model);
    let listed: Vec<(&str, &str)> = view.composer.slash_commands.iter().map(|c| (c.name.as_str(), c.kind.as_str())).collect();
    assert_eq!(listed, [("model", "builtin"), ("plan", "builtin"), ("fork", "builtin"), ("clear", "builtin"), ("compact", "command"), ("review", "command")], "the provider's /clear is the app's");
    assert_eq!(view.composer.slash_commands[5].hint, "[pr]");

    // `/plan` turns plan mode on, without sending anything to the agent.
    let _ = run(&mut model, Event::SetDraft { text: "/plan".into() });
    let sent = sent_requests(&run(&mut model, Event::Send));
    assert_eq!(sent.len(), 1);
    assert_eq!((sent[0]["payload"]["type"].as_str(), sent[0]["payload"]["interactionMode"].as_str()), (Some("thread.interaction-mode.set"), Some("plan")));
    assert_eq!(DomovoiApp.view(&model).composer.draft, "");

    // `/model opus` picks the model by name; `/model` alone asks for the picker.
    let _ = run(&mut model, Event::SetDraft { text: "/model opus".into() });
    let sent = sent_requests(&run(&mut model, Event::Send));
    assert_eq!(sent[0]["payload"]["modelSelection"]["model"], "claude-opus-5-5");
    let before = DomovoiApp.view(&model).composer.model_picker_requested;
    let _ = run(&mut model, Event::SetDraft { text: "/mo".into() });
    let _ = run(&mut model, Event::PickSlashCommand { name: "model".into() });
    assert_eq!(DomovoiApp.view(&model).composer.model_picker_requested, before + 1);

    // `/clear …` starts a fresh session with the message.
    let _ = run(&mut model, Event::SelectModel { instance_id: "claudeAgent".into(), model: "claude-haiku-5-5".into() });
    let _ = run(&mut model, Event::SetDraft { text: "/clear start over".into() });
    let sent = sent_requests(&run(&mut model, Event::Send));
    let dispatch = sent.iter().find(|s| s["payload"]["type"] == "message.dispatch").unwrap();
    assert_eq!((dispatch["payload"]["text"].as_str(), dispatch["payload"]["freshContext"].as_bool()), (Some("start over"), Some(true)));

    // `$` lists the skills one may invoke; picking one completes it.
    let _ = run(&mut model, Event::SetDraft { text: "please $de".into() });
    let skills: Vec<String> = DomovoiApp.view(&model).composer.skills.iter().map(|s| format!("{} {}", s.name, s.description)).collect();
    assert_eq!(skills, ["deploy Ship it"]);
    let _ = run(&mut model, Event::PickSkill { name: "deploy".into() });
    assert_eq!(DomovoiApp.view(&model).composer.draft, "please $deploy ");
    let _ = run(&mut model, Event::SetDraft { text: "costs $5".into() });
    assert!(DomovoiApp.view(&model).composer.skills.is_empty(), "money is no skill");

    // Fast mode and the context window, sent with the model.
    let traits: Vec<(String, String, bool)> = DomovoiApp.view(&model).composer.traits.iter().map(|t| (t.id.clone(), t.value.clone(), t.on)).collect();
    assert_eq!(traits, [("fastMode".into(), String::new(), false), ("contextWindow".into(), "200k".into(), false)]);
    let sent = sent_requests(&run(&mut model, Event::SetModelOption { id: "fastMode".into(), value: "true".into() }));
    assert_eq!(sent[0]["payload"]["modelSelection"]["options"], json!([{ "id": "fastMode", "value": true }]));
    assert!(DomovoiApp.view(&model).composer.traits[0].on);

    // ↑ brings back the thread's earlier messages, newest first; ↓ the draft.
    let _ = run(&mut model, Event::SetDraft { text: "half typed".into() });
    let _ = run(&mut model, Event::RecallPrompt { delta: -1 });
    assert_eq!(DomovoiApp.view(&model).composer.draft, "and docs");
    let _ = run(&mut model, Event::RecallPrompt { delta: -1 });
    assert_eq!(DomovoiApp.view(&model).composer.draft, "then add tests");
    let _ = run(&mut model, Event::RecallPrompt { delta: 1 });
    let _ = run(&mut model, Event::RecallPrompt { delta: 1 });
    assert_eq!(DomovoiApp.view(&model).composer.draft, "half typed");

    // The stash keeps a draft for later.
    let _ = run(&mut model, Event::StashDraft);
    let view = DomovoiApp.view(&model);
    assert_eq!((view.composer.draft.as_str(), view.composer.stash.as_slice()), ("", &["half typed".to_owned()][..]));
    let _ = run(&mut model, Event::SetDraft { text: "other".into() });
    let _ = run(&mut model, Event::RestoreStash { index: 0 });
    let view = DomovoiApp.view(&model);
    assert_eq!((view.composer.draft.as_str(), view.composer.stash.as_slice()), ("half typed", &["other".to_owned()][..]), "what was typed goes aside");

    // A stop that doesn't end the turn: a force stop after a while.
    let _ = run(&mut model, Event::Interrupt);
    assert!(!DomovoiApp.view(&model).composer.can_force_stop);
    model.now_ms += 10_000;
    assert!(DomovoiApp.view(&model).composer.can_force_stop);
    let sent = sent_requests(&run(&mut model, Event::ForceStop));
    assert_eq!((sent[0]["tag"].as_str(), sent[0]["payload"]["threadId"].as_str()), (Some("orchestration.forceStopThread"), Some("t1")));
}

#[test]
fn background_activity_updates_independently_of_the_foreground_turn() {
    use domovoi_core::delta::{ThreadChange, ViewDelta};

    let (mut model, _) = connected();
    working_thread(&mut model);
    let before = DomovoiApp.view(&model);
    let existing = &before.thread.as_ref().unwrap().background_tasks[0];
    assert_eq!(existing.child_thread_id.as_deref(), Some("c1"));
    assert_eq!(existing.progress.as_deref(), Some("Running in the background"));

    let thread = model.thread.as_mut().unwrap();
    let mut provider = serde_json::to_value(thread.provider_threads.get("pt1").unwrap()).unwrap();
    provider["pendingBackgroundTasks"] = json!([
        { "kind": "subagent", "taskId": "launch-tool", "nativeItemId": "launch-tool", "childThreadId": "c1", "description": "Provider title" },
        { "kind": "subagent", "taskId": "duplicate", "nativeItemId": "launch-tool" },
        { "kind": "command", "taskId": "build", "command": " cargo test " },
        { "kind": "new_kind", "taskId": "unknown" },
    ]);
    thread.apply(records::domain_event("provider-thread.updated", "2", provider.clone()));
    let mut agent = serde_json::to_value(thread.subagents.get("node-sub").unwrap()).unwrap();
    agent["title"] = json!(" Find callers ");
    agent["progress"] = json!("Checking test fixtures");
    thread.apply(records::domain_event("subagent.updated", "3", agent.clone()));
    thread.apply(records::domain_event("turn-item.updated", "3", json!({
        "id": "reply", "runId": "r2", "ordinal": 5, "type": "assistant_message",
        "messageId": "reply-message", "text": "Here is the first result", "streaming": true, "status": "running",
    })));
    let running = DomovoiApp.view(&model);
    assert_eq!(running.thread.as_ref().unwrap().working_activity.as_ref().unwrap().kind.as_deref(), Some("assistant"));
    let tasks = &running.thread.as_ref().unwrap().background_tasks;
    assert_eq!(tasks.iter().map(|task| task.label.as_str()).collect::<Vec<_>>(), ["Find callers", "cargo test", "Background task"]);
    assert_eq!(tasks[0].progress.as_deref(), Some("Checking test fixtures"));
    assert_eq!(tasks[1].progress, None);
    let stable_id = tasks[0].id.clone();
    let Some(ThreadChange::Page(delta)) = ViewDelta::between(Some(&before), &running).thread else { panic!("updated page") };
    assert_eq!(&delta.header.unwrap().background_tasks, tasks);

    provider["pendingBackgroundTasks"][0]["taskId"] = json!("native-task");
    provider["pendingBackgroundTasks"].as_array_mut().unwrap().remove(1);
    model.thread.as_mut().unwrap().apply(records::domain_event("provider-thread.updated", "4", provider.clone()));
    model.threads[0] = records::shell(json!({ "id": "t1", "projectId": "p1", "status": "running",
        "pendingRuntimeRequest": { "id": "request", "kind": "command" } }));
    let approval = DomovoiApp.view(&model).thread.unwrap();
    assert_eq!(approval.status, "approval");
    assert_eq!(approval.background_tasks[0].id, stable_id);

    agent["status"] = json!("completed");
    model.thread.as_mut().unwrap().apply(records::domain_event("subagent.updated", "5", agent));
    model.threads[0] = records::shell(json!({ "id": "t1", "projectId": "p1", "status": "completed" }));
    let completed = DomovoiApp.view(&model).thread.unwrap();
    assert!(!completed.working);
    assert_eq!(completed.background_tasks.len(), 2);

    provider["pendingBackgroundTasks"] = json!([]);
    model.thread.as_mut().unwrap().apply(records::domain_event("provider-thread.updated", "6", provider));
    assert!(DomovoiApp.view(&model).thread.unwrap().background_tasks.is_empty());
}

#[test]
fn steering_and_queue_intents_become_commands() {
    let (mut model, _) = connected();
    working_thread(&mut model);
    let _ = run(&mut model, Event::SetDraft { text: "also check the API".into() });
    let sent = sent_requests(&run(&mut model, Event::Steer));
    assert_eq!(sent[0]["payload"]["dispatchMode"], json!({ "type": "steer_active", "targetRunId": "r2" }));
    assert!(!model.composer.steer_next);
    let _ = run(&mut model, Event::SetDraft { text: "later".into() });
    let sent = sent_requests(&run(&mut model, Event::Send));
    assert_eq!(sent[0]["payload"]["dispatchMode"]["type"], "queue_after_active", "Send waits for the turn");

    let p = |e: Event, model: &mut Model| sent_requests(&run(model, e)).remove(0)["payload"].clone();
    let c = p(Event::SteerQueued { run_id: "r3".into() }, &mut model);
    assert_eq!((c["type"].as_str(), c["queuedRunId"].as_str(), c["targetRunId"].as_str()), (Some("queued-message.promote-to-steer"), Some("r3"), Some("r2")));
    let c = p(Event::MoveQueued { run_id: "r4".into(), delta: -1 }, &mut model);
    assert_eq!((c["type"].as_str(), c["beforeRunId"].as_str()), (Some("queued-run.reorder"), Some("r3")));
    let c = p(Event::MoveQueued { run_id: "r3".into(), delta: 1 }, &mut model);
    assert_eq!(c["beforeRunId"], Value::Null, "to the end");
    let c = p(Event::EditQueued { run_id: "r3".into(), text: " tests first ".into() }, &mut model);
    assert_eq!((c["type"].as_str(), c["text"].as_str()), (Some("queued-run.edit"), Some("tests first")));
    let c = p(Event::EditQueued { run_id: "r3".into(), text: " ".into() }, &mut model);
    assert_eq!(c["type"], "queued-run.cancel", "emptied: removed");
    let c = p(Event::StopSubagent { subagent_id: "i-sub".into() }, &mut model);
    assert_eq!((c["type"].as_str(), c["subagentId"].as_str(), c["runId"].as_str()), (Some("subagent.stop"), Some("node-sub"), Some("r2")));
    let c = p(Event::ImplementPlan { plan_id: "plan1".into() }, &mut model);
    assert_eq!(c["sourcePlanRef"], json!({ "threadId": "t1", "planId": "plan1" }));
}

#[test]
fn a_fork_opens_once_the_server_lists_it() {
    let (mut model, sent) = connected();
    working_thread(&mut model);
    assert!(DomovoiApp.view(&model).thread.unwrap().can_fork);
    let sent_fork = sent_requests(&run(&mut model, Event::ForkThread { thread_id: "t1".into() }));
    let c = &sent_fork[0]["payload"];
    assert_eq!((c["type"].as_str(), c["sourceThreadId"].as_str(), c["title"].as_str()), (Some("thread.fork"), Some("t1"), Some("Search (fork)")));
    let target = c["targetThreadId"].as_str().unwrap().to_owned();

    let shell_id = sent.iter().find(|r| r["tag"] == "orchestration.subscribeShell").unwrap()["id"].as_str().unwrap().to_owned();
    let fork = json!({ "id": target, "projectId": "p1", "title": "Search (fork)", "updatedAt": "2026-11-01T00:00:03.000Z",
        "lineage": { "parentThreadId": "t1", "relationshipToParent": "fork", "rootThreadId": "t1" } });
    let effects = sock(&mut model, chunk(&shell_id, &[records::thread_updated(fork)]));
    assert!(sent_requests(&effects).iter().any(|r| r["tag"] == "orchestration.subscribeThread" && r["payload"]["threadId"] == target.as_str()));
    model.sidebar_layout.grouping = Grouping::Project;
    let view = DomovoiApp.view(&model);
    let page = view.thread.unwrap();
    assert_eq!(page.parent.map(|p| (p.title, p.relation)), Some(("Search".into(), "fork".into())));
    let row = view.sidebar.projects[0].threads.iter().find(|t| t.id == target).unwrap();
    assert_eq!(row.relation, "fork");
}

#[test]
fn an_approval_notification_names_its_server_and_request_and_can_be_answered() {
    let mut shell = Shell::paired();
    let mut model = Model::default();
    let _ = shell.drive(&mut model, Event::Connect { server_url: "http://h:1".into(), pairing_code: "CODE".into() });
    let sent = sent_requests(&sock(&mut model, SocketEvent::Opened));
    let lifecycle = sent.iter().find(|r| r["tag"] == "subscribeServerLifecycle").unwrap()["id"].as_str().unwrap().to_owned();
    let _ = shell.drive_sock(&mut model, chunk(&lifecycle, &[records::welcome(json!({ "environmentId": "srv-1", "label": "Mac" }))]));
    let shell_id = sent.iter().find(|r| r["tag"] == "orchestration.subscribeShell").unwrap()["id"].as_str().unwrap().to_owned();
    let thread = json!({ "id": "t1", "title": "Fix it", "status": "idle", "pendingRuntimeRequest": null });
    let _ = sock(&mut model, chunk(&shell_id, &[records::shell_snapshot(1, json!([]), json!([thread]))]));

    // The thread starts waiting on a command approval while the user looks elsewhere.
    let waiting = json!({ "id": "t1", "title": "Fix it", "status": "running", "pendingRuntimeRequest": { "id": "req-1", "kind": "command" } });
    let effects = sock(&mut model, chunk(&shell_id, &[records::thread_updated(waiting)]));
    let note = effects.iter().find_map(|e| match e { Effect::Notify(r) => Some(r.operation.clone()), _ => None }).expect("notified");
    assert_eq!(note.body, "Needs your approval");
    assert_eq!(note.environment_id, "srv-1");
    assert_eq!(note.request_id.as_deref(), Some("req-1"));

    // A question is answered in the app, not from the notification.
    let _ = sock(&mut model, chunk(&shell_id, &[records::thread_updated(thread)]));
    let asking = json!({ "id": "t1", "title": "Fix it", "status": "running", "pendingRuntimeRequest": { "id": "q-1", "kind": "user_input" } });
    let effects = sock(&mut model, chunk(&shell_id, &[records::thread_updated(asking)]));
    let note = effects.iter().find_map(|e| match e { Effect::Notify(r) => Some(r.operation.clone()), _ => None }).expect("notified");
    assert_eq!(note.request_id, None);

    // Allow from the notification answers the request on this server.
    let effects = run(&mut model, Event::RespondFromNotification { environment_id: "srv-1".into(), thread_id: "t1".into(), request_id: "req-1".into(), decision: "accept".into() });
    let sent = sent_requests(&effects);
    assert!(
        sent.iter().any(|r| r["payload"]["type"] == "runtime-request.respond" && r["payload"]["requestId"] == "req-1"),
        "{sent:?}"
    );
}

/// Claude plus an OpenCode with 30 models from three upstream providers,
/// listed in no particular order.
fn many_models_snapshot(model: &mut Model, sent: &[Value], settings: Value) -> String {
    let config_id = sent.iter().find(|r| r["tag"] == "subscribeServerConfig").unwrap()["id"].as_str().unwrap().to_owned();
    let mut open: Vec<Value> = vec![];
    for i in (0..10).rev() {
        for upstream in ["openrouter", "anthropic", "openai"] {
            let name = match upstream {
                "openai" => format!("GPT {i}"),
                _ => format!("Claude Sonnet {i}"),
            };
            let id = name.to_lowercase().replace(' ', "-");
            open.push(json!({ "slug": format!("{upstream}/{id}"), "name": name, "capabilities": { "optionDescriptors": [] } }));
        }
    }
    let providers = json!([
        { "instanceId": "claudeAgent", "driver": "claudeAgent", "displayName": "Claude", "status": "ready", "enabled": true, "models": [
            { "slug": "claude-haiku-5-5", "name": "Haiku 5.5", "isDefault": true },
            { "slug": "claude-opus-5-5", "name": "Opus 5.5" }] },
        { "instanceId": "opencode", "driver": "opencode", "displayName": "OpenCode", "status": "ready", "enabled": true, "models": open },
    ]);
    let _ = sock(model, chunk(&config_id, &[records::config_snapshot(providers, settings)]));
    config_id
}

fn row_names(rows: &[domovoi_core::view::ModelRowView]) -> Vec<String> {
    rows.iter().map(|r| r.option.model.clone()).collect()
}

#[test]
fn favorite_models_come_first_on_every_client() {
    let (mut model, sent) = connected();
    let config_id = many_models_snapshot(&mut model, &sent, json!({ "favorites": null }));
    let view = DomovoiApp.view(&model).composer;
    assert!(view.favorite_models.is_empty());
    assert_eq!(view.model_count, 32);
    let open = view.model_groups.iter().find(|g| g.instance_id == "opencode").unwrap();
    assert_eq!(open.models[0].option.model, "anthropic/claude-sonnet-0", "OpenCode's models are sorted by upstream provider, then name");
    assert_eq!(open.models[0].detail, "anthropic");
    assert_eq!(open.models[0].name, "Claude Sonnet 0");

    // Marking a favorite shows at once and asks the server for that state.
    let effects = run(&mut model, Event::SetModelFavorite { instance_id: "opencode".into(), model: "openai/gpt-3".into(), favorite: true });
    let req = sent_requests(&effects).into_iter().find(|r| r["tag"] == "server.updateSettings").expect("settings update");
    assert_eq!(req["payload"]["patch"], json!({ "setModelFavorites": [{ "provider": "opencode", "model": "openai/gpt-3", "favorite": true }] }));
    let view = DomovoiApp.view(&model).composer;
    assert_eq!(row_names(&view.favorite_models), ["openai/gpt-3"]);
    assert_eq!(view.favorite_models[0].detail, "OpenCode · openai");
    assert!(view.favorite_models[0].favorite);
    let open = view.model_groups.iter().find(|g| g.instance_id == "opencode").unwrap();
    assert!(open.models.iter().all(|r| r.option.model != "openai/gpt-3"), "a favorite isn't listed twice");
    assert_eq!(view.model_count, 32);

    // The server refusing undoes it.
    let _ = sock(&mut model, exit_fail(req["id"].as_str().unwrap(), json!({ "_tag": "ServerSettingsError", "message": "nope" })));
    assert!(DomovoiApp.view(&model).composer.favorite_models.is_empty());

    // Another client's favorites arrive with the settings.
    let settings = json!({ "favorites": [{ "provider": "claudeAgent", "model": "claude-opus-5-5" }, { "provider": "opencode", "model": "anthropic/claude-sonnet-9" }] });
    let _ = sock(&mut model, chunk(&config_id, &[json!({ "version": 1, "type": "settingsUpdated", "payload": { "settings": settings } })]));
    let view = DomovoiApp.view(&model).composer;
    assert_eq!(row_names(&view.favorite_models), ["claude-opus-5-5", "anthropic/claude-sonnet-9"], "in the order they were added");
    let claude = view.model_groups.iter().find(|g| g.instance_id == "claudeAgent").unwrap();
    assert_eq!(row_names(&claude.models), ["claude-haiku-5-5"]);
}

/// Pi's sessions are named as Pi in the import picker and the sidebar, and
/// its models (pi's default first) are grouped by upstream provider.
#[test]
fn pi_sessions_and_models_are_named_as_pi() {
    let (mut model, sent) = connected();
    model.now_ms = 1_794_700_000_000;
    model.shown_ms = model.now_ms;
    let scan = sent_requests(&run(&mut model, Event::ScanAgentSessions));
    let candidates = json!({ "candidates": [
        { "path": "/w/calc", "title": "calc", "sources": ["claudeAgent", "pi"], "threadCount": 2, "lastActiveAt": "2026-11-13T10:00:00.000Z", "alreadyImported": false, "git": null },
    ], "scannedAt": "2026-11-14T00:00:00.000Z" });
    let _ = sock(&mut model, exit_ok(scan[0]["id"].as_str().unwrap(), candidates));
    assert_eq!(DomovoiApp.view(&model).agent_sessions.candidates[0].sources, "Claude Code · Pi");

    model.projects = vec![records::project(json!({ "id": "p1", "title": "calc", "workspaceRoot": "/w/calc" }))];
    model.threads = vec![records::shell(json!({ "id": "import:pi:01a1efd6", "projectId": "p1", "title": "fixes", "updatedAt": "2026-11-13T10:00:00.000Z",
        "deletedAt": null, "archivedAt": null }))];
    model.sidebar_layout.grouping = Grouping::Project;
    let p = &DomovoiApp.view(&model).sidebar.projects[0];
    let row = p.threads.iter().chain(p.earlier.iter()).find(|t| t.id == "import:pi:01a1efd6").expect("the imported thread");
    assert_eq!(row.source, "Pi");

    let config_id = sent.iter().find(|r| r["tag"] == "subscribeServerConfig").unwrap()["id"].as_str().unwrap().to_owned();
    let providers = json!([{ "instanceId": "pi", "driver": "pi", "displayName": "Pi", "status": "ready", "enabled": true, "models": [
        { "slug": "default", "name": "Pi default" },
        { "slug": "zai/glm-5.3", "name": "GLM 5.3", "subProvider": "zai" },
        { "slug": "anthropic/claude-sonnet-5", "name": "Claude Sonnet 5", "subProvider": "anthropic" },
    ] }]);
    let _ = sock(&mut model, chunk(&config_id, &[records::config_snapshot(providers, json!({}))]));
    let group = DomovoiApp.view(&model).composer.model_groups.into_iter().find(|g| g.instance_id == "pi").expect("pi's models");
    assert_eq!(row_names(&group.models), ["default", "anthropic/claude-sonnet-5", "zai/glm-5.3"]);
    assert_eq!(group.models[1].detail, "anthropic");
}

#[test]
fn hidden_models_leave_the_picker_until_shown_or_searched_for() {
    let (mut model, sent) = connected();
    let _ = many_models_snapshot(&mut model, &sent, json!({ "favorites": [{ "provider": "opencode", "model": "openai/gpt-1" }] }));

    // Hiding a favorite also unmarks it, in one patch.
    let effects = run(&mut model, Event::SetModelHidden { instance_id: "opencode".into(), model: "openai/gpt-1".into(), hidden: true });
    let req = sent_requests(&effects).into_iter().find(|r| r["tag"] == "server.updateSettings").expect("settings update");
    assert_eq!(req["payload"]["patch"]["setModelsHidden"], json!([{ "provider": "opencode", "model": "openai/gpt-1", "hidden": true }]));
    assert_eq!(req["payload"]["patch"]["setModelFavorites"], json!([{ "provider": "opencode", "model": "openai/gpt-1", "favorite": false }]));
    let _ = run(&mut model, Event::SetModelHidden { instance_id: "opencode".into(), model: "openai/gpt-2".into(), hidden: true });
    let view = DomovoiApp.view(&model).composer;
    assert!(view.favorite_models.is_empty());
    assert_eq!(view.model_count, 30);
    let open = view.model_groups.iter().find(|g| g.instance_id == "opencode").unwrap();
    assert_eq!(open.hidden_count, 2);
    assert_eq!(open.models.len(), 28);
    assert!(open.models.iter().all(|r| !r.hidden));

    let _ = run(&mut model, Event::SetShowHiddenModels { show: true });
    let open = DomovoiApp.view(&model).composer.model_groups.into_iter().find(|g| g.instance_id == "opencode").unwrap();
    assert_eq!(open.models.len(), 30);
    assert_eq!(open.models.iter().filter(|r| r.hidden).count(), 2);
    let _ = run(&mut model, Event::SetShowHiddenModels { show: false });

    // A search finds hidden models (to show them again).
    let _ = run(&mut model, Event::SetModelQuery { query: "gpt 2".into() });
    let view = DomovoiApp.view(&model).composer;
    assert_eq!(view.model_groups.len(), 1, "groups without a match are left out");
    assert_eq!(row_names(&view.model_groups[0].models), ["openai/gpt-2"]);
    assert!(view.model_groups[0].models[0].hidden);

    // Choosing a model clears the search; a hidden selected model still shows.
    let _ = run(&mut model, Event::SelectModel { instance_id: "opencode".into(), model: "openai/gpt-2".into() });
    let view = DomovoiApp.view(&model).composer;
    assert_eq!(view.model_query, "");
    let open = view.model_groups.iter().find(|g| g.instance_id == "opencode").unwrap();
    assert_eq!(open.models.iter().filter(|r| r.hidden).map(|r| r.option.model.as_str()).collect::<Vec<_>>(), ["openai/gpt-2"]);

    // Marking it favorite shows it again.
    let effects = run(&mut model, Event::SetModelFavorite { instance_id: "opencode".into(), model: "openai/gpt-2".into(), favorite: true });
    let req = sent_requests(&effects).into_iter().find(|r| r["tag"] == "server.updateSettings").unwrap();
    assert_eq!(req["payload"]["patch"]["setModelsHidden"], json!([{ "provider": "opencode", "model": "openai/gpt-2", "hidden": false }]));
    let view = DomovoiApp.view(&model).composer;
    assert_eq!(row_names(&view.favorite_models), ["openai/gpt-2"]);
    assert!(!view.favorite_models[0].hidden);
}

#[test]
fn searching_models_ranks_name_matches_first() {
    let (mut model, sent) = connected();
    let _ = many_models_snapshot(&mut model, &sent, json!({ "favorites": [{ "provider": "opencode", "model": "openrouter/claude-sonnet-4" }] }));
    let _ = run(&mut model, Event::SetModelQuery { query: "openrouter sonnet 4".into() });
    let view = DomovoiApp.view(&model).composer;
    assert_eq!(row_names(&view.favorite_models), ["openrouter/claude-sonnet-4"], "favorites are searched too");
    assert!(view.model_groups.is_empty(), "the one match is a favorite");

    let _ = run(&mut model, Event::SetModelQuery { query: "opus".into() });
    let view = DomovoiApp.view(&model).composer;
    assert!(view.favorite_models.is_empty());
    assert_eq!(view.model_groups.len(), 1);
    assert_eq!(row_names(&view.model_groups[0].models), ["claude-opus-5-5"]);

    let _ = run(&mut model, Event::SetModelQuery { query: "anthropic".into() });
    let view = DomovoiApp.view(&model).composer;
    let names: Vec<String> = view.model_groups.iter().flat_map(|g| row_names(&g.models)).collect();
    assert_eq!(names.len(), 10, "the upstream provider in the slug matches");
    assert!(names.iter().all(|n| n.starts_with("anthropic/")));
}

#[test]
fn turning_a_setting_on_still_patches_that_key() {
    let (mut model, sent) = connected();
    providers_snapshot(&mut model, &sent);
    let effects = run(&mut model, Event::SetSettingBool { key: "enableAgentBrowserAccess".into(), value: true });
    let req = sent_requests(&effects).into_iter().find(|r| r["tag"] == "server.updateSettings").unwrap();
    assert_eq!(req["payload"]["patch"], json!({ "enableAgentBrowserAccess": true }));
    assert_eq!(model.settings.as_ref().and_then(|settings| settings.enable_agent_browser_access), Some(true));
}

/// What crosses FFI is a delta against the view sent before: a shell that
/// applies each one in order holds exactly the view model, after every
/// frame of the recorded session (the shell stream, selecting the thread,
/// every snapshot and event of it, scrolling the panel away and closing it).
#[test]
fn deltas_applied_in_order_reproduce_the_view_after_every_frame() {
    // A thread of many rows, and one whose reply streams.
    let (_, rows, whole) = deltas_reproduce_the_view(THREAD);
    assert!(rows * 4 < whole, "rows sent ({rows} B) are a fraction of every row every time ({whole} B)");
    let (from_rows, _, _) = deltas_reproduce_the_view(STREAMED_THREAD);
    assert!(from_rows > 0, "the recorded reply streamed: some views were made from their rows alone");
}

/// Replays `thread` of the recorded session, checking after every frame that
/// the deltas applied so far make the view. Returns how many views were made
/// from the rows that changed alone, the bytes of the rows sent, and the
/// bytes sending every row every time would have taken.
fn deltas_reproduce_the_view(thread: &str) -> (usize, usize, usize) {
    use domovoi_core::delta::{FfiApp, ThreadChange, ViewDelta};
    // Checked: a view made from the rows that changed must equal the view built whole.
    let ffi = FfiApp::checked();
    let (mut model, sent) = connected();
    let mut shadow = domovoi_core::view::ViewModel::default();
    let (mut renders, mut row_bytes, mut full_bytes) = (0usize, 0usize, 0usize);
    let mut check = |model: &Model, what: &str| {
        let delta: ViewDelta = ffi.view(model);
        let full = DomovoiApp.view(model);
        if let Some(ThreadChange::Page(p)) = &delta.thread {
            if renders > 0 {
                row_bytes += bincode_len(&p.rows);
                full_bytes += bincode_len(&full.thread.as_ref().map(|t| &t.items));
            }
        }
        delta.apply(&mut shadow);
        assert_eq!(shadow, full, "after {what}");
        renders += 1;
    };
    check(&model, "connecting");
    let shell_id = sent.iter().find(|r| r["tag"] == "orchestration.subscribeShell").expect("shell subscription")["id"].as_str().unwrap().to_owned();
    let (shell, thread_items, _) = recorded_of(thread);
    for (i, v) in shell.iter().enumerate() {
        let _ = sock(&mut model, chunk(&shell_id, std::slice::from_ref(v)));
        check(&model, &format!("shell item {i}"));
    }
    let sent = sent_requests(&run(&mut model, Event::SelectThread { thread_id: thread.into() }));
    check(&model, "selecting the thread");
    let thread_id = sent.iter().find(|r| r["tag"] == "orchestration.subscribeThread").expect("thread subscription")["id"].as_str().unwrap().to_owned();
    for (i, v) in thread_items.iter().enumerate() {
        let _ = sock(&mut model, chunk(&thread_id, std::slice::from_ref(v)));
        check(&model, &format!("thread item {i} ({})", v["kind"]));
    }
    // Another thread, then none.
    let _ = run(&mut model, Event::NewThread { project_id: None });
    check(&model, "a new thread");
    let _ = run(&mut model, Event::SelectThread { thread_id: thread.into() });
    check(&model, "the thread again");
    assert!(renders > thread_items.len());
    println!("{thread}: {renders} views, {} of them from the rows that changed", ffi.views_from_rows());
    (ffi.views_from_rows() as usize, row_bytes, full_bytes)
}

fn bincode_len<T: serde::Serialize>(v: &T) -> usize {
    use crux_core::bridge::{BincodeFfiFormat, FfiFormat};
    let mut out = Vec::new();
    BincodeFfiFormat::serialize(&mut out, v).expect("serializes");
    out.len()
}

/// Empty history and live thinking share the same visibility rule, including
/// row membership changes sent across FFI to the native clients.
#[test]
fn empty_thinking_is_hidden_and_visibility_changes_match_full_views() {
    use domovoi_core::delta::{FfiApp, ViewDelta};
    let (mut model, _) = connected();
    let stream = open_stream(&mut model);
    model.threads = vec![records::shell(json!({ "id": "t1", "projectId": "p1", "status": "running",
        "activeRunId": "r1", "activityRunStatus": "running" }))];
    let thinking = |text: &str, status: &str| json!({ "id": "thought", "type": "reasoning", "ordinal": 2,
        "text": text, "status": status, "streaming": status == "running" });
    let snapshot = |text: &str, commands: bool| {
        let mut items = vec![thinking(text, "running"),
            json!({ "id": "empty2", "type": "reasoning", "ordinal": 4, "text": "", "status": "completed" })];
        if commands {
            items.extend([
                json!({ "id": "c1", "type": "command_execution", "ordinal": 1, "input": "pwd" }),
                json!({ "id": "c2", "type": "command_execution", "ordinal": 3, "input": "make" }),
            ]);
        }
        json!({ "runs": [{ "id": "r1", "status": "running" }], "turnItems": items })
    };
    let _ = sock(&mut model, chunk(&stream, &[records::snapshot(1, snapshot("", false))]));
    let ffi = FfiApp::checked();
    let mut shadow = domovoi_core::view::ViewModel::default();
    ffi.view(&model).apply(&mut shadow);
    assert!(shadow.thread.as_ref().unwrap().items.is_empty(), "an empty-only turn has no disclosure");
    assert!(shadow.thread.as_ref().unwrap().working, "the run activity indicator remains");

    let _ = sock(&mut model, chunk(&stream, &[records::snapshot(2, snapshot(" \n", true))]));
    ffi.view(&model).apply(&mut shadow);
    let page = shadow.thread.as_ref().unwrap();
    assert_eq!(page.items.iter().map(|r| r.id.as_str()).collect::<Vec<_>>(), ["c1", "c2"]);
    assert_eq!(page.items[0].group_summary, "Ran 2 commands");
    assert_eq!(page.items[0].group, page.items[1].group);

    // First readable text changes membership; subsequent growth should use the row path.
    for (n, text) in [" \n\t", " \nA thought", " \nA thought grows", "", "Another thought"].iter().enumerate() {
        let before = ffi.views_from_rows();
        let event = records::stream_event(3 + n as u64, "turn-item.updated", "2", thinking(text, "running"));
        let _ = sock(&mut model, chunk(&stream, &[event]));
        let delta: ViewDelta = ffi.view(&model);
        delta.apply(&mut shadow);
        assert_eq!(shadow, DomovoiApp.view(&model));
        let page = shadow.thread.as_ref().unwrap();
        let row = page.items.iter().find(|r| r.id == "thought");
        assert_eq!(row.is_some(), !text.trim().is_empty());
        if let Some(row) = row {
            assert_eq!(row.text, *text, "formatting is preserved");
            assert!(row.collapsed);
        }
        assert_eq!(page.items[0].group_summary, "Ran 2 commands");
        if n == 2 {
            assert_eq!(ffi.views_from_rows(), before + 1, "ordinary growth keeps the fast path");
        }
    }
    // A completed item with no text (e.g. from an older server) disappears too.
    let _ = sock(&mut model, chunk(&stream, &[records::stream_event(8, "turn-item.updated", "3", thinking(" ", "completed"))]));
    ffi.view(&model).apply(&mut shadow);
    assert_eq!(shadow.thread.as_ref().unwrap().items.len(), 2);
    // Reconnecting to the same history applies the same visibility rule.
    let _ = sock(&mut model, chunk(&stream, &[records::snapshot(9, snapshot(" ", true))]));
    ffi.view(&model).apply(&mut shadow);
    assert_eq!(shadow, DomovoiApp.view(&model));
    assert_eq!(shadow.thread.as_ref().unwrap().items.len(), 2);
}

/// A view made from the rows whose text grew equals the view built whole,
/// whatever the thread's events and however often the view is read: a
/// reply, thinking and a command's output growing, between replies ending
/// and starting, runs, queued messages edited, approvals asked and
/// answered, rows expanded. `FfiApp::checked` compares after every view.
#[test]
fn views_made_from_grown_rows_equal_views_built_whole() {
    use domovoi_core::delta::{FfiApp, ViewDelta};
    use domovoi_core::rpc::Route;
    let (mut model, _) = connected();
    long_thread(&mut model, 5);
    model.changes.wide(); // the thread was put in place by hand
    let (stream, _) = model.rpc.request("orchestration.subscribeThread", &json!({ "threadId": "t1" }), Route::Thread("t1".into()));
    let ffi = FfiApp::checked();
    let mut shadow = domovoi_core::view::ViewModel::default();
    // A small generator: the same sequence every run.
    let mut seed = 0x2545_F491_4F6C_DD1Du64;
    let mut pick = |n: u64| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        seed % n
    };
    let (mut seq, mut at) = (10u64, 0u64);
    let mut event = |ty: &str, payload: Value| {
        seq += 1;
        records::stream_event(seq, ty, &format!("3.{seq}"), payload)
    };
    let node = |status: &str| json!({ "id": "nl", "threadId": "t1", "runId": "rl", "kind": "assistant_message", "status": status });
    let message = |id: &str, role: &str, text: &str, streaming: bool, at: u64| json!({ "id": id, "threadId": "t1", "runId": "rl", "role": role, "text": text, "attachments": [], "streaming": streaming, "updatedAt": format!("3.{at}") });
    let reply = |n: u64, text: &str, streaming: bool, at: u64| json!({ "id": format!("il{n}"), "threadId": "t1", "runId": "rl", "type": "assistant_message", "messageId": format!("ml{n}"),
        "ordinal": 1000 + n * 10, "status": if streaming { "running" } else { "completed" }, "text": text, "streaming": streaming, "updatedAt": format!("3.{at}") });
    let thinking = |text: &str, status: &str, at: u64| json!({ "id": "ir", "threadId": "t1", "runId": "rl", "type": "reasoning", "ordinal": 901, "status": status, "text": text, "streaming": true, "updatedAt": format!("3.{at}") });
    let command = |output: &str, at: u64| json!({ "id": "icl", "threadId": "t1", "runId": "rl", "type": "command_execution", "input": "make", "output": output, "ordinal": 902, "status": "running", "streaming": true, "updatedAt": format!("3.{at}") });
    let run = |id: &str, status: &str, ordinal: u64, message: &str| json!({ "id": id, "threadId": "t1", "ordinal": ordinal, "status": status, "userMessageId": message, "startedAt": "2026-11-16T10:00:00.000Z", "queuePosition": 1 });

    // A running turn with thinking, a command and a reply under way, and a message queued behind it.
    let (mut reply_n, mut reply_text, mut thought, mut output) = (0u64, String::from("Start. "), String::from("Hm. "), String::from("cc a.c\n"));
    let opening = [
        event("run.updated", run("rl", "running", 50, "mu4")),
        event("turn-item.updated", thinking(&thought, "running", 0)),
        event("turn-item.updated", command(&output, 0)),
        event("message.updated", message("ml0", "assistant", &reply_text, true, 0)),
        event("turn-item.updated", reply(0, &reply_text, true, 0)),
        event("message.updated", message("mq", "user", "queued question", false, 0)),
        event("run.updated", run("rq", "queued", 51, "mq")),
    ];
    let _ = sock(&mut model, chunk(&stream, &opening));
    let (mut asked, mut views) = (false, 0u32);
    for step in 0..600 {
        at += 1;
        let frame: Vec<Value> = match pick(16) {
            // A flush of the reply, as the server writes it: node, message, turn item.
            0..=5 => {
                reply_text.push_str("more words ");
                vec![event("node.updated", node("running")), event("message.updated", message(&format!("ml{reply_n}"), "assistant", &reply_text, true, at)), event("turn-item.updated", reply(reply_n, &reply_text, true, at))]
            }
            6 => {
                thought.push_str("and then ");
                vec![event("turn-item.updated", thinking(&thought, "running", at))]
            }
            7 => {
                output.push_str("ld a.o\n");
                vec![event("turn-item.updated", command(&output, at))]
            }
            8 => vec![event("node.updated", node("running"))],
            // The reply ends and the next one starts.
            9 => {
                let done = vec![event("message.updated", message(&format!("ml{reply_n}"), "assistant", &reply_text, false, at)), event("turn-item.updated", reply(reply_n, &reply_text, false, at))];
                reply_n += 1;
                reply_text = String::from("Next. ");
                let mut frame = done;
                frame.push(event("message.updated", message(&format!("ml{reply_n}"), "assistant", &reply_text, true, at)));
                frame.push(event("turn-item.updated", reply(reply_n, &reply_text, true, at)));
                frame
            }
            10 => vec![event("run.updated", run("rl", if step % 2 == 0 { "waiting" } else { "running" }, 50, "mu4"))],
            // The queued message is edited: the page's queue shows it, no row does.
            11 => vec![event("message.updated", message("mq", "user", &format!("queued question, edited at {at}"), false, at))],
            12 => {
                asked = !asked;
                vec![event("runtime-request.updated", json!({ "id": "req1", "threadId": "t1", "runId": "rl", "kind": "approval", "status": if asked { "pending" } else { "resolved" } }))]
            }
            13 => {
                let _ = run_event(&mut model, Event::ExpandItem { id: "icl".into() });
                vec![]
            }
            // Text that grows together with a status: more than the row's text.
            14 => {
                thought.push_str("wait ");
                vec![event("turn-item.updated", thinking(&thought, if step % 2 == 0 { "waiting" } else { "running" }, at))]
            }
            // Growth and a wider change in one frame.
            _ => {
                reply_text.push_str("and ");
                vec![event("turn-item.updated", reply(reply_n, &reply_text, true, at)), event("run.updated", run("rq", "queued", 51 + at, "mq"))]
            }
        };
        if !frame.is_empty() {
            let _ = sock(&mut model, chunk(&stream, &frame));
        }
        // Typing and quiet ticks between frames: they reach the composer, or nothing shown.
        match pick(8) {
            0 => {
                let text = if step % 3 == 0 { String::new() } else if step % 3 == 1 { "/re".to_owned() } else { format!("a reply {step}") };
                let _ = run_event(&mut model, Event::SetDraft { text });
            }
            1 => {
                let _ = run_event(&mut model, Event::Tick { now_ms: 1_000 + i64::from(step) });
            }
            _ => {}
        }
        // Read after some steps and not others: several changes meet in one view.
        if pick(5) < 3 {
            let delta: ViewDelta = ffi.view(&model);
            delta.apply(&mut shadow);
            assert_eq!(shadow, DomovoiApp.view(&model), "the deltas so far give the view (step {step})");
            views += 1;
        }
    }
    let quick = ffi.views_from_rows();
    println!("{views} views, {quick} of them from the rows that changed");
    assert!(quick > 50 && u64::from(views) > quick + 50, "both ways of building the view were exercised ({quick} of {views})");
    let page = shadow.thread.expect("thread page");
    assert!(page.items.iter().any(|r| r.text.ends_with("more words ") || r.text.ends_with("and ")), "the reply's last words are shown");
}

/// Typing reaches the composer only: the send button follows the draft at
/// once, and the next streamed token is still built from its row alone.
#[test]
fn typing_updates_the_composer_and_keeps_streamed_tokens_narrow() {
    use domovoi_core::delta::{FfiApp, ViewDelta};
    use domovoi_core::rpc::Route;
    let (mut model, _) = connected();
    long_thread(&mut model, 5);
    model.changes.wide();
    let (stream, _) = model.rpc.request("orchestration.subscribeThread", &json!({ "threadId": "t1" }), Route::Thread("t1".into()));
    let ffi = FfiApp::checked();
    let mut shadow = domovoi_core::view::ViewModel::default();
    let reply = |text: &str, seq: u64| {
        records::stream_event(seq, "turn-item.updated", &format!("3.{seq}"), json!({ "id": "il0", "threadId": "t1", "runId": "rl", "type": "assistant_message", "messageId": "ml0",
            "ordinal": 1000, "status": "running", "text": text, "streaming": true, "updatedAt": format!("3.{seq}") }))
    };
    let _ = sock(&mut model, chunk(&stream, &[reply("Start.", 11)]));
    ffi.view(&model).apply(&mut shadow);
    assert!(!shadow.composer.can_send, "nothing typed yet");
    // The first character asks for a render: the send button turns on.
    let effects = run_event(&mut model, Event::SetDraft { text: "h".into() });
    assert!(effects.iter().any(|e| matches!(e, Effect::Render(_))), "the send button changes, so the view is read");
    let delta: ViewDelta = ffi.view(&model);
    assert!(delta.composer.as_ref().is_some_and(|c| c.can_send) && delta.thread.is_none() && delta.sidebar.is_none(), "only the composer changed: {delta:?}");
    delta.apply(&mut shadow);
    // More characters change nothing the composer derives: no render.
    let effects = run_event(&mut model, Event::SetDraft { text: "hello".into() });
    assert!(!effects.iter().any(|e| matches!(e, Effect::Render(_))), "a keystroke that changes nothing shown is not rendered");
    let _ = run_event(&mut model, Event::Tick { now_ms: 5_000 });
    let _ = run_event(&mut model, Event::Tick { now_ms: 10_000 });
    // The next token: from its row alone, though typing and ticks came between.
    let from_rows = ffi.views_from_rows();
    let _ = sock(&mut model, chunk(&stream, &[reply("Start. More.", 12)]));
    ffi.view(&model).apply(&mut shadow);
    assert_eq!(ffi.views_from_rows(), from_rows + 1, "the token after typing was built from its row");
    assert_eq!(shadow, DomovoiApp.view(&model));
    // Clearing the draft turns the button off again.
    let effects = run_event(&mut model, Event::SetDraft { text: "  ".into() });
    assert!(effects.iter().any(|e| matches!(e, Effect::Render(_))));
    ffi.view(&model).apply(&mut shadow);
    assert!(!shadow.composer.can_send);
}

/// An offline copy saved by the build that kept the thread list as the JSON
/// the server sent (the copy the recorded session's walk left behind,
/// `tests/view_digests.rs`).
const OFFLINE_COPY_FROM_JSON_STATE: &str = include_str!("fixtures/offline-copy-from-json-state.json");

/// A launch from `saved` with no server, the thread it holds open.
fn launched_from(saved: &Value) -> Model {
    let open = saved["thread"]["thread"]["id"].as_str().map(str::to_owned);
    let mut model = Model { active_env: Some("env".into()), selected_thread: open, route: "thread".into(), ..Default::default() };
    let _ = run(&mut model, Event::CacheLoaded { environment_id: "env".into(), value: Some(saved.to_string()) });
    model
}

#[test]
fn an_offline_copy_saved_as_json_state_still_loads_and_loses_nothing() {
    let saved: Value = serde_json::from_str(OFFLINE_COPY_FROM_JSON_STATE).unwrap();
    let mut model = launched_from(&saved);
    model.sidebar_layout.grouping = Grouping::Project;
    let view = DomovoiApp.view(&model);
    assert!(view.connection.showing_saved);
    let listed = view.sidebar.projects.iter().map(|p| p.threads.len() + p.earlier.len()).sum::<usize>();
    assert!(listed > 0 && model.threads.len() == saved["threads"].as_array().unwrap().len(), "every saved thread is held");
    assert!(!view.thread.expect("the thread that was open").items.is_empty(), "with its rows");
    // The records held are the ones saved: each encodes back to what the copy has.
    assert_eq!(serde_json::to_value(&model.projects).unwrap(), saved["projects"]);
    assert_eq!(serde_json::to_value(&model.threads).unwrap(), saved["threads"]);
}

#[test]
fn an_offline_copy_this_app_cannot_read_is_left_alone() {
    // A thread in the list that is no thread shell (saved by a version that kept another shape).
    let mut saved: Value = serde_json::from_str(OFFLINE_COPY_FROM_JSON_STATE).unwrap();
    saved["threads"][0].as_object_mut().unwrap().remove("title");
    let mut model = launched_from(&saved);
    let view = DomovoiApp.view(&model);
    assert!(model.threads.is_empty() && model.projects.is_empty() && model.thread.is_none(), "nothing of it is shown");
    assert!(!view.connection.showing_saved && view.toast.is_none(), "and nothing is said: the server's own list comes next");
    // Which it does, as on any launch.
    model.connection = domovoi_core::model::Connection::Connected;
    let snapshot = records::shell_snapshot(1, json!([{ "id": "p1" }]), json!([{ "id": "t1", "projectId": "p1", "title": "From the server" }]));
    let _ = domovoi_core::replies::handle(&mut model, domovoi_core::rpc::Reply::Chunk { id: "x".into(), route: domovoi_core::rpc::Route::Shell, values: vec![snapshot] });
    model.sidebar_layout.grouping = Grouping::Project;
    assert_eq!(DomovoiApp.view(&model).sidebar.projects[0].threads[0].title, "From the server");
}

#[test]
fn what_the_server_sends_that_fits_no_record_is_left_out_and_the_user_is_told() {
    let (mut model, sent) = connected();
    let id_of = |tag: &str| sent.iter().find(|r| r["tag"] == tag).unwrap()["id"].as_str().unwrap().to_owned();
    let (shell_id, config_id) = (id_of("orchestration.subscribeShell"), id_of("subscribeServerConfig"));
    let _ = sock(&mut model, chunk(&shell_id, &[records::shell_snapshot(5, json!([{ "id": "p1" }]), json!([{ "id": "t1", "projectId": "p1", "title": "Kept" }]))]));

    // A thread of the list without what every thread has: the list stays as
    // it was, the stream goes on from after the item, and the user is told.
    let unreadable = |sequence: u64| json!({ "kind": "thread.updated", "sequence": sequence, "location": "active", "thread": { "id": "t2", "title": 7 } });
    let _ = sock(&mut model, chunk(&shell_id, &[unreadable(6)]));
    model.sidebar_layout.grouping = Grouping::Project;
    let view = DomovoiApp.view(&model);
    assert_eq!(view.sidebar.projects[0].threads.iter().map(|t| t.title.as_str()).collect::<Vec<_>>(), ["Kept"]);
    let toast = view.toast.expect("the user is told");
    assert!(toast.kind == "error" && toast.message.contains("part of the thread list"), "{}", toast.message);
    assert_eq!((model.shell_sequence, model.unreadable.get("part of the thread list")), (Some(6), Some(&1)));
    // Told once: the next one is counted.
    let _ = run(&mut model, Event::DismissToast);
    let _ = sock(&mut model, chunk(&shell_id, &[unreadable(7)]));
    assert!(DomovoiApp.view(&model).toast.is_none());
    assert_eq!((model.shell_sequence, model.unreadable.get("part of the thread list")), (Some(7), Some(&2)));

    // One provider of two that fits no record: the other is listed.
    let providers = json!([{ "instanceId": "claudeAgent", "displayName": "Claude" }, { "instanceId": 5, "displayName": "Mystery" }]);
    let _ = sock(&mut model, chunk(&config_id, &[records::config_snapshot(providers, json!({}))]));
    let view = DomovoiApp.view(&model);
    assert_eq!(view.settings.providers.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(), ["Claude"]);
    assert!(view.toast.expect("the user is told").message.contains("a provider"));
    let _ = run(&mut model, Event::DismissToast);

    // A call's answer that is not what the call answers with: nothing of it is kept.
    let sent = sent_requests(&run(&mut model, Event::LoadUsage { days: 7 }));
    let _ = sock(&mut model, exit_ok(sent[0]["id"].as_str().unwrap(), json!({ "buckets": "many" })));
    let view = DomovoiApp.view(&model);
    assert!(!view.usage.loading && view.usage.rows.is_empty() && model.pages.usage.is_none());
    assert!(view.toast.expect("the user is told").message.contains("the usage summary"));
}

#[test]
fn usage_days_follow_the_time_zone_the_shell_reports() {
    let (mut model, _) = connected();
    model.now_ms = 1_795_563_000_000; // 2026-11-24 23:30 UTC
    // No time zone from the shell: the days are UTC's.
    let sent = sent_requests(&run(&mut model, Event::LoadUsage { days: 1 }));
    let body = &sent.iter().find(|r| r["tag"] == "server.getUsageSummary").expect("usage asked for")["payload"];
    assert_eq!((body["sinceDay"].as_str(), body["untilDay"].as_str(), body["timeZone"].as_str()), (Some("2026-11-24"), Some("2026-11-24"), Some("UTC")));
    // An hour east of UTC it is already the 25th there.
    let _ = run(&mut model, Event::SetTimeZone { iana: "Europe/Warsaw".into(), utc_offset_secs: 3_600 });
    let sent = sent_requests(&run(&mut model, Event::LoadUsage { days: 7 }));
    let body = &sent.iter().find(|r| r["tag"] == "server.getUsageSummary").expect("usage asked for")["payload"];
    assert_eq!((body["sinceDay"].as_str(), body["untilDay"].as_str(), body["timeZone"].as_str()), (Some("2026-11-19"), Some("2026-11-25"), Some("Europe/Warsaw")));
}

#[test]
fn a_notice_from_the_shell_shows_in_the_toast() {
    let (mut model, _) = connected();
    let _ = run(&mut model, Event::ShowNotice { kind: "success".into(), message: "Image copied".into() });
    let toast = DomovoiApp.view(&model).toast.expect("the notice shows");
    assert_eq!((toast.kind.as_str(), toast.message.as_str()), ("success", "Image copied"));
    // The same one again isn't shown twice; an unknown kind reads as info.
    let _ = run(&mut model, Event::ShowNotice { kind: "success".into(), message: "Image copied".into() });
    let _ = run(&mut model, Event::ShowNotice { kind: "shout".into(), message: "Saved".into() });
    assert_eq!(DomovoiApp.view(&model).toast.unwrap().queued, 1);
    let _ = run(&mut model, Event::DismissToast);
    let toast = DomovoiApp.view(&model).toast.expect("the next one shows");
    assert_eq!((toast.kind.as_str(), toast.message.as_str()), ("info", "Saved"));
}

// ---- everything the core sends ----

/// When `every_request_the_core_sends_is_as_recorded` happens.
const SENDS_NOW: i64 = 1_795_000_000_000;

/// A client driven through every request it can make of a server. Each
/// step records what it sent: an RPC request as its method and payload, an
/// HTTP request as its path and body. Ids the core makes up are named by
/// their order.
struct Sends {
    shell: Shell,
    model: Model,
    made: Vec<String>,
    steps: Vec<Value>,
    /// The id of the last request sent of each RPC method.
    open: HashMap<String, String>,
    /// The commands sent and not answered yet, by request id.
    commands: Vec<String>,
}

impl Sends {
    fn new() -> Self {
        let mut shell = Shell::paired();
        shell.refused = Some(vec![]);
        Self { shell, model: Model::default(), made: vec![], steps: vec![], open: HashMap::new(), commands: vec![] }
    }

    /// Names the ids the core made up by their order, in `text`.
    fn named(&mut self, text: &str) -> String {
        let mut out = String::with_capacity(text.len());
        let mut from = 0;
        for at in common::uuids(text) {
            let id = &text[at..at + 36];
            out.push_str(&text[from..at]);
            from = at + 36;
            let n = self.made.iter().position(|made| made == id).unwrap_or_else(|| {
                self.made.push(id.to_owned());
                self.made.len() - 1
            });
            out.push_str(&format!("made-{n}"));
        }
        out.push_str(&text[from..]);
        out
    }

    /// What `effects` and the HTTP requests since the last step sent, in an
    /// order that does not depend on the ids in it. Acknowledgements of
    /// chunks are framing, and left out.
    fn sent(&mut self, effects: &[Effect]) -> Vec<Value> {
        let mut sent: Vec<Value> = std::mem::take(&mut self.shell.requests)
            .into_iter()
            .map(|request| {
                let path = request.url.splitn(4, '/').nth(3).map(|path| format!("/{path}")).unwrap_or_default();
                let body = serde_json::from_slice::<Value>(&request.body).unwrap_or_else(|_| json!(String::from_utf8_lossy(&request.body)));
                json!({ "http": request.method, "path": path, "body": body })
            })
            .collect();
        for effect in effects {
            let Effect::Socket(request) = effect else { continue };
            match &request.operation {
                SocketOperation::Send { text } => {
                    let frame: Value = serde_json::from_str(text).expect("a frame is JSON");
                    match frame["_tag"].as_str() {
                        Some("Request") => {
                            let (method, id) = (frame["tag"].as_str().unwrap_or_default().to_owned(), frame["id"].as_str().unwrap_or_default().to_owned());
                            if method == "orchestration.dispatchCommand" {
                                self.commands.push(id.clone());
                            }
                            self.open.insert(method, id);
                            assert_eq!(frame.as_object().map(|fields| fields.len()), Some(4), "a request is its id, its method and its payload: {frame}");
                            sent.push(json!({ "rpc": frame["tag"], "payload": frame["payload"] }));
                        }
                        Some("Ack") => {}
                        other => sent.push(json!({ "rpc": other })),
                    }
                }
                SocketOperation::Open { .. } => sent.push(json!("socket opened")),
                other => sent.push(json!(format!("{other:?}"))),
            }
        }
        let mut keyed: Vec<(String, String)> = sent
            .iter()
            .map(|frame| {
                let text = common::canonical(frame);
                let mut blank = text.clone();
                for at in common::uuids(&text) {
                    blank.replace_range(at..at + 36, "------------------------------------");
                }
                (blank, text)
            })
            .collect();
        keyed.sort();
        keyed.into_iter().map(|(_, text)| serde_json::from_str(&self.named(&text)).expect("canonical JSON")).collect()
    }

    fn record(&mut self, label: &str, effects: &[Effect]) -> usize {
        let sent = self.sent(effects);
        let count = sent.len();
        self.steps.push(json!({ "step": label, "sent": sent }));
        count
    }

    /// An intent that sends something. The server takes the commands it
    /// sent, so none waits to be sent again.
    fn go(&mut self, label: &str, event: Event) {
        let effects = self.shell.drive(&mut self.model, event);
        assert!(self.record(label, &effects) > 0, "{label}: nothing was sent");
        self.commands_taken();
    }

    fn commands_taken(&mut self) {
        for id in std::mem::take(&mut self.commands) {
            let effects = self.shell.drive_sock(&mut self.model, exit_ok(&id, json!({ "sequence": 1 })));
            let sent = self.sent(&effects);
            assert!(sent.is_empty(), "a command's answer sent {sent:?}");
        }
    }

    /// An intent that sends nothing.
    fn set(&mut self, event: Event) {
        let what = format!("{event:?}");
        let effects = self.shell.drive(&mut self.model, event);
        let sent = self.sent(&effects);
        assert!(sent.is_empty(), "{what} sent {sent:?}");
    }

    /// Something from the server, and whatever the core sends on it.
    fn socket(&mut self, label: &str, event: SocketEvent) {
        let effects = self.shell.drive_sock(&mut self.model, event);
        let sent = self.sent(&effects);
        if !sent.is_empty() {
            self.steps.push(json!({ "step": label, "sent": sent }));
        }
        self.commands_taken();
    }

    fn id(&self, method: &str) -> String {
        self.open.get(method).unwrap_or_else(|| panic!("no {method} request was sent")).clone()
    }

    /// The server answers the last request of `method`.
    fn answer(&mut self, method: &str, value: Value) {
        let id = self.id(method);
        self.socket(&format!("{method} answered"), exit_ok(&id, value));
    }

    /// The server sends `values` on the stream last opened with `method`.
    fn chunk(&mut self, method: &str, values: &[Value]) {
        let id = self.id(method);
        self.socket(&format!("{method} sent"), chunk(&id, values));
    }
}

/// The thread the sends are made on: a run under way with a subagent, two
/// queued messages, a plan, a checkpoint and a message with an image.
fn sends_thread() -> Value {
    json!({
        "thread": { "id": "t1", "activeProviderThreadId": "pt1" },
        "runs": [
            { "id": "r1", "ordinal": 1, "status": "completed", "startedAt": "2026-11-01T00:00:00.000Z" },
            { "id": "r2", "ordinal": 2, "status": "running", "startedAt": "2026-11-01T00:00:01.000Z" },
            { "id": "r4", "ordinal": 4, "status": "queued", "queuePosition": 2, "userMessageId": "m4" },
            { "id": "r3", "ordinal": 3, "status": "queued", "queuePosition": 1, "userMessageId": "m3" },
        ],
        "messages": [
            { "id": "m1", "runId": "r1", "role": "user", "text": "look", "attachments": [{ "type": "image", "id": "t1-abc", "name": "shot.png", "mimeType": "image/png", "sizeBytes": 3 }] },
            { "id": "m3", "runId": "r3", "role": "user", "text": "then add tests" },
            { "id": "m4", "runId": "r4", "role": "user", "text": "and docs" },
        ],
        "providerThreads": [{ "id": "pt1" }],
        // What it asked: an approval, and questions (answered by `rq1`, `rq2`).
        "runtimeRequests": [{ "id": "rq1", "runId": "r2" }, { "id": "rq2", "runId": "r2", "kind": "user_input" }],
        "plans": [{ "id": "plan1", "kind": "proposed_plan", "status": "active" }],
        "subagents": [{ "id": "node-sub", "status": "running", "nativeTaskRef": { "nativeId": "toolu_1" }, "childThreadId": null }],
        "checkpoints": [{
            "id": "cp1", "threadId": "t1", "scopeId": "scope1", "runId": "r1", "nodeId": "root", "parentCheckpointId": null, "ordinalWithinScope": 1,
            "appRunOrdinal": 1, "ref": "refs/domovoi/cp1", "status": "ready", "files": [], "capturedAt": "2026-11-01T00:00:00.500Z",
        }],
        "turnItems": [
            { "id": "i-user", "runId": "r1", "ordinal": 1, "type": "user_message", "messageId": "m1", "status": "completed" },
            { "id": "i-plan", "runId": "r1", "ordinal": 2, "type": "proposed_plan", "planId": "plan1", "markdown": "1. Do it", "status": "completed" },
            { "id": "i-sub", "runId": "r2", "ordinal": 3, "type": "subagent", "subagentId": "node-sub", "title": "Find callers", "prompt": "Find every caller", "status": "running" },
            { "id": "i-sub-no-run", "ordinal": 4, "type": "subagent", "subagentId": "node-other", "title": "Stray", "prompt": "", "status": "running" },
        ],
    })
}

/// The thread list the sends are made on.
fn sends_shells(t1: Value) -> Value {
    let mut running = json!({
        "id": "t1", "projectId": "p1", "title": "Search", "providerInstanceId": "claudeAgent",
        "modelSelection": { "instanceId": "claudeAgent", "model": "claude-haiku-5-5" }, "status": "running", "activeRunId": "r2",
        "activityRunStatus": "running", "pendingRuntimeRequest": null, "lineage": null, "updatedAt": "2026-11-01T00:00:02.000Z",
    });
    running.as_object_mut().expect("an object").extend(t1.as_object().cloned().unwrap_or_default());
    json!([
        running,
        { "id": "t2", "projectId": "p1", "title": "A fork", "status": "idle", "updatedAt": "2026-11-01T00:00:02.000Z",
          "lineage": { "parentThreadId": "t1", "relationshipToParent": "fork", "rootThreadId": "t1" } },
        { "id": "t3", "projectId": "p1", "title": "Waiting", "status": "running", "updatedAt": "2026-11-01T00:00:02.000Z", "lineage": null,
          "pendingRuntimeRequest": { "id": "rq3", "kind": "command" } },
    ])
}

/// Every request the core can make of a server, as it was recorded
/// (`fixtures/requests-sent.json`): the commands field by field, the other
/// RPCs' payloads, the HTTP bodies. A request that changes shows here as the
/// one step that sent it. `DOMOVOI_REQUESTS_SENT_OUT` names a file to write what is
/// sent now to, as the next recording.
///
/// Along the way every command and launch must be one the server reads as
/// it was written (`common::read_by_the_server`).
#[test]
fn every_request_the_core_sends_is_as_recorded() {
    let mut s = Sends::new();
    for path in ["/api/domovoi/remote", "/api/domovoi/remote/configure"] {
        s.shell.routes.insert(path.into(), (200, json!({ "enabled": true, "running": true })));
    }
    s.set(Event::Tick { now_ms: SENDS_NOW });
    s.set(Event::Launch { surface: "mobile".into(), device_label: "A phone".into(), device_os: "iOS 19".into() });
    s.go("pair with a code and connect", Event::Connect { server_url: "http://127.0.0.1:1".into(), pairing_code: "CODE".into() });
    s.socket("the socket opens", SocketEvent::Opened);
    // Before the server has said which models it has, a message names none.
    s.go("open a thread the list does not have yet", Event::SelectThread { thread_id: "t0".into() });
    s.set(Event::SetDraft { text: "early".into() });
    s.go("send a message before the providers are known", Event::Send);

    // What the server says of itself, its providers and its threads.
    let providers = json!([
        { "instanceId": "claudeAgent", "driver": "claudeAgent", "displayName": "Claude", "status": "ready", "enabled": true, "models": [
            { "slug": "claude-haiku-5-5", "name": "Haiku 5.5", "isDefault": true, "capabilities": { "optionDescriptors": [
                { "id": "effort", "type": "select", "options": [{ "id": "low", "label": "Low" }, { "id": "medium", "label": "Medium", "isDefault": true }] }] } },
            { "slug": "claude-sonnet-5", "name": "Sonnet 5" }] },
        { "instanceId": "codex", "driver": "codex", "displayName": "Codex", "status": "ready", "enabled": true, "models": [{ "slug": "gpt-6", "name": "GPT-6" }] },
    ]);
    let settings = json!({
        "defaultRuntimeMode": "approval-required", "sidebarAutoSettleAfterDays": 7, "favorites": [{ "provider": "codex", "model": "gpt-6" }],
        "providerModelPreferences": { "claudeAgent": { "hiddenModels": ["claude-sonnet-5"], "modelOrder": [] } },
    });
    s.chunk("subscribeServerConfig", &[records::config_snapshot(providers, settings)]);
    s.chunk("subscribeServerLifecycle", &[records::welcome(json!({}))]);
    let projects = json!([{ "id": "p1", "title": "App", "workspaceRoot": "/w" }, { "id": "p-site", "title": "site", "workspaceRoot": "/w/site" }]);
    s.chunk("orchestration.subscribeShell", &[records::shell_snapshot(5, projects, sends_shells(json!({})))]);

    // ---- a thread, and the commands made on it ----
    s.go("open a thread", Event::SelectThread { thread_id: "t1".into() });
    let thread_stream = s.id("orchestration.subscribeThread");
    s.socket("the thread arrives, with an image", chunk(&thread_stream, &[records::snapshot(7, sends_thread())]));
    s.go("answer an approval", Event::Respond { request_id: "rq1".into(), decision: "acceptForSession".into() });
    let answers = ["Which database?", "Which features?", "Anything else?"].into_iter().zip([vec!["SQLite"], vec![], vec!["Auth", "Search"]]);
    let answers = answers.map(|(question, values)| QuestionAnswer { question_id: question.into(), values: values.into_iter().map(str::to_owned).collect() }).collect();
    s.go("answer a question", Event::RespondUserInput { request_id: "rq2".into(), answers });
    s.go("skip a question", Event::DismissUserInput { request_id: "rq2".into() });
    s.go("stop the turn", Event::Interrupt);
    s.go("remove a queued message", Event::CancelQueued { run_id: "r4".into() });
    s.go("edit a queued message", Event::EditQueued { run_id: "r3".into(), text: " tests first ".into() });
    s.go("empty a queued message", Event::EditQueued { run_id: "r3".into(), text: " ".into() });
    s.go("send a queued message now", Event::SteerQueued { run_id: "r3".into() });
    s.go("move a queued message up", Event::MoveQueued { run_id: "r4".into(), delta: -1 });
    s.go("move a queued message to the end", Event::MoveQueued { run_id: "r3".into(), delta: 1 });
    s.go("resume the queue", Event::ResumeQueue);
    s.go("fork a thread", Event::ForkThread { thread_id: "t1".into() });
    s.go("fork a thread the list does not have", Event::ForkThread { thread_id: "gone".into() });
    s.go("stop a subagent", Event::StopSubagent { subagent_id: "i-sub".into() });
    s.go("stop a subagent whose row names no run", Event::StopSubagent { subagent_id: "i-sub-no-run".into() });
    s.go("stop a subagent whose row is gone", Event::StopSubagent { subagent_id: "node-gone".into() });
    s.go("implement a plan", Event::ImplementPlan { plan_id: "plan1".into() });
    s.go("regenerate the title", Event::RegenerateTitle { thread_id: "t1".into() });
    s.go("revert to a checkpoint", Event::Revert { checkpoint_id: "cp1".into() });
    s.go("revert to a checkpoint the thread does not have", Event::Revert { checkpoint_id: "cp-gone".into() });
    s.go("change permissions", Event::SetRuntimeMode { mode: "approval-required".into() });
    s.go("change to plan mode", Event::SetInteractionMode { mode: "plan".into() });
    s.go("find in the thread", Event::FindInThread { query: "look".into() });
    let found = |active: Value| json!({ "complete": true, "snapshotSequence": 7, "totalMatches": 2, "activeIndex": 0, "match": active, "navigation": [{ "entryId": "m1", "runId": "r1", "startIndex": 0, "count": 2 }] });
    s.answer("orchestration.searchThread", found(json!({ "entryId": "m1", "runId": "r1", "occurrence": 0 })));
    s.go("the next match", Event::FindNext { forward: true });
    s.answer("orchestration.searchThread", found(Value::Null));
    s.go("the match before, from none", Event::FindNext { forward: false });
    s.set(Event::CloseFind);
    s.go("change the model", Event::SelectModel { instance_id: "claudeAgent".into(), model: "claude-sonnet-5".into() });
    s.go("change the model to one with an effort", Event::SelectModel { instance_id: "claudeAgent".into(), model: "claude-haiku-5-5".into() });
    s.go("change the effort", Event::SelectEffort { value: "low".into() });
    s.set(Event::SetDraft { text: "later".into() });
    s.go("send a message", Event::Send);
    s.set(Event::SetDraft { text: "now".into() });
    s.go("send a message into the turn", Event::Steer);
    s.set(Event::AddAttachment { name: "shot.png".into(), mime_type: "image/png".into(), size_bytes: 3, data_url: "data:image/png;base64,AAAA".into() });
    s.set(Event::SetDraft { text: "see this".into() });
    s.go("send a message with an image: the image first", Event::Send);
    s.answer("assets.persistChatAttachments", json!({ "attachments": [{ "type": "image", "id": "t1-new", "name": "shot.png", "mimeType": "image/png", "sizeBytes": 3 }] }));
    // In plan mode a plan is implemented out of it.
    let shell_stream = s.id("orchestration.subscribeShell");
    let in_plan_mode = sends_shells(json!({ "interactionMode": "plan" }))[0].clone();
    s.socket("the thread is in plan mode", chunk(&shell_stream, &[json!({ "kind": "thread.updated", "sequence": 6, "location": "active", "thread": records::shell_json(in_plan_mode) })]));
    s.go("implement a plan from plan mode", Event::ImplementPlan { plan_id: "plan1".into() });
    s.go("rename a thread", Event::RenameThread { thread_id: "t1".into(), title: " New name ".into() });
    s.go("pin a thread", Event::PinThread { thread_id: "t1".into(), pinned: true });
    s.go("unpin a thread", Event::PinThread { thread_id: "t1".into(), pinned: false });
    s.go("settle a thread", Event::SettleThread { thread_id: "t1".into(), settled: true });
    s.go("unsettle a thread", Event::SettleThread { thread_id: "t1".into(), settled: false });
    s.go("snooze a thread", Event::SnoozeThread { thread_id: "t1".into(), until_ms: SENDS_NOW + 3_600_000 });
    s.go("undo the snooze", Event::ToastAction);
    s.go("wake a thread", Event::SnoozeThread { thread_id: "t1".into(), until_ms: 0 });
    s.go("mark a thread unread", Event::MarkThreadUnread { thread_id: "t1".into() });
    s.go("answer an approval from its notification", Event::RespondFromNotification { environment_id: String::new(), thread_id: "t3".into(), request_id: "rq3".into(), decision: "accept".into() });
    s.go("answer an approval from a live activity", Event::RespondToThread { environment_id: String::new(), thread_id: "t3".into(), decision: "decline".into() });
    s.go("answer a question from its notification", Event::AnswerFromNotification { environment_id: String::new(), thread_id: "t3".into(), request_id: "rq3".into(), question_id: "Which database?".into(), text: " SQLite ".into() });

    // ---- the panel ----
    s.go("the thread's diff", Event::ShowPanel { tab: "diff".into() });
    s.go("a turn's diff", Event::SelectDiffSource { source: "turn:1".into() });
    s.go("the branch's diff", Event::SelectDiffSource { source: "branch".into() });
    s.go("the working tree's diff", Event::SelectDiffSource { source: "working".into() });
    s.go("the files", Event::ShowPanel { tab: "files".into() });
    s.go("a folder", Event::ToggleDirectory { path: "src".into() });
    s.go("a file", Event::OpenFile { path: "src/main.rs".into() });
    s.answer("projects.readFile", json!({
        "relativePath": "src/main.rs", "contents": "", "byteLength": 9, "truncated": false, "exists": true,
        "lfs": { "oid": "ab".repeat(32), "size": 9, "state": "missing" }
    }));
    s.go("fetch the file's Git LFS object", Event::FetchLfsObject { path: "src/main.rs".into() });
    s.go("search the files", Event::SetFileQuery { query: "ma".into() });
    s.go("a terminal", Event::ShowPanel { tab: "terminal".into() });
    s.go("type in the terminal", Event::TerminalKey { key: "a".into(), text: "a".into(), ctrl: false, alt: false });
    s.go("paste in the terminal", Event::TerminalPaste { text: "ls -la".into() });
    s.go("clear the terminal", Event::ClearTerminal);
    s.go("resize the terminal", Event::TerminalResize { rows: 30, cols: 100 });
    s.go("a second terminal", Event::NewTerminal);
    s.go("close a terminal", Event::CloseTerminal { terminal_id: "term-2".into() });
    s.go("refresh git", Event::RefreshGit);
    s.go("commit and push", Event::GitAction { action: "commit_push".into(), message: " Fix it ".into() });
    s.answer("git.runStackedAction", Value::Null);
    s.model.panel.git.busy = false;
    s.go("push without a message", Event::GitAction { action: "push".into(), message: " ".into() });
    s.go("pull", Event::GitPull);
    s.go("switch branch", Event::SwitchBranch { name: "main".into() });
    s.go("create a branch", Event::CreateBranch { name: " feature ".into() });

    // ---- the thread list ----
    s.go("open a fork", Event::SelectThread { thread_id: "t2".into() });
    s.go("merge a fork back", Event::MergeBack);
    while s.model.toast.is_some() {
        s.set(Event::DismissToast);
    }
    s.go("archive the open thread", Event::ArchiveThread { thread_id: "t2".into(), archived: true });
    s.go("undo the archive", Event::ToastAction);
    s.go("unarchive a thread", Event::ArchiveThread { thread_id: "t3".into(), archived: false });
    s.go("delete a thread", Event::DeleteThread { thread_id: "t3".into() });
    s.go("search the threads", Event::SetSearch { query: " the ".into() });

    // ---- new threads ----
    s.go("mention a file", Event::SetDraft { text: "look at @sr".into() });
    s.set(Event::SetDraft { text: "First line\nand more".into() });
    s.go("start a thread in the project's folder", Event::Send);
    s.answer("orchestration.launchThread", json!({ "threadId": "t-new" }));
    s.go("a new thread", Event::NewThread { project_id: Some("p1".into()) });
    s.model.panel.git.busy = false;
    s.go("commit with no thread open", Event::GitAction { action: "commit".into(), message: "Fix it".into() });
    s.answer("git.runStackedAction", Value::Null);
    s.model.panel.git.busy = false;
    s.set(Event::SetWorkspaceMode { mode: "worktree".into() });
    s.set(Event::SetDraft { text: "In a worktree".into() });
    s.go("start a thread in a worktree", Event::Send);
    s.answer("orchestration.launchThread", json!({ "threadId": "t-new" }));
    s.go("a new thread", Event::NewThread { project_id: Some("p1".into()) });
    s.set(Event::SetBaseBranch { branch: "main".into() });
    s.set(Event::SetRuntimeMode { mode: "full-access".into() });
    s.set(Event::SetInteractionMode { mode: "plan".into() });
    s.set(Event::SelectModel { instance_id: "codex".into(), model: "gpt-6".into() });
    s.set(Event::AddAttachment { name: "plan.png".into(), mime_type: "image/png".into(), size_bytes: 4, data_url: "data:image/png;base64,BBBB".into() });
    s.set(Event::SetDraft { text: "In a worktree of main".into() });
    s.go("start a thread with an image: the image first", Event::Send);
    s.answer("assets.persistChatAttachments", json!({ "attachments": [{ "type": "image", "id": "new-img", "name": "plan.png", "mimeType": "image/png", "sizeBytes": 4 }] }));
    s.answer("orchestration.launchThread", json!({ "threadId": "t-new" }));
    s.go("a scheduled task", Event::CreateScheduled { name: "Nightly".into(), prompt: "Run the tests".into(), project_id: "p1".into(), interval_minutes: 90 });

    // ---- pull requests ----
    s.go("the pull requests", Event::ShowRoute { route: "pull-requests".into() });
    s.model.pages.prs = vec![records::pull_request(json!({ "projectId": "p1", "repository": "o/calc", "number": 12, "title": "Fix parser", "state": "open", "isDraft": false }))];
    let key = "p1#o/calc#12".to_owned();
    s.go("a pull request", Event::SelectPullRequest { key: key.clone() });
    s.go("merge a pull request", Event::PullRequestAction { key: key.clone(), action: "merge".into(), merge_method: "squash".into() });
    s.go("merge a pull request the default way", Event::PullRequestAction { key: key.clone(), action: "merge".into(), merge_method: String::new() });
    s.go("close a pull request", Event::PullRequestAction { key: key.clone(), action: "close".into(), merge_method: "squash".into() });
    s.go("review a pull request", Event::ReviewPullRequest { key: key.clone(), verdict: "approve".into(), body: " fine ".into() });
    s.go("comment on a pull request", Event::CommentOnPullRequest { key: key.clone(), body: " note ".into() });
    s.go("a thread for a pull request", Event::StartPullRequestThread { key: key.clone() });
    let pull_request = json!({ "number": 12, "title": "Fix parser", "url": "https://example.test/o/calc/pull/12", "baseBranch": "main", "headBranch": "fix-parser", "state": "open" });
    s.answer("git.preparePullRequestThread", json!({ "pullRequest": pull_request, "worktreePath": "/wt/pr-12", "branch": "fix-parser" }));
    s.set(Event::SetDraft { text: "Review this".into() });
    s.go("start a thread in the pull request's worktree", Event::Send);
    s.answer("orchestration.launchThread", json!({ "threadId": "t-new" }));

    // ---- projects, imports, folders ----
    s.go("add a project by its path", Event::AddProject { path: "/work/new/".into() });
    s.go("create a project", Event::CreateProject { name: " new ".into() });
    s.go("rename a project", Event::RenameProject { project_id: "p1".into(), title: " Renamed ".into() });
    s.go("remove a project", Event::RemoveProject { project_id: "p-site".into() });
    s.go("the scratch project", Event::OpenScratch);
    s.go("a project from a scratch thread", Event::PromoteScratchThread { thread_id: "t1".into() });
    s.go("look for sessions to import", Event::ScanAgentSessions);
    s.answer("agentSessions.scan", json!({ "candidates": [
        { "path": "/w/calc", "title": "calc", "sources": ["claudeAgent"], "threadCount": 3, "lastActiveAt": "2026-11-13T10:00:00.000Z", "alreadyImported": false, "git": null },
        { "path": "/w/site", "title": "site", "sources": ["codex"], "threadCount": 1, "lastActiveAt": "2026-11-10T10:00:00.000Z", "alreadyImported": true, "projectId": "p-site", "git": null },
    ], "scannedAt": "2026-11-14T00:00:00.000Z" }));
    s.go("import a folder's sessions: its project first", Event::ImportAgentSessions { paths: vec!["/w/calc".into(), "/w/site".into()] });
    s.answer("projects.mutate", json!({ "sequence": 1 }));
    s.answer("agentSessions.import", json!({ "importedCount": 3, "skippedCount": 0 }));
    s.answer("agentSessions.import", json!({ "importedCount": 1, "skippedCount": 0 }));
    s.go("browse the server's folders", Event::BrowseFolders { path: "/w/".into() });

    // ---- settings ----
    for section in ["providers", "scheduled", "archived", "storage", "diagnostics", "access"] {
        s.go(&format!("the {section} settings"), Event::LoadSettings { section: section.into() });
    }
    s.go("switch a setting", Event::SetSettingBool { key: "defaultAutoPull".into(), value: true });
    s.go("type a setting", Event::SetSettingText { key: "worktreesDirectory".into(), value: "somewhere".into() });
    s.go("choose a setting", Event::SetSettingText { key: "defaultRuntimeMode".into(), value: "full-access".into() });
    s.go("type a number setting", Event::SetSettingText { key: "sidebarAutoSettleAfterDays".into(), value: " 12 ".into() });
    s.go("type words into a number setting", Event::SetSettingText { key: "sidebarAutoSettleAfterDays".into(), value: "soon".into() });
    s.go("switch a setting this build does not know", Event::SetSettingBool { key: "aNewerSetting".into(), value: false });
    s.go("favorite a model", Event::SetModelFavorite { instance_id: "claudeAgent".into(), model: "claude-haiku-5-5".into(), favorite: true });
    s.go("favorite a hidden model", Event::SetModelFavorite { instance_id: "claudeAgent".into(), model: "claude-sonnet-5".into(), favorite: true });
    s.go("unfavorite a model", Event::SetModelFavorite { instance_id: "claudeAgent".into(), model: "claude-sonnet-5".into(), favorite: false });
    s.go("hide a favorite model", Event::SetModelHidden { instance_id: "codex".into(), model: "gpt-6".into(), hidden: true });
    s.go("show a hidden model", Event::SetModelHidden { instance_id: "codex".into(), model: "gpt-6".into(), hidden: false });
    s.go("refresh the providers", Event::RefreshProviders);
    s.go("sign a provider in", Event::StartProviderSignIn { instance_id: "antigravity".into() });
    s.go("install a provider's runtime", Event::InstallProvider { instance_id: "antigravity".into() });
    s.go("remove a provider's runtime", Event::RemoveProviderInstall { instance_id: "antigravity".into() });
    let key = ProviderInstanceEdit {
        driver: "antigravity".into(), display_name: "Work".into(), enabled: true, auth_method: "gemini-api-key".into(), api_key: "AIza-key".into(), ..Default::default()
    };
    s.go("add a provider instance with a key", Event::SaveProviderInstance { edit: Box::new(key) });
    let vertex = ProviderInstanceEdit {
        instance_id: "ag-work".into(), driver: "antigravity".into(), display_name: "Work".into(), enabled: false, auth_method: "agent-platform".into(),
        gcp_project: "my-project".into(), gcp_location: "us-central1".into(), binary_path: "/opt/agy".into(), ..Default::default()
    };
    s.go("change a provider instance, keeping its key", Event::SaveProviderInstance { edit: Box::new(vertex) });
    s.go("remove a provider instance", Event::RemoveProviderInstance { instance_id: "ag-work".into() });
    s.go("clean up storage", Event::RunStorageCleanup);
    s.go("reclaim database space", Event::ReclaimDatabase);
    s.go("turn a scheduled task off", Event::SetScheduledEnabled { id: "task-1".into(), enabled: false });
    s.go("run a scheduled task now", Event::RunScheduledNow { id: "task-1".into() });
    s.go("delete a scheduled task", Event::DeleteScheduled { id: "task-1".into() });
    s.go("remove a keybinding", Event::RemoveKeybinding { command: "thread.new".into(), shortcut: "mod+n".into(), condition: String::new() });
    s.go("remove a keybinding with a condition", Event::RemoveKeybinding { command: "thread.new".into(), shortcut: "mod+n".into(), condition: "!terminalFocus".into() });
    s.go("the usage", Event::LoadUsage { days: 7 });
    let weekly = ScheduledTaskEdit {
        name: "Standup".into(), prompt: "Summarize yesterday".into(), project_id: "p1".into(), schedule_kind: "fixed_time".into(), time_of_day: "9:30".into(),
        weekdays: vec![5, 1, 3, 1], thread_mode: "fresh_session".into(), thread_id: "t1".into(), workspace: "worktree".into(), base_branch: "main".into(),
        model_instance: "codex".into(), model: "gpt-6".into(), runtime_mode: "full-access".into(), interaction_mode: "plan".into(), enabled: true, ..Default::default()
    };
    s.go("a scheduled task with every setting", Event::SaveScheduled { task: Box::new(weekly) });
    let first_run = ScheduledTaskEdit {
        name: "Nightly".into(), prompt: "Run the tests".into(), project_id: "p1".into(), schedule_kind: "interval".into(), interval_minutes: 60,
        thread_mode: "continue".into(), workspace: "root".into(), runtime_mode: "full-access".into(), enabled: true, ..Default::default()
    };
    s.go("a scheduled task continuing the thread its first run starts", Event::SaveScheduled { task: Box::new(first_run) });
    let continued = ScheduledTaskEdit {
        id: "task-1".into(), name: "Nightly".into(), prompt: "Run the tests".into(), project_id: "p1".into(), schedule_kind: "interval".into(), interval_minutes: 60,
        thread_mode: "continue".into(), thread_id: "t1".into(), workspace: "root".into(), runtime_mode: "full-access".into(), enabled: true, ..Default::default()
    };
    s.go("a scheduled task continuing a chosen thread", Event::SaveScheduled { task: Box::new(continued) });
    let hook = ScheduledTaskEdit { id: "task-1".into(), name: "On push".into(), prompt: "Review {{body.ref}}".into(), project_id: "p1".into(), schedule_kind: "webhook".into(), enabled: false, ..Default::default() };
    s.go("a scheduled task changed to a webhook", Event::SaveScheduled { task: Box::new(hook) });
    s.go("a webhook task's new URL", Event::RotateWebhookToken { id: "task-1".into() });
    s.go("a webhook task's deliveries", Event::LoadWebhookDeliveries { id: "task-1".into() });
    s.go("a scheduled task's runs", Event::LoadScheduledRuns { id: "task-1".into() });
    s.go("forget a thread's allowed tools", Event::ForgetThreadPermissions { thread_id: "t1".into() });
    s.go("a scheduled task's thread gets other permissions", Event::SetThreadRuntimeMode { thread_id: "t1".into(), mode: "full-access".into() });

    // ---- over HTTP ----
    s.go("a push token", Event::PushToken { token: "apns-1".into(), environment: "production".into() });
    s.go("a live activity's token", Event::ActivityToken { thread_id: "t1".into(), token: "activity-1".into(), environment: "sandbox".into() });
    s.go("remote access", Event::LoadRemoteAccess);
    s.go("turn remote access on", Event::SetRemoteAccess { enabled: true });
    s.go("a pairing link", Event::CreatePairingLink);
    s.go("sign a device out", Event::RevokeClient { session_id: "session-1".into() });

    // ---- the connection ----
    s.go("open the thread again", Event::SelectThread { thread_id: "t1".into() });
    let thread_stream = s.id("orchestration.subscribeThread");
    s.socket("the thread arrives again", chunk(&thread_stream, &[records::snapshot(9, sends_thread())]));
    s.go("pull to refresh", Event::Resync);
    s.socket("the socket closes", SocketEvent::Closed { reason: "gone".into() });
    s.set(Event::SettleThread { thread_id: "t1".into(), settled: true });
    s.go("reconnect", Event::Reconnect);
    s.socket("the socket opens again: what waited is sent", SocketEvent::Opened);
    let environment = s.model.active_env.clone().expect("a server is active");
    s.go("forget the server", Event::ForgetEnvironment { id: environment });

    let refused = s.shell.refused.take().unwrap_or_default();
    let sent = Value::Array(std::mem::take(&mut s.steps));
    if let Ok(path) = std::env::var("DOMOVOI_REQUESTS_SENT_OUT") {
        std::fs::write(&path, serde_json::to_string_pretty(&sent).expect("JSON") + "\n").expect("the recording written");
        println!("{} steps written to {path}; {} commands or launches the server would not read as sent", sent.as_array().map_or(0, Vec::len), refused.len());
        for why in &refused {
            println!("    {why}");
        }
        return;
    }
    let recorded: Value = serde_json::from_str(include_str!("fixtures/requests-sent.json")).expect("the recording");
    let (now, was) = (sent.as_array().expect("steps"), recorded.as_array().expect("recorded steps"));
    let mut different = vec![];
    for n in 0..now.len().max(was.len()) {
        match (now.get(n), was.get(n)) {
            (Some(now), Some(was)) if now == was => {}
            (now, was) => different.push(format!(
                "step {n}:\n  recorded: {}\n  now:      {}",
                was.map_or("nothing".to_owned(), |step| step.to_string()),
                now.map_or("nothing".to_owned(), |step| step.to_string())
            )),
        }
    }
    assert!(different.is_empty(), "{} steps send something other than what was recorded:\n{}", different.len(), different.join("\n"));
    assert!(refused.is_empty(), "the server would not read these as sent:\n{}", refused.join("\n"));
}

#[test]
fn project_icons_resolve_legacy_records_and_update_rows_optimistically() {
    let (mut model, _) = connected();
    model.projects = vec![records::project(json!({ "id": "p1", "title": "api2" }))];
    model.threads = vec![records::shell(json!({ "id": "t1", "projectId": "p1" }))];
    let automatic = DomovoiApp.view(&model).sidebar.projects[0].icon.clone();
    assert!(!automatic.custom);
    for icon in [
        json!({ "kind": "monogram", "text": "XY", "color": "rose" }),
        json!({ "kind": "lucide", "name": "folder", "color": "blue", "monogramText": "XY", "monogram": "ZZ" }),
        json!({ "kind": "emoji", "emoji": "🚀" }),
        json!({ "kind": "lucide", "name": "terminal", "color": "green" }),
    ] {
        model.projects[0].project_icon = Some(Some(serde_json::from_value(icon).unwrap()));
        let sidebar = DomovoiApp.view(&model).sidebar;
        assert!(sidebar.projects[0].icon.custom);
        assert_eq!(sidebar.projects[0].icon, sidebar.surface_threads[0].project_icon);
        if sidebar.projects[0].icon.kind == "monogram" { assert_eq!(sidebar.projects[0].icon.text, "XY"); }
    }
    let effects = run(&mut model, Event::SetProjectIcon { project_id: "p1".into(), kind: "monogram".into(), text: "AB".into(), symbol: String::new(), color: "pink".into() });
    assert!(effects.iter().any(|e| matches!(e, Effect::Render(_))));
    assert_eq!(DomovoiApp.view(&model).sidebar.surface_threads[0].project_icon.text, "AB");
    let _ = run(&mut model, Event::SetProjectIcon { project_id: "p1".into(), kind: "monogram".into(), text: "ABC".into(), symbol: String::new(), color: "pink".into() });
    assert_eq!(DomovoiApp.view(&model).sidebar.projects[0].icon.text, "AB");
    use domovoi_core::rpc::{Reply, Route};
    let reply = |ok| Reply::Exit { route: Route::ProjectIconChanged("p1".into()), ok, value: json!({ "message": "Save rejected" }) };
    let _ = domovoi_core::replies::handle(&mut model, reply(true));
    let _ = run(&mut model, Event::SetProjectIcon { project_id: "p1".into(), kind: String::new(), text: String::new(), symbol: String::new(), color: "gray".into() });
    let _ = domovoi_core::replies::handle(&mut model, reply(true));
    assert_eq!(DomovoiApp.view(&model).sidebar.projects[0].icon, automatic);
}

#[test]
fn rejected_project_icon_restores_the_previous_icon_without_overwriting_a_newer_choice() {
    use domovoi_core::rpc::{Reply, Route};
    use domovoi_model::typed::{ProjectIcon, EmojiProjectIcon};
    let (mut model, _) = connected();
    model.projects = vec![records::project(json!({ "id": "p1", "faviconPath": "logo.svg" }))];
    let change = Event::SetProjectIcon { project_id: "p1".into(), kind: "emoji".into(), text: "🚀".into(), symbol: String::new(), color: "gray".into() };
    let rejection = || Reply::Exit { route: Route::ProjectIconChanged("p1".into()), ok: false, value: json!({ "message": "Rejected" }) };
    let _ = run(&mut model, change.clone());
    assert_eq!(model.projects[0].favicon_path, Some(None));
    let _ = domovoi_core::replies::handle(&mut model, rejection());
    assert_eq!(model.projects[0].favicon_path, Some(Some("logo.svg".into())));
    assert!(model.projects[0].project_icon.as_ref().and_then(Option::as_ref).is_none());

    let _ = run(&mut model, change);
    let newer = Some(Some(ProjectIcon::Emoji(EmojiProjectIcon { emoji: "🌱".into() })));
    model.projects[0].project_icon = newer.clone();
    let _ = domovoi_core::replies::handle(&mut model, rejection());
    assert_eq!(model.projects[0].project_icon, newer);
}

#[test]
fn project_file_picker_ignores_stale_results_and_saves_the_previewed_file() {
    let (mut model, _) = connected();
    model.projects = vec![records::project(json!({ "id": "p1", "workspaceRoot": "/workspace" }))];
    let search = |query: &str| Event::SearchProjectIconFiles { project_id: "p1".into(), query: query.into() };
    let first = sent_requests(&run(&mut model, search("old"))).remove(0);
    let latest = sent_requests(&run(&mut model, search("logo"))).remove(0);
    assert_eq!(latest["payload"], json!({ "cwd": "/workspace", "kind": "file", "imageOnly": true, "query": "logo", "limit": 50 }));
    let _ = sock(&mut model, exit_ok(first["id"].as_str().unwrap(), json!({ "entries": [{ "path": "old.png", "kind": "file" }], "truncated": false })));
    assert!(DomovoiApp.view(&model).settings.project_icon_files.files.is_empty());
    let _ = sock(&mut model, exit_ok(latest["id"].as_str().unwrap(), json!({ "entries": [{ "path": "logo.svg", "kind": "file" }], "truncated": false })));
    let thumb = Event::LoadProjectIconThumbnail { project_id: "p1".into(), path: "logo.svg".into() };
    let request = sent_requests(&run(&mut model, thumb)).remove(0);
    assert_eq!(request["payload"]["resource"]["path"], "logo.svg");
    let _ = sock(&mut model, exit_ok(request["id"].as_str().unwrap(), json!({ "relativeUrl": "/api/assets/example", "sourcePath": "logo.svg", "expiresAt": 2_000_000_000_000_i64 })));
    let save = Event::SetProjectIconFile { project_id: "p1".into(), path: "logo.svg".into() };
    let request = sent_requests(&run(&mut model, save)).remove(0);
    assert_eq!(request["payload"]["faviconPath"], "logo.svg");
    assert_eq!(request["payload"]["projectIcon"], Value::Null);
    let view = DomovoiApp.view(&model);
    assert_eq!(view.settings.projects[0].icon.file_path.as_deref(), Some("logo.svg"));
    assert!(view.sidebar.projects[0].icon.image_url.as_deref().is_some_and(|url| url.ends_with("/api/assets/example")));
}

#[test]
fn file_preview_round_trips_over_the_native_bridge() {
    use crux_core::bridge::{BincodeFfiFormat, FfiFormat};
    use domovoi_core::view::{FilePreviewView, LfsView};

    let lfs = LfsView { oid: "ab".repeat(32), state: "missing".into(), message: Some("Not here".into()), can_fetch: true, ..Default::default() };
    for (image_url, lfs) in [(None, None), (Some("http://127.0.0.1:1/api/assets/project-image/example".into()), Some(lfs))] {
        let preview = FilePreviewView { path: "file.png".into(), image_url, size_label: Some("2 B".into()), lfs, ..Default::default() };
        let mut bytes = vec![];
        BincodeFfiFormat::serialize(&mut bytes, &preview).unwrap();
        assert_eq!(BincodeFfiFormat::deserialize::<FilePreviewView>(&bytes).unwrap(), preview);
    }
}

#[test]
fn file_image_previews_follow_the_latest_selection() {
    let (mut model, _) = connected();
    model.panel.files.root = "/w".into();
    let open = |model: &mut Model, path: &str| {
        let sent = sent_requests(&run(model, Event::OpenFile { path: path.into() }));
        let read = sent.iter().find(|request| request["tag"] == "projects.readFile").expect("file read");
        assert_eq!(read["payload"], json!({ "cwd": "/w", "relativePath": path, "previewImages": true }));
        read["id"].as_str().unwrap().to_owned()
    };
    let image = |path: &str| json!({
        "relativePath": path, "contents": "", "byteLength": 40, "truncated": false, "exists": true,
        "imageUrl": "/api/assets/project-image/example"
    });
    let first = open(&mut model, "first.png");
    let second = open(&mut model, "second.png");
    sock(&mut model, exit_ok(&first, image("first.png")));
    let preview = DomovoiApp.view(&model).panel.files.preview.unwrap();
    assert_eq!(preview.path, "second.png");
    assert!(preview.loading);
    assert!(preview.image_url.is_none());
    sock(&mut model, exit_ok(&second, image("second.png")));
    let preview = DomovoiApp.view(&model).panel.files.preview.unwrap();
    assert_eq!(preview.image_url.as_deref(), Some("http://127.0.0.1:1/api/assets/project-image/example"));
    assert!(preview.spans.is_empty());
    let text = open(&mut model, "readme.txt");
    sock(&mut model, exit_ok(&text, json!({
        "relativePath": "readme.txt", "contents": "hello", "byteLength": 5, "truncated": false
    })));
    let preview = DomovoiApp.view(&model).panel.files.preview.unwrap();
    assert_eq!(preview.text, "hello");
    assert!(preview.image_url.is_none());
}

#[test]
fn git_lfs_files_show_their_object_and_fetch_it_on_request() {
    let (mut model, _) = connected();
    model.panel.files.root = "/w".into();
    let oid = "4859402c258b836d02e955d1090e29f586e58b2040504d68afec3d8d43757bba";
    let pointer = format!("version https://git-lfs.github.com/spec/v1\noid sha256:{oid}\nsize 2048\n");
    let open = |model: &mut Model| {
        let sent = sent_requests(&run(model, Event::OpenFile { path: "art.png".into() }));
        sent.iter().find(|request| request["tag"] == "projects.readFile").expect("file read")["id"].as_str().unwrap().to_owned()
    };
    let read = open(&mut model);
    sock(&mut model, exit_ok(&read, json!({
        "relativePath": "art.png", "contents": pointer, "byteLength": 2048, "truncated": false, "exists": true,
        "lfs": { "oid": oid, "size": 2048, "state": "missing" }
    })));
    let preview = DomovoiApp.view(&model).panel.files.preview.unwrap();
    assert!(!preview.editable, "a pointer file is never saved over");
    assert_eq!(preview.size_label.as_deref(), Some("2.0 KB"));
    let lfs = preview.lfs.expect("an LFS card");
    assert_eq!((lfs.oid_short.as_str(), lfs.state_label.as_str(), lfs.size_label.as_str()), ("4859402c25", "Not downloaded", "2.0 KB"));
    assert!(lfs.can_fetch && !lfs.fetching);

    // Fetching: the request, then a failure shown on the card.
    let fetch = || Event::FetchLfsObject { path: "art.png".into() };
    let sent = sent_requests(&run(&mut model, fetch()));
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0]["tag"], "projects.fetchLfsObject");
    assert_eq!(sent[0]["payload"], json!({ "cwd": "/w", "relativePath": "art.png" }));
    assert!(DomovoiApp.view(&model).panel.files.preview.unwrap().lfs.unwrap().fetching);
    assert!(sent_requests(&run(&mut model, fetch())).is_empty(), "one fetch at a time");
    let id = sent[0]["id"].as_str().unwrap();
    sock(&mut model, exit_fail(id, json!({ "_tag": "ProjectReadFileError", "cwd": "/w", "relativePath": "art.png", "failure": "operation_failed", "message": "Git LFS couldn't fetch art.png: offline" })));
    let lfs = DomovoiApp.view(&model).panel.files.preview.unwrap().lfs.unwrap();
    assert!(!lfs.fetching);
    assert!(lfs.message.as_deref().is_some_and(|m| m.contains("offline")), "{lfs:?}");

    // Fetched: the file is read again, and shows its object.
    let sent = sent_requests(&run(&mut model, fetch()));
    let sent = sent_requests(&sock(&mut model, exit_ok(sent[0]["id"].as_str().unwrap(), json!({ "relativePath": "art.png", "state": "cached" }))));
    let reread = sent.iter().find(|request| request["tag"] == "projects.readFile").expect("read again");
    sock(&mut model, exit_ok(reread["id"].as_str().unwrap(), json!({
        "relativePath": "art.png", "contents": "", "byteLength": 2048, "truncated": false, "exists": true,
        "imageUrl": "/api/assets/project-image/example", "lfs": { "oid": oid, "size": 2048, "state": "cached" }
    })));
    let preview = DomovoiApp.view(&model).panel.files.preview.unwrap();
    assert!(preview.image_url.is_some());
    let lfs = preview.lfs.unwrap();
    assert_eq!(lfs.state_label, "Stored locally");
    assert!(!lfs.can_fetch && lfs.message.is_none());

    // A fetch answered after another file was opened changes nothing.
    let sent = sent_requests(&run(&mut model, Event::OpenFile { path: "other.txt".into() }));
    assert!(sent.iter().all(|request| request["tag"] != "projects.fetchLfsObject"));
    assert!(sent_requests(&run(&mut model, fetch())).is_empty(), "not the open file");
}

#[test]
fn workspace_artifact_reads_do_not_cross_threads_or_reopen_closed_previews() {
    let (mut model, _) = connected();
    model.projects.push(records::project(json!({ "id": "p", "workspaceRoot": "/w" })));
    for id in ["one", "two"] {
        model.threads.push(records::shell(json!({ "id": id, "projectId": "p" })));
    }
    run(&mut model, Event::SelectThread { thread_id: "one".into() });
    let open = |model: &mut Model, thread: &str| {
        let sent = sent_requests(&run(model, Event::OpenArtifactFile { thread_id: thread.into(), path: "report.html".into() }));
        let read = sent.iter().find(|request| request["tag"] == "projects.readFile").expect("file read");
        assert_eq!(read["payload"]["cwd"], "/w");
        assert!(read["payload"].get("previewImages").is_none());
        read["id"].as_str().unwrap().to_owned()
    };
    let first = open(&mut model, "one");
    run(&mut model, Event::SelectThread { thread_id: "two".into() });
    let second = open(&mut model, "two");
    let answer = |text: &str| json!({ "relativePath": "report.html", "contents": text, "byteLength": text.len(), "truncated": false, "exists": true });
    sock(&mut model, exit_ok(&first, answer("stale")));
    assert!(DomovoiApp.view(&model).panel.files.artifact_preview.unwrap().loading);
    sock(&mut model, exit_ok(&second, answer("current")));
    assert_eq!(DomovoiApp.view(&model).panel.files.artifact_preview.unwrap().text, "current");
    let cancelled = open(&mut model, "two");
    run(&mut model, Event::CloseArtifactFile);
    sock(&mut model, exit_ok(&cancelled, answer("dismissed")));
    assert!(DomovoiApp.view(&model).panel.files.artifact_preview.is_none());
}

#[test]
fn companion_looks_are_saved_per_server_and_reset_offline() {
    let mut shell = Shell::paired();
    let mut model = Model::default();
    model.environments = serde_json::from_value(json!([
        { "id": "a", "label": "Studio", "url": "http://a:1" },
        { "id": "b", "label": "Office", "url": "http://b:2" }
    ])).unwrap();
    model.active_env = Some("a".into());
    let original = DomovoiApp.view(&model).connection;
    let mut look = original.companion.look.clone();
    look.body = "violet".into();
    look.ears = "pointy".into();
    let _ = shell.drive(&mut model, Event::SetCompanion { id: "a".into(), look: look.clone() });
    let saved: Vec<domovoi_core::model::Environment> = serde_json::from_str(&shell.store["environments"]).unwrap();
    assert_eq!(saved[0].companion.as_ref(), Some(&look));
    assert!(saved[1].companion.is_none());
    assert!(DomovoiApp.view(&model).connection.companion.custom);

    let mut invalid = look.clone();
    invalid.eye_shape = "triangle".into();
    let _ = shell.drive(&mut model, Event::SetCompanion { id: "a".into(), look: invalid });
    let mut invalid = look.clone();
    invalid.body = "unknown".into();
    let _ = shell.drive(&mut model, Event::SetCompanion { id: "a".into(), look: invalid });
    assert_eq!(DomovoiApp.view(&model).connection.companion.look, look);

    let _ = shell.drive(&mut model, Event::ResetCompanion { id: "a".into() });
    let reset = DomovoiApp.view(&model).connection;
    assert_eq!(reset.companion, original.companion);
    assert_eq!(reset.environments[1].companion, original.environments[1].companion);
    assert!(!shell.store["environments"].contains("companion"));

    use domovoi_core::companion::derive;
    assert_eq!(derive("http://a:1"), original.companion.look);
    assert!((1..=20).map(|port| derive(&format!("http://a:{port}")).body).collect::<std::collections::HashSet<_>>().len() > 5);
}

#[test]
fn companion_activity_tracks_live_threads_and_only_reacts_to_transitions() {
    let (mut model, sent) = connected();
    let id = sent.iter().find(|r| r["tag"] == "orchestration.subscribeShell").unwrap()["id"].as_str().unwrap();
    let thread = |status| json!({ "id": "t1", "projectId": "p1", "status": status });
    let approval = json!({
        "id": "t2", "projectId": "p1",
        "pendingRuntimeRequest": { "id": "request", "kind": "userInput", "createdAt": "2026-01-01T00:00:00Z" }
    });
    let snapshot = records::shell_snapshot(1, json!([{ "id": "p1" }]), json!([
        thread("running"),
        approval,
        { "id": "archived", "projectId": "p1", "status": "running", "archivedAt": "2026-01-01T00:00:00Z" },
        { "id": "deleted", "projectId": "p1", "status": "running", "deletedAt": "2026-01-01T00:00:00Z" },
        { "id": "child", "projectId": "p1", "status": "running",
          "lineage": { "parentThreadId": "t1", "relationshipToParent": "subagent", "rootThreadId": "t1" } }
    ]));
    let _ = sock(&mut model, chunk(id, &[snapshot]));
    let view = DomovoiApp.view(&model);
    assert_eq!(view.connection.activity.working, 1);
    assert_eq!(view.connection.activity.attention, 1);
    assert_eq!(view.connection.activity.attention, view.sidebar.threads_needing_attention);
    assert_eq!(view.connection.activity.beat_seq, 0);
    model.focused = true;
    model.selected_thread = Some("t1".into());
    for (status, beat, seq) in [("completed", "finished", 1), ("running", "finished", 1), ("failed", "failed", 2)] {
        let _ = sock(&mut model, chunk(id, &[records::thread_updated(thread(status))]));
        let activity = DomovoiApp.view(&model).connection.activity;
        assert_eq!((activity.beat.as_str(), activity.beat_seq), (beat, seq));
    }
    let snapshot = records::shell_snapshot(5, json!([{ "id": "p1" }]), json!([thread("running")]));
    let _ = sock(&mut model, chunk(id, &[snapshot]));
    let snapshot = records::shell_snapshot(6, json!([{ "id": "p1" }]), json!([thread("completed")]));
    let _ = sock(&mut model, chunk(id, &[snapshot]));
    let activity = DomovoiApp.view(&model).connection.activity;
    assert_eq!(activity.beat_seq, 2);
    assert_eq!((activity.working, activity.attention), (0, 0));
    model.clear_server_state();
    let activity = DomovoiApp.view(&model).connection.activity;
    assert!(activity.beat.is_empty());
    assert_eq!(activity.beat_seq, 2);
}

/// Antigravity as the server lists it: models with a thinking level (a
/// select whose role is reasoning), the current one answering to
/// `antigravity-default`; and an added instance that can't sign in.
fn antigravity_snapshot(model: &mut Model, sent: &[Value]) {
    let config_id = sent.iter().find(|r| r["tag"] == "subscribeServerConfig").unwrap()["id"].as_str().unwrap().to_owned();
    let level = |current: Option<&str>| {
        let mut d = json!({ "id": "thought_level", "label": "Thinking Level", "type": "select", "role": "reasoning", "options": [{ "id": "low", "label": "Low" }, { "id": "high", "label": "High" }] });
        if let Some(c) = current {
            d["currentValue"] = json!(c);
        }
        json!({ "optionDescriptors": [d] })
    };
    let providers = json!([
        { "instanceId": "antigravity", "driver": "antigravity", "displayName": "Antigravity", "status": "ready", "enabled": true, "models": [
            { "slug": "gemini-4-neon", "name": "Gemini 4 Neon", "isDefault": true, "aliases": ["antigravity-default"], "capabilities": level(Some("high")) },
            { "slug": "gemini-3.8-flash", "name": "Gemini 3.8 Flash", "capabilities": level(None) }] },
        { "instanceId": "ag-two", "driver": "antigravity", "displayName": "Antigravity (work)", "status": "warning", "enabled": true, "models": [],
          "message": "Connect with Gemini API key to use Antigravity and load models." }]);
    let _ = sock(model, chunk(&config_id, &[records::config_snapshot(providers, json!({ "defaultRuntimeMode": "approval-required" }))]));
}

#[test]
fn a_thinking_level_is_the_effort_control_and_aliases_pick_their_model() {
    let (mut model, sent) = connected();
    model.selected_project = Some("p1".into());
    model.route = "new-thread".into();
    antigravity_snapshot(&mut model, &sent);

    // Selected by its alias: the model that carries it, its level as the effort control.
    let _ = run(&mut model, Event::SelectModel { instance_id: "antigravity".into(), model: "antigravity-default".into() });
    let view = DomovoiApp.view(&model);
    let c = &view.composer;
    assert_eq!(c.selected_model.as_ref().map(|m| (m.model.as_str(), m.label.as_str())), Some(("antigravity-default", "Antigravity · Gemini 4 Neon")));
    assert_eq!((c.effort_label.as_str(), c.selected_effort.as_deref()), ("Thinking Level", Some("high")));
    assert!(c.traits.is_empty(), "not also a plain model option: {:?}", c.traits);
    let chosen: Vec<&str> = c.model_groups.iter().flat_map(|g| &g.models).filter(|r| r.chosen).map(|r| r.option.model.as_str()).collect();
    assert_eq!(chosen, ["gemini-4-neon"], "the alias's model is checked");

    // A model whose level the server hasn't seen: "Default", and nothing sent for it.
    let _ = run(&mut model, Event::SelectModel { instance_id: "antigravity".into(), model: "gemini-3.8-flash".into() });
    let c = DomovoiApp.view(&model).composer;
    assert_eq!(c.efforts.iter().map(|e| e.label.as_str()).collect::<Vec<_>>(), ["Default", "Low", "High"]);
    assert_eq!(c.selected_effort.as_deref(), Some(""));
    let _ = run(&mut model, Event::SetDraft { text: "hi".into() });
    let launch = sent_requests(&run(&mut model, Event::Send)).into_iter().find(|r| r["tag"] == "orchestration.launchThread").expect("launch");
    assert_eq!(launch["payload"]["modelSelection"], json!({ "instanceId": "antigravity", "model": "gemini-3.8-flash" }));

    // A level picked is sent under the option's own id.
    let (mut model, sent) = connected();
    model.selected_project = Some("p1".into());
    model.route = "new-thread".into();
    antigravity_snapshot(&mut model, &sent);
    let _ = run(&mut model, Event::SelectModel { instance_id: "antigravity".into(), model: "gemini-3.8-flash".into() });
    let _ = run(&mut model, Event::SelectEffort { value: "low".into() });
    let _ = run(&mut model, Event::SetDraft { text: "again".into() });
    let launch = sent_requests(&run(&mut model, Event::Send)).into_iter().find(|r| r["tag"] == "orchestration.launchThread").expect("launch");
    assert_eq!(launch["payload"]["modelSelection"]["options"], json!([{ "id": "thought_level", "value": "low" }]));

    // A provider that lists no models still shows, saying why.
    let groups = DomovoiApp.view(&model).composer.model_groups;
    let two = groups.iter().find(|g| g.instance_id == "ag-two").expect("the signed-out provider's group");
    assert!(two.models.is_empty() && two.message.contains("Connect with Gemini API key"), "{two:?}");
}

#[test]
fn a_composer_pick_the_provider_no_longer_lists_is_dropped() {
    let (mut model, sent) = connected();
    model.selected_project = Some("p1".into());
    model.route = "new-thread".into();
    antigravity_snapshot(&mut model, &sent);
    let _ = run(&mut model, Event::SelectModel { instance_id: "antigravity".into(), model: "gemini-3.8-flash".into() });
    // Signed in to another account: that model is gone.
    providers_snapshot(&mut model, &sent);
    let c = DomovoiApp.view(&model).composer;
    assert_eq!(c.selected_model.as_ref().map(|m| m.model.as_str()), Some("claude-haiku-5-5"), "falls back to a model that is listed");
}

/// Antigravity with its setup offered, and an added key instance in settings.
fn setup_snapshot(model: &mut Model, sent: &[Value]) {
    let config_id = sent.iter().find(|r| r["tag"] == "subscribeServerConfig").unwrap()["id"].as_str().unwrap().to_owned();
    let providers = json!([
        { "instanceId": "antigravity", "driver": "antigravity", "displayName": "Antigravity", "status": "warning", "enabled": true, "models": [],
          "auth": { "status": "unauthenticated", "type": "oauth-personal", "canLogout": false }, "setup": { "canAuthenticate": true, "canInstall": true } },
        { "instanceId": "ag-key", "driver": "antigravity", "displayName": "Work", "status": "ready", "enabled": true, "models": [],
          "auth": { "status": "authenticated", "type": "gemini-api-key", "label": "Gemini API key" }, "setup": { "canAuthenticate": true, "canInstall": true } }]);
    let settings = json!({ "defaultRuntimeMode": "approval-required", "providerInstances": { "ag-key": { "driver": "antigravity", "displayName": "Work", "enabled": true,
        "environment": [{ "name": "X", "value": "1" }], "config": { "authMethod": "gemini-api-key", "apiKey": "\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}", "keep": "me" } } } });
    let _ = sock(model, chunk(&config_id, &[records::config_snapshot(providers, settings)]));
}

#[test]
fn a_provider_signs_in_installs_and_is_edited_from_settings() {
    let (mut model, sent) = connected();
    setup_snapshot(&mut model, &sent);
    // The providers page watches each instance's sign-in and install.
    let opened = sent_requests(&run(&mut model, Event::LoadSettings { section: "providers".into() }));
    let tags: Vec<&str> = opened.iter().filter_map(|r| r["tag"].as_str()).collect();
    assert!(tags.contains(&"provider.auth.subscribe") && tags.contains(&"provider.install.subscribe"), "{tags:?}");
    let auth_stream = opened.iter().find(|r| r["tag"] == "provider.auth.subscribe" && r["payload"]["instanceId"] == "antigravity").unwrap()["id"].as_str().unwrap().to_owned();
    let install_stream = opened.iter().find(|r| r["tag"] == "provider.install.subscribe" && r["payload"]["instanceId"] == "antigravity").unwrap()["id"].as_str().unwrap().to_owned();

    let started = sent_requests(&run(&mut model, Event::StartProviderSignIn { instance_id: "antigravity".into() }));
    assert_eq!(started.iter().find(|r| r["tag"] == "provider.auth.start").unwrap()["payload"], json!({ "instanceId": "antigravity" }));
    let _ = sock(&mut model, chunk(&auth_stream, &[json!({ "instanceId": "antigravity", "phase": "waiting", "flowId": "f1", "authorizationUrl": "https://accounts.google.com/o/oauth2/auth?x",
        "expiresAt": null, "message": "Open the Google sign-in link.", "interaction": { "type": "browser", "id": "f1", "url": "https://accounts.google.com/o/oauth2/auth?x", "requiresConsent": false, "acceptsCallback": true },
        "credentialOwner": "provider" })]));
    let view = DomovoiApp.view(&model);
    let p = view.settings.providers.iter().find(|p| p.instance_id == "antigravity").unwrap();
    assert!(p.can_authenticate && p.can_install && !p.signed_in);
    assert_eq!((p.sign_in.url.as_str(), p.sign_in.takes_redirect, p.sign_in.busy), ("https://accounts.google.com/o/oauth2/auth?x", true, true));
    let done = sent_requests(&run(&mut model, Event::CompleteProviderSignIn { instance_id: "antigravity".into(), callback_url: " http://localhost:5/auth/callback?state=s&code=c ".into() }));
    assert_eq!(done.iter().find(|r| r["tag"] == "provider.auth.complete").unwrap()["payload"],
        json!({ "instanceId": "antigravity", "flowId": "f1", "callbackUrl": "http://localhost:5/auth/callback?state=s&code=c" }));

    // The runtime: installing, with its progress.
    let install = sent_requests(&run(&mut model, Event::InstallProvider { instance_id: "antigravity".into() }));
    assert!(install.iter().any(|r| r["tag"] == "provider.install.start"));
    let _ = sock(&mut model, chunk(&install_stream, &[json!({ "driver": "antigravity", "operationId": "op1", "phase": "downloading", "downloadedBytes": 52_428_800u64, "totalBytes": 104_857_600u64,
        "version": "1.5.14", "installedVersion": null, "canRemove": true, "message": "Downloading" })]));
    let p = DomovoiApp.view(&model).settings.providers.into_iter().find(|p| p.instance_id == "antigravity").unwrap();
    assert_eq!((p.install.busy, p.install.progress, p.install.progress_label.as_str()), (true, 500, "50 MB of 100 MB"));
    let cancel = sent_requests(&run(&mut model, Event::CancelProviderInstall { instance_id: "antigravity".into() }));
    assert_eq!(cancel.iter().find(|r| r["tag"] == "provider.install.cancel").unwrap()["payload"], json!({ "instanceId": "antigravity", "operationId": "op1" }));

    // An instance's settings: an untouched key stays the saved one, the rest of the instance is kept.
    let key = DomovoiApp.view(&model).settings.providers.into_iter().find(|p| p.instance_id == "ag-key").unwrap();
    assert!(key.config.editable && key.config.has_api_key && !key.config.built_in);
    assert_eq!(key.config.auth_methods.iter().map(|m| m.value.as_str()).collect::<Vec<_>>(), ["gemini-api-key", "agent-platform"], "added instances can't use the shared Google sign-in");
    let edit = |id: &str, key: &str| Box::new(ProviderInstanceEdit { instance_id: id.into(), driver: "antigravity".into(), display_name: "Work".into(), enabled: true,
        auth_method: "agent-platform".into(), api_key: key.into(), gcp_project: " p1 ".into(), gcp_location: "us-central1".into(), binary_path: String::new() });
    let saved = sent_requests(&run(&mut model, Event::SaveProviderInstance { edit: edit("ag-key", "") }));
    let m = &saved.iter().find(|r| r["tag"] == "server.updateSettings").unwrap()["payload"]["providerInstanceMutation"];
    assert_eq!((m["operation"].as_str(), m["instanceId"].as_str()), (Some("upsert"), Some("ag-key")));
    assert_eq!(m["instance"]["config"], json!({ "authMethod": "agent-platform", "apiKey": "\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}", "keep": "me", "gcpProject": "p1", "gcpLocation": "us-central1", "binaryPath": "" }));
    assert_eq!(m["instance"]["environment"], json!([{ "name": "X", "value": "1" }]));
    // A new one gets an id of its own, and its key.
    let added = sent_requests(&run(&mut model, Event::SaveProviderInstance { edit: edit("", "k-1") }));
    let m = &added.iter().find(|r| r["tag"] == "server.updateSettings").unwrap()["payload"]["providerInstanceMutation"];
    assert_eq!(m["instanceId"], "antigravity-2", "{m}");
    assert_eq!(m["instance"]["config"]["apiKey"], "k-1");
    let removed = sent_requests(&run(&mut model, Event::RemoveProviderInstance { instance_id: "ag-key".into() }));
    assert_eq!(removed.iter().find(|r| r["tag"] == "server.updateSettings").unwrap()["payload"]["providerInstanceMutation"], json!({ "operation": "remove", "instanceId": "ag-key" }));
}

#[test]
fn streaming_proposed_plan_reports_draft_and_antigravity_shows_plan_toggle() {
    let (mut model, _) = connected();
    working_thread(&mut model);
    model.providers = vec![records::provider(json!({
        "instanceId": "antigravity", "driver": "antigravity", "displayName": "Antigravity", "status": "ready", "enabled": true,
        "showInteractionModeToggle": true,
        "models": [{ "slug": "gemini-4-neon", "name": "Gemini 4 Neon", "isDefault": true }]
    }))];
    model.threads[0] = records::shell(json!({
        "id": "t1", "projectId": "p1", "title": "Plan", "status": "working", "providerInstanceId": "antigravity",
        "modelSelection": { "instanceId": "antigravity", "model": "gemini-4-neon" }, "interactionMode": "plan"
    }));
    model.thread = Some(records::thread(json!({
        "thread": { "id": "t1", "providerInstanceId": "antigravity", "interactionMode": "plan" },
        "runs": [{ "id": "r1", "ordinal": 1, "status": "running" }],
        "plans": [{ "id": "plan1", "runId": "r1", "nodeId": "n-plan", "kind": "proposed_plan", "status": "draft", "markdown": "# Draft Plan" }],
        "turnItems": [{ "id": "i-plan", "runId": "r1", "nodeId": "n-plan", "ordinal": 1, "type": "proposed_plan", "planId": "plan1", "markdown": "# Draft Plan", "streaming": true, "status": "running" }]
    })));
    let view = DomovoiApp.view(&model);
    assert!(view.composer.shows_interaction_mode, "Antigravity shows Plan toggle");
    let page = view.thread.unwrap();
    let row = page.items.iter().find(|i| i.id == "i-plan").unwrap();
    assert!(row.streaming && !row.can_implement, "streaming plan row stays draft");
    let progress = page.plan.unwrap();
    assert_eq!(progress.label, "Plan draft");
    assert!(!progress.can_implement);
}

#[test]
fn antigravity_background_subagent_shows_background_activity_without_unsupported_stop() {
    let (mut model, _) = connected();
    working_thread(&mut model);
    model.thread = Some(records::thread(json!({
        "thread": { "id": "t1", "providerInstanceId": "antigravity", "activeProviderThreadId": "pt1" },
        "runs": [{ "id": "r1", "ordinal": 1, "status": "running", "startedAt": "2026-11-01T00:00:00.000Z" }],
        "providerThreads": [{ "id": "pt1",
            "pendingBackgroundTasks": [{
                "kind": "subagent", "taskId": "child-conv-1", "nativeItemId": "call_1",
                "description": "App Icon Designer A", "childThreadId": "c1", "awaited": true
            }] }],
        "subagents": [{
            "id": "node-sub", "driver": "antigravity", "providerInstanceId": "antigravity", "providerThreadId": "pt1",
            "status": "running", "nativeTaskRef": { "driver": "antigravity", "nativeId": "call_1" },
            "childThreadId": "c1", "title": "App Icon Designer A", "progress": "Generating concept_a"
        }],
        "turnItems": [{
            "id": "i-sub", "runId": "r1", "ordinal": 1, "type": "subagent", "subagentId": "node-sub",
            "driver": "antigravity", "providerInstanceId": "antigravity", "title": "App Icon Designer A",
            "prompt": "Render concept A", "childThreadId": "c1", "status": "running"
        }]
    })));
    let page = DomovoiApp.view(&model).thread.unwrap();
    assert_eq!(page.background, "Waiting for a background agent");
    assert_eq!(page.background_tasks.len(), 1);
    assert_eq!(page.background_tasks[0].label, "App Icon Designer A");
    assert_eq!(page.background_tasks[0].progress.as_deref(), Some("Generating concept_a"));
    assert_eq!(page.background_tasks[0].child_thread_id.as_deref(), Some("c1"));
    let sub = page.items.iter().find(|i| i.id == "i-sub").unwrap();
    assert_eq!(sub.child_thread_id, "c1");
    assert_eq!(sub.progress, "Generating concept_a");
    assert!(!sub.can_stop, "Antigravity subagents cannot be stopped individually");
}

#[test]
fn unified_artifacts_panel_versions_modes_and_thread_switch() {
    let (mut model, _) = connected();
    model.projects.push(records::project(json!({ "id": "p1", "workspaceRoot": "/w" })));
    model.threads.push(records::shell(json!({ "id": "t1", "projectId": "p1" })));
    model.threads.push(records::shell(json!({ "id": "t2", "projectId": "p1" })));
    model.selected_thread = Some("t1".into());
    model.thread = Some(records::thread(json!({
        "thread": { "id": "t1", "projectId": "p1" },
        "runs": [{ "id": "r1", "ordinal": 1, "status": "completed" }],
        "turnItems": [
            {
                "id": "i-create", "runId": "r1", "ordinal": 1, "type": "dynamic_tool",
                "toolName": "mcp__t3__t3_artifact_create", "status": "completed",
                "input": { "id": "calc", "title": "Calculator", "kind": "html", "content": "<!doctype html><html><body>v1</body></html>" },
                "output": { "id": "calc", "version": 1 }
            },
            {
                "id": "i-update", "runId": "r1", "ordinal": 2, "type": "dynamic_tool",
                "toolName": "mcp__domovoi__artifact_update", "status": "completed",
                "input": { "id": "calc", "title": "Calculator Pro", "content": "<!doctype html><html><body>v2</body></html>" },
                "output": { "id": "calc", "version": 2 }
            },
            {
                "id": "i-claude", "runId": "r1", "ordinal": 3, "type": "dynamic_tool",
                "toolName": "Artifact", "status": "completed",
                "input": { "title": "Published App", "type": "application/vnd.ant.react", "content": "export default function App() { return <div>Hi</div>; }" },
                "output": "Published to https://claude.ai/public/artifacts/abc-123"
            }
        ]
    })));

    let view = DomovoiApp.view(&model);
    let page = view.thread.as_ref().unwrap();
    assert_eq!(page.artifact_count, 2, "calc (2 versions) + claude hosted app");
    let create_row = page.items.iter().find(|i| i.id == "i-create").unwrap();
    let update_row = page.items.iter().find(|i| i.id == "i-update").unwrap();
    let claude_row = page.items.iter().find(|i| i.id == "i-claude").unwrap();
    assert_eq!(create_row.kind, "artifact");
    assert_eq!(create_row.artifact_version, 1);
    assert_eq!(update_row.kind, "artifact");
    assert_eq!(update_row.artifact_version, 2);
    assert_eq!(create_row.artifact_id, update_row.artifact_id);
    assert_eq!(claude_row.kind, "artifact");
    assert_eq!(claude_row.artifact_hosted_url.as_deref(), Some("https://claude.ai/public/artifacts/abc-123"));
    assert_eq!(view.panel.artifacts.items.len(), 2);

    // Open calc -> selects latest version (v2), opens artifacts panel tab.
    let aid = update_row.artifact_id.clone().unwrap();
    let _ = run(&mut model, Event::OpenArtifact { thread_id: "t1".into(), artifact_id: aid.clone(), version: None, dark: true });
    let view = DomovoiApp.view(&model);
    assert!(view.panel.open && view.panel.tab == "artifacts");
    let active = view.panel.artifacts.active.as_ref().unwrap();
    assert_eq!((active.version, active.versions.len(), active.mode.as_str()), (2, 2, "preview"));
    assert!(active.source.contains("v2"));

    // Select v1 and toggle view mode to code.
    let _ = run(&mut model, Event::SelectArtifactVersion { version: 1, dark: true });
    let _ = run(&mut model, Event::SetArtifactViewMode { mode: "code".into() });
    let active = DomovoiApp.view(&model).panel.artifacts.active.unwrap();
    assert_eq!((active.version, active.mode.as_str()), (1, "code"));
    assert!(active.source.contains("v1"));

    // Open the React Claude artifact -> requests server-hosted React preview URL.
    let claude_id = claude_row.artifact_id.clone().unwrap();
    let sent = sent_requests(&run(&mut model, Event::OpenArtifact { thread_id: "t1".into(), artifact_id: claude_id, version: None, dark: true }));
    let asset = sent.iter().find(|r| r["tag"] == "assets.createUrl").expect("react preview URL requested");
    assert_eq!(asset["payload"]["resource"]["_tag"], "artifact-react");
    assert_eq!(asset["payload"]["resource"]["theme"], "dark");
    let exp = model.now_ms + 300_000;
    let _ = sock(&mut model, exit_ok(asset["id"].as_str().unwrap(), json!({ "relativeUrl": "/api/assets/artifact-react/tok1", "expiresAt": exp })));
    let active = DomovoiApp.view(&model).panel.artifacts.active.unwrap();
    assert_eq!(active.preview_url.as_deref(), Some("http://127.0.0.1:1/api/assets/artifact-react/tok1"));
    assert_eq!(active.hosted_url.as_deref(), Some("https://claude.ai/public/artifacts/abc-123"));

    // AskAboutArtifact pre-fills composer draft.
    let _ = run(&mut model, Event::AskAboutArtifact { artifact_id: aid.clone(), prompt: "Make the buttons larger".into() });
    assert!(DomovoiApp.view(&model).composer.draft.contains("Make the buttons larger"));

    // CloseArtifact clears active, and switching threads resets the panel.
    let _ = run(&mut model, Event::CloseArtifact);
    assert!(DomovoiApp.view(&model).panel.artifacts.active.is_none());
    let _ = run(&mut model, Event::OpenArtifact { thread_id: "t1".into(), artifact_id: aid, version: None, dark: false });
    assert!(DomovoiApp.view(&model).panel.artifacts.active.is_some());
    let _ = run(&mut model, Event::SelectThread { thread_id: "t2".into() });
    assert!(DomovoiApp.view(&model).panel.artifacts.active.is_none());
}

#[test]
fn opening_workspace_artifact_path_reads_source_and_requests_bundle_url() {
    let (mut model, _) = connected();
    model.projects.push(records::project(json!({ "id": "p1", "workspaceRoot": "/w" })));
    model.threads.push(records::shell(json!({ "id": "t1", "projectId": "p1" })));
    model.selected_thread = Some("t1".into());
    model.thread = Some(records::thread(json!({ "thread": { "id": "t1", "projectId": "p1" } })));

    let sent = sent_requests(&run(&mut model, Event::OpenArtifactWorkspacePath { thread_id: "t1".into(), path: "dist/index.html".into(), dark: false }));
    let read = sent.iter().find(|r| r["tag"] == "projects.readFile").expect("reads workspace file");
    let bundle = sent.iter().find(|r| r["tag"] == "assets.createUrl").expect("requests bundle url");
    assert_eq!(read["payload"]["relativePath"], "dist/index.html");
    assert_eq!(bundle["payload"]["resource"]["_tag"], "artifact-bundle");
    assert_eq!(bundle["payload"]["resource"]["path"], "dist/index.html");

    let _ = sock(&mut model, exit_ok(read["id"].as_str().unwrap(), json!({ "relativePath": "dist/index.html", "contents": "<!doctype html><h1>Bundle</h1>", "byteLength": 30, "truncated": false, "exists": true })));
    let exp = model.now_ms + 300_000;
    let _ = sock(&mut model, exit_ok(bundle["id"].as_str().unwrap(), json!({ "relativeUrl": "/api/assets/artifact/b1/dist/index.html", "expiresAt": exp })));

    let view = DomovoiApp.view(&model);
    assert!(view.panel.open && view.panel.tab == "artifacts");
    let active = view.panel.artifacts.active.unwrap();
    assert_eq!(active.path.as_deref(), Some("dist/index.html"));
    assert_eq!(active.source, "<!doctype html><h1>Bundle</h1>");
    assert_eq!(active.preview_url.as_deref(), Some("http://127.0.0.1:1/api/assets/artifact/b1/dist/index.html"));
}

// ---- the server on this machine, links from outside, and what is saved when ----

const LOCAL_URL: &str = "http://127.0.0.1:13773";

/// A shell beside a running server: its descriptor, a request file that can
/// be written, and the server's answer to it.
fn beside_a_server() -> Shell {
    let mut shell = Shell::paired();
    shell.descriptor = Some(json!({ "version": 1, "url": LOCAL_URL, "environmentId": "srv-local", "label": "Studio" }).to_string());
    shell.local_request = Some(("ab".repeat(16), "5ec2e7".into()));
    shell.routes.insert("/api/domovoi/local/session".into(), (200, json!({ "accessToken": "tok-local", "proof": "5ec2e7", "environmentId": "srv-local" })));
    shell
}

fn launch(shell: &mut Shell, model: &mut Model, surface: &str) -> Vec<Effect> {
    shell.drive(model, Event::Launch { surface: surface.into(), device_label: "Test Mac".into(), device_os: "macOS".into() })
}

fn bearer(request: &HttpRequest) -> Option<&str> {
    request.headers.iter().find(|h| h.name.eq_ignore_ascii_case("authorization")).map(|h| h.value.as_str())
}

fn socket_closed(effects: &[Effect]) -> bool {
    effects.iter().any(|e| matches!(e, Effect::Socket(r) if r.operation == SocketOperation::Close))
}

#[test]
fn a_fresh_install_signs_in_to_the_server_on_its_machine_by_itself() {
    let mut shell = beside_a_server();
    let mut model = Model::default();
    let effects = launch(&mut shell, &mut model, "desktop");
    assert_eq!(shell.local_asked, [LocalOperation::Watch, LocalOperation::Request]);

    // The request file's id went to the server at the descriptor's address, with what this app is.
    let sign_in = shell.requests.iter().find(|r| r.url.ends_with("/api/domovoi/local/session")).expect("signed in without a code");
    assert_eq!((sign_in.url.as_str(), sign_in.method.as_str()), ("http://127.0.0.1:13773/api/domovoi/local/session", "POST"));
    assert_eq!(serde_json::from_slice::<Value>(&sign_in.body).unwrap(), json!({ "request": "ab".repeat(16), "clientLabel": "Test Mac", "clientDeviceType": "desktop", "clientOs": "macOS" }));
    assert!(shell.requests.iter().all(|r| !r.url.ends_with("/oauth/token")), "no pairing code involved");
    // Its token opens the socket there.
    let ticket = shell.requests.iter().find(|r| r.url.ends_with("/api/auth/websocket-ticket/rpc")).expect("a ticket");
    assert_eq!((ticket.url.as_str(), bearer(ticket)), ("http://127.0.0.1:13773/api/auth/websocket-ticket/rpc", Some("Bearer tok-local")));
    assert!(socket_url(&effects).unwrap().starts_with("ws://127.0.0.1:13773/ws?"));

    // Saved: the server under the machine's name, that it is the active one, its token, and that this happened.
    let id = model.active_env.clone().expect("the server is active");
    assert_eq!(shell.store.get(&format!("token:{id}")).map(String::as_str), Some("tok-local"));
    assert_eq!(shell.store.get("activeEnvironment"), Some(&id));
    assert_eq!(shell.store.get("localAuto").map(String::as_str), Some("1"));
    let saved: Value = serde_json::from_str(&shell.store["environments"]).unwrap();
    assert_eq!(saved, json!([{ "id": id, "label": "Studio", "url": LOCAL_URL, "ticket": "", "server_id": "srv-local" }]));
    let view = DomovoiApp.view(&model);
    assert!(view.connection.local_server.is_none(), "nothing to offer: the app is using it");
    assert!(!view.connection.needs_pairing);

    // The next launch is an ordinary one: the saved token, no new sign-in.
    let mut model = Model::default();
    shell.requests.clear();
    shell.local_asked.clear();
    let effects = launch(&mut shell, &mut model, "desktop");
    assert!(socket_url(&effects).is_some());
    assert_eq!(shell.local_asked, [LocalOperation::Watch]);
    assert_eq!(shell.requests.iter().map(|r| r.url.rsplit('/').next().unwrap()).collect::<Vec<_>>(), ["rpc"], "only a ticket");
}

#[test]
fn while_signing_in_to_this_machine_the_app_is_connecting_not_asking_for_a_code() {
    // A launch with nothing saved, the store answered at once; then the
    // shell reports the descriptor, and has not written the request file yet.
    let mut shell = Shell::paired();
    let mut model = Model::default();
    let _ = launch(&mut shell, &mut model, "desktop");
    assert_eq!(DomovoiApp.view(&model).connection.state, "disconnected");
    let effects = run(&mut model, Event::LocalServer { out: LocalOutput::Descriptor { text: beside_a_server().descriptor } });
    assert!(effects.iter().any(|e| matches!(e, Effect::Local(r) if r.operation == LocalOperation::Request)), "the request file is asked for");
    assert!(effects.iter().all(|e| !matches!(e, Effect::Http(_))), "nothing is sent before it is written");
    let view = DomovoiApp.view(&model);
    assert_eq!(view.connection.state, "connecting", "not pairing: there is no code to type");
    assert!(!view.connection.needs_pairing && view.connection.attempting);
    assert!(view.connection.local_server.is_none(), "not offered while it is being joined");
    assert!(!shell.store.contains_key("environments") && !shell.store.contains_key("activeEnvironment"), "nothing is saved before the server has answered");

    // An answer for a sign-in the user has left behind (they connected elsewhere meanwhile) is not used.
    let attempt = model.attempt;
    let _ = shell.drive(&mut model, Event::Connect { server_url: "http://other:2".into(), pairing_code: "CODE".into() });
    shell.requests.clear();
    let _ = shell.drive(&mut model, Event::LocalRequested { attempt, out: LocalOutput::Request { id: "ab".repeat(16), secret: "5ec2e7".into() } });
    assert!(shell.requests.is_empty());
    assert_eq!(model.environments.len(), 1, "the entry made for it went with it");
    assert_eq!(model.environments[0].url, "http://other:2");
}

#[test]
fn with_saved_servers_this_machines_server_is_only_offered() {
    let mut shell = beside_a_server();
    let mut model = Model::default();
    connected_to(&mut shell, &mut model, "http://other:2", "CODE");
    let other = model.active_env.clone().unwrap();

    let mut model = Model::default();
    shell.requests.clear();
    let _ = launch(&mut shell, &mut model, "desktop");
    assert_eq!(model.active_env.as_ref(), Some(&other), "the last server is the one connected to");
    assert!(shell.requests.iter().all(|r| !r.url.contains("/local/")), "no sign-in nobody asked for");
    assert_eq!(shell.local_asked, [LocalOperation::Watch]);
    assert!(!shell.store.contains_key("localAuto"));
    let _ = sock(&mut model, SocketEvent::Opened);
    let offered = DomovoiApp.view(&model).connection.local_server.expect("offered beside the saved servers");
    assert_eq!((offered.label.as_str(), offered.url.as_str()), ("Studio", LOCAL_URL));

    // One click: the other server is left, and this one signed in to and saved.
    let effects = shell.drive(&mut model, Event::ConnectLocalServer);
    assert!(socket_closed(&effects), "the server in use is left");
    assert!(socket_url(&effects).unwrap().starts_with("ws://127.0.0.1:13773/ws?"));
    let id = model.active_env.clone().unwrap();
    assert_ne!(id, other);
    assert_eq!(shell.store.get(&format!("token:{id}")).map(String::as_str), Some("tok-local"));
    assert_eq!(shell.store.get("activeEnvironment"), Some(&id));
    assert!(shell.store["environments"].contains(LOCAL_URL) && shell.store["environments"].contains("http://other:2"));
    assert!(DomovoiApp.view(&model).connection.local_server.is_none());
    // Its saved entry is the one used from then on: no second one.
    let _ = sock(&mut model, SocketEvent::Opened);
    let _ = shell.drive(&mut model, Event::SelectEnvironment { id: other });
    assert!(DomovoiApp.view(&model).connection.local_server.is_none(), "it is among the saved servers now");
    assert_eq!(model.environments.len(), 2);

    // Without a server beside it, a client asked for the card's action only says so.
    let (mut model, _) = connected();
    let effects = run(&mut model, Event::ConnectLocalServer);
    assert!(!socket_closed(&effects));
    assert!(DomovoiApp.view(&model).toast.unwrap().message.contains("No Domovoi server is running"));
}

#[test]
fn an_answer_without_the_request_files_secret_is_dropped() {
    // Something else listens on the port the descriptor names (the server
    // died and left it; another program took the port): it can answer, but
    // it cannot have read the request file.
    for answer in [
        (200, json!({ "accessToken": "tok-evil", "proof": "guess", "environmentId": "srv-local" })),
        (200, json!({ "accessToken": "tok-evil", "proof": "", "environmentId": "srv-local" })),
        (200, json!({ "accessToken": "tok-evil", "environmentId": "srv-local" })),
        (200, json!({ "accessToken": "tok-evil", "proof": "5ec2e7", "environmentId": "another-server" })),
        (200, json!({ "accessToken": "", "proof": "5ec2e7", "environmentId": "srv-local" })),
        (401, json!({ "_tag": "EnvironmentAuthInvalidError" })),
        (404, json!({})),
    ] {
        let mut shell = beside_a_server();
        shell.routes.insert("/api/domovoi/local/session".into(), answer.clone());
        let mut model = Model::default();
        let effects = launch(&mut shell, &mut model, "desktop");
        assert!(socket_url(&effects).is_none(), "{answer:?}");
        assert!(shell.requests.iter().all(|r| bearer(r).is_none()), "the token it sent is used for nothing: {answer:?}");
        // Nothing of it is kept: no server, no token; only that the app tried once.
        assert_eq!(shell.store.keys().collect::<Vec<_>>(), ["localAuto"], "{answer:?}");
        assert!(model.environments.is_empty() && model.active_env.is_none() && model.token.is_none(), "{answer:?}");
        let view = DomovoiApp.view(&model);
        assert_eq!(view.connection.state, "disconnected", "the plain connect screen: {answer:?}");
        assert!(view.toast.is_none() && view.connection.error.is_none(), "nobody asked, nobody is told: {answer:?}");
        assert!(view.connection.local_server.is_some(), "still offered");

        // Asked for, the failure is said; and still nothing is saved.
        let _ = shell.drive(&mut model, Event::ConnectLocalServer);
        let view = DomovoiApp.view(&model);
        assert!(view.toast.unwrap().message.contains("Could not sign in to the server on this computer"), "{answer:?}");
        assert_eq!(view.connection.state, "disconnected");
        assert_eq!(shell.store.keys().collect::<Vec<_>>(), ["localAuto"], "{answer:?}");
        assert!(model.environments.is_empty());
    }

    // No request file can be written (the folder is not this user's): the same.
    let mut shell = beside_a_server();
    shell.local_request = None;
    let mut model = Model::default();
    let _ = launch(&mut shell, &mut model, "desktop");
    assert!(shell.requests.is_empty(), "nothing is sent");
    assert_eq!(shell.store.keys().collect::<Vec<_>>(), ["localAuto"]);
    assert!(model.environments.is_empty());
}

#[test]
fn a_saved_entry_for_this_machines_server_is_signed_in_again_once() {
    let mut shell = beside_a_server();
    shell.descriptor = Some(json!({ "version": 1, "url": "http://127.0.0.1:1", "environmentId": "srv-local", "label": "Studio" }).to_string());
    let mut model = Model::default();
    // Paired the old way, by address, before the server said who it is.
    connected_to(&mut shell, &mut model, "http://localhost:1", "CODE");
    let id = model.active_env.clone().unwrap();
    assert_eq!(shell.store[&format!("token:{id}")], "tok-1");

    // The server ended that sign-in (`remote revoke`): the next launch gets 401 for its token.
    let mut model = Model::default();
    shell.requests.clear();
    shell.routes.insert("/api/auth/websocket-ticket/rpc".into(), (401, json!({ "_tag": "EnvironmentAuthInvalidError" })));
    let effects = launch(&mut shell, &mut model, "desktop");
    // The app asked the server for a new one, by itself, exactly once: it got one, and the server still said no.
    let asked: Vec<&str> = shell.requests.iter().map(|r| r.url.strip_prefix("http://").unwrap().split_once('/').unwrap().1).collect();
    assert_eq!(asked, ["api/auth/websocket-ticket/rpc", "api/domovoi/local/session", "api/auth/websocket-ticket/rpc"], "one renewal, no loop");
    assert_eq!(shell.requests[0].url, "http://localhost:1/api/auth/websocket-ticket/rpc");
    assert_eq!(shell.requests[1].url, "http://127.0.0.1:1/api/domovoi/local/session", "at the descriptor's address");
    assert_eq!(bearer(&shell.requests[2]), Some("Bearer tok-local"));
    assert!(socket_url(&effects).is_none());
    let view = DomovoiApp.view(&model);
    assert_eq!(view.connection.state, "failed");
    assert!(view.connection.needs_pairing, "then the pairing form, as for any server");
    assert!(!shell.store.keys().any(|k| k.starts_with("token:")));
    assert_eq!(model.environments.len(), 1, "the saved entry, not a second one");

    // With a server that takes the new sign-in, the user sees nothing of it.
    shell.routes.insert("/api/auth/websocket-ticket/rpc".into(), (200, ticket_answer("tick-2")));
    let mut model = Model::default();
    shell.requests.clear();
    let effects = launch(&mut shell, &mut model, "desktop");
    assert!(socket_url(&effects).unwrap().starts_with("ws://127.0.0.1:1/ws?"), "no token saved: signed in, then connected");
    assert_eq!(shell.store[&format!("token:{id}")], "tok-local");
    assert_eq!(model.environments.len(), 1);
    // The entry is this server's now: its id, and the address it listens on.
    assert_eq!((model.environments[0].url.as_str(), model.environments[0].server_id.as_str()), ("http://127.0.0.1:1", "srv-local"));
    assert!(shell.store["environments"].contains("srv-local"));
    let view = DomovoiApp.view(&model);
    assert!(view.toast.is_none() && !view.connection.needs_pairing);

    // Connected (the server's welcome came), then signed out again by the
    // server: asked once more, since the last sign-in it gave did work.
    let sent = sent_requests(&sock(&mut model, SocketEvent::Opened));
    let lifecycle = sent.iter().find(|r| r["tag"] == "subscribeServerLifecycle").unwrap()["id"].as_str().unwrap().to_owned();
    let _ = shell.drive_sock(&mut model, chunk(&lifecycle, &[records::welcome(json!({ "environmentId": "srv-local" }))]));
    shell.routes.insert("/api/auth/websocket-ticket/rpc".into(), (401, json!({})));
    let _ = sock(&mut model, SocketEvent::Closed { reason: "signed out".into() });
    shell.requests.clear();
    let _ = shell.drive(&mut model, Event::Reconnect);
    assert_eq!(shell.requests.iter().filter(|r| r.url.ends_with("/local/session")).count(), 1);
    assert!(DomovoiApp.view(&model).connection.needs_pairing, "and then no more");

    // A saved server that is not this machine's is never asked this way.
    let mut shell = beside_a_server();
    let mut model = Model::default();
    connected_to(&mut shell, &mut model, "http://nas.lan:13773", "CODE");
    shell.routes.insert("/api/auth/websocket-ticket/rpc".into(), (401, json!({})));
    let mut model = Model::default();
    shell.requests.clear();
    let _ = launch(&mut shell, &mut model, "desktop");
    assert!(shell.requests.iter().all(|r| !r.url.contains("/local/")));
    assert!(DomovoiApp.view(&model).connection.needs_pairing);
}

#[test]
fn a_server_that_starts_after_the_app_signs_its_waiting_entry_in() {
    let mut shell = beside_a_server();
    let mut model = Model::default();
    let _ = launch(&mut shell, &mut model, "desktop");
    let id = model.active_env.clone().unwrap();
    // Next launch: the token is gone and the server is not running (no descriptor).
    shell.store.remove(&format!("token:{id}"));
    let descriptor = shell.descriptor.take();
    let mut model = Model::default();
    shell.requests.clear();
    let _ = launch(&mut shell, &mut model, "desktop");
    assert!(DomovoiApp.view(&model).connection.needs_pairing);
    assert!(shell.requests.is_empty());
    // The server starts: its descriptor appears, and the entry is signed in.
    let effects = shell.drive(&mut model, Event::LocalServer { out: LocalOutput::Descriptor { text: descriptor } });
    assert!(socket_url(&effects).is_some());
    assert_eq!(shell.store[&format!("token:{id}")], "tok-local");
}

#[test]
fn forgetting_a_server_stops_the_app_signing_in_by_itself() {
    let mut shell = beside_a_server();
    shell.routes.insert("/api/domovoi/session/end".into(), (200, json!({ "signedOut": true })));
    let mut model = Model::default();
    let _ = launch(&mut shell, &mut model, "desktop");
    let _ = sock(&mut model, SocketEvent::Opened);
    let id = model.active_env.clone().unwrap();
    let _ = shell.drive(&mut model, Event::ForgetEnvironment { id: id.clone() });
    assert!(model.environments.is_empty());
    let mut left: Vec<(&str, &str)> = shell.store.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    left.sort_unstable();
    assert_eq!(left, [("environments", "[]"), ("localAuto", "1")], "no server, token, draft or copy is left: only the note that it happened");

    // Relaunched: offered, not connected.
    let mut model = Model::default();
    shell.requests.clear();
    let effects = launch(&mut shell, &mut model, "desktop");
    assert!(socket_url(&effects).is_none() && shell.requests.is_empty());
    let view = DomovoiApp.view(&model);
    assert_eq!(view.connection.state, "disconnected");
    assert!(view.connection.local_server.is_some(), "one click away");
    let effects = shell.drive(&mut model, Event::ConnectLocalServer);
    assert!(socket_url(&effects).is_some());

    // A server paired by code and then forgotten: the same, though the app never signed in by itself.
    let mut shell = beside_a_server();
    shell.descriptor = None;
    let mut model = Model::default();
    let _ = launch(&mut shell, &mut model, "desktop");
    connected_to(&mut shell, &mut model, "http://other:2", "CODE");
    let other = model.active_env.clone().unwrap();
    let _ = shell.drive(&mut model, Event::ForgetEnvironment { id: other });
    assert_eq!(shell.store.get("localAuto").map(String::as_str), Some("1"));
    shell.requests.clear();
    let descriptor = beside_a_server().descriptor;
    let effects = shell.drive(&mut model, Event::LocalServer { out: LocalOutput::Descriptor { text: descriptor } });
    assert!(socket_url(&effects).is_none() && shell.requests.is_empty(), "a server starting later is offered, not joined");
    assert!(DomovoiApp.view(&model).connection.local_server.is_some());
}

#[test]
fn a_phone_never_looks_for_a_server_beside_it() {
    let mut shell = beside_a_server();
    let mut model = Model::default();
    let effects = launch(&mut shell, &mut model, "mobile");
    assert!(shell.local_asked.is_empty(), "no watch");
    assert!(effects.iter().all(|e| !matches!(e, Effect::Local(_))));
    assert!(shell.requests.is_empty() && model.environments.is_empty());
    assert!(DomovoiApp.view(&model).connection.local_server.is_none());
    // Told of one anyway, it is not acted on by itself... and what is not a descriptor is none.
    let mut model = Model::default();
    let _ = launch(&mut shell, &mut model, "desktop");
    for text in ["not json", r#"{"version":2,"url":"http://127.0.0.1:1","environmentId":"e","label":"x"}"#, r#"{"version":1,"url":"http://evil.example:1","environmentId":"e","label":"x"}"#] {
        let mut model = Model::default();
        let mut shell = Shell::paired();
        shell.descriptor = Some(text.into());
        let _ = launch(&mut shell, &mut model, "desktop");
        assert!(model.local_server.is_none() && shell.requests.is_empty(), "{text}");
    }
}

const LINK: &str = "domovoi://connect?ticket=endpointnew&code=LINKCODE&name=Free%20%E2%80%AEGPUs";

#[test]
fn a_link_from_outside_the_app_connects_nothing_until_the_user_says_so() {
    let mut shell = Shell::paired();
    let mut model = Model::default();
    connected_to(&mut shell, &mut model, "http://a:1", "CODE");
    let saved = shell.store.clone();
    shell.requests.clear();

    // A page opens a link: the connection stays up, nothing is sent, nothing is saved.
    let effects = shell.drive(&mut model, Event::OpenLink { link: LINK.into() });
    assert!(!socket_closed(&effects) && tunnel_started(&effects).is_none());
    assert!(shell.requests.is_empty());
    assert_eq!(shell.store, saved);
    let view = DomovoiApp.view(&model);
    assert_eq!(view.connection.state, "connected");
    let asked = view.connection.link_request.expect("the user is asked");
    assert_eq!(asked.title, "Connect to a new server?");
    assert_eq!((asked.endpoint.as_str(), asked.known_label.as_str(), asked.confirm_label.as_str()), ("endpointne", "", "Connect"));
    // The name is the link's own claim: shown as that, without the character that would reorder the text.
    assert_eq!(asked.link_name, "Free GPUs");
    assert!(asked.message.contains("only if you made this link") && !asked.message.contains("Free"), "{}", asked.message);

    // No: as if the link had never come.
    let effects = shell.drive(&mut model, Event::AnswerLinkRequest { id: asked.id, accept: false });
    assert!(effects.iter().all(|e| matches!(e, Effect::Render(_))));
    let view = DomovoiApp.view(&model);
    assert!(view.connection.link_request.is_none());
    assert_eq!((view.connection.state.as_str(), model.environments.len()), ("connected", 1));
    assert_eq!(shell.store, saved);

    // A second link replaces the first, and an answer to the first is for nothing.
    let _ = shell.drive(&mut model, Event::OpenLink { link: "domovoi://connect?ticket=endpointone&code=ONE".into() });
    let first = DomovoiApp.view(&model).connection.link_request.unwrap().id;
    let _ = shell.drive(&mut model, Event::OpenLink { link: LINK.into() });
    let second = DomovoiApp.view(&model).connection.link_request.unwrap();
    assert_ne!(first, second.id);
    let effects = shell.drive(&mut model, Event::AnswerLinkRequest { id: first, accept: true });
    assert!(effects.is_empty() && !socket_closed(&effects), "a stale answer");
    assert_eq!(DomovoiApp.view(&model).connection.link_request.map(|r| r.id), Some(second.id), "the question stays");

    // Yes: now the server in use is left and the link's is paired with.
    let mut effects = shell.drive(&mut model, Event::AnswerLinkRequest { id: second.id, accept: true });
    assert!(socket_closed(&effects));
    assert_eq!(tunnel_start(&mut effects).expect("the tunnel starts").operation, TunnelOperation::Start { ticket: "endpointnew".into() });
    assert!(DomovoiApp.view(&model).connection.link_request.is_none());
    assert_eq!(shell.store, saved, "still nothing saved: the server has not answered");
    let effects = shell.drive_tunnel(&mut model, TunnelOutput::Started { port: 4242 });
    assert!(String::from_utf8(shell.requests[0].body.clone()).unwrap().contains("subject_token=LINKCODE"));
    assert!(socket_url(&effects).is_some());
    assert!(shell.store["environments"].contains("endpointnew"));
    assert_eq!(model.active_environment().unwrap().label, "Free GPUs");

    // A link that is none says so and asks nothing.
    for bad in ["domovoi://connect?code=X", "https://example.com/x", "endpointabc", ""] {
        let effects = shell.drive(&mut model, Event::OpenLink { link: bad.into() });
        assert!(!socket_closed(&effects), "{bad}");
        let view = DomovoiApp.view(&model);
        assert!(view.connection.link_request.is_none(), "{bad}");
        assert!(view.toast.unwrap().message.contains("not a Domovoi pairing link"), "{bad}");
        let _ = run(&mut model, Event::DismissToast);
    }
}

fn tunnel_started(effects: &[Effect]) -> Option<&TunnelOperation> {
    effects.iter().find_map(|e| match e {
        Effect::Tunnel(r) if matches!(r.operation, TunnelOperation::Start { .. }) => Some(&r.operation),
        _ => None,
    })
}

#[test]
fn a_link_for_a_saved_server_changes_nothing_saved_until_the_server_answers_through_it() {
    let mut shell = Shell::paired();
    let mut model = Model::default();
    let _ = shell.drive(&mut model, Event::ConnectRemote { link: "domovoi://connect?ticket=endpointabc&code=CODE&name=Studio".into() });
    let _ = shell.drive_tunnel(&mut model, TunnelOutput::Started { port: 4242 });
    let _ = sock(&mut model, SocketEvent::Opened);
    let id = model.active_env.clone().unwrap();
    let _ = shell.drive(&mut model, Event::RenameEnvironment { id: id.clone(), label: "My studio".into() });
    let saved = shell.store.clone();

    // A link names the same server (same key) with another name and no code: the user is asked about the server they know.
    let _ = shell.drive(&mut model, Event::OpenLink { link: "domovoi://connect?ticket=endpointabc&name=Totally+Yours".into() });
    let asked = DomovoiApp.view(&model).connection.link_request.unwrap();
    assert_eq!((asked.title.as_str(), asked.known_label.as_str(), asked.link_name.as_str()), ("Connect to My studio?", "My studio", "Totally Yours"));
    assert!(!asked.message.contains("Totally Yours"), "a saved server goes by its saved name: {}", asked.message);
    let _ = shell.drive(&mut model, Event::AnswerLinkRequest { id: asked.id, accept: true });
    assert_eq!(model.environments.len(), 1);
    assert_eq!(model.environments[0].label, "My studio", "the link does not rename it");
    assert_eq!(shell.store, saved);
}

#[test]
fn the_pairing_url_the_server_prints_can_be_pasted_where_a_code_or_a_link_goes() {
    use domovoi_core::environments::parse_pairing_url;
    assert_eq!(parse_pairing_url(" http://localhost:13773/pair#token=ABCDEFGH2345 "), Some(("http://localhost:13773".into(), "ABCDEFGH2345".into())));
    assert_eq!(parse_pairing_url("https://domovoi.example.com/pair?token=ABC&x=1"), Some(("https://domovoi.example.com".into(), "ABC".into())));
    assert_eq!(parse_pairing_url("http://[::1]:13773/pair#token=ABC"), Some(("http://[::1]:13773".into(), "ABC".into())));
    for none in ["ABCDEFGH2345", "http://localhost:13773", "http://localhost:13773/pair#token=", "domovoi://connect?ticket=x&token=y", "http:///pair#token=A", "see http://h:1/pair#token=A"] {
        assert_eq!(parse_pairing_url(none), None, "{none}");
    }

    // In the code's place: its code, and the address typed beside it.
    let mut shell = Shell::paired();
    let mut model = Model::default();
    let effects = shell.drive(&mut model, Event::Connect { server_url: "http://192.168.1.5:13773".into(), pairing_code: "http://localhost:13773/pair#token=THECODE".into() });
    assert!(socket_url(&effects).is_some());
    assert_eq!(shell.requests[0].url, "http://192.168.1.5:13773/oauth/token");
    assert!(String::from_utf8(shell.requests[0].body.clone()).unwrap().contains("subject_token=THECODE&"));
    // With no address typed, the URL's own.
    let mut shell = Shell::paired();
    let mut model = Model::default();
    let _ = shell.drive(&mut model, Event::Connect { server_url: " ".into(), pairing_code: "http://localhost:13773/pair#token=THECODE".into() });
    assert_eq!(shell.requests[0].url, "http://localhost:13773/oauth/token");
    // In the address's place: both from it.
    let mut shell = Shell::paired();
    let mut model = Model::default();
    let _ = shell.drive(&mut model, Event::Connect { server_url: "http://localhost:13773/pair#token=THECODE".into(), pairing_code: String::new() });
    assert_eq!(shell.requests[0].url, "http://localhost:13773/oauth/token");
    assert!(String::from_utf8(shell.requests[0].body.clone()).unwrap().contains("subject_token=THECODE&"));
    // Where a remote link goes: it is a local server's, and pairs as one.
    let mut shell = Shell::paired();
    let mut model = Model::default();
    let effects = shell.drive(&mut model, Event::ConnectRemote { link: "http://localhost:13773/pair#token=THECODE".into() });
    assert!(tunnel_started(&effects).is_none() && socket_url(&effects).is_some());
    assert_eq!(shell.requests[0].url, "http://localhost:13773/oauth/token");
    assert!(!model.environments[0].is_remote());
    // And the rule for plain http still holds for it.
    let mut model = Model::default();
    let _ = shell.drive(&mut model, Event::ConnectRemote { link: "http://example.com/pair#token=THECODE".into() });
    assert!(DomovoiApp.view(&model).connection.error.unwrap().contains("outside your network"));
}

#[test]
fn a_first_pairing_that_fails_leaves_nothing_saved() {
    let mut shell = Shell::paired();
    let mut model = Model::default();
    connected_to(&mut shell, &mut model, "http://a:1", "CODE");
    let a = model.active_env.clone().unwrap();
    let saved = shell.store.clone();

    // A wrong code for a new server, then an address nothing answers on, then a link whose server refuses.
    shell.routes.insert("/oauth/token".into(), (401, json!({ "_tag": "EnvironmentAuthInvalidError" })));
    let _ = shell.drive(&mut model, Event::Connect { server_url: "http://b:2".into(), pairing_code: "WRONG".into() });
    let view = DomovoiApp.view(&model);
    assert_eq!(view.connection.state, "failed");
    assert!(view.connection.needs_pairing, "the form stays, to try another code");
    assert_eq!(shell.store, saved, "no token-less server is left behind");
    assert_eq!(model.environments.len(), 2, "it is there while it is being paired");

    let _ = shell.drive(&mut model, Event::Connect { server_url: "http://c:3".into(), pairing_code: "WRONG".into() });
    assert_eq!(model.environments.iter().map(|e| e.url.as_str()).collect::<Vec<_>>(), ["http://a:1", "http://c:3"], "the one before went when another was tried");
    let _ = shell.drive(&mut model, Event::ConnectRemote { link: "domovoi://connect?ticket=endpointzzz&code=WRONG".into() });
    let _ = shell.drive_tunnel(&mut model, TunnelOutput::Started { port: 4242 });
    assert_eq!(DomovoiApp.view(&model).connection.state, "failed");
    assert_eq!(shell.store, saved);

    // A relaunch is back on the server that was saved.
    let mut relaunched = Model::default();
    shell.routes.remove("/oauth/token");
    let effects = launch(&mut shell, &mut relaunched, "mobile");
    assert_eq!(relaunched.active_env.as_ref(), Some(&a));
    assert_eq!(relaunched.environments.len(), 1);
    assert!(socket_url(&effects).is_some());

    // Forgetting the one that never paired removes it and nothing else; going back to the saved one drops it too.
    let unpaired = model.active_env.clone().unwrap();
    let _ = shell.drive(&mut model, Event::ForgetEnvironment { id: unpaired });
    assert_eq!(shell.store, saved);
    assert_eq!(model.environments.len(), 1);
    let _ = shell.drive(&mut model, Event::Connect { server_url: "http://d:4".into(), pairing_code: "WRONG".into() });
    let effects = shell.drive(&mut model, Event::SelectEnvironment { id: a.clone() });
    assert!(socket_url(&effects).is_some());
    assert_eq!(model.environments.len(), 1);

    // The right code, at last: saved with its token, and active.
    shell.routes.insert("/oauth/token".into(), Shell::paired().routes["/oauth/token"].clone());
    let _ = shell.drive(&mut model, Event::Connect { server_url: "http://b:2".into(), pairing_code: "RIGHT".into() });
    let b = model.active_env.clone().unwrap();
    assert!(shell.store["environments"].contains("http://b:2") && shell.store.contains_key(&format!("token:{b}")));
    assert_eq!(shell.store["activeEnvironment"], b);
}

#[test]
fn forgetting_a_server_that_is_not_the_connected_one_ends_its_sign_in_and_says_what_happened() {
    let mut shell = Shell::paired();
    shell.routes.insert("/api/domovoi/session/end".into(), (200, json!({ "signedOut": true })));
    let mut model = Model::default();
    connected_to(&mut shell, &mut model, "http://a:1", "CODE");
    let a = model.active_env.clone().unwrap();
    shell.routes.insert("/oauth/token".into(), (200, json!({ "access_token": "tok-b", "issued_token_type": "urn:ietf:params:oauth:token-type:access_token", "token_type": "Bearer", "expires_in": 100, "scope": "orchestration:read" })));
    connected_to(&mut shell, &mut model, "http://b:2", "CODE2");
    let b = model.active_env.clone().unwrap();
    // What this device keeps for a server besides its token.
    shell.store.insert(format!("drafts:{a}"), "{}".into());
    shell.store.insert(format!("cache:shell:{a}"), "{}".into());
    shell.requests.clear();

    // Forgetting A while connected to B: B stays up, and A is asked to end A's sign-in with A's token.
    let effects = shell.drive(&mut model, Event::ForgetEnvironment { id: a.clone() });
    assert!(!socket_closed(&effects), "the server in use is not left");
    assert_eq!(shell.requests.len(), 1);
    assert_eq!((shell.requests[0].url.as_str(), bearer(&shell.requests[0])), ("http://a:1/api/domovoi/session/end", Some("Bearer tok-1")));
    assert_eq!(model.active_env.as_ref(), Some(&b));
    let view = DomovoiApp.view(&model);
    assert_eq!(view.connection.state, "connected");
    assert!(view.toast.unwrap().message.contains("signed this device out"));
    assert!(!shell.store.keys().any(|k| k.contains(&a)), "its token, drafts and offline copy are gone: {:?}", shell.store.keys());
    assert!(shell.store.contains_key(&format!("token:{b}")));

    // A server that cannot be reached: forgotten here, and the user is told the sign-in is still there.
    let _ = run(&mut model, Event::DismissToast);
    shell.routes.insert("/oauth/token".into(), Shell::paired().routes["/oauth/token"].clone());
    connected_to(&mut shell, &mut model, "http://c:3", "CODE3");
    let c = model.active_env.clone().unwrap();
    let _ = shell.drive(&mut model, Event::SelectEnvironment { id: b.clone() });
    let _ = sock(&mut model, SocketEvent::Opened);
    shell.routes.insert("/api/domovoi/session/end".into(), (503, json!({})));
    let _ = shell.drive(&mut model, Event::ForgetEnvironment { id: c.clone() });
    let said = DomovoiApp.view(&model).toast.unwrap().message;
    assert!(said.contains("could not be reached") && said.contains("until it expires"), "{said}");
    assert!(!shell.store.contains_key(&format!("token:{c}")));

    // A remote server that is not connected is not dialled just to say goodbye; that is said too.
    let _ = run(&mut model, Event::DismissToast);
    let _ = shell.drive(&mut model, Event::ConnectRemote { link: "domovoi://connect?ticket=endpointabc&code=CODE&name=Far".into() });
    let _ = shell.drive_tunnel(&mut model, TunnelOutput::Started { port: 4242 });
    let _ = sock(&mut model, SocketEvent::Opened);
    let far = model.active_env.clone().unwrap();
    let _ = shell.drive(&mut model, Event::SelectEnvironment { id: b.clone() });
    let _ = sock(&mut model, SocketEvent::Opened);
    shell.requests.clear();
    let effects = shell.drive(&mut model, Event::ForgetEnvironment { id: far.clone() });
    assert!(shell.requests.is_empty() && tunnel_started(&effects).is_none());
    let said = DomovoiApp.view(&model).toast.unwrap().message;
    assert!(said.contains("Forgot Far") && said.contains("stays signed in"), "{said}");
    assert!(!shell.store.contains_key(&format!("token:{far}")));
    assert_eq!(model.environments.len(), 1);
}

#[test]
fn a_server_reached_under_a_name_it_does_not_answer_to_says_what_to_do() {
    let refusal = json!({ "_tag": "HostNotAllowedError", "host": "domovoi.example.com", "message": "This server does not answer to the name \"domovoi.example.com\". If you reach it by that name (a reverse proxy, a DNS name of your own), start it with --allowed-host domovoi.example.com." });
    // Pairing: the server's own words, not "Pairing failed (403)".
    let mut shell = Shell::paired();
    shell.routes.insert("/oauth/token".into(), (403, refusal.clone()));
    let mut model = Model::default();
    let _ = shell.drive(&mut model, Event::Connect { server_url: "https://domovoi.example.com".into(), pairing_code: "CODE".into() });
    assert!(DomovoiApp.view(&model).connection.error.unwrap().contains("--allowed-host domovoi.example.com"));

    // A paired device after the server's flags changed: told the same, not told to pair again or to forget the server.
    let mut shell = Shell::paired();
    let mut model = Model::default();
    connected_to(&mut shell, &mut model, "https://domovoi.example.com", "CODE");
    let id = model.active_env.clone().unwrap();
    shell.routes.insert("/api/auth/websocket-ticket/rpc".into(), (403, refusal));
    let _ = sock(&mut model, SocketEvent::Closed { reason: "restarted".into() });
    let _ = shell.drive(&mut model, Event::Reconnect);
    let view = DomovoiApp.view(&model);
    assert_eq!(view.connection.state, "failed");
    assert!(view.connection.error.unwrap().contains("--allowed-host"));
    assert!(!view.connection.server_changed && !view.connection.needs_pairing);
    assert!(shell.store.contains_key(&format!("token:{id}")), "the sign-in is kept");
    // Fixed on the server, a retry connects.
    shell.routes.insert("/api/auth/websocket-ticket/rpc".into(), Shell::paired().routes["/api/auth/websocket-ticket/rpc"].clone());
    let effects = shell.drive(&mut model, Event::Reconnect);
    assert!(socket_url(&effects).is_some());
}
