//! What a shell pays per render: `view()` plus the bincode encoding that
//! `CoreFfi::view` sends across FFI, on threads of 100, 500 and 2000 items
//! (markdown, a long command output and an edit per turn, as in
//! `tests/client_replay.rs`), cold and per streamed token.
//!
//! `socket_token` is the whole client cost of one streamed flush as a shell
//! pays it: the socket frame through `DomovoiApp.update` (decode, apply, ack),
//! then `FfiApp::view` and its encoding, on a model booted from the recorded
//! server config (11 providers) with 300 threads in the sidebar.
//!
//! `cargo bench -p domovoi_core --bench view`

use crux_core::App;
use crux_core::bridge::{BincodeFfiFormat, FfiFormat};
use serde_json::{Value, json};
use domovoi_core::delta::FfiApp;
use domovoi_core::model::Connection;
use domovoi_core::rpc::Route;
use domovoi_core::socket::SocketEvent;
use domovoi_core::{Event, Model, DomovoiApp};
use domovoi_model::typed::{DomainEvent, RecordKind, Thread, ThreadStreamItem};

/// A long thread: per turn a user message, a 400-line command, a 150-line
/// edit and a markdown answer.
fn long_thread(items: usize) -> Model {
    let mut model = Model::default();
    model.selected_thread = Some("t1".into());
    model.route = "thread".into();
    let md = "## Plan\n\nSome **bold** text with `code` and a [link](http://x).\n\n- one\n- two\n\n```rust\nfn main() { println!(\"hi\"); }\n```\n";
    let (mut messages, mut turn_items) = (vec![], vec![]);
    for t in 0..items / 4 {
        let (u, a) = (format!("mu{t}"), format!("ma{t}"));
        messages.push(message(&u, "user", &format!("question {t}"), false, "1"));
        messages.push(message(&a, "assistant", &md.repeat(4), false, "1"));
        let (base, run) = ((t * 4) as u64, format!("r{t}"));
        turn_items.push(turn_item(&format!("iu{t}"), &run, base, "completed", "1", json!({ "type": "user_message", "createdBy": "user", "creationSource": "web", "messageId": u, "inputIntent": "turn_start", "text": "", "attachments": [] })));
        turn_items.push(turn_item(&format!("ic{t}"), &run, base + 1, "completed", "1", json!({ "type": "command_execution", "input": "ls", "output": "a\n".repeat(400) })));
        turn_items.push(turn_item(&format!("ie{t}"), &run, base + 2, "completed", "1", json!({ "type": "file_change", "fileName": "a.rs", "oldStr": "x\n".repeat(150), "newStr": "y\n".repeat(150) })));
        turn_items.push(turn_item(&format!("ia{t}"), &run, base + 3, "completed", "1", json!({ "type": "assistant_message", "messageId": a, "text": "", "streaming": false })));
    }
    let record = json!({
        "createdBy": "user", "creationSource": "web", "id": "t1", "projectId": "p1", "title": "Long", "providerInstanceId": "codex",
        "modelSelection": { "instanceId": "codex", "model": "model-1" }, "runtimeMode": "full-access", "interactionMode": "default",
        "branch": null, "worktreePath": null, "activeProviderThreadId": null,
        "lineage": { "parentThreadId": null, "relationshipToParent": null, "rootThreadId": "t1" }, "forkedFrom": null,
        "createdAt": "1", "updatedAt": "1", "archivedAt": null, "deletedAt": null,
    });
    let mut projection = json!({ "thread": record, "messages": messages, "turnItems": turn_items, "visibleTurnItems": [], "updatedAt": "1" });
    for kind in RecordKind::ALL {
        if projection.get(kind.collection()).is_none() {
            projection[kind.collection()] = json!([]);
        }
    }
    let snapshot = ThreadStreamItem::from_value(json!({ "kind": "snapshot", "snapshotSequence": 1, "projection": projection })).expect("the snapshot fits the schema");
    let ThreadStreamItem::Snapshot(snapshot) = snapshot else { unreachable!("a snapshot") };
    let thread = Thread::from_snapshot(*snapshot);
    // Listed as its own records give it.
    model.threads = vec![thread.shell()];
    model.thread = Some(thread);
    model
}

/// A message of thread `t1`, as the server sends it.
fn message(id: &str, role: &str, text: &str, streaming: bool, at: &str) -> Value {
    json!({
        "createdBy": if role == "user" { "user" } else { "agent" }, "creationSource": "web", "id": id, "threadId": "t1", "runId": null,
        "nodeId": null, "role": role, "text": text, "attachments": [], "streaming": streaming, "createdAt": "1", "updatedAt": at,
    })
}

/// A turn item of thread `t1`, as the server sends it: the fields every
/// item has, and `own`, its type's.
fn turn_item(id: &str, run: &str, ordinal: u64, status: &str, at: &str, own: Value) -> Value {
    let mut item = json!({
        "id": id, "threadId": "t1", "runId": run, "nodeId": null, "providerThreadId": null, "providerTurnId": null, "nativeItemRef": null,
        "parentItemId": null, "ordinal": ordinal, "status": status, "title": null, "startedAt": null, "completedAt": null, "updatedAt": at,
    });
    if let (Value::Object(item), Value::Object(own)) = (&mut item, own) {
        item.extend(own);
    }
    item
}

/// An event of thread `t1`, as the server records it.
fn event(n: u64, event_type: &str, at: &str, payload: Value) -> Value {
    json!({ "id": format!("event-{n}"), "threadId": "t1", "occurredAt": at, "type": event_type, "payload": payload })
}

fn last_message_id(items: usize) -> String {
    format!("ma{}", items / 4 - 1)
}

/// `view()` and its bincode encoding: what one render costs the core.
fn render(model: &Model) -> usize {
    let mut out = Vec::new();
    BincodeFfiFormat::serialize(&mut out, &DomovoiApp.view(model)).expect("view model serializes");
    out.len()
}

const SIZES: [usize; 3] = [100, 500, 2000];

/// `FfiApp::view` (the delta against the last view) and its encoding: what
/// one render costs the core and the FFI since view deltas.
fn render_delta(app: &FfiApp, model: &Model) -> usize {
    let mut out = Vec::new();
    BincodeFfiFormat::serialize(&mut out, &app.view(model)).expect("view delta serializes");
    out.len()
}

/// One streamed token's model change.
fn stream_token(model: &mut Model, id: &str, text: &mut String, k: u64) {
    text.push_str("more words ");
    let event = event(k, "message.updated", "2", message(id, "assistant", text, true, &format!("2.{k}")));
    model.thread.as_mut().expect("thread").apply(DomainEvent::from_value(event).expect("the event fits the schema"));
    // Changed by hand, not by an update: the view is built whole.
    model.changes.wide();
}

/// From the recorded TypeScript-server session: the config snapshot item,
/// the shell stream's items, and one thread shell to copy.
fn recorded_boot() -> (Value, Vec<Value>, Value) {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/golden/ts-session1/wire.ndjson");
    let text = std::fs::read_to_string(path).expect("fixture");
    let mut tags: std::collections::HashMap<(i64, String), String> = Default::default();
    let (mut config, mut shell, mut template) = (None, vec![], Value::Null);
    for rec in text.lines().map(|l| serde_json::from_str::<Value>(l).expect("fixture line")) {
        let f = &rec["frame"];
        if rec["kind"] != "ws" || !f.is_object() {
            continue;
        }
        let conn = rec["conn"].as_i64().unwrap_or(0);
        if f["_tag"] == "Request" {
            tags.insert((conn, f["id"].to_string()), f["tag"].as_str().unwrap_or_default().to_owned());
            continue;
        }
        let Some(tag) = tags.get(&(conn, f["requestId"].to_string())) else { continue };
        for v in f["values"].as_array().into_iter().flatten() {
            match tag.as_str() {
                "subscribeServerConfig" if v["type"] == "snapshot" && config.is_none() => config = Some(v.clone()),
                "orchestration.subscribeShell" => {
                    if v["kind"] == "thread.updated" {
                        template = v["thread"].clone();
                    }
                    shell.push(v.clone());
                }
                _ => {}
            }
        }
    }
    (config.expect("recorded config snapshot"), shell, template)
}

fn chunk(request: &str, values: &[Value]) -> String {
    json!({ "_tag": "Chunk", "requestId": request, "values": values }).to_string()
}

/// One socket frame through `update`, its commands run to their effects.
fn feed(model: &mut Model, text: String) -> usize {
    let attempt = model.attempt;
    let mut cmd = DomovoiApp.update(Event::Socket { attempt, event: SocketEvent::Message { text } }, model);
    cmd.effects().count()
}

/// `long_thread(items)` as a connected client holds it: the recorded server
/// config and shell, 300 more threads in the sidebar, and the thread's
/// subscription open. Returns the model and that subscription's request id.
fn connected_thread(items: usize) -> (Model, String) {
    let (config, shell, template) = recorded_boot();
    let mut model = long_thread(items);
    model.connection = Connection::Connected;
    let (config_id, _) = model.rpc.request("subscribeServerConfig", &json!({}), Route::Config);
    feed(&mut model, chunk(&config_id, &[config]));
    let (shell_id, _) = model.rpc.request("orchestration.subscribeShell", &json!({}), Route::Shell);
    feed(&mut model, chunk(&shell_id, &shell));
    let project = model.projects.first().map(|p| json!(p.id)).unwrap_or(Value::Null);
    let threads: Vec<Value> = (0..=300)
        .map(|i| {
            let mut thread = template.clone();
            thread["id"] = json!(if i == 300 { "t1".to_owned() } else { format!("s{i}") });
            thread["projectId"] = project.clone();
            json!({ "kind": "thread.updated", "sequence": 1_000_000 + i, "location": "active", "thread": thread })
        })
        .collect();
    feed(&mut model, chunk(&shell_id, &threads));
    assert!(model.threads.len() > 300 && !model.providers.is_empty(), "booted from the recording");
    let (thread_id, _) = model.rpc.request("orchestration.subscribeThread", &json!({ "threadId": "t1" }), Route::Thread("t1".into()));
    (model, thread_id)
}

/// A streamed flush as the server sends it: the reply's node, message and
/// turn item, each with the whole text so far, in one frame.
fn flush_frame(request: &str, items: usize, text: &str, k: u64) -> String {
    let turn = items / 4 - 1;
    let (message_id, at) = (last_message_id(items), format!("2.{k}"));
    let streamed = |n: u64, ty: &str, payload: Value| json!({ "kind": "event", "sequence": 2_000_000 + 3 * k + n, "event": event(3 * k + n, ty, &at, payload) });
    let node = json!({
        "id": "n1", "threadId": "t1", "runId": null, "parentNodeId": null, "rootNodeId": "n0", "kind": "assistant_message", "status": "running",
        "countsForRun": true, "providerThreadId": null, "providerTurnId": null, "nativeItemRef": null, "runtimeRequestId": null,
        "checkpointScopeId": null, "startedAt": null, "completedAt": null,
    });
    let reply = json!({ "type": "assistant_message", "messageId": message_id, "text": text, "streaming": true });
    chunk(request, &[
        streamed(0, "node.updated", node),
        streamed(1, "message.updated", message(&message_id, "assistant", text, true, &at)),
        streamed(2, "turn-item.updated", turn_item(&format!("ia{turn}"), &format!("r{turn}"), (turn * 4 + 3) as u64, "running", &at, reply)),
    ])
}

const SOCKET_SIZES: [usize; 3] = [500, 2000, 10_000];

fn main() {
    {
        let (mut model, request) = connected_thread(2000);
        let app = FfiApp::default();
        let first = render_delta(&app, &model);
        feed(&mut model, flush_frame(&request, 2000, "more words ", 1));
        eprintln!("2000 items, booted: first delta {first} bytes, then {} bytes per streamed flush", render_delta(&app, &model));
    }
    for n in SIZES {
        let model = long_thread(n);
        let page = DomovoiApp.view(&model).thread.expect("thread page");
        assert_eq!(page.items.len(), n);
        eprintln!("{n} items: {} bytes cross FFI per render (whole view model)", render(&model));
        let (app, mut model) = (FfiApp::default(), model);
        let first = render_delta(&app, &model);
        let mut text = String::new();
        stream_token(&mut model, &last_message_id(n), &mut text, 1);
        eprintln!("{n} items: first delta {first} bytes, then {} bytes per streamed token", render_delta(&app, &model));
    }
    for models in [0, 600] {
        let mut model = with_many_models(500, models);
        let app = FfiApp::default();
        let _ = render_delta(&app, &model);
        let _ = DomovoiApp.update(Event::SetDraft { text: "a".into() }, &mut model);
        eprintln!("500 items, {models} more models: {} bytes per composer keystroke", render_delta(&app, &model));
    }
    divan::main();
}

/// First render of the thread: every row built.
#[divan::bench(args = SIZES)]
fn cold_render(b: divan::Bencher, n: usize) {
    b.with_inputs(|| long_thread(n)).bench_local_refs(|m| render(m));
}

/// `view()` alone with warm row caches (no streaming change).
#[divan::bench(args = SIZES)]
fn warm_view_only(b: divan::Bencher, n: usize) {
    let model = long_thread(n);
    let _ = render(&model);
    b.bench_local(|| DomovoiApp.view(&model).thread.map_or(0, |p| p.items.len()));
}

/// One streamed token into the last message: apply the event, `view()`, encode.
#[divan::bench(args = SIZES)]
fn streaming_token(b: divan::Bencher, n: usize) {
    let mut model = long_thread(n);
    let _ = render(&model);
    let id = last_message_id(n);
    let mut text = String::new();
    let mut k = 0u64;
    b.bench_local(|| {
        k += 1;
        text.push_str("more words ");
        let event = event(k, "message.updated", "2", message(&id, "assistant", &text, true, &format!("2.{k}")));
        model.thread.as_mut().expect("thread").apply(DomainEvent::from_value(event).expect("the event fits the schema"));
        render(&model)
    });
}

/// A change that is not a reply's text growing (here: made by hand):
/// apply the event, `FfiApp::view` (the whole view, compared with the
/// last), encode the delta.
#[divan::bench(args = SIZES)]
fn streaming_token_delta(b: divan::Bencher, n: usize) {
    let mut model = long_thread(n);
    let app = FfiApp::default();
    let _ = render_delta(&app, &model);
    let id = last_message_id(n);
    let mut text = String::new();
    let mut k = 0u64;
    b.bench_local(|| {
        k += 1;
        stream_token(&mut model, &id, &mut text, k);
        render_delta(&app, &model)
    });
}

/// One streamed flush as a shell pays for it: the socket frame through
/// `update` (decode, apply, ack), `FfiApp::view`, the delta's encoding.
/// Building the frame is the server's work and is not timed.
fn socket_flushes(b: divan::Bencher, n: usize, start: &str) {
    let (mut model, request) = connected_thread(n);
    let app = FfiApp::default();
    let _ = render_delta(&app, &model);
    let mut text = start.to_owned();
    let mut k = 0u64;
    b.with_inputs(|| {
        k += 1;
        text.push_str("more words ");
        flush_frame(&request, n, &text, k)
    })
    .bench_local_values(|frame| {
        feed(&mut model, frame);
        render_delta(&app, &model)
    });
}

/// A flush early in a reply, by thread length.
#[divan::bench(args = SOCKET_SIZES)]
fn socket_token(b: divan::Bencher, n: usize) {
    socket_flushes(b, n, "");
}

/// A flush 20,000 characters into a reply (each carries the whole text).
#[divan::bench(args = [2000])]
fn socket_token_long_reply(b: divan::Bencher, n: usize) {
    socket_flushes(b, n, &"more words ".repeat(1818));
}

/// Decoding that frame alone (`RpcClient::receive`), 20,000 characters in.
#[divan::bench]
fn receive_flush_frame(b: divan::Bencher) {
    let (mut model, request) = connected_thread(100);
    let frame = flush_frame(&request, 100, &"more words ".repeat(1818), 1);
    b.bench_local(|| model.rpc.receive(&frame).len());
}

/// `connected_thread(items)` with `models` more models on the first enabled
/// provider (a pi or OpenCode install lists hundreds).
fn with_many_models(items: usize, models: usize) -> Model {
    let (mut model, _) = connected_thread(items);
    let provider = model.providers.iter_mut().find(|p| p.enabled && !p.models.is_empty()).expect("a provider with models");
    let template = provider.models[0].clone();
    for i in 0..models {
        let mut m = template.clone();
        m.slug = format!("vendor/model-{i}");
        m.name = format!("Model {i}");
        m.is_default = None;
        provider.models.push(m);
    }
    model
}

/// One keystroke in the composer with many models listed: `update`, then
/// `FfiApp::view` and the delta's encoding (the composer part carries every
/// model row).
#[divan::bench(args = [0, 600])]
fn composer_keystroke(b: divan::Bencher, models: usize) {
    let mut model = with_many_models(500, models);
    let app = FfiApp::default();
    let _ = render_delta(&app, &model);
    let mut text = String::new();
    b.bench_local(|| {
        text.push('a');
        let _ = DomovoiApp.update(Event::SetDraft { text: text.clone() }, &mut model);
        render_delta(&app, &model)
    });
}
