//! A measurement over a server's own database and the recorded sessions:
//! the thread page the core builds for each must be the page recorded
//! earlier, row for row and field for field.
//!
//! A recording is one JSON line per page: its id, its row count, a hash of
//! the whole page, a short hash of each field outside the rows, and for
//! each row a hash of the row and a short hash of each of its fields (so a
//! difference names the row and the field). `DOMOVOI_VIEW_DIGESTS_OUT` names a
//! file to write the pages built now to, as the next recording.

use crux_core::App;
use serde_json::{Map, Value, json};
use std::collections::BTreeMap;
use domovoi_core::model::Connection;
use domovoi_core::rpc::Route;
use domovoi_core::socket::SocketEvent;
use domovoi_core::view::ThreadPageView;
use domovoi_core::{Event, Model, DomovoiApp};
use domovoi_model::typed::RecordKind;

/// FNV-1a: the same on every machine and toolchain.
fn hash(text: &str) -> u64 {
    text.bytes().fold(0xcbf2_9ce4_8422_2325, |hash, byte| (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3))
}

fn short(value: &Value) -> String {
    format!("{:04x}", hash(&value.to_string()) & 0xffff)
}

/// A page as its fields outside the rows, and its rows.
fn parts(page: &ThreadPageView) -> (Map<String, Value>, Vec<Map<String, Value>>) {
    let Ok(Value::Object(mut header)) = serde_json::to_value(page) else { panic!("a page is an object") };
    let Some(Value::Array(items)) = header.remove("items") else { panic!("a page has rows") };
    let rows = items.into_iter().map(|row| if let Value::Object(row) = row { row } else { panic!("a row is an object") }).collect();
    (header, rows)
}

/// The recording of one page.
fn digest(id: &str, page: Option<&ThreadPageView>) -> Value {
    let Some(page) = page else { return json!({ "id": id, "rows": 0, "page": "none" }) };
    let (header, rows) = parts(page);
    let whole = serde_json::to_string(page).expect("a page serializes");
    json!({
        "id": id,
        "rows": rows.len(),
        "page": format!("{:016x}", hash(&whole)),
        "header": header.values().map(short).collect::<String>(),
        "items": rows.iter().map(|row| format!("{:016x}{}", hash(&Value::Object(row.clone()).to_string()), row.values().map(short).collect::<String>())).collect::<Vec<_>>(),
    })
}

/// How `page` differs from what was recorded of it, for a reader.
fn difference(recorded: &Value, page: Option<&ThreadPageView>) -> String {
    let Some(page) = page else { return "no page now".into() };
    if recorded["page"] == "none" {
        return "no page was recorded, there is one now".into();
    }
    let (header, rows) = parts(page);
    let mut out = vec![];
    // The names of the fields whose short hashes differ.
    let fields = |names: Vec<&String>, was: &str, now: String| -> Vec<String> {
        names.iter().enumerate().filter(|(n, _)| was.get(n * 4..n * 4 + 4) != now.get(n * 4..n * 4 + 4)).map(|(_, name)| (*name).clone()).collect()
    };
    let changed = fields(header.keys().collect(), recorded["header"].as_str().unwrap_or_default(), header.values().map(short).collect());
    for name in changed {
        out.push(format!("page.{name} is now {}", clip(&header[&name])));
    }
    let was_rows = recorded["items"].as_array().map(Vec::as_slice).unwrap_or_default();
    if was_rows.len() != rows.len() {
        out.push(format!("{} rows were recorded, there are {} now", was_rows.len(), rows.len()));
    }
    for (at, (was, row)) in was_rows.iter().zip(&rows).enumerate() {
        let was = was.as_str().unwrap_or_default();
        if was.get(..16) == Some(format!("{:016x}", hash(&Value::Object(row.clone()).to_string())).as_str()) {
            continue;
        }
        let changed = fields(row.keys().collect(), was.get(16..).unwrap_or_default(), row.values().map(short).collect());
        let shown: Vec<String> = changed.iter().map(|name| format!("{name} is now {}", clip(&row[name]))).collect();
        out.push(format!("row {at} ({} {}): {}", row["kind"], row["id"], if shown.is_empty() { "differs".into() } else { shown.join("; ") }));
        if out.len() >= 6 {
            break;
        }
    }
    if out.is_empty() { "the page's hash differs but no field's does".into() } else { out.join("\n        ") }
}

fn clip(value: &Value) -> String {
    let text = value.to_string();
    if text.chars().count() > 160 { format!("{}…", text.chars().take(160).collect::<String>()) } else { text }
}

/// One thread of the export, as rows of `(kind, record)`.
fn stored(row: &Value) -> &[Value] {
    row["entities"].as_array().map(Vec::as_slice).unwrap_or_default()
}

fn records<'a>(row: &'a Value, kind: &'a str) -> impl Iterator<Item = &'a Value> {
    stored(row).iter().filter(move |stored| stored["kind"] == kind).map(|stored| &stored["record"])
}

/// The model of a client that has the thread of `row` open: selected, its
/// records received as a snapshot over its stream, and listed with the
/// shell those records give.
fn opened(row: &Value) -> Model {
    let thread = row["threadId"].as_str().unwrap_or_default();
    let mut projection: Map<String, Value> = RecordKind::ALL.iter().map(|kind| (kind.collection().to_owned(), records(row, kind.collection()).cloned().collect())).collect();
    projection.insert("thread".into(), row["thread"].clone());
    projection.insert("visibleTurnItems".into(), json!([]));
    projection.insert("updatedAt".into(), row["thread"]["updatedAt"].clone());
    let mut model = Model::default();
    model.connection = Connection::Connected;
    model.selected_thread = Some(thread.to_owned());
    model.route = "thread".into();
    let (stream, _) = model.rpc.request("orchestration.subscribeThread", &json!({ "threadId": thread }), Route::Thread(thread.to_owned()));
    feed(&mut model, &stream, &[json!({ "kind": "snapshot", "snapshotSequence": 1, "projection": projection })]);
    // Listed with the shell its records give, under a project of its own.
    if let Some(shell) = model.thread.as_ref().map(|thread| thread.shell()) {
        let project = json!({
            "id": shell.project_id, "title": "Project", "workspaceRoot": "/work/project", "defaultModelSelection": null, "scripts": [],
            "createdAt": "2026-01-01T00:00:00.000Z", "updatedAt": "2026-01-01T00:00:00.000Z",
        });
        model.projects = vec![serde_json::from_value(project).expect("a project as the server lists one")];
        model.threads = vec![shell];
    }
    model.changes.wide();
    model
}

/// The same client after it expanded every row, got every attachment's URL
/// and searched the thread (every third item matches, the first is active).
fn explore(model: &mut Model, row: &Value) {
    let entry = |item: &Value| item["messageId"].as_str().or(item["id"].as_str()).unwrap_or_default().to_owned();
    for item in records(row, "turnItems") {
        model.expanded.insert(item["id"].as_str().unwrap_or_default().to_owned());
    }
    for attachment in records(row, "messages").flat_map(|message| message["attachments"].as_array().into_iter().flatten()) {
        let id = attachment["id"].as_str().unwrap_or_default();
        model.asset_urls.insert(id.to_owned(), format!("/assets/{id}"));
    }
    let hits: Vec<Value> = records(row, "turnItems").step_by(3).enumerate().map(|(at, item)| json!({ "entryId": entry(item), "startIndex": at * 2, "count": 2 })).collect();
    model.server_url = "http://server".into();
    model.find.open = true;
    model.find.query = "a".into();
    // Nothing found has no active match.
    let active = hits.first().map(|hit| json!({ "entryId": hit["entryId"], "occurrence": 0 }));
    let found = json!({ "snapshotSequence": 1, "match": active, "totalMatches": hits.len() * 2, "activeIndex": 0, "navigation": hits });
    model.find.result = Some(serde_json::from_value(found).expect("a find result as the server answers one"));
    model.changes.wide();
}

/// The recorded sessions.
const FIXTURES: [&str; 1] = ["session1"];

/// The subscription chunks of a recorded session, in the order they were
/// sent: `(request tag, thread subscribed to, the chunk's values)`.
fn recorded_chunks(name: &str) -> Vec<(String, String, Vec<Value>)> {
    let path = format!("{}/../../fixtures/native/{name}/wire.ndjson", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(path).expect("the recorded session");
    let mut requests: BTreeMap<(i64, String), Value> = BTreeMap::new();
    let mut chunks = vec![];
    for record in text.lines().map(|line| serde_json::from_str::<Value>(line).expect("a JSON line")) {
        let frame = &record["frame"];
        if record["kind"] != "ws" || !frame.is_object() {
            continue;
        }
        let connection = record["conn"].as_i64().unwrap_or(0);
        if frame["_tag"] == "Request" {
            requests.insert((connection, frame["id"].to_string()), frame.clone());
            continue;
        }
        let Some(request) = requests.get(&(connection, frame["requestId"].to_string())) else { continue };
        if frame["_tag"] == "Chunk" {
            let tag = request["tag"].as_str().unwrap_or_default().to_owned();
            let thread = request["payload"]["threadId"].as_str().unwrap_or_default().to_owned();
            chunks.push((tag, thread, frame["values"].as_array().cloned().unwrap_or_default()));
        }
    }
    chunks
}

fn feed(model: &mut Model, request: &str, values: &[Value]) {
    let text = json!({ "_tag": "Chunk", "requestId": request, "values": values }).to_string();
    let attempt = model.attempt;
    let mut command = DomovoiApp.update(Event::Socket { attempt, event: SocketEvent::Message { text } }, model);
    let _ = command.effects().count();
}

/// Replays each recorded session into a connected client once per thread
/// the session opened, with that thread open, and hands over the thread
/// page after every frame of the thread's stream and of the thread list's.
fn replay_fixtures(mut page: impl FnMut(String, Option<&ThreadPageView>)) {
    for name in FIXTURES {
        let chunks = recorded_chunks(name);
        let mut threads: Vec<&str> = chunks.iter().filter(|(tag, ..)| tag == "orchestration.subscribeThread").map(|(_, thread, _)| thread.as_str()).collect();
        threads.sort_unstable();
        threads.dedup();
        for thread in threads {
            let mut model = Model::default();
            model.connection = Connection::Connected;
            model.selected_thread = Some(thread.to_owned());
            model.route = "thread".into();
            let (config, _) = model.rpc.request("subscribeServerConfig", &json!({}), Route::Config);
            let (shell, _) = model.rpc.request("orchestration.subscribeShell", &json!({}), Route::Shell);
            let (stream, _) = model.rpc.request("orchestration.subscribeThread", &json!({ "threadId": thread }), Route::Thread(thread.to_owned()));
            let mut frame = 0;
            for (tag, of_thread, values) in &chunks {
                let request = match tag.as_str() {
                    "subscribeServerConfig" => &config,
                    "orchestration.subscribeShell" => &shell,
                    "orchestration.subscribeThread" if of_thread == thread => &stream,
                    _ => continue,
                };
                feed(&mut model, request, values);
                if tag != "subscribeServerConfig" {
                    frame += 1;
                    page(format!("fixture:{name}:{thread}#{frame}"), DomovoiApp.view(&model).thread.as_ref());
                }
            }
        }
    }
}

/// Every page of the measurement, each with its id: for every thread of the
/// export `DOMOVOI_THREADS` names (if it names one) the page as the thread opens
/// and the page once explored, then every page of the recorded sessions.
fn pages(mut page: impl FnMut(String, Option<&ThreadPageView>)) {
    if let Ok(path) = std::env::var("DOMOVOI_THREADS") {
        let text = std::fs::read_to_string(path).expect("the exported threads");
        for line in text.lines().filter(|line| !line.trim().is_empty()) {
            let row: Value = serde_json::from_str(line).expect("a JSON line");
            let id = row["threadId"].as_str().unwrap_or("?");
            let mut model = opened(&row);
            page(format!("thread:{id}"), DomovoiApp.view(&model).thread.as_ref());
            explore(&mut model, &row);
            page(format!("thread:{id}#explored"), DomovoiApp.view(&model).thread.as_ref());
        }
    }
    replay_fixtures(page);
}

/// The pages the core builds are the pages recorded. `DOMOVOI_VIEW_DIGESTS`
/// names the recording; `DOMOVOI_THREADS` the export it was made from, one line
/// per thread (see `stored_threads_agree` in `domovoi-model` for the query that
/// writes it). Without `DOMOVOI_THREADS` only the recorded sessions are compared.
///
/// ```sh
/// DOMOVOI_THREADS=threads.ndjson DOMOVOI_VIEW_DIGESTS=digests.ndjson \
///   cargo test -p domovoi_core --test thread_page_digests -- --ignored --nocapture
/// ```
#[test]
#[ignore = "measurement over a server's own database; needs DOMOVOI_VIEW_DIGESTS, and DOMOVOI_THREADS for the threads"]
fn thread_pages_match_recorded_digests() {
    use std::io::Write as _;
    let recording = std::env::var("DOMOVOI_VIEW_DIGESTS").expect("DOMOVOI_VIEW_DIGESTS names the recording");
    let recording = std::fs::read_to_string(recording).expect("the recording");
    let mut recorded: BTreeMap<String, Value> = recording
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str::<Value>(line).expect("a JSON line"))
        .map(|page| (page["id"].as_str().unwrap_or_default().to_owned(), page))
        .collect();
    let mut out = std::env::var("DOMOVOI_VIEW_DIGESTS_OUT").ok().map(|path| std::io::BufWriter::new(std::fs::File::create(path).expect("the file to write")));
    // Pages that match, by what they are of; and the ones that do not.
    let (mut same, mut rows) = (BTreeMap::<&str, usize>::new(), 0);
    let mut different: Vec<String> = vec![];
    pages(|id, page| {
        let now = digest(&id, page);
        if let Some(out) = &mut out {
            writeln!(out, "{now}").expect("written");
        }
        let of = if id.starts_with("fixture:") { "frames of the recorded sessions" } else if id.ends_with("#explored") { "threads, explored" } else { "threads" };
        match recorded.remove(&id) {
            Some(was) if was == now => {
                *same.entry(of).or_default() += 1;
                rows += page.map_or(0, |page| page.items.len());
            }
            Some(was) => different.push(format!("{id}\n        {}", difference(&was, page))),
            None => different.push(format!("{id}\n        not in the recording")),
        }
    });
    // What the recording holds and was not built: threads without their export.
    let threads_given = std::env::var("DOMOVOI_THREADS").is_ok();
    let unbuilt: Vec<&String> = recorded.keys().filter(|id| threads_given || id.starts_with("fixture:")).collect();
    for (of, count) in &same {
        println!("{count} {of} match the recording");
    }
    println!("{rows} rows in the pages that match");
    if !threads_given {
        println!("{} recorded pages of threads not compared: DOMOVOI_THREADS names no export", recorded.len() - unbuilt.len());
    }
    println!("{} pages differ from the recording, {} recorded pages were not built", different.len(), unbuilt.len());
    for page in different.iter().take(12) {
        println!("    {page}");
    }
    for id in unbuilt.iter().take(12) {
        println!("    not built: {id}");
    }
    assert!(different.is_empty() && unbuilt.is_empty(), "{} pages differ from the recording, {} were not built", different.len(), unbuilt.len());
}
