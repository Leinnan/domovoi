//! Token usage from the providers' local session logs (t3code `usage/`):
//! Claude Code's `projects/**/*.jsonl` assistant `usage` records, Codex's
//! `sessions/**/rollout-*.jsonl` `token_count` events and Pi's session files
//! (assistant messages' `usage`, and `usage` entries for work outside one),
//! bucketed per day (or hour) in the requested time zone, per provider and model.
//!
//! These logs run to gigabytes, so scanning is incremental:
//! - a file last modified before the window cannot hold records in it and is
//!   not read at all;
//! - parsed files keep their records (a few numbers each) in a process-wide
//!   cache keyed by path; the logs are append-only, so a grown file is read
//!   from where the last scan stopped, and a shrunk or replaced one from the start;
//! - files that need reading are parsed in parallel, with typed decoding of
//!   only the fields used, after a substring prefilter that skips most lines.
//!
//! Limit: t3code prices tokens from a downloaded rate table; Domovoi has none, so
//! every bucket is `unpriced` with zero cost and `pricing.status` is
//! `unavailable`.

use crate::agent_sessions::{ScanEnv, Source};
use crate::rpc::shell::{if_fits, text_or_empty};
use serde::{Deserialize, Serialize};
use serde_json::Number;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;
use domovoi_model::typed::{
    UsageBucket, UsageCostSource, UsageModelFilter, UsageModelSelector, UsagePricing, UsagePricingStatus, UsageProviderKind, UsageSource,
    UsageSourceFingerprint, UsageSourceStatus, UsageSummary, UsageTokenTotals,
};

pub const CONTRACT_VERSION: u32 = 1;
const MAX_FILES: usize = 20_000;
/// Records may be written a little after their timestamp; files modified this
/// long before the window still get read.
const MTIME_SLACK_MS: i64 = 3_600_000;

/// One usage record, as few bytes as possible.
#[derive(Clone)]
struct Rec {
    ts_ms: i64,
    model: Arc<str>,
    session: Arc<str>,
    uncached: u64,
    cached: u64,
    creation: u64,
    output: u64,
    reasoning: u64,
    /// Claude streams the same message into more than one record (and resumed
    /// sessions replay history into new files): a hash of message and request
    /// id, counted once. 0 = no key.
    dedupe: u64,
}

/// What the cache knows about one log file.
struct FileEntry {
    len: u64,
    modified: SystemTime,
    /// Bytes consumed (always at a line boundary).
    offset: u64,
    records: Vec<Rec>,
    malformed: u32,
    /// Codex: the model of the latest `turn_context`, for records appended later.
    model: Arc<str>,
}

fn cache() -> &'static Mutex<HashMap<PathBuf, Arc<FileEntry>>> {
    static CACHE: OnceLock<Mutex<HashMap<PathBuf, Arc<FileEntry>>>> = OnceLock::new();
    CACHE.get_or_init(Default::default)
}

#[derive(Default, Clone)]
struct Totals {
    uncached: u64,
    cached: u64,
    creation: u64,
    output: u64,
    reasoning: u64,
    records: u64,
    sessions: HashSet<Arc<str>>,
}

struct Window {
    tz: jiff::tz::TimeZone,
    since: jiff::civil::Date,
    until: jiff::civil::Date,
    start_ms: i64,
    end_ms: i64,
    hourly: bool,
}

impl Window {
    /// The bucket key (`day`, `hourStart`) of a timestamp inside the window.
    fn key(&self, ts_ms: i64) -> Option<(String, Option<String>)> {
        if ts_ms < self.start_ms || ts_ms >= self.end_ms {
            return None;
        }
        let zoned = jiff::Timestamp::from_millisecond(ts_ms).ok()?.to_zoned(self.tz.clone());
        let day = zoned.date();
        if day < self.since || day > self.until {
            return None;
        }
        let hour = self.hourly.then(|| {
            let start = zoned.with().minute(0).second(0).subsec_nanosecond(0).build().unwrap_or(zoned.clone());
            start.timestamp().to_string()
        });
        Some((day.to_string(), hour))
    }
}

fn hash(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut h);
    h.finish().max(1)
}

fn ts_ms(s: &str) -> Option<i64> {
    s.parse::<jiff::Timestamp>().ok().map(|t| t.as_millisecond())
}

// ---- typed line decoding (only the fields used) ----

#[derive(Deserialize)]
struct ClaudeLine<'a> {
    #[serde(rename = "type", borrow)]
    ty: Option<std::borrow::Cow<'a, str>>,
    #[serde(borrow)]
    timestamp: Option<std::borrow::Cow<'a, str>>,
    #[serde(rename = "sessionId", borrow)]
    session_id: Option<std::borrow::Cow<'a, str>>,
    #[serde(rename = "requestId", borrow)]
    request_id: Option<std::borrow::Cow<'a, str>>,
    #[serde(borrow)]
    message: Option<ClaudeMessage<'a>>,
}

#[derive(Deserialize)]
struct ClaudeMessage<'a> {
    #[serde(borrow)]
    id: Option<std::borrow::Cow<'a, str>>,
    #[serde(borrow)]
    model: Option<std::borrow::Cow<'a, str>>,
    usage: Option<ClaudeUsage>,
}

#[derive(Deserialize, Default)]
struct ClaudeUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    cache_read_input_tokens: u64,
    #[serde(default)]
    cache_creation_input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
}

#[derive(Deserialize)]
struct CodexLine<'a> {
    #[serde(rename = "type", borrow)]
    ty: Option<std::borrow::Cow<'a, str>>,
    #[serde(borrow)]
    timestamp: Option<std::borrow::Cow<'a, str>>,
    #[serde(borrow)]
    payload: Option<CodexPayload<'a>>,
}

#[derive(Deserialize)]
struct CodexPayload<'a> {
    #[serde(rename = "type", borrow)]
    ty: Option<std::borrow::Cow<'a, str>>,
    #[serde(borrow)]
    model: Option<std::borrow::Cow<'a, str>>,
    info: Option<CodexInfo>,
}

#[derive(Deserialize)]
struct CodexInfo {
    last_token_usage: Option<CodexUsage>,
}

#[derive(Deserialize, Default)]
struct CodexUsage {
    #[serde(default)]
    input_tokens: u64,
    #[serde(default)]
    cached_input_tokens: u64,
    #[serde(default)]
    output_tokens: u64,
    #[serde(default)]
    reasoning_output_tokens: u64,
}

/// A pi session entry: an assistant `message` with its `usage`, or a `usage`
/// entry (cache warming and other calls outside a message).
#[derive(Deserialize)]
struct PiLine<'a> {
    #[serde(rename = "type", borrow)]
    ty: Option<std::borrow::Cow<'a, str>>,
    #[serde(borrow)]
    id: Option<std::borrow::Cow<'a, str>>,
    #[serde(borrow)]
    timestamp: Option<std::borrow::Cow<'a, str>>,
    #[serde(borrow)]
    message: Option<PiMessage<'a>>,
    usage: Option<PiUsage>,
    #[serde(borrow)]
    model: Option<std::borrow::Cow<'a, str>>,
}

#[derive(Deserialize)]
struct PiMessage<'a> {
    #[serde(borrow)]
    role: Option<std::borrow::Cow<'a, str>>,
    #[serde(borrow)]
    model: Option<std::borrow::Cow<'a, str>>,
    #[serde(rename = "responseModel", borrow)]
    response_model: Option<std::borrow::Cow<'a, str>>,
    usage: Option<PiUsage>,
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase", default)]
struct PiUsage {
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
    reasoning: u64,
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    memchr::memmem::find(hay, needle).is_some()
}

/// Lines of `bytes` (split at `\n`, the last one possibly unterminated).
fn lines(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    let mut start = 0;
    let mut ends = memchr::memchr_iter(b'\n', bytes).chain(std::iter::once(bytes.len()));
    std::iter::from_fn(move || {
        let end = ends.next()?;
        let line = &bytes[start..end];
        start = end + 1;
        Some(line)
    })
    .filter(|l| !l.is_empty())
}

/// Parses complete lines of `bytes` into `entry` (Claude or Codex rules).
fn parse_lines(source: Source, path: &Path, bytes: &[u8], entry: &mut FileEntry, interner: &mut HashMap<String, Arc<str>>) {
    let mut intern = |s: &str| -> Arc<str> {
        if let Some(a) = interner.get(s) {
            return a.clone();
        }
        let a: Arc<str> = s.into();
        interner.insert(s.to_owned(), a.clone());
        a
    };
    let stem: Arc<str> = path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default().into();
    for line in lines(bytes) {
        match source {
            Source::Claude => {
                if !contains(line, b"\"usage\"") {
                    continue;
                }
                let Ok(l) = serde_json::from_slice::<ClaudeLine>(line) else {
                    entry.malformed += 1;
                    continue;
                };
                if l.ty.as_deref() != Some("assistant") {
                    continue;
                }
                let Some(m) = l.message else { continue };
                let model = m.model.as_deref().unwrap_or("unknown");
                let Some(u) = m.usage else { continue };
                if model == "<synthetic>" {
                    continue;
                }
                let Some(ts) = l.timestamp.as_deref().and_then(ts_ms) else { continue };
                let key = format!("{}:{}", m.id.as_deref().unwrap_or_default(), l.request_id.as_deref().unwrap_or_default());
                entry.records.push(Rec {
                    ts_ms: ts,
                    model: intern(model),
                    session: intern(l.session_id.as_deref().unwrap_or_default()),
                    uncached: u.input_tokens,
                    cached: u.cache_read_input_tokens,
                    creation: u.cache_creation_input_tokens,
                    output: u.output_tokens,
                    reasoning: 0,
                    dedupe: if key == ":" { 0 } else { hash(&key) },
                });
            }
            Source::Pi => {
                if !contains(line, b"\"usage\"") {
                    continue;
                }
                let Ok(l) = serde_json::from_slice::<PiLine>(line) else {
                    entry.malformed += 1;
                    continue;
                };
                let (usage, model) = match (l.ty.as_deref(), l.message) {
                    (Some("message"), Some(m)) if m.role.as_deref() == Some("assistant") => (m.usage, m.response_model.or(m.model)),
                    (Some("usage"), _) => (l.usage, l.model),
                    _ => continue,
                };
                let Some(u) = usage else { continue };
                let Some(ts) = l.timestamp.as_deref().and_then(ts_ms) else { continue };
                let model = model.as_deref().unwrap_or("unknown").to_owned();
                // A forked session copies its history: the same entry counts once.
                let key = format!("pi:{}:{}:{model}", l.id.as_deref().unwrap_or_default(), l.timestamp.as_deref().unwrap_or_default());
                entry.records.push(Rec {
                    ts_ms: ts,
                    model: intern(&model),
                    // `<timestamp>_<session id>.jsonl`.
                    session: intern(stem.rsplit('_').next().unwrap_or(&stem)),
                    uncached: u.input,
                    cached: u.cache_read,
                    creation: u.cache_write,
                    output: u.output,
                    reasoning: u.reasoning,
                    dedupe: l.id.is_some().then(|| hash(&key)).unwrap_or(0),
                });
            }
            Source::Codex => {
                if !contains(line, b"turn_context") && !contains(line, b"token_count") {
                    continue;
                }
                let Ok(l) = serde_json::from_slice::<CodexLine>(line) else {
                    entry.malformed += 1;
                    continue;
                };
                let Some(p) = l.payload else { continue };
                if l.ty.as_deref() == Some("turn_context") {
                    if let Some(m) = p.model.as_deref() {
                        entry.model = intern(m);
                    }
                    continue;
                }
                let Some(u) = p.info.and_then(|i| i.last_token_usage) else { continue };
                if p.ty.as_deref() != Some("token_count") {
                    continue;
                }
                let Some(ts) = l.timestamp.as_deref().and_then(ts_ms) else { continue };
                entry.records.push(Rec {
                    ts_ms: ts,
                    model: entry.model.clone(),
                    session: stem.clone(),
                    // Codex counts cached input inside input_tokens.
                    uncached: u.input_tokens.saturating_sub(u.cached_input_tokens),
                    cached: u.cached_input_tokens,
                    creation: 0,
                    output: u.output_tokens,
                    reasoning: u.reasoning_output_tokens,
                    dedupe: 0,
                });
            }
        }
    }
}

/// The cache entry for `path`, read (or extended) if the file changed.
fn refresh(source: Source, path: &Path, len: u64, modified: SystemTime, old: Option<Arc<FileEntry>>) -> Arc<FileEntry> {
    if let Some(e) = &old {
        if e.len == len && e.modified == modified {
            return e.clone();
        }
    }
    let mut entry = match old {
        // Grown: continue after the last complete line.
        Some(e) if len >= e.offset => FileEntry { len, modified, offset: e.offset, records: e.records.clone(), malformed: e.malformed, model: e.model.clone() },
        _ => FileEntry { len, modified, offset: 0, records: vec![], malformed: 0, model: "unknown".into() },
    };
    let Ok(mut f) = std::fs::File::open(path) else { return Arc::new(entry) };
    if f.seek(SeekFrom::Start(entry.offset)).is_err() {
        return Arc::new(entry);
    }
    let mut bytes = Vec::with_capacity(len.saturating_sub(entry.offset) as usize);
    if f.read_to_end(&mut bytes).is_err() {
        return Arc::new(entry);
    }
    let mut complete = memchr::memrchr(b'\n', &bytes).map_or(0, |i| i + 1);
    // A last line without a newline counts once it is valid JSON; a line still
    // being written is read again next time.
    let tail = &bytes[complete..];
    if !tail.is_empty() && serde_json::from_slice::<serde::de::IgnoredAny>(tail).is_ok() {
        complete = bytes.len();
    }
    let mut interner = HashMap::new();
    parse_lines(source, path, &bytes[..complete], &mut entry, &mut interner);
    entry.offset += complete as u64;
    Arc::new(entry)
}

/// Every log file of a provider home with its size and modification time.
fn log_files(source: Source, dir: &Path) -> Option<Vec<(PathBuf, u64, SystemTime)>> {
    let root = match source {
        Source::Claude => dir.join("projects"),
        Source::Codex => dir.join("sessions"),
        // A pi home is its sessions folder.
        Source::Pi => dir.to_path_buf(),
    };
    if !root.is_dir() {
        return None;
    }
    let mut out = vec![];
    let mut stack = vec![root];
    while let Some(d) = stack.pop() {
        let Ok(read) = std::fs::read_dir(&d) else { continue };
        for e in read.flatten() {
            let Ok(t) = e.file_type() else { continue };
            if t.is_dir() {
                stack.push(e.path());
                continue;
            }
            let p = e.path();
            let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let wanted = name.ends_with(".jsonl") && (source != Source::Codex || name.starts_with("rollout-"));
            if !t.is_file() || !wanted || out.len() >= MAX_FILES {
                continue;
            }
            if let Ok(m) = e.metadata() {
                out.push((p, m.len(), m.modified().unwrap_or(SystemTime::UNIX_EPOCH)));
            }
        }
    }
    Some(out)
}

fn millis(t: SystemTime) -> i64 {
    t.duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// How the scan of one provider's logs went.
struct Scanned {
    /// `Missing` when the provider has no folder of logs.
    status: UsageSourceStatus,
    scanned_files: u64,
    /// Files last written too long before the window to hold records of it.
    skipped_files: u64,
    malformed_records: u64,
}

/// Brings the cache up to date for the files that can hold records in the window.
fn scan(source: Source, dir: &Path, w: &Window) -> (Vec<Arc<FileEntry>>, Scanned) {
    let Some(files) = log_files(source, dir) else {
        return (vec![], Scanned { status: UsageSourceStatus::Missing, scanned_files: 0, skipped_files: 0, malformed_records: 0 });
    };
    let total = files.len();
    let relevant: Vec<(PathBuf, u64, SystemTime)> = files.into_iter().filter(|(_, _, m)| millis(*m) + MTIME_SLACK_MS >= w.start_ms).collect();
    let skipped = total - relevant.len();
    let previous: Vec<Option<Arc<FileEntry>>> = {
        let c = cache().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        relevant.iter().map(|(p, _, _)| c.get(p).cloned()).collect()
    };
    // Parse what changed, spread over the cores.
    let jobs: Vec<usize> = (0..relevant.len()).filter(|&i| previous[i].as_ref().is_none_or(|e| e.len != relevant[i].1 || e.modified != relevant[i].2)).collect();
    let workers = std::thread::available_parallelism().map_or(4, |n| n.get()).min(jobs.len().max(1));
    let next = std::sync::atomic::AtomicUsize::new(0);
    let fresh: Mutex<Vec<(usize, Arc<FileEntry>)>> = Mutex::new(Vec::with_capacity(jobs.len()));
    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| loop {
                let k = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let Some(&i) = jobs.get(k) else { break };
                let (p, len, m) = &relevant[i];
                let e = refresh(source, p, *len, *m, previous[i].clone());
                crate::util::locked(&fresh).push((i, e));
            });
        }
    });
    let mut entries = previous;
    {
        let mut c = cache().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        for (i, e) in fresh.into_inner().unwrap_or_else(std::sync::PoisonError::into_inner) {
            c.insert(relevant[i].0.clone(), e.clone());
            entries[i] = Some(e);
        }
    }
    let entries: Vec<Arc<FileEntry>> = entries.into_iter().flatten().collect();
    let malformed: u32 = entries.iter().map(|e| e.malformed).sum();
    let scanned = Scanned { status: UsageSourceStatus::Ok, scanned_files: relevant.len() as u64, skipped_files: skipped as u64, malformed_records: malformed.into() };
    (entries, scanned)
}

type Buckets = BTreeMap<(String, Option<String>, &'static str, Arc<str>), Totals>;

fn aggregate(provider: &'static str, entries: &[Arc<FileEntry>], w: &Window, seen: &mut HashSet<u64>, buckets: &mut Buckets) -> usize {
    let mut sessions: HashSet<Arc<str>> = HashSet::new();
    for e in entries {
        for r in &e.records {
            // Deduplicate before the window: a message whose records straddle
            // midnight counts on the day of its first record, in every window.
            if r.dedupe != 0 && !seen.insert(r.dedupe) {
                continue;
            }
            let Some((day, hour)) = w.key(r.ts_ms) else { continue };
            sessions.insert(r.session.clone());
            let t = buckets.entry((day, hour, provider, r.model.clone())).or_default();
            t.uncached += r.uncached;
            t.cached += r.cached;
            t.creation += r.creation;
            t.output += r.output;
            t.reasoning += r.reasoning;
            t.records += 1;
            t.sessions.insert(r.session.clone());
        }
    }
    sessions.len()
}

fn matches_filter(filter: &UsageModelFilter, provider: &str, model: &str) -> bool {
    let hit = |list: &[UsageModelSelector]| list.iter().any(|e| e.model == model && e.provider.as_deref().is_none_or(|p| p == provider));
    filter.include.as_deref().is_none_or(hit) && !filter.exclude.as_deref().is_some_and(hit)
}

/// `server.refreshUsageRates`: the price table, of which there is none.
pub fn pricing() -> UsagePricing {
    UsagePricing { status: UsagePricingStatus::Unavailable, source: "none".into(), fetched_at: None, known_models: 0 }
}

/// Why `server.getUsageSummary` failed (the `reason` of `UsageReadError`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ReadFailure {
    /// The scan itself broke off.
    ScanFailed,
    /// The days or the time zone asked for name no window.
    InvalidWindow,
}

/// A failed usage call, written as its tagged error.
#[derive(Debug, thiserror::Error, Serialize)]
#[serde(tag = "_tag")]
pub enum Failure {
    #[error("Usage read failed: {detail}")]
    #[serde(rename = "UsageReadError")]
    Read { reason: ReadFailure, detail: String },
}

/// What a usage summary is asked for (`UsageSummaryInput`). A day left out
/// reads as an empty one, which is no day; a zone left out as UTC; and each of
/// them too when it is no text. The filter, when there is one, must be one.
#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SummaryInput {
    #[serde(default, deserialize_with = "if_fits")]
    time_zone: Option<String>,
    #[serde(default, deserialize_with = "text_or_empty")]
    since_day: String,
    #[serde(default, deserialize_with = "text_or_empty")]
    until_day: String,
    /// `hour` for buckets by the hour; anything else is by the day.
    #[serde(default, deserialize_with = "if_fits")]
    resolution: Option<String>,
    #[serde(default)]
    model_filter: Option<UsageModelFilter>,
}

/// Fills the cache for the last 30 days, so the first usage request is fast.
pub fn warm(env: &ScanEnv) {
    let now = domovoi_contracts::time_fmt::now_millis();
    let w = Window {
        tz: jiff::tz::TimeZone::UTC,
        since: jiff::civil::date(1970, 1, 1),
        until: jiff::civil::date(9999, 12, 31),
        start_ms: now - 31 * 86_400_000,
        end_ms: now + 86_400_000,
        hourly: false,
    };
    for (source, _, dir) in &env.homes {
        let _ = scan(*source, dir, &w);
    }
}

/// A cost of nothing, written `0.0` as t3code writes its costs.
fn no_cost() -> Number {
    Number::from_f64(0.0).expect("zero is finite")
}

/// `server.getUsageSummary`.
pub fn summary(env: &ScanEnv, input: &SummaryInput) -> Result<UsageSummary, Failure> {
    let err = |detail: String| Failure::Read { reason: ReadFailure::InvalidWindow, detail };
    let started = std::time::Instant::now();
    let tz_name = input.time_zone.as_deref().unwrap_or("UTC");
    let tz = jiff::tz::TimeZone::get(tz_name).map_err(|e| err(format!("Unknown time zone {tz_name}: {e}")))?;
    let day = |d: &str| -> Result<jiff::civil::Date, Failure> { d.parse::<jiff::civil::Date>().map_err(|_| err(format!("Invalid day {d}."))) };
    let (since, until) = (day(&input.since_day)?, day(&input.until_day)?);
    if since > until {
        return Err(err("sinceDay is after untilDay.".into()));
    }
    let start = since.to_zoned(tz.clone()).map_err(|e| err(e.to_string()))?.timestamp().as_millisecond();
    let end = until.tomorrow().and_then(|d| d.to_zoned(tz.clone())).map_err(|e| err(e.to_string()))?.timestamp().as_millisecond();
    let w = Window { tz, since, until, start_ms: start, end_ms: end, hourly: input.resolution.as_deref() == Some("hour") };
    let mut buckets: Buckets = BTreeMap::new();
    let mut seen = HashSet::new();
    let mut sources = vec![];
    for (source, _, dir) in &env.homes {
        let provider = match source {
            Source::Claude => "claude",
            Source::Codex => "codex",
            Source::Pi => "pi",
        };
        let (entries, scanned) = scan(*source, dir, &w);
        let distinct_sessions = aggregate(provider, &entries, &w, &mut seen, &mut buckets) as u64;
        sources.push(UsageSource {
            fingerprint: UsageSourceFingerprint {
                host_id: "local".into(),
                provider: UsageProviderKind::from(provider),
                resolved_home_path: dir.to_string_lossy().into_owned(),
                volume_id: "local".into(),
            },
            status: scanned.status,
            scanned_files: scanned.scanned_files,
            skipped_files: scanned.skipped_files,
            malformed_records: scanned.malformed_records,
            distinct_sessions,
            message: None,
            refreshing: None,
        });
    }
    let filter = input.model_filter.as_ref();
    let rows: Vec<UsageBucket> = buckets
        .into_iter()
        .filter(|((_, _, p, m), _)| filter.is_none_or(|filter| matches_filter(filter, p, m)))
        .map(|((day, hour, provider, model), t)| UsageBucket {
            day,
            // `null` in a summary by the day.
            hour_start: Some(hour),
            provider: UsageProviderKind::from(provider),
            model: model.to_string(),
            source_path: None,
            totals: UsageTokenTotals {
                uncached_input_tokens: t.uncached,
                cached_input_tokens: t.cached,
                cache_creation_tokens: t.creation,
                output_tokens: t.output,
                reasoning_tokens: t.reasoning,
            },
            cost_usd: no_cost(),
            cache_savings_usd: no_cost(),
            category_cost_usd: None,
            fast_cost_usd: None,
            ultrafast_cost_usd: None,
            speed_premium_usd: None,
            cost_source: UsageCostSource::Unpriced,
            records: t.records,
            unpriced_records: t.records,
            unpriced_tokens: Some(t.uncached + t.cached + t.creation + t.output + t.reasoning),
            sessions: t.sessions.len() as u64,
        })
        .collect();
    Ok(UsageSummary {
        contract_version: CONTRACT_VERSION.into(),
        read_at: domovoi_contracts::iso_now().as_str().into(),
        time_zone: tz_name.to_owned(),
        since_day: since.to_string(),
        until_day: until.to_string(),
        buckets: rows.into(),
        sources: sources.into(),
        pricing: pricing(),
        scan_duration_ms: started.elapsed().as_millis() as u64,
        model_filter: input.model_filter.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    /// The summary `asked` for, as it is written.
    fn summary(env: &ScanEnv, asked: &Value) -> Value {
        let input: SummaryInput = serde_json::from_value(asked.clone()).expect("an input");
        json!(super::summary(env, &input).expect("a summary"))
    }

    /// Why no summary was made of what was `asked` for, as it is written.
    fn refused(env: &ScanEnv, asked: &Value) -> Value {
        let input: SummaryInput = serde_json::from_value(asked.clone()).expect("an input");
        json!(super::summary(env, &input).err().expect("a failure"))
    }

    #[test]
    fn scans_claude_and_codex_logs() {
        let home = std::env::temp_dir().join(format!("domovoi-usage-{}", crate::util::new_uuid()));
        let claude = home.join(".claude/projects/p");
        let codex = home.join(".codex/sessions/2026/11/13");
        std::fs::create_dir_all(&claude).unwrap();
        std::fs::create_dir_all(&codex).unwrap();
        let rec = |id: &str, ts: &str| json!({ "type": "assistant", "timestamp": ts, "sessionId": "s1", "requestId": "r",
            "message": { "id": id, "model": "claude-haiku-5-5", "usage": { "input_tokens": 10, "cache_read_input_tokens": 5, "cache_creation_input_tokens": 2, "output_tokens": 3 } } }).to_string();
        std::fs::write(claude.join("a.jsonl"), [rec("m1", "2026-11-13T10:00:00Z"), rec("m1", "2026-11-13T10:00:01Z"), "{\"usage\": bad".into(), rec("m2", "2026-11-13T23:30:00Z")].join("\n")).unwrap();
        std::fs::write(codex.join("rollout-x.jsonl"), [
            json!({ "type": "turn_context", "timestamp": "2026-11-13T09:00:00Z", "payload": { "model": "gpt-6-luna" } }).to_string(),
            json!({ "type": "event_msg", "timestamp": "2026-11-13T09:00:05Z", "payload": { "type": "token_count", "info": { "last_token_usage": { "input_tokens": 100, "cached_input_tokens": 40, "output_tokens": 7, "reasoning_output_tokens": 2 } } } }).to_string(),
        ].join("\n")).unwrap();
        let env = ScanEnv {
            home: home.clone(),
            homes: vec![(Source::Claude, "claudeAgent".into(), home.join(".claude")), (Source::Codex, "codex".into(), home.join(".codex"))],
            base_dir: home.join("domovoi"), worktrees_dir: home.join("wt"), temp_dir: std::env::temp_dir(),
        };
        let s = summary(&env, &json!({ "sinceDay": "2026-11-13", "untilDay": "2026-11-13", "timeZone": "UTC" }));
        let b = s["buckets"].as_array().unwrap();
        let claude_row = b.iter().find(|r| r["provider"] == "claude").unwrap();
        assert_eq!(claude_row["records"], 2, "duplicate streaming records count once");
        assert_eq!(claude_row["totals"]["uncachedInputTokens"], 20);
        let codex_row = b.iter().find(|r| r["provider"] == "codex").unwrap();
        assert_eq!((codex_row["model"].as_str(), codex_row["totals"]["uncachedInputTokens"].as_i64()), (Some("gpt-6-luna"), Some(60)));
        assert_eq!(s["sources"][0]["malformedRecords"], 1);
        // In Tokyo the 23:30 UTC record falls on the next day.
        let s = summary(&env, &json!({ "sinceDay": "2026-11-14", "untilDay": "2026-11-14", "timeZone": "Asia/Tokyo" }));
        assert_eq!(s["buckets"].as_array().unwrap().len(), 1);
        assert_eq!(
            refused(&env, &json!({ "sinceDay": "2026-11-14", "untilDay": "2026-11-13", "timeZone": "UTC" })),
            json!({ "_tag": "UsageReadError", "reason": "invalidWindow", "detail": "sinceDay is after untilDay." })
        );
        assert_eq!(
            json!(Failure::Read { reason: ReadFailure::ScanFailed, detail: "task 7 panicked".into() }),
            json!({ "_tag": "UsageReadError", "reason": "scanFailed", "detail": "task 7 panicked" })
        );
        // The filter is echoed as it applies; costs are written as decimals.
        let filtered = summary(&env, &json!({ "sinceDay": "2026-11-13", "untilDay": "2026-11-13", "timeZone": "UTC", "resolution": "hour",
            "modelFilter": { "include": [{ "provider": "codex", "model": "gpt-6-luna" }], "exclude": [{ "model": "claude-haiku-5-5" }] } }));
        assert_eq!(filtered["modelFilter"], json!({ "include": [{ "provider": "codex", "model": "gpt-6-luna" }], "exclude": [{ "model": "claude-haiku-5-5" }] }));
        assert_eq!(filtered["buckets"], json!([{
            "day": "2026-11-13", "hourStart": "2026-11-13T09:00:00Z", "provider": "codex", "model": "gpt-6-luna",
            "totals": { "uncachedInputTokens": 60, "cachedInputTokens": 40, "cacheCreationTokens": 0, "outputTokens": 7, "reasoningTokens": 2 },
            "costUsd": 0.0, "cacheSavingsUsd": 0.0, "costSource": "unpriced", "records": 1, "unpricedRecords": 1, "unpricedTokens": 109, "sessions": 1,
        }]));
        assert_eq!(filtered["buckets"][0]["costUsd"].to_string(), "0.0");
        assert_eq!(json!(pricing()), json!({ "status": "unavailable", "source": "none", "fetchedAt": null, "knownModels": 0 }));

        // Appended records are picked up without rereading; a half-written line waits.
        let day = json!({ "sinceDay": "2026-11-13", "untilDay": "2026-11-13", "timeZone": "UTC" });
        let records = |s: &Value| s["buckets"].as_array().unwrap().iter().filter(|r| r["provider"] == "claude").map(|r| r["records"].as_i64().unwrap()).sum::<i64>();
        let file = claude.join("a.jsonl");
        let mut text = std::fs::read_to_string(&file).unwrap();
        text.push_str(&format!("\n{}\n{}", rec("m3", "2026-11-13T11:00:00Z"), &rec("m4", "2026-11-13T11:30:00Z")[..40]));
        std::fs::write(&file, &text).unwrap();
        assert_eq!(records(&summary(&env, &day)), 3, "the partial line is not counted yet");
        text.truncate(text.len() - 40);
        text.push_str(&rec("m4", "2026-11-13T11:30:00Z"));
        text.push('\n');
        std::fs::write(&file, &text).unwrap();
        assert_eq!(records(&summary(&env, &day)), 4);
        // A file older than the window is not read.
        let old = home.join(".claude/projects/p/old.jsonl");
        std::fs::write(&old, rec("m9", "2026-11-13T12:00:00Z")).unwrap();
        let past = std::time::SystemTime::now() - std::time::Duration::from_secs(400 * 86_400);
        std::fs::File::options().write(true).open(&old).unwrap().set_modified(past).unwrap();
        let s = summary(&env, &day);
        assert_eq!(records(&s), 4);
        assert_eq!(s["sources"][0]["skippedFiles"], 1);
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn scans_pi_sessions_counting_a_forked_copy_once() {
        let home = std::env::temp_dir().join(format!("domovoi-usage-pi-{}", crate::util::new_uuid()));
        let dir = home.join("sessions/--w--");
        std::fs::create_dir_all(&dir).unwrap();
        let reply = |id: &str, ts: &str| json!({ "type": "message", "id": id, "parentId": null, "timestamp": ts,
            "message": { "role": "assistant", "provider": "zai", "model": "glm-5.3", "usage": { "input": 100, "output": 20, "cacheRead": 50, "cacheWrite": 5, "totalTokens": 175, "cost": { "total": 0.01 } } } }).to_string();
        let lines = [
            json!({ "type": "session", "version": 3, "id": "s1", "timestamp": "2026-11-13T10:00:00Z", "cwd": "/w" }).to_string(),
            json!({ "type": "message", "id": "u1", "parentId": null, "timestamp": "2026-11-13T10:00:01Z", "message": { "role": "user", "content": "hi" } }).to_string(),
            reply("a1", "2026-11-13T10:00:02Z"),
            "{\"usage\": bad".to_owned(),
            json!({ "type": "usage", "id": "w1", "parentId": "a1", "timestamp": "2026-11-13T10:05:00Z", "kind": "cache-warm", "model": "glm-5.3", "usage": { "input": 0, "output": 0, "cacheWrite": 900 } }).to_string(),
        ];
        std::fs::write(dir.join("2026-11-13T10-00-00-000Z_s1.jsonl"), lines.join("\n")).unwrap();
        // A fork copies the history (same entries) and adds its own reply.
        std::fs::write(dir.join("2026-11-13T11-00-00-000Z_s2.jsonl"), [lines[0].clone(), lines[1].clone(), lines[2].clone(), reply("a2", "2026-11-13T11:00:02Z")].join("\n")).unwrap();
        let env = ScanEnv {
            home: home.clone(),
            homes: vec![(Source::Pi, "pi".into(), home.join("sessions"))],
            base_dir: home.join("domovoi"), worktrees_dir: home.join("wt"), temp_dir: std::env::temp_dir(),
        };
        let s = summary(&env, &json!({ "sinceDay": "2026-11-13", "untilDay": "2026-11-13", "timeZone": "UTC" }));
        let row = s["buckets"].as_array().unwrap().iter().find(|r| r["provider"] == "pi").unwrap_or_else(|| panic!("{s}"));
        assert_eq!((row["model"].as_str(), row["records"].as_i64()), (Some("glm-5.3"), Some(3)), "a1 once, a2, and the cache warming");
        assert_eq!(row["totals"], json!({ "uncachedInputTokens": 200, "cachedInputTokens": 100, "cacheCreationTokens": 910, "outputTokens": 40, "reasoningTokens": 0 }));
        assert_eq!(row["sessions"], 2);
        assert_eq!((s["sources"][0]["fingerprint"]["provider"].as_str(), s["sources"][0]["malformedRecords"].as_i64()), (Some("pi"), Some(1)));
        std::fs::remove_dir_all(home).unwrap();
    }
}

#[cfg(test)]
mod bench {
    /// `cargo test -p domovoi-server --release usage::bench -- --ignored --nocapture`:
    /// times the scan against this machine's real logs (read-only).
    #[test]
    #[ignore]
    fn real_logs() {
        let env = crate::agent_sessions::ScanEnv::new(&crate::settings::Settings::default(), std::path::Path::new("/tmp/domovoi-bench"));
        let now = domovoi_contracts::iso_now();
        let until = &now[..10];
        for days in [1i64, 7, 30] {
            let since = domovoi_contracts::time_fmt::iso_from_millis(domovoi_contracts::time_fmt::now_millis() - (days - 1) * 86_400_000)[..10].to_owned();
            let input = serde_json::json!({ "sinceDay": since, "untilDay": until, "timeZone": "Europe/Warsaw" });
            for pass in ["cold", "warm"] {
                let t = std::time::Instant::now();
                let s = serde_json::json!(super::summary(&env, &serde_json::from_value(input.clone()).unwrap()).unwrap());
                let tokens: i64 = s["buckets"].as_array().unwrap().iter().map(|b| ["uncachedInputTokens", "cachedInputTokens", "cacheCreationTokens", "outputTokens", "reasoningTokens"].iter().map(|k| b["totals"][k].as_i64().unwrap_or(0)).sum::<i64>()).sum();
                let before_today: i64 = s["buckets"].as_array().unwrap().iter().filter(|b| b["day"].as_str() != Some(until)).map(|b| ["uncachedInputTokens", "cachedInputTokens", "cacheCreationTokens", "outputTokens", "reasoningTokens"].iter().map(|k| b["totals"][k].as_i64().unwrap_or(0)).sum::<i64>()).sum();
                println!("{days:>2} day(s) {pass}: {:>7.3}s  {} buckets  {tokens} tokens ({before_today} before today)  files {}", t.elapsed().as_secs_f64(), s["buckets"].as_array().unwrap().len(),
                    s["sources"].as_array().unwrap().iter().map(|x| x["scannedFiles"].to_string()).collect::<Vec<_>>().join("+"));
            }
        }
    }
}
