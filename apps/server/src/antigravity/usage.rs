//! Antigravity's token usage from its local session logs (t3code
//! `Drivers/AntigravityUsage.ts`, `antigravityUsageReader.ts`): every
//! conversation database (`*.db`) of the user's Antigravity apps (the CLI,
//! the IDE) and of Domovoi's instance profiles. Usage sits in protobuf
//! blobs of the `gen_metadata` and `steps` tables; the same model call can
//! appear in both, and in several databases, so records sharing an id are
//! one. A database is read again only when it (or its WAL) changed.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// One model call's usage.
#[derive(Debug, Clone, PartialEq)]
pub struct Record {
    pub session: String,
    pub ts_ms: i64,
    pub model: String,
    pub uncached: u64,
    pub cached: u64,
    pub creation: u64,
    pub output: u64,
    pub reasoning: u64,
}

// ---------------------------------------------------------------------------
// Protobuf, as much as the usage records need
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum Field<'a> {
    Number(u64),
    Bytes(&'a [u8]),
}

type Fields<'a> = HashMap<u64, Vec<Field<'a>>>;

fn fields(bytes: &[u8]) -> Option<Fields<'_>> {
    let mut out: Fields = HashMap::new();
    let mut at = 0usize;
    let varint = |at: &mut usize| -> Option<u64> {
        let mut value = 0u64;
        for shift in (0..70).step_by(7) {
            let byte = *bytes.get(*at)?;
            *at += 1;
            if shift == 63 && byte > 1 {
                return None;
            }
            value |= u64::from(byte & 0x7f) << shift;
            if byte < 0x80 {
                return Some(value);
            }
        }
        None
    };
    while at < bytes.len() {
        let tag = varint(&mut at)?;
        let (number, wire) = (tag / 8, tag % 8);
        if number == 0 {
            return None;
        }
        let value = match wire {
            0 => Field::Number(varint(&mut at)?),
            1 | 5 | 2 => {
                let len = if wire == 2 { usize::try_from(varint(&mut at)?).ok()? } else if wire == 1 { 8 } else { 4 };
                let end = at.checked_add(len).filter(|e| *e <= bytes.len())?;
                let v = &bytes[at..end];
                at = end;
                if wire != 2 {
                    continue;
                }
                Field::Bytes(v)
            }
            _ => return None,
        };
        out.entry(number).or_default().push(value);
    }
    Some(out)
}

fn number(f: &Fields, key: u64) -> u64 {
    match f.get(&key).and_then(|v| v.first()) {
        Some(Field::Number(n)) => *n,
        _ => 0,
    }
}

fn bytes<'a>(f: &Fields<'a>, key: u64) -> Option<&'a [u8]> {
    match f.get(&key).and_then(|v| v.first()) {
        Some(Field::Bytes(b)) => Some(b),
        _ => None,
    }
}

fn nested<'a>(f: &Fields<'a>, key: u64) -> Option<Fields<'a>> {
    match bytes(f, key) {
        Some(b) => fields(b),
        None => Some(HashMap::new()),
    }
}

fn text(f: &Fields, key: u64) -> Option<String> {
    match bytes(f, key) {
        Some(b) => std::str::from_utf8(b).ok().map(|s| s.trim().to_owned()),
        None => Some(String::new()),
    }
}

fn timestamp(f: &Fields) -> Option<i64> {
    let seconds = number(f, 1) as i64;
    (seconds > 0).then(|| seconds * 1000 + (number(f, 2) / 1_000_000) as i64)
}

/// An id the agent's model names are normalized to (`Claude 4.6 Sonnet` → `claude-sonnet-4-6`).
fn model_name(name: &str) -> String {
    if name.is_empty() {
        return String::new();
    }
    let lower = name.to_lowercase();
    let suffix = regex::Regex::new(r"\s*\(([^)]*)\)\s*$").map(|r| r.replace(&lower, "-$1").into_owned()).unwrap_or(lower);
    let dashed = regex::Regex::new(r"\s+").map(|r| r.replace_all(&suffix, "-").into_owned()).unwrap_or(suffix);
    if dashed.starts_with("claude-") {
        let reordered = regex::Regex::new(r"^claude-(\d+(?:\.\d+)?)-(sonnet|opus|haiku)").map(|r| r.replace(&dashed, "claude-$2-$1").into_owned()).unwrap_or(dashed);
        return reordered.replace('.', "-");
    }
    dashed
}

fn concrete(model: &str) -> bool {
    let named = regex::Regex::new(r"^(?:gemini-\d|claude-(?:sonnet|opus|haiku)-\d|gpt-)").is_ok_and(|r| r.is_match(model));
    let routing = regex::Regex::new(r"^gemini-3-flash-(?:agent(?:-[ab])?|[ab])$").is_ok_and(|r| r.is_match(model));
    named && !routing
}

/// Models of records that carry only an id (t3code's offline table; names
/// seen in the logs win).
fn known_model(id: u64) -> Option<&'static str> {
    Some(match id {
        246 => "gemini-2.5-pro",
        312 => "gemini-2.5-flash",
        313 | 329 => "gemini-2.5-flash-thinking",
        330 => "gemini-2.5-flash-lite",
        281 | 282 => "claude-sonnet-4",
        290 | 291 => "claude-opus-4",
        333 | 334 => "claude-sonnet-4-5",
        340 | 341 => "claude-haiku-4-5",
        342 => "gpt-oss-120b-medium",
        1026 => "claude-opus-4-6",
        1035 => "claude-sonnet-4-6",
        1016 | 1036 | 1037 => "gemini-3.1-pro",
        1018 | 1084 | 1047 => "gemini-3-flash-preview",
        1020 => "gemini-3.5-flash-medium",
        1021 => "gemini-3.1-flash-image",
        1050 => "gemini-3.1-flash-lite",
        1071 => "gemini-3.6-flash-high",
        1072 => "gemini-3.6-flash-medium",
        1073 => "gemini-3.6-flash-low",
        1132 | 1133 => "gemini-3.5-flash-high",
        1187 => "gemini-3.5-flash-extra-low",
        1196 | 1266 => "gemini-3.6-flash",
        1298 => "gemini-3.7-flash-high",
        1299 => "gemini-3.7-flash-medium",
        1300 => "gemini-3.7-flash-low",
        1301 => "gemini-3.7-flash-tiered",
        1318 => "gemini-3.8-flash-high",
        1319 => "gemini-3.8-flash-medium",
        1320 => "gemini-3.8-flash-low",
        1322 => "gemini-3.8-flash-tiered",
        1402 => "claude-opus-5-5",
        1417..=1419 => "gemini-4-argon",
        _ => return None,
    })
}

/// A record of one model call, with how sure its model and time are.
#[derive(Debug, Clone)]
struct Candidate {
    record: Record,
    model_id: u64,
    model_quality: u8,
    time_quality: u8,
    keys: Vec<String>,
}

struct Metadata {
    model: String,
    model_id: u64,
    ts_ms: Option<i64>,
    /// Each usage message's fields, owned.
    usages: Vec<Vec<u8>>,
}

/// A generation's (`gen_metadata.data`) or step's (`steps.metadata`) metadata.
fn metadata(blob: &[u8], step: bool) -> Option<Metadata> {
    let root = fields(blob)?;
    if !step && bytes(&root, 1).is_none() {
        return None;
    }
    let data = if step { root } else { nested(&root, 1)? };
    let model = if step { nested(&data, 24)? } else { data.clone() };
    let primary = bytes(&data, if step { 9 } else { 4 });
    let mut usages: Vec<Vec<u8>> = primary.map(|u| vec![u.to_vec()]).unwrap_or_default();
    for retry in data.get(&(if step { 28 } else { 17 })).into_iter().flatten() {
        let Field::Bytes(r) = retry else { return None };
        if let Some(u) = bytes(&fields(r)?, 2) {
            usages.push(u.to_vec());
        }
    }
    let names: Vec<String> = [text(&model, if step { 12 } else { 19 })?, text(&model, if step { 8 } else { 21 })?].iter().map(|n| model_name(n)).collect();
    let model_name = names.iter().find(|n| concrete(n)).or_else(|| names.iter().find(|n| !n.is_empty() && !n.starts_with("model_") && !n.starts_with("model-"))).cloned().unwrap_or_default();
    let primary_id = primary.and_then(fields).map(|u| number(&u, 1)).unwrap_or(0);
    let model_id = match number(&model, if step { 1 } else { 3 }) {
        0 => primary_id,
        id => id,
    };
    let ts_ms = if step { nested(&data, 8).as_ref().and_then(timestamp).or_else(|| nested(&data, 1).as_ref().and_then(timestamp)) } else { nested(&data, 9).and_then(|d| nested(&d, 4)).as_ref().and_then(timestamp) };
    Some(Metadata { model: model_name, model_id, ts_ms, usages })
}

/// What one database holds: its records, and the model each id was seen as.
#[derive(Clone, Default)]
struct Database {
    candidates: Vec<Candidate>,
    models: HashMap<u64, Option<String>>,
}

fn remember(models: &mut HashMap<u64, Option<String>>, id: u64, model: Option<String>) {
    match models.get(&id) {
        None => {
            models.insert(id, model);
        }
        Some(previous) if *previous != model => {
            // Two names for one id: neither is trusted.
            models.insert(id, None);
        }
        _ => {}
    }
}

fn read_database(path: &Path, session: &str, fallback_ms: i64) -> Option<Database> {
    let db = rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX).ok()?;
    let _ = db.busy_timeout(std::time::Duration::from_millis(100));
    let tables: HashSet<String> = {
        let mut stmt = db.prepare("SELECT name FROM sqlite_master WHERE type = 'table'").ok()?;
        stmt.query_map([], |r| r.get::<_, String>(0)).ok()?.flatten().collect()
    };
    if !tables.contains("gen_metadata") && !tables.contains("steps") {
        return None;
    }
    let read = |query: &str, step: bool| -> Option<Vec<(i64, Metadata)>> {
        let mut stmt = db.prepare(query).ok()?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, Option<i64>>(0)?, r.get::<_, Option<Vec<u8>>>(1)?))).ok()?;
        let mut out = vec![];
        for row in rows {
            let (idx, data) = row.ok()?;
            let Some(data) = data else {
                if step {
                    continue;
                }
                return None;
            };
            out.push((idx?, metadata(&data, step)?));
        }
        out.sort_by_key(|(idx, _)| *idx);
        Some(out)
    };
    let generations = if tables.contains("gen_metadata") { read("SELECT idx, data FROM gen_metadata ORDER BY rowid", false)? } else { vec![] };
    let steps = if tables.contains("steps") { read("SELECT idx, metadata FROM steps ORDER BY rowid", true)? } else { vec![] };
    let trajectory_ms = if tables.contains("trajectory_metadata_blob") {
        let mut stmt = db.prepare("SELECT data FROM trajectory_metadata_blob").ok()?;
        let blobs: Vec<Vec<u8>> = stmt.query_map([], |r| r.get::<_, Vec<u8>>(0)).ok()?.flatten().collect();
        blobs.iter().find_map(|b| fields(b).and_then(|f| nested(&f, 2)).as_ref().and_then(timestamp))
    } else {
        None
    };
    let mut out = Database::default();
    for (_, m) in generations.iter().chain(steps.iter()) {
        if m.model_id > 0 && concrete(&m.model) {
            remember(&mut out.models, m.model_id, Some(m.model.clone()));
        }
    }
    let by_idx: HashMap<i64, &Metadata> = generations.iter().map(|(i, m)| (*i, m)).collect();
    for (source, entries) in [("step", &steps), ("generation", &generations)] {
        for (index, (idx, entry)) in entries.iter().enumerate() {
            for (usage_index, usage) in entry.usages.iter().enumerate() {
                let Some(u) = fields(usage) else { continue };
                let model_id = match number(&u, 1) {
                    0 => entry.model_id,
                    id => id,
                };
                let generation = (source == "step").then(|| by_idx.get(idx).copied()).flatten();
                // A retry can use another model than its generation or step.
                let matches = |m: &Metadata| model_id == 0 || m.model_id == model_id;
                let model = if matches(entry) && !entry.model.is_empty() {
                    entry.model.clone()
                } else {
                    generation.filter(|g| matches(g)).map(|g| g.model.clone()).unwrap_or_default()
                };
                let output = number(&u, 3).max(number(&u, 9) + number(&u, 10));
                let (uncached, cached, creation) = (number(&u, 2), number(&u, 5), number(&u, 4));
                if uncached + cached + creation + output == 0 {
                    continue;
                }
                let keys: Vec<String> = [11u64, 12, 7].iter().filter_map(|k| text(&u, *k).filter(|t| !t.is_empty()).map(|id| format!("antigravity:{k}:{id}"))).collect();
                let keys = if keys.is_empty() { vec![format!("antigravity:{session}:{source}:{index}:{usage_index}")] } else { keys };
                out.candidates.push(Candidate {
                    record: Record {
                        session: session.to_owned(),
                        ts_ms: entry.ts_ms.or(trajectory_ms).unwrap_or(fallback_ms),
                        model: model.clone(),
                        uncached,
                        cached,
                        creation,
                        output,
                        reasoning: output.min(number(&u, 9)),
                    },
                    model_id,
                    model_quality: if concrete(&model) { 4 } else if model.is_empty() { 0 } else { 1 },
                    time_quality: if entry.ts_ms.is_some() { 2 } else if trajectory_ms.is_some() { 1 } else { 0 },
                    keys,
                });
            }
        }
    }
    Some(out)
}

/// A database's identity for the cache: its and its WAL's size and times.
fn fingerprint(path: &Path) -> Option<String> {
    let part = |p: &Path| -> Option<String> {
        let m = std::fs::metadata(p).ok()?;
        let t = |t: std::io::Result<std::time::SystemTime>| t.ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos());
        #[cfg(unix)]
        let ctime = {
            use std::os::unix::fs::MetadataExt;
            m.ctime_nsec() as u128 + m.ctime() as u128 * 1_000_000_000
        };
        #[cfg(not(unix))]
        let ctime = 0u128;
        Some(format!("{}:{}:{ctime}", m.len(), t(m.modified())))
    };
    let wal = path.with_extension("db-wal");
    Some(format!("{}/{}", part(path)?, part(&wal).unwrap_or_else(|| "-".into())))
}

static CACHE: Mutex<BTreeMap<PathBuf, (String, Arc<Database>)>> = Mutex::new(BTreeMap::new());

/// The folders holding conversation databases: the Antigravity apps'
/// (`ANTIGRAVITY_DATA_DIR`, comma-separated, or the usual ones under `home`)
/// and each instance profile's, each as its `conversations` folder when it has one.
pub fn roots(home: &Path, profiles: &[PathBuf]) -> Vec<PathBuf> {
    let configured: Vec<PathBuf> = std::env::var("ANTIGRAVITY_DATA_DIR")
        .ok()
        .map(|v| v.split(',').map(str::trim).filter(|s| !s.is_empty()).map(|s| s.strip_prefix("~/").map_or_else(|| PathBuf::from(s), |r| home.join(r))).collect())
        .unwrap_or_default();
    let defaults = ["antigravity", "antigravity-cli", "antigravity-ide", "antigravity-backup"].iter().map(|n| home.join(".gemini").join(n)).chain([home.join(".config/antigravity")]).collect();
    let mut seen = vec![];
    for root in (if configured.is_empty() { defaults } else { configured }).into_iter().chain(profiles.iter().map(|p| p.join("antigravity-acp"))) {
        let root = std::fs::canonicalize(&root).unwrap_or(root);
        let nested = root.join("conversations");
        let dir = if nested.is_dir() { nested } else { root };
        let dir = std::fs::canonicalize(&dir).unwrap_or(dir);
        if !seen.contains(&dir) {
            seen.push(dir);
        }
    }
    seen
}

/// The databases under `dir`, sorted (links are not followed).
fn databases(dir: &Path, out: &mut Vec<PathBuf>, errors: &mut bool) {
    let Ok(read) = std::fs::read_dir(dir) else {
        if dir.exists() {
            *errors = true;
        }
        return;
    };
    let mut entries: Vec<_> = read.flatten().collect();
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for e in entries {
        let Ok(t) = e.file_type() else { continue };
        let p = e.path();
        if t.is_dir() {
            databases(&p, out, errors);
        } else if t.is_file() && p.extension().is_some_and(|x| x == "db") {
            out.push(p);
        }
    }
}

/// How a scan of one folder went.
pub struct Scanned {
    pub dir: PathBuf,
    pub exists: bool,
    pub files: u64,
    pub failed: bool,
}

/// Every model call recorded from `since_ms` on, across `roots`, each once.
pub fn read(roots: &[PathBuf], since_ms: i64) -> (Vec<Record>, Vec<Scanned>) {
    let mut scanned = vec![];
    let mut visited: HashSet<PathBuf> = HashSet::new();
    let mut loaded: Vec<Arc<Database>> = vec![];
    for root in roots {
        let mut files = vec![];
        let mut failed = false;
        databases(root, &mut files, &mut failed);
        let mut count = 0;
        for path in files {
            let canonical = std::fs::canonicalize(&path).unwrap_or(path.clone());
            if !visited.insert(canonical.clone()) {
                continue;
            }
            let Some(print) = fingerprint(&path) else {
                failed = true;
                continue;
            };
            let cached = crate::util::locked(&CACHE).get(&canonical).filter(|(f, _)| *f == print).map(|(_, d)| d.clone());
            let db = match cached {
                Some(d) => d,
                None => {
                    let session = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                    let mtime = std::fs::metadata(&path).ok().and_then(|m| m.modified().ok()).and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_millis() as i64);
                    match read_database(&path, &session, mtime) {
                        Some(d) => {
                            let d = Arc::new(d);
                            crate::util::locked(&CACHE).insert(canonical.clone(), (print, d.clone()));
                            d
                        }
                        None => {
                            failed = true;
                            continue;
                        }
                    }
                }
            };
            count += 1;
            loaded.push(db);
        }
        scanned.push(Scanned { dir: root.clone(), exists: root.exists(), files: count, failed });
    }
    crate::util::locked(&CACHE).retain(|p, _| visited.contains(p));
    // The model each id was seen as, across every database.
    let mut models: HashMap<u64, Option<String>> = HashMap::new();
    for db in &loaded {
        for (id, m) in &db.models {
            remember(&mut models, *id, m.clone());
        }
    }
    // Records sharing a key are one call: union them, keeping the most of
    // each count, the surest model and the surest (earliest) time.
    let mut groups: Vec<(usize, Candidate)> = vec![];
    let mut by_key: HashMap<String, usize> = HashMap::new();
    fn find(groups: &mut [(usize, Candidate)], mut i: usize) -> usize {
        while groups[i].0 != i {
            let parent = groups[i].0;
            groups[i].0 = groups[parent].0;
            i = parent;
        }
        i
    }
    for db in &loaded {
        for c in &db.candidates {
            let mut c = c.clone();
            let learned = models.get(&c.model_id).cloned();
            let fallback = match &learned {
                Some(None) => None,
                _ => known_model(c.model_id),
            };
            let resolved = if c.model_quality == 4 {
                c.record.model.clone()
            } else if let Some(Some(m)) = &learned {
                m.clone()
            } else if let Some(f) = fallback {
                f.to_owned()
            } else if !c.record.model.is_empty() {
                c.record.model.clone()
            } else if c.model_id > 0 {
                format!("antigravity-model-{}", c.model_id)
            } else {
                "antigravity-unknown".to_owned()
            };
            c.model_quality = if c.model_quality == 4 { 4 } else if matches!(learned, Some(Some(_))) { 3 } else if fallback.is_some() { 2 } else { c.model_quality };
            c.record.model = resolved;
            let index = groups.len();
            groups.push((index, c.clone()));
            for key in &c.keys {
                if let Some(&other) = by_key.get(key) {
                    let (a, b) = (find(&mut groups, index), find(&mut groups, other));
                    if a != b {
                        // The earlier record is the group's.
                        let (keep, gone) = if a < b { (a, b) } else { (b, a) };
                        let source = groups[gone].1.clone();
                        let target = &mut groups[keep].1;
                        if source.model_quality > target.model_quality {
                            target.record.model = source.record.model.clone();
                            target.model_quality = source.model_quality;
                        }
                        if source.time_quality > target.time_quality || (source.time_quality == target.time_quality && source.record.ts_ms < target.record.ts_ms) {
                            target.record.ts_ms = source.record.ts_ms;
                            target.time_quality = source.time_quality;
                        }
                        let (t, s) = (&mut target.record, &source.record);
                        t.uncached = t.uncached.max(s.uncached);
                        t.cached = t.cached.max(s.cached);
                        t.creation = t.creation.max(s.creation);
                        t.output = t.output.max(s.output);
                        t.reasoning = t.reasoning.max(s.reasoning);
                        groups[gone].0 = keep;
                    }
                }
                by_key.insert(key.clone(), index);
            }
        }
    }
    let records = (0..groups.len()).filter(|&i| groups[i].0 == i).map(|i| groups[i].1.record.clone()).filter(|r| r.ts_ms >= since_ms).collect();
    (records, scanned)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn varint(mut v: u64, out: &mut Vec<u8>) {
        loop {
            let b = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                out.push(b);
                return;
            }
            out.push(b | 0x80);
        }
    }

    fn num(field: u64, v: u64) -> Vec<u8> {
        let mut out = vec![];
        varint(field << 3, &mut out);
        varint(v, &mut out);
        out
    }

    fn msg(field: u64, body: &[u8]) -> Vec<u8> {
        let mut out = vec![];
        varint((field << 3) | 2, &mut out);
        varint(body.len() as u64, &mut out);
        out.extend_from_slice(body);
        out
    }

    /// A usage message: model id, uncached, output, cached, a response id.
    fn usage(model: u64, uncached: u64, output: u64, cached: u64, id: &str) -> Vec<u8> {
        [num(1, model), num(2, uncached), num(3, output), num(5, cached), msg(11, id.as_bytes())].concat()
    }

    #[test]
    fn calls_are_read_once_across_tables_and_databases() {
        let dir = std::env::temp_dir().join(format!("domovoi-ag-usage-{}", crate::util::new_uuid()));
        let conversations = dir.join("conversations");
        std::fs::create_dir_all(&conversations).unwrap();
        let ts = msg(8, &[num(1, 1_800_000_000), num(2, 5_000_000)].concat());
        // A step: its usage (field 9), its model (24: id 1, name 12), its time (8).
        let step = [msg(9, &usage(1318, 100, 20, 50, "resp-1")), msg(24, &[num(1, 1318), msg(12, b"Gemini 3.8 Flash (High)")].concat()), ts.clone()].concat();
        // The same call in gen_metadata (data in field 1, usage in 4), and another call with only an id.
        let generation = msg(1, &msg(4, &usage(1318, 100, 20, 50, "resp-1")));
        let other = msg(1, &msg(4, &usage(1402, 7, 3, 0, "resp-2")));
        for name in ["a.db", "b.db"] {
            let db = rusqlite::Connection::open(conversations.join(name)).unwrap();
            db.execute_batch("CREATE TABLE steps (idx INTEGER, metadata BLOB); CREATE TABLE gen_metadata (idx INTEGER, data BLOB);").unwrap();
            db.execute("INSERT INTO steps VALUES (1, ?1)", [&step]).unwrap();
            db.execute("INSERT INTO gen_metadata VALUES (1, ?1)", [&generation]).unwrap();
            db.execute("INSERT INTO gen_metadata VALUES (2, ?1)", [&other]).unwrap();
        }
        let (mut records, scanned) = read(&[conversations.clone()], 0);
        records.sort_by(|a, b| a.model.cmp(&b.model));
        assert_eq!(scanned[0].files, 2);
        assert_eq!(records.len(), 2, "each call once: {records:?}");
        assert_eq!((records[0].model.as_str(), records[0].uncached, records[0].output), ("claude-opus-5-5", 7, 3), "named by the offline table");
        let flash = &records[1];
        assert_eq!((flash.model.as_str(), flash.uncached, flash.cached, flash.output, flash.ts_ms), ("gemini-3.8-flash-high", 100, 50, 20, 1_800_000_000_005));
        assert!(read(&[conversations], 1_900_000_000_000).0.iter().all(|r| r.ts_ms >= 1_900_000_000_000), "before the window: left out");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn model_names_are_normalized() {
        assert_eq!(model_name("Gemini 3.8 Flash (High)"), "gemini-3.8-flash-high");
        assert_eq!(model_name("Claude 4.6 Sonnet"), "claude-sonnet-4-6");
        assert!(concrete("gemini-4-argon") && !concrete("gemini-3-flash-agent") && !concrete("model_placeholder"));
    }
}
