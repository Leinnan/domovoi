//! What clients are sent for the threads of a stored server home, as digests:
//! for every thread its shell as the shell snapshot sends it, the snapshot a
//! subscription to it starts with, and the events a resume from sequence 0
//! replays.
//!
//! A digest maps a path (`shell.title`, `snapshot.projection.runs[run:1]`,
//! `resume[42]`) to a hash of the JSON sent there, with object keys sorted:
//! two servers send the same JSON values exactly when their digests are equal.

use crate::db::Db;
use crate::orchestration::stream::Sent;
use crate::store::Store;
use serde_json::Value;
use sha2::Digest as _;
use std::collections::BTreeMap;

/// Path to hash, for one thread.
type Digest = BTreeMap<String, String>;

/// `value` as JSON text with every object's keys in order.
fn canonical(value: &Value, out: &mut String) {
    match value {
        Value::Object(fields) => {
            let mut keys: Vec<&String> = fields.keys().collect();
            keys.sort();
            out.push('{');
            for (i, key) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&Value::String(key.clone()).to_string());
                out.push(':');
                canonical(&fields[key], out);
            }
            out.push('}');
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                canonical(item, out);
            }
            out.push(']');
        }
        other => out.push_str(&other.to_string()),
    }
}

fn hash_text(text: &str) -> String {
    hex::encode(&sha2::Sha256::digest(text.as_bytes())[..8])
}

/// The hash of `value` whatever order its keys are in.
fn hash_of(value: &Value) -> String {
    let mut text = String::new();
    canonical(value, &mut text);
    hash_text(&text)
}

/// The hash of `value` as it is written, key order included.
fn hash_as_written(value: &Value) -> String {
    hash_text(&value.to_string())
}

/// Each field of `object` under `prefix`.
fn fields(digest: &mut Digest, prefix: &str, object: &Value) {
    match object.as_object() {
        Some(map) => {
            for (key, value) in map {
                digest.insert(format!("{prefix}.{key}"), hash_of(value));
            }
        }
        None => {
            digest.insert(prefix.to_owned(), hash_of(object));
        }
    }
}

/// A thread's shell and the list of the snapshot it is in (`active`, `archive`).
fn digest_shell(digest: &mut Digest, location: &str, shell: &Value) {
    digest.insert("shell.(location)".into(), location.to_owned());
    digest.insert("shell.(bytes)".into(), hash_as_written(shell));
    fields(digest, "shell", shell);
}

/// The first item of a subscription to a thread: its snapshot, or the error sent instead.
fn digest_snapshot(digest: &mut Digest, item: Result<&Value, &Value>) {
    let item = match item {
        Ok(item) => item,
        Err(error) => {
            digest.insert("snapshot.(error)".into(), hash_of(error));
            return;
        }
    };
    digest.insert("snapshot.(bytes)".into(), hash_as_written(item));
    for (key, value) in item.as_object().into_iter().flatten() {
        if key != "projection" {
            digest.insert(format!("snapshot.{key}"), hash_of(value));
        }
    }
    for (key, value) in item["projection"].as_object().into_iter().flatten() {
        let path = format!("snapshot.projection.{key}");
        match value {
            Value::Array(records) => {
                let id_of = |record: &Value| record["id"].as_str().or(record["sourceItemId"].as_str()).unwrap_or("?").to_owned();
                let order: Vec<String> = records.iter().map(id_of).collect();
                digest.insert(format!("{path}.(order)"), hash_text(&order.join("\n")));
                for record in records {
                    digest.insert(format!("{path}[{}]", id_of(record)), hash_of(record));
                }
            }
            Value::Object(_) => fields(digest, &path, value),
            other => {
                digest.insert(path, hash_of(other));
            }
        }
    }
}

/// The items a subscription resumed from sequence 0 replays.
fn digest_resume(digest: &mut Digest, items: &[Value]) {
    let mut written = String::new();
    let mut sequences = String::new();
    for item in items {
        written.push_str(&item.to_string());
        sequences.push_str(&format!("{},", item["sequence"]));
        digest.insert(format!("resume[{}]", item["sequence"]), hash_of(item));
    }
    digest.insert("resume.(bytes)".into(), hash_text(&written));
    digest.insert("resume.(sequences)".into(), hash_text(&sequences));
}

/// Every thread the database knows: those with a record, and those with events only.
fn stored_thread_ids(store: &Store) -> Vec<String> {
    store
        .db()
        .read(|c| {
            let mut st = c.prepare(
                "SELECT id FROM threads UNION SELECT DISTINCT aggregate_id FROM events WHERE aggregate_kind = 'thread' ORDER BY 1",
            )?;
            let rows = st.query_map([], |r| r.get::<_, String>(0))?;
            rows.collect()
        })
        .expect("thread ids")
}

fn open_home() -> Store {
    let home = std::env::var("DOMOVOI_SERVER_HOME").expect("DOMOVOI_SERVER_HOME names a copy of a server home");
    Store::new(Db::open(&std::path::Path::new(&home).join("userdata").join("domovoi.sqlite")).expect("the home's database opens"))
}

/// The digests of everything `store` sends for its threads.
fn digests(store: &Store) -> BTreeMap<String, Digest> {
    let mut all: BTreeMap<String, Digest> = BTreeMap::new();
    let snapshot = crate::rpc::shell::snapshot_of(store).expect("shell snapshot");
    let mut whole = Digest::new();
    whole.insert("schemaVersion".into(), snapshot.schema_version.to_string());
    whole.insert("snapshotSequence".into(), snapshot.snapshot_sequence.to_string());
    whole.insert("projects".into(), hash_of(&serde_json::to_value(&snapshot.projects).expect("projects")));
    for (location, shells) in [("active", &snapshot.threads), ("archive", &snapshot.archived_threads)] {
        let order: Vec<&str> = shells.iter().map(|shell| shell.id.as_str()).collect();
        whole.insert(format!("{location}.(order)"), hash_text(&order.join("\n")));
        for shell in shells {
            digest_shell(all.entry(shell.id.as_str().to_owned()).or_default(), location, &sent(shell));
        }
    }
    all.insert("(shell snapshot)".into(), whole);

    let through = store.latest_sequence().expect("sequence");
    for id in stored_thread_ids(store) {
        let digest = all.entry(id.clone()).or_default();
        let item = crate::orchestration::stream::snapshot_item(store, &id, false).map(|(_, item)| sent(&Sent::Item(item))).map_err(|failure| sent(&failure));
        digest_snapshot(digest, item.as_ref());
        let replayed = crate::orchestration::stream::replay(store, &id, 0, through, false).expect("events");
        let items: Vec<Value> = replayed.into_iter().map(|item| sent(&Sent::Item(item))).collect();
        digest_resume(digest, &items);
    }
    all
}

/// `item` as the RPC layer puts it on the wire: the JSON value it encodes.
fn sent<T: serde::Serialize>(item: &T) -> Value {
    serde_json::to_value(item).expect("what is sent serializes")
}

/// What the stored threads of a real server home are sent as has not
/// changed since the digests in `DOMOVOI_SERVER_DIGESTS` were taken of it.
///
/// `DOMOVOI_SERVER_HOME` names a copy of the home (its database is opened, which
/// may write to it). The comparison is of JSON values: the order of an
/// object's keys is not part of what is compared, and how many threads were
/// also sent byte for byte the same is reported apart.
#[test]
#[ignore = "needs a copy of a server home and its digests; run with DOMOVOI_SERVER_HOME and DOMOVOI_SERVER_DIGESTS"]
fn stored_threads_are_sent_as_before() {
    let store = open_home();
    let path = std::env::var("DOMOVOI_SERVER_DIGESTS").expect("DOMOVOI_SERVER_DIGESTS names the digests taken before");
    let before: BTreeMap<String, Digest> = serde_json::from_str(&std::fs::read_to_string(&path).expect("the digests are read")).expect("digests");
    let now = digests(&store);

    let byte_for_byte = |part: &str| {
        let key = format!("{part}.(bytes)");
        now.iter().filter(|(id, digest)| before.get(*id).is_some_and(|b| b.contains_key(&key) && b.get(&key) == digest.get(&key))).count()
    };
    let (mut matched, mut differed, mut shown) = (0usize, 0usize, 0usize);
    let threads: std::collections::BTreeSet<&String> = before.keys().chain(now.keys()).collect();
    for id in &threads {
        let empty = Digest::new();
        let (was, is) = (before.get(*id).unwrap_or(&empty), now.get(*id).unwrap_or(&empty));
        // Key order is not compared: the byte-for-byte hashes are counted apart.
        let paths: std::collections::BTreeSet<&String> = was.keys().chain(is.keys()).filter(|path| !path.ends_with(".(bytes)")).collect();
        let differing: Vec<&String> = paths.into_iter().filter(|path| was.get(*path) != is.get(*path)).collect();
        if differing.is_empty() {
            matched += 1;
            continue;
        }
        differed += 1;
        for path in differing.iter().take(8) {
            if shown < 60 {
                shown += 1;
                let side = |digest: &Digest| digest.get(*path).map_or("(not sent)", String::as_str).to_owned();
                println!("thread {id}: {path} differs: was {}, is {}", side(was), side(is));
            }
        }
        if differing.len() > 8 {
            println!("thread {id}: and {} more paths", differing.len() - 8);
        }
    }
    println!("{} compared ({} threads, and what the shell snapshot says besides them): {matched} matched, {differed} differed", threads.len(), threads.len() - 1);
    println!(
        "sent byte for byte as before (key order included): {} shells, {} thread snapshots, {} resumes, of {} threads",
        byte_for_byte("shell"), byte_for_byte("snapshot"), byte_for_byte("resume"), now.len() - 1,
    );
    assert_eq!(differed, 0, "{differed} threads are sent differently");
}
