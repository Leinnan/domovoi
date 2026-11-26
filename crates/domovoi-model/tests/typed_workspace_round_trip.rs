//! Everything the recorded sessions hold of projects, the shell stream and a
//! project's workspace (files, terminals, assets) must decode into its typed
//! record and encode back to exactly the JSON it came from, and so must one
//! sample, written from what the Domovoi server sends, of each shape no recording
//! holds.

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Debug;
use domovoi_model::typed::{
    ArchivedShellSnapshot, AssetCreateUrlResult, DispatchCommandResult, FilesystemBrowseResult, FindInThreadResult, OlderSettledThreadsPage,
    PersistChatAttachmentsResult, Project, ProjectCreateFromThreadResult, ProjectCreateNewResult, ProjectEnsureScratchResult,
    ProjectEntriesResult, ProjectIcon, ProjectReadFileResult, ProjectShell, ReviewDiffPreviewResult, SearchThreadsResult, ShellHttpAnswer,
    ShellSnapshot, ShellStreamItem, ShellThreadLocation, TerminalAttachEvent, TerminalMetadataEvent, TerminalSessionSnapshot,
    TerminalSessionStatus, ThreadLaunchResult, ThreadTurnDiff,
};

const FIXTURES: [&str; 1] = ["session1"];

/// What the recorded sessions are known to contain. Checking none of one of
/// these means the walk over the fixtures broke, not that the types are right.
const KINDS_IN_FIXTURES: [&str; 11] = [
    "orchestration.subscribeShell synchronized",
    "orchestration.subscribeShell snapshot",
    "orchestration.subscribeShell project.updated",
    "orchestration.subscribeShell thread.updated",
    "shell snapshot",
    "project shell",
    "subscribeTerminalMetadata snapshot",
    "filesystem.browse",
    "projects.readFile",
    "projects.mutate",
    "orchestration.launchThread",
];

fn fixture(name: &str) -> Vec<Value> {
    let path = format!("{}/../../fixtures/native/{name}/wire.ndjson", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap().lines().map(|line| serde_json::from_str(line).unwrap()).collect()
}

/// What the server sent in the fixtures.
#[derive(Default)]
struct Recorded {
    /// Every stream item, with the RPC it answered.
    streamed: Vec<(String, Value)>,
    /// Every successful result, with the RPC it answered.
    results: Vec<(String, Value)>,
    /// The body of every answer to `GET /api/orchestration/shell`.
    shell_bodies: Vec<Value>,
}

fn recorded() -> Recorded {
    let mut recorded = Recorded::default();
    for name in FIXTURES {
        let mut requests: BTreeMap<(i64, String), String> = BTreeMap::new();
        for line in fixture(name) {
            if line["kind"] == "http" {
                if line["path"].as_str().is_some_and(|path| path.starts_with("/api/orchestration/shell")) && line["status"] == 200 {
                    recorded.shell_bodies.push(line["res"].clone());
                }
                continue;
            }
            let frame = &line["frame"];
            if line["kind"] != "ws" || !frame.is_object() {
                continue;
            }
            let connection = line["conn"].as_i64().unwrap_or(0);
            if frame["_tag"] == "Request" {
                requests.insert((connection, frame["id"].to_string()), frame["tag"].as_str().unwrap_or_default().to_owned());
                continue;
            }
            let Some(rpc) = requests.get(&(connection, frame["requestId"].to_string())) else { continue };
            if frame["_tag"] == "Chunk" {
                recorded.streamed.extend(frame["values"].as_array().unwrap().iter().map(|value| (rpc.clone(), value.clone())));
            } else if frame["_tag"] == "Exit" && frame["exit"]["_tag"] == "Success" {
                recorded.results.push((rpc.clone(), frame["exit"]["value"].clone()));
            }
        }
    }
    recorded
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
fn round_trip<T: DeserializeOwned + Serialize + Debug>(record: &Value) -> Result<T, String> {
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
    Ok(typed)
}

/// Counts of what was checked by kind, and each distinct way a record failed.
#[derive(Default)]
struct Tally {
    checked: BTreeMap<String, usize>,
    failures: BTreeMap<String, usize>,
}

impl Tally {
    fn check<T: DeserializeOwned + Serialize + Debug>(&mut self, kind: &str, record: &Value) -> Option<T> {
        *self.checked.entry(kind.to_owned()).or_default() += 1;
        match round_trip::<T>(record) {
            Ok(typed) => Some(typed),
            Err(why) => {
                *self.failures.entry(format!("{kind} {why}")).or_default() += 1;
                None
            }
        }
    }

    fn fail(&mut self, kind: &str, why: &str) {
        *self.failures.entry(format!("{kind} {why}")).or_default() += 1;
    }

    fn count(&self, kind: &str) -> usize {
        self.checked.get(kind).copied().unwrap_or(0)
    }

    fn report(&self, title: &str) {
        println!("{title}:");
        for (kind, count) in &self.checked {
            println!("  {kind:<48} {count}");
        }
        let failures: Vec<String> = self.failures.iter().map(|(failure, count)| format!("{failure}\n    ({count} times)")).collect();
        assert!(failures.is_empty(), "payloads that do not round-trip:\n{}", failures.join("\n"));
    }
}

/// One shell stream item through its type: whole, taken apart, and the
/// snapshot and projects it carries on their own.
fn check_shell_item(tally: &mut Tally, item: &Value) {
    let kind = item["kind"].as_str().unwrap_or("<no kind>");
    let Some(typed) = tally.check::<ShellStreamItem>(&format!("orchestration.subscribeShell {kind}"), item) else { return };
    if ShellStreamItem::from_value(item.clone()).ok().as_ref() != Some(&typed) {
        tally.fail(kind, "taking parsed JSON apart gives a different item than reading it");
    }
    let sequence = item["sequence"].as_u64().or_else(|| item["snapshot"]["snapshotSequence"].as_u64());
    if typed.sequence() != sequence {
        tally.fail(kind, "has another sequence than the JSON says");
    }
    if kind == "snapshot" {
        tally.check::<ShellSnapshot>("shell snapshot", &item["snapshot"]);
        for project in item["snapshot"]["projects"].as_array().into_iter().flatten() {
            tally.check::<ProjectShell>("project shell", project);
        }
    }
    if kind == "project.updated" {
        tally.check::<ProjectShell>("project shell", &item["project"]);
    }
}

fn check_terminal_metadata(tally: &mut Tally, event: &Value) {
    let event_type = event["type"].as_str().unwrap_or("<no type>");
    let Some(typed) = tally.check::<TerminalMetadataEvent>(&format!("subscribeTerminalMetadata {event_type}"), event) else { return };
    if TerminalMetadataEvent::from_value(event.clone()).ok().as_ref() != Some(&typed) {
        tally.fail(event_type, "taking parsed JSON apart gives a different event than reading it");
    }
}

fn check_terminal_event(tally: &mut Tally, event: &Value) {
    let event_type = event["type"].as_str().unwrap_or("<no type>");
    let Some(typed) = tally.check::<TerminalAttachEvent>(&format!("terminal.attach {event_type}"), event) else { return };
    if TerminalAttachEvent::from_value(event.clone()).ok().as_ref() != Some(&typed) {
        tally.fail(event_type, "taking parsed JSON apart gives a different event than reading it");
    }
    let sequence = event["sequence"].as_u64().or_else(|| event["snapshot"]["sequence"].as_u64());
    if typed.sequence() != sequence {
        tally.fail(event_type, "has another sequence than the JSON says");
    }
}

/// The body of one answer to `GET /api/orchestration/shell`.
fn check_shell_body(tally: &mut Tally, body: &Value) {
    // The recorder kept a compressed body as the text its bytes do not spell: nothing to decode.
    if body.is_string() {
        *tally.checked.entry("GET /api/orchestration/shell (unreadable: recorded compressed)".into()).or_default() += 1;
        return;
    }
    let kind = if body.get("resumable").is_some() { "resumable" } else { "snapshot" };
    if let Some(answer) = tally.check::<ShellHttpAnswer>(&format!("GET /api/orchestration/shell {kind}"), body)
        && matches!(answer, ShellHttpAnswer::Resumable) != (kind == "resumable")
    {
        tally.fail(kind, "decodes as the other answer");
    }
}

#[test]
fn recorded_payloads_round_trip_through_their_types() {
    let recorded = recorded();
    let mut tally = Tally::default();
    for (rpc, item) in &recorded.streamed {
        match rpc.as_str() {
            "orchestration.subscribeShell" => check_shell_item(&mut tally, item),
            "subscribeTerminalMetadata" => check_terminal_metadata(&mut tally, item),
            "terminal.attach" | "terminal.observe" => check_terminal_event(&mut tally, item),
            _ => {}
        }
    }
    for (rpc, result) in &recorded.results {
        match rpc.as_str() {
            "filesystem.browse" => {
                tally.check::<FilesystemBrowseResult>(rpc, result);
            }
            "projects.readFile" => {
                tally.check::<ProjectReadFileResult>(rpc, result);
            }
            "projects.mutate" => {
                tally.check::<Project>(rpc, result);
            }
            "assets.createUrl" => {
                tally.check::<AssetCreateUrlResult>(rpc, result);
            }
            "orchestration.launchThread" => {
                tally.check::<ThreadLaunchResult>(rpc, result);
            }
            "orchestration.dispatchCommand" => {
                tally.check::<DispatchCommandResult>(rpc, result);
            }
            "orchestration.getOlderSettledThreads" => {
                tally.check::<OlderSettledThreadsPage>(rpc, result);
            }
            "terminal.open" => {
                tally.check::<TerminalSessionSnapshot>(rpc, result);
            }
            _ => {}
        }
    }
    for body in &recorded.shell_bodies {
        check_shell_body(&mut tally, body);
    }

    tally.report("recorded payloads checked, by kind");
    for kind in KINDS_IN_FIXTURES {
        assert!(tally.count(kind) > 0, "the fixtures contain `{kind}`, but none was checked");
    }
}

fn sample(name: &str) -> Value {
    let path = format!("{}/tests/samples/{name}.json", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{path}: {error}"))).unwrap()
}

/// The items of a sample that holds a stream.
fn sample_items(name: &str) -> Vec<Value> {
    match sample(name) {
        Value::Array(items) => items,
        _ => panic!("the sample `{name}` is an array of stream items"),
    }
}

#[test]
fn samples_round_trip_through_their_types() {
    let mut tally = Tally::default();
    for item in sample_items("orchestration.subscribeShell") {
        check_shell_item(&mut tally, &item);
    }
    for event in sample_items("subscribeTerminalMetadata") {
        check_terminal_metadata(&mut tally, &event);
    }
    for event in sample_items("terminal.attach") {
        check_terminal_event(&mut tally, &event);
    }
    check_shell_body(&mut tally, &sample("orchestration.shell.http"));
    tally.check::<ArchivedShellSnapshot>("orchestration.getArchivedShellSnapshot", &sample("orchestration.getArchivedShellSnapshot"));
    tally.check::<SearchThreadsResult>("orchestration.searchThreads", &sample("orchestration.searchThreads"));
    tally.check::<FindInThreadResult>("orchestration.searchThread", &sample("orchestration.searchThread"));
    tally.check::<ThreadTurnDiff>("orchestration.getTurnDiff", &sample("orchestration.getTurnDiff"));
    tally.check::<ThreadTurnDiff>("orchestration.getFullThreadDiff", &sample("orchestration.getFullThreadDiff"));
    tally.check::<ReviewDiffPreviewResult>("review.getDiffPreview", &sample("review.getDiffPreview"));
    tally.check::<ProjectEntriesResult>("projects.searchEntries", &sample("projects.searchEntries"));
    tally.check::<ProjectEntriesResult>("projects.listEntries", &sample("projects.listEntries"));
    tally.check::<PersistChatAttachmentsResult>("assets.persistChatAttachments", &sample("assets.persistChatAttachments"));
    tally.check::<ProjectCreateNewResult>("projects.createNew", &sample("projects.createNew"));
    tally.check::<ProjectEnsureScratchResult>("projects.ensureScratch", &sample("projects.ensureScratch"));
    tally.check::<ProjectCreateFromThreadResult>("projects.createFromThread", &sample("projects.createFromThread"));
    tally.report("sample payloads checked, by kind");

    // The stream samples hold every kind their schema has.
    for kind in ["synchronized", "snapshot-required", "snapshot", "project.updated", "project.removed", "thread.updated", "thread.removed"] {
        assert!(tally.count(&format!("orchestration.subscribeShell {kind}")) > 0, "the shell stream sample has no `{kind}` item");
    }
    for event_type in ["snapshot", "upsert", "remove"] {
        assert!(tally.count(&format!("subscribeTerminalMetadata {event_type}")) > 0, "the terminal metadata sample has no `{event_type}` event");
    }
    for event_type in ["snapshot", "output", "exited", "closed", "error", "cleared", "restarted", "activity"] {
        assert!(tally.count(&format!("terminal.attach {event_type}")) > 0, "the terminal sample has no `{event_type}` event");
    }
    assert_eq!(tally.count("GET /api/orchestration/shell snapshot"), 1);
}

/// The shell stream sample says what it looks like to: which thread moved
/// where, which project went, and where in the log each item is.
#[test]
fn shell_stream_items_read_as_what_they_are() {
    let items: Vec<ShellStreamItem> =
        sample_items("orchestration.subscribeShell").into_iter().map(|item| ShellStreamItem::from_value(item).unwrap()).collect();
    assert_eq!(
        items.iter().map(ShellStreamItem::sequence).collect::<Vec<_>>(),
        [Some(292), Some(292), None, Some(293), Some(294), Some(295), Some(296), Some(297), None]
    );
    let ShellStreamItem::Snapshot { snapshot, resolved_repository_identity_roots: None } = &items[0] else { panic!("{:?}", items[0]) };
    assert_eq!((snapshot.threads.len(), snapshot.archived_threads.len(), snapshot.projects.len()), (4, 1, 3));
    assert!(matches!(snapshot.projects[0].project_icon, Some(Some(ProjectIcon::Monogram(_)))));
    assert!(matches!(&snapshot.projects[2].project_icon, Some(Some(ProjectIcon::Lucide(icon))) if icon.name == "message-square-dashed"));
    assert_eq!(snapshot.projects[0].scripts[0].preview_url.as_deref(), Some("http://localhost:8000"));
    assert!(matches!(&items[1], ShellStreamItem::Snapshot { resolved_repository_identity_roots: Some(roots), .. } if roots.len() == 3));
    assert!(matches!(&items[5], ShellStreamItem::ThreadUpdated { location: ShellThreadLocation::Archive, thread, .. } if thread.archived_at.is_some()));
    assert!(matches!(&items[7], ShellStreamItem::ProjectRemoved { project_id, .. } if project_id.as_str() == "0b6b2c0e-6c0c-4a59-9a39-3f2f8c9a7d11"));
    assert_eq!(items[8], ShellStreamItem::SnapshotRequired);

    let ShellHttpAnswer::Snapshot(http) = serde_json::from_value(sample("orchestration.shell.http")).unwrap() else { panic!("a snapshot") };
    let (from_http, identities) = http.into_parts();
    assert_eq!((&from_http, identities), (&**snapshot, None));
}

/// A stream item, an icon and an enum value from a newer server survive a
/// round trip whole, and say what they can about themselves.
#[test]
fn unknown_members_survive_a_round_trip() {
    let item = json!({ "kind": "project.reordered", "sequence": 41, "projectIds": ["p2", "p1"] });
    let typed = ShellStreamItem::from_value(item.clone()).unwrap();
    assert!(matches!(&typed, ShellStreamItem::Other(unknown) if unknown.tag() == "project.reordered" && unknown.record()["projectIds"][0] == "p2"));
    assert_eq!(typed.sequence(), Some(41));
    assert_eq!(serde_json::to_string(&typed).unwrap(), item.to_string(), "kept as sent, in the order sent");
    assert_eq!(serde_json::from_value::<ShellStreamItem>(item).unwrap(), typed);

    let project = json!({
        "id": "p1", "title": "Orbit", "workspaceRoot": "/work/orbit", "defaultModelSelection": null, "scripts": [],
        "projectIcon": { "kind": "hologram", "beam": [1, 2] }, "defaultThreadEnvMode": "container",
        "createdAt": "2026-01-01T00:00:00.000Z", "updatedAt": "2026-01-01T00:00:00.000Z",
    });
    let typed: ProjectShell = serde_json::from_value(project.clone()).unwrap();
    assert!(matches!(&typed.project_icon, Some(Some(ProjectIcon::Other(icon))) if icon.tag() == "hologram"));
    assert_eq!(typed.default_thread_env_mode.as_ref().and_then(Option::as_ref).map(|mode| mode.as_str()), Some("container"));
    assert_eq!(fallbacks(&typed).len(), 2, "the unknown icon and the unknown environment mode");
    assert_eq!(serde_json::to_value(&typed).unwrap(), project);

    let moved = json!({ "kind": "thread.removed", "sequence": 9, "location": "trash", "threadId": "t1" });
    let typed = ShellStreamItem::from_value(moved.clone()).unwrap();
    assert!(matches!(&typed, ShellStreamItem::ThreadRemoved { location, .. } if location.as_str() == "trash"));
    assert_eq!(serde_json::to_value(&typed).unwrap(), moved);

    let event = json!({ "type": "renamed", "threadId": "t1", "terminalId": "term-1", "label": "build" });
    let typed = TerminalMetadataEvent::from_value(event.clone()).unwrap();
    assert!(matches!(&typed, TerminalMetadataEvent::Other(unknown) if unknown.tag() == "renamed"));
    assert_eq!(serde_json::to_value(&typed).unwrap(), event);

    let event = json!({ "type": "bell", "threadId": "t1", "terminalId": "term-1", "sequence": 12 });
    let typed = TerminalAttachEvent::from_value(event.clone()).unwrap();
    assert!(matches!(&typed, TerminalAttachEvent::Other(unknown) if unknown.tag() == "bell"));
    assert_eq!(typed.sequence(), Some(12));
    assert_eq!(serde_json::to_value(&typed).unwrap(), event);

    let mut snapshot = sample_items("terminal.attach").remove(0);
    snapshot["snapshot"]["status"] = "suspended".into();
    let typed = TerminalAttachEvent::from_value(snapshot.clone()).unwrap();
    assert!(matches!(&typed, TerminalAttachEvent::Snapshot { snapshot } if snapshot.status == TerminalSessionStatus::Other("suspended".into())));
    assert_eq!(serde_json::to_value(&typed).unwrap(), snapshot);
}

/// A record of a known kind that lacks a field its schema requires is an
/// error that names the field, never a fallback and never a default.
#[test]
fn a_missing_required_field_is_an_error() {
    fn error<T: DeserializeOwned + Debug>(record: Value) -> String {
        serde_json::from_value::<T>(record).unwrap_err().to_string()
    }
    /// `record` without the field at `path`.
    fn without(mut record: Value, path: &[&str]) -> Value {
        let (last, parents) = path.split_last().unwrap();
        let mut at = &mut record;
        for key in parents {
            at = &mut at[*key];
        }
        assert!(at.as_object_mut().unwrap().remove(*last).is_some(), "the record has no `{}`", path.join("."));
        record
    }

    let items = sample_items("orchestration.subscribeShell");
    let (thread_updated, project_updated, thread_removed) = (&items[3], &items[4], &items[6]);
    for (item, path, named) in [
        (thread_updated, &["sequence"][..], "sequence"),
        (thread_updated, &["location"], "location"),
        (thread_updated, &["thread", "status"], "status"),
        (project_updated, &["project", "workspaceRoot"], "workspaceRoot"),
        (project_updated, &["project", "scripts"], "scripts"),
        (thread_removed, &["threadId"], "threadId"),
        (&items[0], &["snapshot", "archivedThreads"], "archivedThreads"),
        (&items[0], &["snapshot"], "snapshot"),
    ] {
        let broken = without(item.clone(), path);
        let taken_apart = ShellStreamItem::from_value(broken.clone()).unwrap_err().to_string();
        assert!(taken_apart.contains(named), "{taken_apart}");
        assert!(error::<ShellStreamItem>(broken).contains(named));
    }
    assert!(error::<ShellStreamItem>(json!({ "sequence": 1 })).contains("kind"));
    assert!(error::<ShellStreamItem>(json!({ "kind": 7 })).contains("kind"));

    // A lucide icon is known: without its colour it does not become an unknown icon.
    let mut project = project_updated["project"].clone();
    project["projectIcon"] = json!({ "kind": "lucide", "name": "rocket" });
    assert!(error::<ProjectShell>(project).contains("color"));

    assert!(error::<ShellHttpAnswer>(without(sample("orchestration.shell.http"), &["projects"])).contains("projects"));
    // Only `true` says the stream can resume: anything else has to be a snapshot.
    assert!(error::<ShellHttpAnswer>(json!({ "resumable": false })).contains("schemaVersion"));
    assert_eq!(serde_json::from_value::<ShellHttpAnswer>(json!({ "resumable": true })).unwrap(), ShellHttpAnswer::Resumable);

    assert!(error::<Project>(without(recorded_result("projects.mutate"), &["createdAt"])).contains("createdAt"));
    assert!(error::<ProjectReadFileResult>(without(recorded_result("projects.readFile"), &["byteLength"])).contains("byteLength"));
    assert!(error::<FilesystemBrowseResult>(without(recorded_result("filesystem.browse"), &["parentPath"])).contains("parentPath"));
    assert!(error::<AssetCreateUrlResult>(without(recorded_result("assets.createUrl"), &["expiresAt"])).contains("expiresAt"));
    assert!(error::<SearchThreadsResult>(without(sample("orchestration.searchThreads"), &["matches"])).contains("matches"));
    assert!(error::<FindInThreadResult>(without(sample("orchestration.searchThread"), &["totalMatches"])).contains("totalMatches"));
    assert!(error::<ThreadTurnDiff>(without(sample("orchestration.getTurnDiff"), &["diff"])).contains("diff"));
    assert!(error::<ProjectEntriesResult>(without(sample("projects.listEntries"), &["truncated"])).contains("truncated"));

    let events = sample_items("terminal.attach");
    for (event, path, named) in [
        (&events[0], &["snapshot", "history"][..], "history"),
        (&events[2], &["data"], "data"),
        (&events[2], &["terminalId"], "terminalId"),
        (&events[5], &["exitSignal"], "exitSignal"),
        (&events[7], &["label"], "label"),
        (&events[8], &["message"], "message"),
    ] {
        let broken = without(event.clone(), path);
        assert!(TerminalAttachEvent::from_value(broken.clone()).unwrap_err().to_string().contains(named));
        assert!(error::<TerminalAttachEvent>(broken).contains(named));
    }
    let metadata = sample_items("subscribeTerminalMetadata");
    assert!(error::<TerminalMetadataEvent>(without(metadata[1].clone(), &["terminal", "hasRunningSubprocess"])).contains("hasRunningSubprocess"));
    assert!(error::<TerminalMetadataEvent>(without(metadata[4].clone(), &["threadId"])).contains("threadId"));
}

/// The first recorded successful result of `rpc`.
fn recorded_result(rpc: &str) -> Value {
    recorded().results.into_iter().find(|(tag, _)| tag == rpc).unwrap_or_else(|| panic!("the fixtures hold no `{rpc}` result")).1
}

/// Absent and `null` are different on the wire and stay different.
#[test]
fn absent_and_null_stay_distinct() {
    let bare = json!({
        "id": "p1", "title": "Orbit", "workspaceRoot": "/work/orbit", "defaultModelSelection": null, "scripts": [],
        "createdAt": "2026-01-01T00:00:00.000Z", "updatedAt": "2026-01-01T00:00:00.000Z",
    });
    let absent: ProjectShell = serde_json::from_value(bare.clone()).unwrap();
    assert_eq!((&absent.repository_identity, &absent.project_icon, &absent.pinned_at, absent.auto_pull), (&None, &None, &None, None));
    assert_eq!(serde_json::to_value(&absent).unwrap(), bare);

    let mut with_nulls = bare;
    for key in ["repositoryIdentity", "defaultThreadEnvMode", "faviconPath", "projectIcon", "pinnedAt", "archivedAt"] {
        with_nulls[key] = Value::Null;
    }
    let null: ProjectShell = serde_json::from_value(with_nulls.clone()).unwrap();
    assert_eq!((&null.repository_identity, &null.project_icon, &null.pinned_at), (&Some(None), &Some(None), &Some(None)));
    assert_eq!(serde_json::to_value(&null).unwrap(), with_nulls);
}

/// The Domovoi server writes `null` where it has no value for a field the schema
/// makes optional (`search_threads`' `messageId`, `repository_identity`'s
/// `rootPath`): that reads as the field left out, which is what it means.
#[test]
fn a_null_for_an_optional_field_reads_as_absent() {
    let mut result = sample("orchestration.searchThreads");
    result["matches"][0]["messageId"] = Value::Null;
    let typed: SearchThreadsResult = serde_json::from_value(result.clone()).unwrap();
    assert_eq!(typed.matches[0].message_id, None);
    result["matches"][0].as_object_mut().unwrap().remove("messageId");
    assert_eq!(serde_json::to_value(&typed).unwrap(), result);

    let mut project = sample("orchestration.getArchivedShellSnapshot")["projects"][1].clone();
    project["repositoryIdentity"]["rootPath"] = Value::Null;
    let typed: ProjectShell = serde_json::from_value(project).unwrap();
    assert!(matches!(&typed.repository_identity, Some(Some(identity)) if identity.root_path.is_none() && identity.owner.as_deref() == Some("pingdotgg")));
}

/// Domovoi's own Git LFS fields: absent from t3code's answers, kept when
/// sent, and a state this build does not know is kept as it was sent.
#[test]
fn read_file_lfs_fields_round_trip() {
    use domovoi_model::typed::{LfsObjectState, ProjectFetchLfsObjectResult, ProjectFileLfs};
    let plain: ProjectReadFileResult = serde_json::from_value(recorded_result("projects.readFile").clone()).unwrap();
    assert_eq!(plain.lfs, None);
    assert!(serde_json::to_value(&plain).unwrap().get("lfs").is_none());
    let sent = json!({
        "relativePath": "a.png", "contents": "", "byteLength": 5, "truncated": false, "exists": true,
        "lfs": { "oid": "ab", "size": 5, "state": "too-large" },
    });
    let read: ProjectReadFileResult = serde_json::from_value(sent.clone()).unwrap();
    assert_eq!(read.lfs, Some(ProjectFileLfs { oid: "ab".into(), size: 5, state: LfsObjectState::TooLarge }));
    assert_eq!(serde_json::to_value(&read).unwrap(), sent);
    let later: ProjectFetchLfsObjectResult = serde_json::from_value(json!({ "relativePath": "a.png", "state": "evicted" })).unwrap();
    assert_eq!(later.state, LfsObjectState::Other("evicted".into()));
}
