//! Token usage from the providers' local session logs (t3code `usage/`):
//! Claude Code's `projects/**/*.jsonl` assistant `usage` records, Codex's
//! `sessions/**/rollout-*.jsonl` `token_count` events, Pi's session files
//! (assistant messages' `usage`, and `usage` entries for work outside one)
//! and Antigravity's conversation databases (`crate::antigravity::usage`),
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
//! Each record is priced as it is counted (`crate::usage_pricing`: LiteLLM's
//! rate table, `usagePriceOverrides`), under the model `usageModelAliases`
//! maps it to; the model filter matches the model as the log names it.
//!
//! Retention (`usageHistoryRetentionDays`, t3code's scan cache): a log file that
//! is gone (deleted, or archived elsewhere) keeps counting from the cache while
//! it was last written within the retention, so usage outlives its transcript;
//! cache entries last written before it are dropped after each summary.
//!
//! Limits: the cache is the process's, so usage of a deleted transcript is
//! kept only until the server restarts (t3code saves its scan cache to disk);
//! Pi models are named as Pi's log names them, without t3code's `<provider>/`
//! prefix; duplicate copies of a record count the first copy, not the largest
//! of each token count.

use crate::agent_sessions::{ScanEnv, Source};
use crate::rpc::shell::{if_fits, text_or_empty};
use crate::usage_pricing::{ModelRate, RateTable, Snapshot, Speed, Tokens};
use serde::{Deserialize, Serialize};
use serde_json::Number;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::SystemTime;
use domovoi_model::typed::{
    UsageBucket, UsageCostSource, UsageModelFilter, UsageModelSelector, UsagePricing, UsageProviderKind, UsageSource,
    UsageSourceFingerprint, UsageSourceStatus, UsageSummary, UsageTokenTotals, UsageHistoryRetentionDays, ServerSettings,
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
    /// Of `creation`, written with a 1-hour TTL (Claude).
    creation_1h: u64,
    /// The cost the log reports (Claude's `costUSD`, Pi's `cost.total`); NaN for none.
    reported_usd: f64,
    speed: Speed,
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

/// One bucket's sums (`MutableBucket`).
#[derive(Default, Clone)]
struct Totals {
    uncached: u64,
    cached: u64,
    creation: u64,
    output: u64,
    reasoning: u64,
    records: u64,
    sessions: HashSet<Arc<str>>,
    cost_usd: f64,
    cache_savings_usd: f64,
    /// `[input, cacheRead, cacheWrite, output]` of the records whose cost splits.
    category: Option<[f64; 4]>,
    fast_cost_usd: f64,
    ultrafast_cost_usd: f64,
    speed_premium_usd: f64,
    unpriced_records: u64,
    unpriced_tokens: u64,
    provider_reported_records: u64,
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
    #[serde(rename = "costUSD", default, deserialize_with = "if_fits")]
    cost_usd: Option<f64>,
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
    #[serde(default, deserialize_with = "if_fits")]
    cache_creation: Option<ClaudeCacheCreation>,
    #[serde(default, deserialize_with = "if_fits")]
    speed: Option<String>,
}

/// The cache writes by TTL.
#[derive(Deserialize, Default)]
struct ClaudeCacheCreation {
    #[serde(default)]
    ephemeral_1h_input_tokens: u64,
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
    #[serde(deserialize_with = "if_fits")]
    cost: Option<PiCost>,
}

#[derive(Deserialize, Default)]
#[serde(default)]
struct PiCost {
    #[serde(deserialize_with = "if_fits")]
    total: Option<f64>,
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
                    creation_1h: u.cache_creation.as_ref().map_or(0, |c| c.ephemeral_1h_input_tokens),
                    reported_usd: l.cost_usd.filter(|c| c.is_finite()).unwrap_or(f64::NAN),
                    speed: if u.speed.as_deref() == Some("fast") { Speed::Fast } else { Speed::Standard },
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
                    reasoning: u.reasoning.min(u.output),
                    creation_1h: 0,
                    // Pi writes zero for rates it does not know: only a positive cost is one.
                    reported_usd: u.cost.and_then(|c| c.total).filter(|c| c.is_finite() && *c > 0.0).unwrap_or(f64::NAN),
                    speed: Speed::Standard,
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
                    creation_1h: 0,
                    reported_usd: f64::NAN,
                    speed: Speed::Standard,
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

/// The folder below a provider home that holds its logs.
fn log_root(source: Source, dir: &Path) -> PathBuf {
    match source {
        Source::Claude => dir.join("projects"),
        Source::Codex => dir.join("sessions"),
        // A pi home is its sessions folder.
        Source::Pi => dir.to_path_buf(),
    }
}

/// Every log file of a provider home with its size and modification time.
fn log_files(source: Source, dir: &Path) -> Option<Vec<(PathBuf, u64, SystemTime)>> {
    let root = log_root(source, dir);
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

/// Brings the cache up to date for the files that can hold records in the
/// window; then, after them, the cached files of this provider that are gone
/// but were last written since `retained_since_ms`.
fn scan(source: Source, dir: &Path, w: &Window, retained_since_ms: i64) -> (Vec<Arc<FileEntry>>, Scanned) {
    let Some(files) = log_files(source, dir) else {
        return (vec![], Scanned { status: UsageSourceStatus::Missing, scanned_files: 0, skipped_files: 0, malformed_records: 0 });
    };
    let total = files.len();
    let live: HashSet<&Path> = files.iter().map(|(p, _, _)| p.as_path()).collect();
    let root = log_root(source, dir);
    let mut gone: Vec<(PathBuf, Arc<FileEntry>)> = {
        let c = cache().lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        c.iter().filter(|(p, e)| millis(e.modified) >= retained_since_ms && p.starts_with(&root) && !live.contains(p.as_path())).map(|(p, e)| (p.clone(), e.clone())).collect()
    };
    // In one order every time, so the same copy of a duplicated record is the one counted.
    gone.sort_by(|a, b| a.0.cmp(&b.0));
    drop(live);
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
    let entries: Vec<Arc<FileEntry>> = entries.into_iter().flatten().chain(gone.into_iter().map(|(_, e)| e)).collect();
    let malformed: u32 = entries.iter().map(|e| e.malformed).sum();
    let scanned = Scanned { status: UsageSourceStatus::Ok, scanned_files: relevant.len() as u64, skipped_files: skipped as u64, malformed_records: malformed.into() };
    (entries, scanned)
}

type Buckets = BTreeMap<(String, Option<String>, &'static str, Arc<str>), Totals>;

/// How the records of one model are counted: under which model, at which rate.
#[derive(Clone)]
struct Resolved {
    /// The model `usageModelAliases` maps it to, or itself.
    model: Arc<str>,
    rate: Option<Arc<ModelRate>>,
    /// The rate is a `usagePriceOverrides` entry.
    overridden: bool,
}

/// Prices records (`UsageAggregator`'s model mapping and `#accumulate`).
struct Pricer<'a> {
    table: &'a RateTable,
    overrides: RateTable,
    aliases: HashMap<String, String>,
    filter: Option<&'a UsageModelFilter>,
    /// By provider and model as logged; `None` for a model the filter leaves out.
    resolved: HashMap<(&'static str, Arc<str>), Option<Resolved>>,
    /// The names rates were looked up by, for `refreshForUnlistedModels`.
    rate_models: HashSet<String>,
}

/// The provider's own name for a model's rates (a driver's `rateModel`).
fn provider_rate_model(provider: &str, model: &str) -> Option<String> {
    match provider {
        "antigravity" => Some(crate::usage_pricing::antigravity_rate_model(model)),
        // Pi model ids may be `<provider>/<rate model>`.
        "pi" => Some(model.split_once('/').map_or(model, |(_, m)| m).to_owned()),
        _ => None,
    }
}

impl<'a> Pricer<'a> {
    fn new(table: &'a RateTable, settings: &ServerSettings, filter: Option<&'a UsageModelFilter>) -> Pricer<'a> {
        Pricer {
            table,
            overrides: settings.usage_price_overrides.as_ref().map(crate::usage_pricing::override_table).unwrap_or_default(),
            aliases: settings.usage_model_aliases.as_ref().map(crate::usage_pricing::resolve_model_aliases).unwrap_or_default(),
            filter,
            resolved: HashMap::new(),
            rate_models: HashSet::new(),
        }
    }

    fn resolve(&mut self, provider: &'static str, model: &Arc<str>) -> Option<Resolved> {
        if let Some(r) = self.resolved.get(&(provider, model.clone())) {
            return r.clone();
        }
        let kept = self.filter.is_none_or(|f| matches_filter(f, provider, model.trim()));
        let r = kept.then(|| {
            let alias = self.aliases.get(&**model);
            let mapped: Arc<str> = alias.map_or_else(|| model.clone(), |a| a.as_str().into());
            if let Some(rate) = self.overrides.get(mapped.trim()) {
                return Resolved { model: mapped, rate: Some(rate.clone()), overridden: true };
            }
            // An alias takes the target's rate; only an unpriced target goes through the provider's naming.
            let rate_model = match alias {
                Some(a) => crate::usage_pricing::lookup_rate(self.table, a).is_none().then(|| provider_rate_model(provider, a)).flatten(),
                None => provider_rate_model(provider, model),
            };
            self.rate_models.insert(match (alias, &rate_model) {
                (None, Some(r)) => r.clone(),
                _ => mapped.to_string(),
            });
            let rate = rate_model.and_then(|m| crate::usage_pricing::lookup_rate(self.table, &m)).or_else(|| crate::usage_pricing::lookup_rate(self.table, &mapped));
            Resolved { model: mapped, rate, overridden: false }
        });
        self.resolved.insert((provider, model.clone()), r.clone());
        r
    }
}

/// Adds one record to its bucket, priced.
fn accumulate(t: &mut Totals, r: &Rec, resolved: &Resolved, session: &Arc<str>) {
    let tokens = Tokens { uncached: r.uncached, cached: r.cached, creation: r.creation, creation_1h: r.creation_1h, output: r.output };
    let reported = (!r.reported_usd.is_nan()).then_some(r.reported_usd);
    let p = crate::usage_pricing::price(resolved.rate.as_deref(), resolved.overridden, &tokens, r.speed, reported);
    t.uncached += r.uncached;
    t.cached += r.cached;
    t.creation += r.creation;
    t.output += r.output;
    t.reasoning += r.reasoning;
    t.records += 1;
    if !session.is_empty() {
        t.sessions.insert(session.clone());
    }
    t.cost_usd += p.cost_usd;
    t.cache_savings_usd += p.cache_savings_usd;
    if let Some(c) = p.category {
        let sum = t.category.get_or_insert([0.0; 4]);
        for (s, c) in sum.iter_mut().zip(c) {
            *s += c;
        }
    }
    match r.speed {
        Speed::Fast => t.fast_cost_usd += p.cost_usd,
        Speed::Ultrafast => t.ultrafast_cost_usd += p.cost_usd,
        Speed::Standard => {}
    }
    t.speed_premium_usd += p.speed_premium_usd;
    match p.cost_source {
        UsageCostSource::Unpriced => {
            t.unpriced_records += 1;
            t.unpriced_tokens += r.uncached + r.cached + r.creation + r.output;
        }
        UsageCostSource::ProviderReported => t.provider_reported_records += 1,
        _ => {}
    }
}

fn aggregate(provider: &'static str, entries: &[Arc<FileEntry>], w: &Window, seen: &mut HashSet<u64>, buckets: &mut Buckets, pricer: &mut Pricer) -> usize {
    let mut sessions: HashSet<Arc<str>> = HashSet::new();
    for e in entries {
        for r in &e.records {
            // The filter matches the model as logged, so an alias cannot pull excluded usage in.
            let Some(resolved) = pricer.resolve(provider, &r.model) else { continue };
            // Deduplicate before the window: a message whose records straddle
            // midnight counts on the day of its first record, in every window.
            if r.dedupe != 0 && !seen.insert(r.dedupe) {
                continue;
            }
            let Some((day, hour)) = w.key(r.ts_ms) else { continue };
            if !r.session.is_empty() {
                sessions.insert(r.session.clone());
            }
            let t = buckets.entry((day, hour, provider, resolved.model.clone())).or_default();
            accumulate(t, r, &resolved, &r.session);
        }
    }
    sessions.len()
}

fn matches_filter(filter: &UsageModelFilter, provider: &str, model: &str) -> bool {
    let hit = |list: &[UsageModelSelector]| list.iter().any(|e| e.model == model && e.provider.as_deref().is_none_or(|p| p == provider));
    filter.include.as_deref().is_none_or(hit) && !filter.exclude.as_deref().is_some_and(hit)
}

/// A summary, and the model names its rates were looked up by.
pub struct Summarized {
    pub summary: UsageSummary,
    pub rate_models: HashSet<String>,
}

/// When usage kept for gone log files stops counting (`retentionCutoffMs`).
fn retention_cutoff_ms(settings: &ServerSettings, now_ms: i64) -> i64 {
    match settings.usage_history_retention_days.unwrap_or(UsageHistoryRetentionDays::DEFAULT) {
        UsageHistoryRetentionDays::Unlimited => 0,
        UsageHistoryRetentionDays::Days(days) => (now_ms - i64::from(days) * 86_400_000).max(0),
    }
}

/// `pruneScanCache`: drops cached files last written before the retention.
fn prune(cutoff_ms: i64) {
    if cutoff_ms <= 0 {
        return;
    }
    cache().lock().unwrap_or_else(std::sync::PoisonError::into_inner).retain(|_, e| millis(e.modified) >= cutoff_ms);
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

/// Where Antigravity's conversation databases are: its apps' folders in the
/// scanned home, and each instance's profile.
fn antigravity_roots(env: &ScanEnv) -> Vec<PathBuf> {
    let profiles: Vec<PathBuf> = crate::antigravity::instances().iter().map(|id| crate::antigravity::profile(id).home).filter(|p| p.exists()).collect();
    crate::antigravity::usage::roots(&env.home, &profiles)
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
        let _ = scan(*source, dir, &w, i64::MAX);
    }
    let _ = crate::antigravity::usage::read(&antigravity_roots(env), w.start_ms);
}

/// A cost in USD as t3code writes it (`0.0` for none).
fn usd(v: f64) -> Number {
    Number::from_f64(v).or_else(|| Number::from_f64(0.0)).unwrap_or_else(|| Number::from(0))
}

/// `server.getUsageSummary`, priced with `rates`.
pub fn summary(env: &ScanEnv, input: &SummaryInput, settings: &ServerSettings, rates: &Snapshot) -> Result<Summarized, Failure> {
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
    let cutoff = retention_cutoff_ms(settings, domovoi_contracts::time_fmt::now_millis());
    let retained_since = (start - MTIME_SLACK_MS).max(cutoff);
    let mut pricer = Pricer::new(&rates.table, settings, input.model_filter.as_ref());
    let mut buckets: Buckets = BTreeMap::new();
    let mut seen = HashSet::new();
    let mut sources = vec![];
    for (source, _, dir) in &env.homes {
        let provider = match source {
            Source::Claude => "claude",
            Source::Codex => "codex",
            Source::Pi => "pi",
        };
        let (entries, scanned) = scan(*source, dir, &w, retained_since);
        let distinct_sessions = aggregate(provider, &entries, &w, &mut seen, &mut buckets, &mut pricer) as u64;
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
    // Antigravity keeps its history in SQLite databases, not log lines.
    let (records, scanned) = crate::antigravity::usage::read(&antigravity_roots(env), w.start_ms);
    let mut sessions: HashSet<Arc<str>> = HashSet::new();
    let mut models: HashMap<String, Arc<str>> = HashMap::new();
    for r in &records {
        let model = models.entry(r.model.clone()).or_insert_with(|| r.model.as_str().into()).clone();
        let Some(resolved) = pricer.resolve("antigravity", &model) else { continue };
        let Some((day, hour)) = w.key(r.ts_ms) else { continue };
        let session: Arc<str> = r.session.as_str().into();
        if !session.is_empty() {
            sessions.insert(session.clone());
        }
        let rec = Rec {
            ts_ms: r.ts_ms,
            model,
            session: session.clone(),
            uncached: r.uncached,
            cached: r.cached,
            creation: r.creation,
            output: r.output,
            reasoning: r.reasoning,
            creation_1h: 0,
            reported_usd: f64::NAN,
            speed: Speed::Standard,
            dedupe: 0,
        };
        let t = buckets.entry((day, hour, "antigravity", resolved.model.clone())).or_default();
        accumulate(t, &rec, &resolved, &session);
    }
    for s in scanned {
        sources.push(UsageSource {
            fingerprint: UsageSourceFingerprint {
                host_id: "local".into(),
                provider: UsageProviderKind::Antigravity,
                resolved_home_path: s.dir.to_string_lossy().into_owned(),
                volume_id: "local".into(),
            },
            status: if s.failed { UsageSourceStatus::Partial } else if s.exists { UsageSourceStatus::Ok } else { UsageSourceStatus::Missing },
            scanned_files: s.files,
            skipped_files: 0,
            malformed_records: 0,
            distinct_sessions: sessions.len() as u64,
            message: s.failed.then(|| "Some Antigravity history could not be read.".to_owned()),
            refreshing: None,
        });
    }
    prune(cutoff);
    let rounded = |v: f64| Some(crate::usage_pricing::round_usd(v)).filter(|r| *r != 0.0).map(usd);
    let rows: Vec<UsageBucket> = buckets
        .into_iter()
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
            cost_usd: usd(t.cost_usd),
            cache_savings_usd: usd(t.cache_savings_usd),
            category_cost_usd: t.category.map(crate::usage_pricing::category_cost),
            fast_cost_usd: rounded(t.fast_cost_usd),
            ultrafast_cost_usd: rounded(t.ultrafast_cost_usd),
            speed_premium_usd: rounded(t.speed_premium_usd),
            // `resolveCostSource`.
            cost_source: if t.unpriced_records == t.records {
                UsageCostSource::Unpriced
            } else if t.provider_reported_records == t.records {
                UsageCostSource::ProviderReported
            } else {
                UsageCostSource::ModelPriced
            },
            records: t.records,
            unpriced_records: t.unpriced_records,
            unpriced_tokens: Some(t.unpriced_tokens),
            sessions: t.sessions.len() as u64,
        })
        .collect();
    let summary = UsageSummary {
        contract_version: CONTRACT_VERSION.into(),
        read_at: domovoi_contracts::iso_now().as_str().into(),
        time_zone: tz_name.to_owned(),
        since_day: since.to_string(),
        until_day: until.to_string(),
        buckets: rows.into(),
        sources: sources.into(),
        pricing: rates.pricing.clone(),
        scan_duration_ms: started.elapsed().as_millis() as u64,
        model_filter: input.model_filter.clone(),
    };
    Ok(Summarized { summary, rate_models: pricer.rate_models })
}

/// `server.getUsageSummary` as the server answers it: the rates brought up to
/// date first, and the summary made again when a model it met had none and a
/// newer table was fetched.
pub async fn read_summary(env: ScanEnv, input: SummaryInput, settings: ServerSettings) -> Result<UsageSummary, Failure> {
    let rates = crate::usage_pricing::RateService::global();
    let env = Arc::new(env);
    let input = Arc::new(input);
    let settings = Arc::new(settings);
    let run = |snapshot: Snapshot| {
        let (env, input, settings) = (env.clone(), input.clone(), settings.clone());
        async move {
            tokio::task::spawn_blocking(move || summary(&env, &input, &settings, &snapshot))
                .await
                .unwrap_or_else(|e| Err(Failure::Read { reason: ReadFailure::ScanFailed, detail: e.to_string() }))
        }
    };
    let first = run(rates.ensure(false).await).await?;
    if rates.refresh_for_unlisted(&first.rate_models).await {
        return Ok(run(rates.snapshot()).await?.summary);
    }
    Ok(first.summary)
}

/// `server.refreshUsageRates`: the table fetched again unless it was within the minute.
pub async fn refresh_rates() -> UsagePricing {
    crate::usage_pricing::RateService::global().ensure(true).await.pricing
}

#[cfg(test)]
mod tests {
    use super::*;
    use domovoi_model::typed::UsagePricingStatus;
    use serde_json::{Value, json};

    fn no_rates() -> Snapshot {
        Snapshot {
            table: Arc::new(RateTable::new()),
            pricing: UsagePricing { status: UsagePricingStatus::Unavailable, source: crate::usage_pricing::LITELLM_RATES_URL.into(), fetched_at: None, known_models: 0 },
        }
    }

    /// The summary `asked` for, as it is written.
    fn summary(env: &ScanEnv, asked: &Value) -> Value {
        priced(env, asked, &ServerSettings::default(), &no_rates())
    }

    /// The summary `asked` for with these settings and rates, as it is written.
    fn priced(env: &ScanEnv, asked: &Value, settings: &ServerSettings, rates: &Snapshot) -> Value {
        let input: SummaryInput = serde_json::from_value(asked.clone()).expect("an input");
        json!(super::summary(env, &input, settings, rates).expect("a summary").summary)
    }

    /// Why no summary was made of what was `asked` for, as it is written.
    fn refused(env: &ScanEnv, asked: &Value) -> Value {
        let input: SummaryInput = serde_json::from_value(asked.clone()).expect("an input");
        json!(super::summary(env, &input, &ServerSettings::default(), &no_rates()).err().expect("a failure"))
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
            "costUsd": 0.0, "cacheSavingsUsd": 0.0, "costSource": "unpriced", "records": 1, "unpricedRecords": 1, "unpricedTokens": 107, "sessions": 1,
        }]));
        assert_eq!(filtered["buckets"][0]["costUsd"].to_string(), "0.0");

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

    fn claude_env(home: &Path) -> ScanEnv {
        ScanEnv {
            home: home.to_path_buf(),
            homes: vec![(Source::Claude, "claudeAgent".into(), home.join(".claude"))],
            base_dir: home.join("domovoi"), worktrees_dir: home.join("wt"), temp_dir: std::env::temp_dir(),
        }
    }

    fn claude_rec(id: &str, ts: &str, model: &str, extra: Value) -> String {
        let mut usage = json!({ "input_tokens": 1000, "cache_read_input_tokens": 2000, "cache_creation_input_tokens": 300, "output_tokens": 400 });
        if let (Some(u), Some(e)) = (usage.as_object_mut(), extra.as_object()) {
            u.extend(e.clone());
        }
        json!({ "type": "assistant", "timestamp": ts, "sessionId": "s1", "requestId": id, "message": { "id": id, "model": model, "usage": usage } }).to_string()
    }

    #[test]
    fn prices_buckets_with_overrides_and_aliases() {
        let home = std::env::temp_dir().join(format!("domovoi-usage-priced-{}", crate::util::new_uuid()));
        let dir = home.join(".claude/projects/p");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.jsonl"), [
            claude_rec("m1", "2026-11-13T10:00:00Z", "claude-x", json!({ "cache_creation": { "ephemeral_1h_input_tokens": 100 } })),
            claude_rec("m2", "2026-11-13T10:01:00Z", "claude-x", json!({ "speed": "fast" })),
            claude_rec("m3", "2026-11-13T10:02:00Z", "claude-unknown-9", json!({})),
            claude_rec("m4", "2026-11-13T10:03:00Z", "my-alias", json!({})),
            claude_rec("m5", "2026-11-13T10:04:00Z", "house-model", json!({})),
        ].join("\n")).unwrap();
        let env = claude_env(&home);
        let table = crate::usage_pricing::parse_rate_table(json!({
            "anthropic/claude-x": { "input_cost_per_token": 3e-6, "output_cost_per_token": 15e-6, "cache_read_input_token_cost": 0.3e-6,
                "cache_creation_input_token_cost": 3.75e-6, "cache_creation_input_token_cost_above_1hr": 6e-6, "provider_specific_entry": { "fast": 6 } },
        }).to_string().as_bytes());
        let rates = Snapshot { table: Arc::new(table), pricing: UsagePricing { status: UsagePricingStatus::Fresh, source: "x".into(), fetched_at: None, known_models: 2 } };
        let settings: ServerSettings = serde_json::from_value(json!({
            "usageModelAliases": { "my-alias": "claude-x" },
            "usagePriceOverrides": { "house-model": { "inputCostPerMillionTokens": 1, "outputCostPerMillionTokens": 2 } },
        })).unwrap();
        let day = json!({ "sinceDay": "2026-11-13", "untilDay": "2026-11-13", "timeZone": "UTC" });
        let s = priced(&env, &day, &settings, &rates);
        assert_eq!(s["pricing"]["status"], "fresh");
        let row = |m: &str| s["buckets"].as_array().unwrap().iter().find(|b| b["model"] == m).cloned().unwrap_or_else(|| panic!("{m}: {s}"));
        let x = row("claude-x");
        // m1 (with 100 one-hour cache writes), m2 at six times the rates, m4 by its alias.
        let standard = 1000.0 * 3e-6 + 2000.0 * 0.3e-6 + 300.0 * 3.75e-6 + 400.0 * 15e-6;
        let m1 = standard + 100.0 * (6e-6 - 3.75e-6);
        let cost = x["costUsd"].as_f64().unwrap();
        assert!((cost - (m1 + 6.0 * standard + standard)).abs() < 1e-12, "{x}");
        assert_eq!((x["records"].as_i64(), x["costSource"].as_str(), x["unpricedRecords"].as_i64()), (Some(3), Some("modelPriced"), Some(0)));
        assert!((x["fastCostUsd"].as_f64().unwrap() - crate::usage_pricing::round_usd(6.0 * standard)).abs() < 1e-9);
        assert!((x["speedPremiumUsd"].as_f64().unwrap() - crate::usage_pricing::round_usd(5.0 * standard)).abs() < 1e-9);
        assert!((x["cacheSavingsUsd"].as_f64().unwrap() - 2000.0 * 2.7e-6 * 8.0).abs() < 1e-12, "fast savings are at the fast rates: {x}");
        assert_eq!(x["categoryCostUsd"]["input"].as_f64(), Some(crate::usage_pricing::round_usd(1000.0 * 3e-6 * 8.0)));
        let unknown = row("claude-unknown-9");
        assert_eq!((unknown["costUsd"].as_f64(), unknown["costSource"].as_str(), unknown["unpricedTokens"].as_i64()), (Some(0.0), Some("unpriced"), Some(3700)));
        assert!(unknown.get("categoryCostUsd").is_none());
        let house = row("house-model");
        assert!((house["costUsd"].as_f64().unwrap() - (3300.0 * 1e-6 + 400.0 * 2e-6)).abs() < 1e-12, "{house}");
        // The filter matches the model as logged, before the alias.
        let only_alias = priced(&env, &json!({ "sinceDay": "2026-11-13", "untilDay": "2026-11-13", "timeZone": "UTC", "modelFilter": { "include": [{ "model": "my-alias" }] } }), &settings, &rates);
        let b = only_alias["buckets"].as_array().unwrap();
        assert_eq!((b.len(), b[0]["model"].as_str(), b[0]["records"].as_i64()), (1, Some("claude-x"), Some(1)));
        // The names looked up, for a refetch when one is unlisted.
        let input: SummaryInput = serde_json::from_value(day).unwrap();
        let out = super::summary(&env, &input, &settings, &rates).unwrap();
        assert!(out.rate_models.contains("claude-unknown-9") && out.rate_models.contains("claude-x") && !out.rate_models.contains("house-model"));
        std::fs::remove_dir_all(home).unwrap();
    }

    #[test]
    fn a_gone_log_counts_until_its_retention_ends() {
        let home = std::env::temp_dir().join(format!("domovoi-usage-retained-{}", crate::util::new_uuid()));
        let dir = home.join(".claude/projects/p");
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("gone.jsonl");
        let then = domovoi_contracts::time_fmt::iso_from_millis(domovoi_contracts::time_fmt::now_millis() - 10 * 86_400_000);
        std::fs::write(&file, claude_rec("g1", &then, "claude-x", json!({}))).unwrap();
        let ten_days_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(10 * 86_400);
        std::fs::File::options().write(true).open(&file).unwrap().set_modified(ten_days_ago).unwrap();
        let env = claude_env(&home);
        let window = json!({ "sinceDay": &then[..10], "untilDay": &then[..10], "timeZone": "UTC" });
        let records = |s: &Value| s["buckets"].as_array().unwrap().iter().map(|b| b["records"].as_i64().unwrap()).sum::<i64>();
        let settings = |days: Value| -> ServerSettings { serde_json::from_value(json!({ "usageHistoryRetentionDays": days })).unwrap() };
        assert_eq!(records(&priced(&env, &window, &settings(json!(30)), &no_rates())), 1);
        std::fs::remove_file(&file).unwrap();
        let s = priced(&env, &window, &settings(json!(30)), &no_rates());
        assert_eq!((records(&s), s["sources"][0]["scannedFiles"].as_i64()), (1, Some(0)), "the deleted transcript's usage is kept");
        assert_eq!(records(&priced(&env, &window, &settings(json!("unlimited")), &no_rates())), 1);
        // Past its retention it is dropped, and does not come back.
        assert_eq!(records(&priced(&env, &window, &settings(json!(5)), &no_rates())), 0);
        assert_eq!(records(&priced(&env, &window, &settings(json!(30)), &no_rates())), 0);
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
                let rates = crate::usage_pricing::RateService::new(std::path::PathBuf::from("/tmp/domovoi-bench/userdata"), vec![]).snapshot();
                let s = serde_json::json!(super::summary(&env, &serde_json::from_value(input.clone()).unwrap(), &crate::settings::Settings::default(), &rates).unwrap().summary);
                let tokens: i64 = s["buckets"].as_array().unwrap().iter().map(|b| ["uncachedInputTokens", "cachedInputTokens", "cacheCreationTokens", "outputTokens", "reasoningTokens"].iter().map(|k| b["totals"][k].as_i64().unwrap_or(0)).sum::<i64>()).sum();
                let before_today: i64 = s["buckets"].as_array().unwrap().iter().filter(|b| b["day"].as_str() != Some(until)).map(|b| ["uncachedInputTokens", "cachedInputTokens", "cacheCreationTokens", "outputTokens", "reasoningTokens"].iter().map(|k| b["totals"][k].as_i64().unwrap_or(0)).sum::<i64>()).sum();
                println!("{days:>2} day(s) {pass}: {:>7.3}s  {} buckets  {tokens} tokens ({before_today} before today)  files {}", t.elapsed().as_secs_f64(), s["buckets"].as_array().unwrap().len(),
                    s["sources"].as_array().unwrap().iter().map(|x| x["scannedFiles"].to_string()).collect::<Vec<_>>().join("+"));
            }
        }
    }
}
