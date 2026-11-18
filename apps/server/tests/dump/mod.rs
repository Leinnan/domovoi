//! Writes what a test's server recorded, so two builds of the server can be
//! compared: with `DOMOVOI_TURNS_DUMP_DIR` set, every server a test started leaves
//! `<dir>/<name>.json` when it is dropped.
//!
//! What differs from run to run is replaced: generated ids, times of this
//! run, the test's folder and port, tokens, generated branch names, what a
//! process measures of itself.
//!
//! The file says the same thing twice:
//!
//! - `log`: per thread, in the order the threads first appeared, every event
//!   in the log (type and payload) in the order it was written;
//! - `records`: per thread, every record with the versions of it the log
//!   holds, in order, the records themselves in the order of their ids;
//!   and `end`, what the store holds at the end.
//!
//! Two servers that did the same work write the same `records` and `end`.
//! They write the same `log` as well unless two things happened at once (a
//! title generated while a turn starts) or a turn closed several open items,
//! which it does in no fixed order: then only the order of events of
//! different records differs.
//!
//! One thing is left out on purpose. While a reply streams, the node of the
//! item that grows is written again with every flush, unchanged, and how many
//! flushes there are depends on timing. A `node.updated` that repeats the one
//! before it for the same node, once normalised, is dropped.

#![allow(dead_code, reason = "each test binary uses the part it needs")]

use serde_json::{Map, Value, json};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;

struct Normalizer {
    uuid: regex::Regex,
    time: regex::Regex,
    /// What is generated or measured anew by every server: each pattern with what stands for it.
    generated: Vec<(regex::Regex, &'static str)>,
    epoch_ms: regex::Regex,
    folder_hash: regex::Regex,
    folders: Vec<String>,
    uuids: HashMap<String, usize>,
    /// Whether each generated id gets a number of its own (`<uuid#3>`), in the order met.
    numbered: bool,
    started_ms: i64,
}

fn millis(time: &str) -> Option<i64> {
    time.parse::<jiff::Timestamp>().ok().map(|t| t.as_millisecond())
}

fn now_ms() -> i64 {
    jiff::Timestamp::now().as_millisecond()
}

const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// What a process measures of itself or its machine: never the same twice.
fn measured(key: &str) -> bool {
    matches!(key, "pid" | "ppid" | "childPids" | "serverPid" | "cpuTimeMs" | "cpuUtilization" | "collectionDurationMicros" | "scannedProcessCount" | "runTimeMs" | "processes" | "processCount" | "processStarts" | "retainedProcessCount")
        || (key.ends_with("Bytes") && key != "sizeBytes")
}

impl Normalizer {
    fn new(folders: Vec<String>, started_ms: i64, numbered: bool) -> Self {
        let pattern = |text: &str| regex::Regex::new(text).expect("regex");
        Normalizer {
            uuid: pattern("[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}"),
            time: pattern(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(\.\d+)?Z"),
            generated: vec![
                (pattern("domovoi_mcp_[0-9a-f]{32}"), "domovoi_mcp_<token>"),
                (pattern(r"127\.0\.0\.1:\d+"), "127.0.0.1:<port>"),
                (pattern(r"--port \d+"), "--port <port>"),
                // A generated branch name.
                (pattern("t3([/-])[0-9a-f]{8}"), "t3$1<generated>"),
                (pattern(r"t3_session_\d+_[0-9a-f]+"), "t3_session_<generated>"),
                (pattern("/api/attachments/upload/[A-Za-z0-9_.-]+"), "/api/attachments/upload/<signed>"),
                (pattern("/api/hooks/task-webhook/[A-Za-z0-9_-]+"), "/api/hooks/task-webhook/<secret>"),
                // A checkpoint's ref is named after its scope, a hash of the thread's id.
                (pattern("refs/t3/orchestration-v2/checkpoints/[A-Za-z0-9_-]+/"), "refs/t3/orchestration-v2/checkpoints/<scope>/"),
                (pattern(r"\b[0-9a-f]{40}\b"), "<sha>"),
                (pattern(r"\bCommit [0-9a-f]{7}\b"), "Commit <sha>"),
                (pattern(r"\b[0-9a-f]{32}\b"), "<hex32>"),
            ],
            epoch_ms: pattern("1[0-9]{12}"),
            folder_hash: pattern("-[0-9a-f]{16}"),
            folders,
            uuids: HashMap::new(),
            numbered,
            started_ms,
        }
    }

    /// What stands for the time `ms`: one of this run, one near it (an expiry,
    /// a next run, a bucket of recent history), or `None` for a time the test gave.
    fn time_of(&self, ms: i64) -> Option<&'static str> {
        if ms >= self.started_ms && ms <= now_ms() + 5_000 {
            Some("<now>")
        } else if ms >= self.started_ms - 2 * DAY_MS && ms <= now_ms() + 400 * DAY_MS {
            Some("<about now>")
        } else {
            None
        }
    }

    fn text(&mut self, text: &str) -> String {
        let mut text = text.to_owned();
        for folder in &self.folders {
            text = text.replace(folder.as_str(), "<dir>");
        }
        let mut out = String::new();
        let mut rest = 0;
        for found in self.uuid.find_iter(&text) {
            out.push_str(&text[rest..found.start()]);
            if self.numbered {
                let next = self.uuids.len() + 1;
                let n = *self.uuids.entry(found.as_str().to_owned()).or_insert(next);
                out.push_str(&format!("<uuid#{n}>"));
            } else {
                out.push_str("<uuid>");
            }
            rest = found.end();
        }
        out.push_str(&text[rest..]);
        let out = self.time.replace_all(&out, |found: &regex::Captures| millis(&found[0]).and_then(|ms| self.time_of(ms)).map_or_else(|| found[0].to_owned(), str::to_owned));
        let out = self.epoch_ms.replace_all(&out, |found: &regex::Captures| {
            found[0].parse::<i64>().ok().and_then(|ms| self.time_of(ms)).map_or_else(|| found[0].to_owned(), str::to_owned)
        });
        let mut out = out.into_owned();
        for (pattern, stands_for) in &self.generated {
            out = pattern.replace_all(&out, *stands_for).into_owned();
        }
        // The worktree folder named after a branch.
        if out.contains("/worktrees/") { self.folder_hash.replace_all(&out, "-<hash>").into_owned() } else { out }
    }

    fn value(&mut self, value: &Value) -> Value {
        match value {
            Value::String(text) => Value::String(self.text(text)),
            Value::Number(n) => match n.as_i64().and_then(|ms| self.time_of(ms)) {
                Some(stands_for) => json!(stands_for),
                None => value.clone(),
            },
            Value::Array(items) => Value::Array(items.iter().map(|item| self.value(item)).collect()),
            // Keys in the order written: a thread's events are compared as JSON values, whatever the order.
            Value::Object(fields) => Value::Object(
                fields
                    .iter()
                    .map(|(key, value)| {
                        let mut kept = if measured(key) && !value.is_object() && !value.is_string() { json!("<measured>") } else { self.value(value) };
                        // Scheduled tasks are listed by when they run next: by the clock.
                        if let ("tasks", Value::Array(tasks)) = (key.as_str(), &mut kept) {
                            tasks.sort_by_key(|task| task["id"].as_str().map(str::to_owned));
                        }
                        // Buckets of recent history begin before the test and end in it.
                        if key == "buckets" {
                            kept = serde_json::from_str(&kept.to_string().replace("\"<now>\"", "\"<about now>\"")).expect("json");
                        }
                        (self.text(key), kept)
                    })
                    .collect(),
            ),
            other => other.clone(),
        }
    }
}

fn rows(db: &rusqlite::Connection, sql: &str, columns: usize) -> Vec<Vec<Value>> {
    let mut st = db.prepare(sql).expect("query");
    let rows = st
        .query_map([], |r| {
            (0..columns)
                .map(|i| {
                    let text: Option<String> = r.get(i)?;
                    Ok(text.map_or(Value::Null, |text| serde_json::from_str(&text).unwrap_or(Value::String(text))))
                })
                .collect::<rusqlite::Result<Vec<Value>>>()
        })
        .expect("rows");
    rows.collect::<rusqlite::Result<_>>().expect("rows")
}

fn last_sequence(db: &rusqlite::Connection) -> i64 {
    db.query_row("SELECT COALESCE(MAX(sequence), 0) FROM events", [], |r| r.get(0)).unwrap_or(0)
}

/// Everything the server whose home is under `dir` has recorded, normalised.
/// Waits first until it has written nothing for a moment.
pub fn recorded(dir: &Path, started_ms: i64) -> Value {
    let path = dir.join("home").join("userdata").join("domovoi.sqlite");
    let db = rusqlite::Connection::open_with_flags(&path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).expect("the server's database opens");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut seen = last_sequence(&db);
    let mut quiet_since = std::time::Instant::now();
    while quiet_since.elapsed() < std::time::Duration::from_millis(700) && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(50));
        let now = last_sequence(&db);
        if now != seen {
            seen = now;
            quiet_since = std::time::Instant::now();
        }
    }

    let folders = folders_of(dir);
    let events = rows(&db, "SELECT aggregate_kind, aggregate_id, type, payload FROM events ORDER BY sequence", 4);
    // Grouped by what they are about, in the order each first appeared.
    let mut order: Vec<String> = vec![];
    let mut by_aggregate: HashMap<String, Vec<(String, Value)>> = HashMap::new();
    for row in events {
        let aggregate = format!("{}:{}", row[0].as_str().unwrap_or("?"), row[1].as_str().map(str::to_owned).unwrap_or_else(|| row[1].to_string()));
        if !by_aggregate.contains_key(&aggregate) {
            order.push(aggregate.clone());
        }
        let event_type = row[2].as_str().unwrap_or("?").to_owned();
        // A thread's event is stored whole: its record is the `payload` inside.
        let event = match &row[3] {
            Value::Object(event) if event.contains_key("payload") && event.contains_key("occurredAt") => {
                let mut kept = Map::new();
                for key in ["driver", "providerInstanceId", "runId", "nodeId", "payload"] {
                    if let Some(value) = event.get(key) {
                        kept.insert(key.to_owned(), value.clone());
                    }
                }
                Value::Object(kept)
            }
            other => other.clone(),
        };
        by_aggregate.entry(aggregate).or_default().push((event_type, event));
    }

    let mut in_order = Normalizer::new(folders.clone(), started_ms, true);
    let mut by_id = Normalizer::new(folders.clone(), started_ms, false);
    let mut log = vec![];
    let mut records: BTreeMap<String, BTreeMap<String, Vec<Value>>> = BTreeMap::new();
    for aggregate in order {
        let mut written = vec![];
        let mut last_of_node: HashMap<String, Value> = HashMap::new();
        let of_aggregate = records.entry(by_id.text(&aggregate)).or_default();
        for (event_type, event) in by_aggregate.remove(&aggregate).unwrap_or_default() {
            let plain = by_id.value(&event);
            let record_id = plain["payload"]["id"].as_str().unwrap_or_default().to_owned();
            if event_type == "node.updated" {
                if last_of_node.get(&record_id) == Some(&plain) {
                    continue;
                }
                last_of_node.insert(record_id.clone(), plain.clone());
            }
            written.push(json!({ "type": event_type, "event": in_order.value(&event) }));
            let kind = event_type.rsplit_once('.').map_or(event_type.as_str(), |(kind, _)| kind);
            of_aggregate.entry(format!("{kind} {record_id}")).or_default().push(json!({ "type": event_type, "event": plain }));
        }
        log.push(json!({ "of": in_order.text(&aggregate), "events": written }));
    }

    let threads = rows(&db, "SELECT id, record FROM threads ORDER BY rowid", 2);
    let stored = rows(&db, "SELECT thread_id, kind, entity_id, record FROM thread_entities", 4);
    let projects = rows(&db, "SELECT id, shell FROM projects ORDER BY rowid", 2);
    let mut stored: Vec<Value> = stored.into_iter().map(|row| by_id.value(&json!(row))).collect();
    stored.sort_by_key(|row| (row[0].to_string(), row[1].to_string(), row[2].to_string()));
    let end = json!({ "threads": by_id.value(&json!(threads)), "records": stored, "projects": by_id.value(&json!(projects)) });
    json!({ "log": log, "records": records, "end": end })
}

/// The test's folder as it may be spelled, longest first.
fn folders_of(dir: &Path) -> Vec<String> {
    let folder = dir.to_string_lossy().into_owned();
    let canonical = dir.canonicalize().map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| folder.clone());
    let mut folders = vec![canonical, folder];
    folders.sort_by_key(|f| std::cmp::Reverse(f.len()));
    folders
}

/// With `DOMOVOI_TURNS_DUMP_DIR` set, writes what the server under `dir` recorded to `<that dir>/<name>.json`.
pub fn write(dir: &Path, name: &str, started_ms: i64) {
    let Ok(out) = std::env::var("DOMOVOI_TURNS_DUMP_DIR") else { return };
    if !dir.join("home").join("userdata").join("domovoi.sqlite").exists() {
        return;
    }
    let _ = std::fs::create_dir_all(&out);
    let dump = recorded(dir, started_ms);
    let path = Path::new(&out).join(format!("{name}.json"));
    std::fs::write(&path, serde_json::to_string_pretty(&dump).expect("json")).expect("the dump is written");
}

pub fn started() -> i64 {
    now_ms() - 5_000
}

/// With `DOMOVOI_TURNS_DUMP_DIR` set, writes `value` (what a test was answered),
/// normalised like a server's records, to `<that dir>/<name>.json`.
pub fn write_value(dir: &Path, name: &str, value: &Value, started_ms: i64) {
    let Ok(out) = std::env::var("DOMOVOI_TURNS_DUMP_DIR") else { return };
    let _ = std::fs::create_dir_all(&out);
    let normalised = Normalizer::new(folders_of(dir), started_ms, true).value(value);
    std::fs::write(Path::new(&out).join(format!("{name}.json")), serde_json::to_string_pretty(&normalised).expect("json")).expect("the dump is written");
}
