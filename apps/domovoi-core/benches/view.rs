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
    let snapshot = ThreadStreamItem::from_value(json!({ "kind": "snapshot", "snapshotSequence": 1, "projection": projection(items) })).expect("the snapshot fits the schema");
    let ThreadStreamItem::Snapshot(snapshot) = snapshot else { unreachable!("a snapshot") };
    let thread = Thread::from_snapshot(*snapshot);
    // Listed as its own records give it.
    model.threads = vec![thread.shell()];
    model.thread = Some(thread);
    model
}

/// The projection of `long_thread(items)`'s thread, as the server sends it.
fn projection(items: usize) -> Value {
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
    projection
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

/// The recorded session. `DOMOVOI_FIXTURES` names another `fixtures` folder: a
/// build kept by `tools/perf.py` reads the recording it was saved with.
fn recording() -> std::path::PathBuf {
    let fixtures = std::env::var_os("DOMOVOI_FIXTURES").map_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures"), Into::into);
    fixtures.join("native/session1/wire.ndjson")
}

/// From the recorded session: the config snapshot item,
/// the shell stream's items, and one thread shell to copy.
fn recorded_boot() -> (Value, Vec<Value>, Value) {
    let path = recording();
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

/// The reply the socket benches stream: a new item after the thread's last.
const LIVE_ITEM: &str = "ia-live";
/// What each streamed flush adds.
const ADDED: &str = "more words ";

/// A reply's first flush as the server sends it: its turn item, with `text`.
fn first_flush_frame(request: &str, items: usize, text: &str) -> String {
    let reply = json!({ "type": "assistant_message", "messageId": "ma-live", "text": text, "streaming": true });
    let item = turn_item(LIVE_ITEM, &format!("r{}", items / 4 - 1), items as u64 + 1, "running", "2", reply);
    chunk(request, &[json!({ "kind": "event", "sequence": 2_000_000, "event": event(0, "turn-item.updated", "2", item) })])
}

/// A later flush as the server sends it: the text added to the reply, which
/// was `at` bytes long.
fn flush_frame(request: &str, at: usize, k: u64) -> String {
    chunk(request, &[json!({ "kind": "append", "sequence": 2_000_000 + k, "threadId": "t1", "itemId": LIVE_ITEM, "at": at, "text": ADDED, "updatedAt": format!("2.{k}") })])
}

const SOCKET_SIZES: [usize; 3] = [500, 2000, 10_000];

fn main() {
    {
        let (mut model, request) = connected_thread(2000);
        let app = FfiApp::default();
        let first = render_delta(&app, &model);
        feed(&mut model, first_flush_frame(&request, 2000, ""));
        let _ = render_delta(&app, &model);
        feed(&mut model, flush_frame(&request, 0, 1));
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
    feed(&mut model, first_flush_frame(&request, n, start));
    let _ = render_delta(&app, &model);
    let mut at = start.len();
    let mut k = 0u64;
    b.with_inputs(|| {
        k += 1;
        let frame = flush_frame(&request, at, k);
        at += ADDED.len();
        frame
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

/// A flush 20,000 characters into a reply.
#[divan::bench(args = [2000])]
fn socket_token_long_reply(b: divan::Bencher, n: usize) {
    socket_flushes(b, n, &"more words ".repeat(1818));
}

/// Opening a thread as a shell pays for it: the snapshot's socket frame, as
/// the server writes it for this thread, through `update` (decoded, the
/// thread built from it), `FfiApp::view`, the delta's encoding.
#[divan::bench(args = [500, 2000])]
fn socket_snapshot(b: divan::Bencher, n: usize) {
    let (mut model, request) = connected_thread(n);
    let app = FfiApp::default();
    let _ = render_delta(&app, &model);
    let projection = serde_json::to_value(model.thread.as_ref().expect("thread").projection()).expect("a projection is plain data");
    let frame = chunk(&request, &[json!({ "kind": "snapshot", "snapshotSequence": 3_000_000, "projection": projection })]);
    eprintln!("PERF view.socket_snapshot.{n}.frame_bytes {} bytes", frame.len());
    b.with_inputs(|| frame.clone()).bench_local_values(|frame| {
        feed(&mut model, frame);
        render_delta(&app, &model)
    });
}

/// Decoding a flush's frame alone (`RpcClient::receive`).
#[divan::bench]
fn receive_flush_frame(b: divan::Bencher) {
    let (mut model, request) = connected_thread(100);
    let frame = flush_frame(&request, 20_000, 1);
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

/// A keystroke in the composer, then a streamed flush: the flush should cost
/// what `socket_token` costs, not a whole view (typing reaches the composer only).
#[divan::bench(args = [2000])]
fn keystroke_then_token(b: divan::Bencher, n: usize) {
    let (mut model, request) = connected_thread(n);
    let app = FfiApp::default();
    feed(&mut model, first_flush_frame(&request, n, ""));
    let _ = render_delta(&app, &model);
    let mut at = 0;
    let mut k = 0u64;
    b.with_inputs(|| {
        k += 1;
        let draft = if k % 2 == 0 { "a reply" } else { "a reply." };
        let frame = flush_frame(&request, at, k);
        at += ADDED.len();
        (draft, frame)
    })
    .bench_local_values(|(draft, frame)| {
        let _ = DomovoiApp.update(Event::SetDraft { text: draft.into() }, &mut model);
        feed(&mut model, frame);
        render_delta(&app, &model)
    });
}

/// Opening a thread once its snapshot is in: the snapshot frame through
/// `update`, then the first view of the page. (Fetching it is the network's.)
#[divan::bench(args = [200, 2000])]
fn thread_open_snapshot(b: divan::Bencher, n: usize) {
    let frame_of = |request: &str| chunk(request, &[json!({ "kind": "snapshot", "snapshotSequence": 1, "projection": projection(n) })]);
    b.with_inputs(|| {
        let (mut model, _) = connected_thread(4);
        model.thread = None;
        let app = FfiApp::default();
        let _ = render_delta(&app, &model);
        let (request, _) = model.rpc.request("orchestration.subscribeThread", &json!({ "threadId": "t1" }), Route::Thread("t1".into()));
        let frame = frame_of(&request);
        (model, app, frame)
    })
    .bench_local_values(|(mut model, app, frame)| {
        feed(&mut model, frame);
        render_delta(&app, &model)
    });
}

/// The shell stream's first snapshot with 1,000 threads: decode, apply, view.
#[divan::bench]
fn shell_snapshot_apply_1000(b: divan::Bencher) {
    let (_, shell, template) = recorded_boot();
    // The recorded snapshot was sent before the session made its project: the project is in the item after it.
    let projects: Vec<Value> = shell.iter().filter(|v| v["kind"] == "project.updated").map(|v| v["project"].clone()).collect();
    assert!(!projects.is_empty(), "the recording holds a project");
    let project = projects.first().map(|p| p["id"].clone()).unwrap_or(Value::Null);
    let threads: Vec<Value> = (0..1000)
        .map(|i| {
            let mut thread = template.clone();
            thread["id"] = json!(format!("s{i}"));
            thread["projectId"] = project.clone();
            thread
        })
        .collect();
    let snapshot = json!({ "kind": "snapshot", "snapshot": { "schemaVersion": 1, "snapshotSequence": 5, "projects": projects, "threads": threads, "archivedThreads": [] } });
    b.with_inputs(|| {
        let (mut model, _) = connected_thread(4);
        let app = FfiApp::default();
        let _ = render_delta(&app, &model);
        let (request, _) = model.rpc.request("orchestration.subscribeShell", &json!({}), Route::Shell);
        let frame = chunk(&request, std::slice::from_ref(&snapshot));
        (model, app, frame)
    })
    .bench_local_values(|(mut model, app, frame)| {
        feed(&mut model, frame);
        assert!(model.threads.len() >= 1000);
        render_delta(&app, &model)
    });
}

/// Opening a thread opened lately: shown from the recent threads with its
/// rows, then the first view of the page, before anything arrives.
#[divan::bench(args = [200, 2000])]
fn thread_open_cached(b: divan::Bencher, n: usize) {
    b.with_inputs(|| {
        let (mut model, request) = connected_thread(n);
        let app = FfiApp::default();
        // Built once, as when it was read; its stream had a snapshot.
        feed(&mut model, chunk(&request, &[json!({ "kind": "snapshot", "snapshotSequence": 7, "projection": projection(n) })]));
        let _ = render_delta(&app, &model);
        // Another thread opened meanwhile.
        let _ = DomovoiApp.update(Event::SelectThread { thread_id: "s0".into() }, &mut model);
        let _ = render_delta(&app, &model);
        (model, app)
    })
    .bench_local_values(|(mut model, app)| {
        let _ = DomovoiApp.update(Event::SelectThread { thread_id: "t1".into() }, &mut model);
        assert!(model.thread.is_some(), "shown at once");
        render_delta(&app, &model)
    });
}
