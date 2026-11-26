//! `subscribeResourceTelemetry`, after t3code's `ResourceTelemetry` and its
//! process model (`resourceTelemetry/Model.ts`): the server process and every
//! descendant (provider CLIs, terminal shells and what they started), sampled
//! once a second, with CPU from CPU-time deltas, resident/virtual memory, disk
//! I/O and per-group totals including process starts and exits.
//!
//! Limits: t3code samples through a native sidecar and, in the desktop app,
//! Electron's own metrics; Domovoi samples in-process with `sysinfo`, so there is no
//! Electron or resource-monitor group (`health.desktop` is `unavailable`,
//! `sidecarVersion`/`sidecarPid` are `None`), host power state is `unknown`, and
//! no per-operation I/O attribution is recorded.

use super::Never;
use super::admin_failure::Failure;
use crate::state::ConnCtx;
use domovoi_rpc::Router;
use futures::StreamExt;
use futures::stream::BoxStream;
use serde::de::IgnoredAny;
use serde::{Deserialize, Serialize};
use serde_json::Number;
use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;
use sysinfo::{Pid, ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};
use crate::time_fmt::{iso_from_millis, now_millis};
use domovoi_model::typed::{
    HostResourcesSnapshot, ServerProcessDiagnosticsEntry, ServerProcessDiagnosticsError, ServerProcessDiagnosticsResult, TaggedOption, Timestamp,
};

const SAMPLE_INTERVAL_MS: u64 = 1_000;
const MAX_DELTA_INTERVAL_MS: i64 = 30_000;

/// One process as read from the OS.
#[derive(Debug, Clone, Default)]
pub struct RawProcess {
    pub pid: u32,
    pub ppid: u32,
    pub start_time_ms: i64,
    pub name: String,
    pub command: String,
    pub status: String,
    pub cpu_percent: f64,
    pub cpu_time_ms: i64,
    pub resident_bytes: i64,
    pub virtual_bytes: i64,
    pub io_read_bytes: i64,
    pub io_write_bytes: i64,
    pub run_time_ms: i64,
}

#[derive(Clone)]
struct Previous {
    raw: RawProcess,
    peak_resident: i64,
    first_seen_ms: i64,
    sampled_at_ms: i64,
}

#[derive(Default, Clone, Copy)]
struct Counters {
    cpu_time_ms: i64,
    io_read: i64,
    io_write: i64,
    starts: i64,
    exits: i64,
}

/// Rolling state between samples (`mergeProcesses`' `previous` and `counters`).
#[derive(Default)]
pub struct Model {
    previous: HashMap<(u32, i64), Previous>,
    counters: Counters,
}

// ---- what a sample is written as (`resourceTelemetry.ts`) ----

/// Which process is meant (`ResourceTelemetryProcessIdentity`): a pid alone is used again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessIdentity {
    pid: u32,
    start_time_ms: i64,
}

/// What a process is to the server (`ResourceTelemetryProcessCategory`): itself, or one it started.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProcessCategory {
    Server,
    ServerChild,
}

/// What a process's I/O counts are counts of (`ResourceTelemetryIoSemantics`): for Domovoi, disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum IoSemantics {
    Storage,
}

/// One process of the server's tree (`ResourceTelemetryProcess`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Process {
    identity: ProcessIdentity,
    ppid: u32,
    child_pids: Vec<u32>,
    /// 0 for the server, one more for each generation below it.
    depth: usize,
    name: String,
    command: String,
    status: String,
    category: ProcessCategory,
    cpu_percent: f64,
    cpu_time_ms: i64,
    resident_bytes: i64,
    peak_resident_bytes: i64,
    virtual_bytes: i64,
    io_read_bytes: i64,
    io_write_bytes: i64,
    io_read_bytes_per_second: f64,
    io_write_bytes_per_second: f64,
    io_semantics: IoSemantics,
    run_time_ms: i64,
    first_seen_at: String,
    last_seen_at: String,
}

/// The totals of a group of processes (`ResourceTelemetryAggregate`).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Aggregate {
    process_count: usize,
    current_cpu_percent: f64,
    cpu_time_ms: i64,
    current_rss_bytes: i64,
    peak_rss_bytes: i64,
    io_read_bytes: i64,
    io_write_bytes: i64,
    io_read_bytes_per_second: f64,
    io_write_bytes_per_second: f64,
    process_starts: i64,
    process_exits: i64,
}

/// The groups t3code totals (`ResourceTelemetryGroups`). Domovoi has only the
/// backend: no Electron processes and no monitor sidecar.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Groups {
    backend: Aggregate,
    electron: Aggregate,
    monitor: Aggregate,
    all_t3: Aggregate,
}

/// What Domovoi knows of the host's power state: nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum NotKnown {
    Unknown,
}

/// The host's power state (`HostPowerSnapshot`), which Domovoi does not read.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Power {
    source: NotKnown,
    idle: NotKnown,
    idle_seconds: Option<Never>,
    locked: NotKnown,
    suspended: bool,
    on_battery: NotKnown,
    low_power_mode: NotKnown,
    thermal_state: NotKnown,
    stale: bool,
    updated_at: String,
}

impl Power {
    fn unknown(at: &str) -> Self {
        let unknown = NotKnown::Unknown;
        Power { source: unknown, idle: unknown, idle_seconds: None, locked: unknown, suspended: false, on_battery: unknown, low_power_mode: unknown,
            thermal_state: unknown, stale: true, updated_at: at.to_owned() }
    }
}

/// What used the resources, by operation (`ResourceAttributionSnapshot`): Domovoi records none.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Attribution {
    read_at: String,
    entries: [Never; 0],
}

/// How a source of samples is doing (`ResourceTelemetrySourceStatus`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum SourceStatus {
    Starting,
    Healthy,
    Unavailable,
}

/// One source of samples (`ResourceTelemetrySourceHealth`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct SourceHealth {
    status: SourceStatus,
    last_sample_at: TaggedOption<String>,
    last_error: TaggedOption<Never>,
}

/// How sampling is doing (`ResourceTelemetryHealth`). Domovoi samples in-process:
/// no desktop source and no sidecar.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Health {
    native: SourceHealth,
    desktop: SourceHealth,
    sidecar_version: TaggedOption<Never>,
    sidecar_pid: TaggedOption<Never>,
    restart_count: u32,
    collection_duration_micros: i64,
    scanned_process_count: usize,
    retained_process_count: usize,
    inaccessible_process_count: u32,
}

impl Health {
    /// After a sample read at `read_at`.
    fn sampled(read_at: &str, collection_duration_micros: i64, scanned: usize, retained: usize) -> Self {
        let native = SourceHealth { status: SourceStatus::Healthy, last_sample_at: TaggedOption::Some { value: read_at.to_owned() }, last_error: TaggedOption::None };
        Health { native, collection_duration_micros, scanned_process_count: scanned, retained_process_count: retained, ..Health::starting() }
    }

    /// Before the first sample.
    fn starting() -> Self {
        let source = |status| SourceHealth { status, last_sample_at: TaggedOption::None, last_error: TaggedOption::None };
        Health {
            native: source(SourceStatus::Starting),
            desktop: source(SourceStatus::Unavailable),
            sidecar_version: TaggedOption::None,
            sidecar_pid: TaggedOption::None,
            restart_count: 0,
            collection_duration_micros: 0,
            scanned_process_count: 0,
            retained_process_count: 0,
            inaccessible_process_count: 0,
        }
    }
}

/// One sample of the server's process tree (`ResourceTelemetrySnapshot`).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    read_at: String,
    sample_interval_ms: u64,
    processes: Vec<Process>,
    groups: Groups,
    power: Power,
    speed_limit_percent: TaggedOption<Never>,
    attribution: Attribution,
    health: Health,
}

impl Snapshot {
    fn new(read_at: String, processes: Vec<Process>, groups: Groups, health: Health) -> Self {
        Snapshot {
            sample_interval_ms: SAMPLE_INTERVAL_MS,
            processes,
            groups,
            power: Power::unknown(&read_at),
            speed_limit_percent: TaggedOption::None,
            attribution: Attribution { read_at: read_at.clone(), entries: [] },
            health,
            read_at,
        }
    }
}

fn delta(current: i64, previous: i64, elapsed_ms: i64) -> i64 {
    if elapsed_ms <= 0 || elapsed_ms > MAX_DELTA_INTERVAL_MS || current < previous { 0 } else { current - previous }
}

fn finite(v: f64) -> f64 {
    if v.is_finite() && v > 0.0 { v } else { 0.0 }
}

fn aggregate(processes: &[Process], c: Counters) -> Aggregate {
    let sum = |of: fn(&Process) -> f64| processes.iter().map(of).sum::<f64>();
    Aggregate {
        process_count: processes.len(),
        current_cpu_percent: sum(|p| p.cpu_percent),
        cpu_time_ms: c.cpu_time_ms,
        current_rss_bytes: sum(|p| p.resident_bytes as f64) as i64,
        peak_rss_bytes: sum(|p| p.peak_resident_bytes as f64) as i64,
        io_read_bytes: c.io_read,
        io_write_bytes: c.io_write,
        io_read_bytes_per_second: sum(|p| p.io_read_bytes_per_second),
        io_write_bytes_per_second: sum(|p| p.io_write_bytes_per_second),
        process_starts: c.starts,
        process_exits: c.exits,
    }
}

impl Model {
    /// Normalizes the server's process tree in `all` into the processes and
    /// groups of a `ResourceTelemetrySnapshot`, updating rolling state.
    pub fn merge(&mut self, all: &[RawProcess], server_pid: u32, sampled_at_ms: i64) -> (Vec<Process>, Groups) {
        let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
        for p in all {
            if p.pid != p.ppid {
                children.entry(p.ppid).or_default().push(p.pid);
            }
        }
        for c in children.values_mut() {
            c.sort_unstable();
        }
        let by_pid: HashMap<u32, &RawProcess> = all.iter().map(|p| (p.pid, p)).collect();
        // Depth-first from the server, children by pid (`orderProcessTree`).
        let mut ordered: Vec<(u32, usize)> = vec![];
        let mut seen = HashSet::new();
        let mut stack = VecDeque::from([(server_pid, 0usize)]);
        while let Some((pid, depth)) = stack.pop_front() {
            if !by_pid.contains_key(&pid) || !seen.insert(pid) {
                continue;
            }
            ordered.push((pid, depth));
            for (i, child) in children.get(&pid).into_iter().flatten().enumerate() {
                stack.insert(i, (*child, depth + 1));
            }
        }
        let mut next = HashMap::new();
        let mut processes = vec![];
        let mut c = self.counters;
        for (pid, depth) in ordered {
            let p = by_pid[&pid];
            let key = (p.pid, p.start_time_ms);
            let prev = self.previous.get(&key);
            let elapsed = prev.map_or(0, |q| sampled_at_ms - q.sampled_at_ms);
            let d = |cur: i64, f: fn(&RawProcess) -> i64| prev.map_or(0, |q| delta(cur, f(&q.raw), elapsed));
            let cpu_delta = d(p.cpu_time_ms, |r| r.cpu_time_ms);
            let read_delta = d(p.io_read_bytes, |r| r.io_read_bytes);
            let write_delta = d(p.io_write_bytes, |r| r.io_write_bytes);
            let cpu_percent = match prev {
                Some(_) if elapsed > 0 && elapsed <= MAX_DELTA_INTERVAL_MS => cpu_delta as f64 / elapsed as f64 * 100.0,
                _ => p.cpu_percent,
            };
            let rate = |delta: i64| if elapsed > 0 { finite(delta as f64 * 1000.0 / elapsed as f64) } else { 0.0 };
            let peak = p.resident_bytes.max(prev.map_or(0, |q| q.peak_resident));
            let first_seen = prev.map_or(sampled_at_ms, |q| q.first_seen_ms);
            let mut child_pids = children.get(&pid).cloned().unwrap_or_default();
            child_pids.retain(|c| by_pid.contains_key(c));
            processes.push(Process {
                identity: ProcessIdentity { pid: p.pid, start_time_ms: p.start_time_ms },
                ppid: p.ppid,
                child_pids,
                depth,
                name: p.name.clone(),
                command: p.command.clone(),
                status: p.status.clone(),
                category: if p.pid == server_pid { ProcessCategory::Server } else { ProcessCategory::ServerChild },
                cpu_percent: finite(cpu_percent),
                cpu_time_ms: p.cpu_time_ms,
                resident_bytes: p.resident_bytes,
                peak_resident_bytes: peak,
                virtual_bytes: p.virtual_bytes,
                io_read_bytes: p.io_read_bytes,
                io_write_bytes: p.io_write_bytes,
                io_read_bytes_per_second: rate(read_delta),
                io_write_bytes_per_second: rate(write_delta),
                io_semantics: IoSemantics::Storage,
                run_time_ms: p.run_time_ms,
                first_seen_at: iso_from_millis(first_seen),
                last_seen_at: iso_from_millis(sampled_at_ms),
            });
            c.cpu_time_ms += cpu_delta;
            c.io_read += read_delta;
            c.io_write += write_delta;
            if prev.is_none() {
                c.starts += 1;
            }
            next.insert(key, Previous { raw: p.clone(), peak_resident: peak, first_seen_ms: first_seen, sampled_at_ms });
        }
        c.exits += self.previous.keys().filter(|k| !next.contains_key(*k)).count() as i64;
        self.previous = next;
        self.counters = c;
        let empty = aggregate(&[], Counters::default());
        let groups = Groups { backend: aggregate(&processes, c), electron: empty.clone(), monitor: empty, all_t3: aggregate(&processes, c) };
        (processes, groups)
    }
}

fn read_processes(sys: &mut System) -> Vec<RawProcess> {
    let kind = ProcessRefreshKind::nothing().with_memory().with_cpu().with_disk_usage().with_cmd(UpdateKind::OnlyIfNotSet);
    sys.refresh_processes_specifics(ProcessesToUpdate::All, true, kind);
    sys.processes()
        .values()
        .map(|p| {
            let disk = p.disk_usage();
            let cmd = p.cmd().iter().map(|a| a.to_string_lossy()).collect::<Vec<_>>().join(" ");
            RawProcess {
                pid: p.pid().as_u32(),
                ppid: p.parent().map_or(0, Pid::as_u32),
                start_time_ms: p.start_time() as i64 * 1000,
                name: p.name().to_string_lossy().into_owned(),
                command: cmd,
                status: p.status().to_string(),
                cpu_percent: f64::from(p.cpu_usage()),
                cpu_time_ms: p.accumulated_cpu_time() as i64,
                resident_bytes: p.memory() as i64,
                virtual_bytes: p.virtual_memory() as i64,
                io_read_bytes: disk.total_read_bytes as i64,
                io_write_bytes: disk.total_written_bytes as i64,
                run_time_ms: p.run_time() as i64 * 1000,
            }
        })
        .collect()
}

/// One sample: `(snapshot, system)`; the `System` is handed back for the next sample.
fn sample(mut sys: System, model: &mut Model, server_pid: u32) -> (Snapshot, System) {
    let started = std::time::Instant::now();
    let all = read_processes(&mut sys);
    let sampled_at = now_millis();
    let (processes, groups) = model.merge(&all, server_pid, sampled_at);
    let read_at = iso_from_millis(sampled_at);
    let health = Health::sampled(&read_at, started.elapsed().as_micros() as i64, all.len(), processes.len());
    (Snapshot::new(read_at, processes, groups, health), sys)
}

fn telemetry_stream() -> BoxStream<'static, Result<Snapshot, Failure>> {
    async_stream::stream! {
        let Ok(mut sys) = System::new() else {
            yield Err(Failure::TelemetryUnavailable { message: "Process telemetry is unavailable on this platform." });
            return;
        };
        let mut model = Model::default();
        let server_pid = std::process::id();
        loop {
            let (snapshot, back, next_model) = match tokio::task::spawn_blocking(move || {
                let (snapshot, sys) = sample(sys, &mut model, server_pid);
                (snapshot, sys, model)
            })
            .await
            {
                Ok(v) => v,
                Err(_) => break,
            };
            sys = back;
            model = next_model;
            yield Ok(snapshot);
            tokio::time::sleep(Duration::from_millis(SAMPLE_INTERVAL_MS)).await;
        }
    }
    .boxed()
}

// ---- retained history (server.getResourceTelemetryHistory and friends) ----

/// Samples kept for history queries: 15 minutes at one per second.
const HISTORY_SAMPLES: usize = 900;

/// A sample with when it was kept, in milliseconds since the epoch.
type Kept = (i64, Arc<Snapshot>);

pub struct History {
    samples: std::sync::Mutex<VecDeque<Kept>>,
}

/// The process-wide history, sampling in the background from first use.
pub fn history() -> &'static History {
    static H: std::sync::OnceLock<History> = std::sync::OnceLock::new();
    H.get_or_init(|| {
        tokio::spawn(async {
            let Ok(mut sys) = System::new() else { return };
            let mut model = Model::default();
            let server_pid = std::process::id();
            loop {
                let Ok((snapshot, back, next_model)) = tokio::task::spawn_blocking(move || {
                    let (snapshot, sys) = sample(sys, &mut model, server_pid);
                    (snapshot, sys, model)
                })
                .await
                else {
                    return;
                };
                sys = back;
                model = next_model;
                history().push(snapshot);
                tokio::time::sleep(Duration::from_millis(SAMPLE_INTERVAL_MS)).await;
            }
        });
        History { samples: std::sync::Mutex::new(VecDeque::new()) }
    })
}

impl History {
    fn push(&self, snapshot: Snapshot) {
        let mut s = crate::util::locked(&self.samples);
        s.push_back((now_millis(), Arc::new(snapshot)));
        while s.len() > HISTORY_SAMPLES {
            s.pop_front();
        }
    }

    pub fn latest(&self) -> Option<Arc<Snapshot>> {
        crate::util::locked(&self.samples).back().map(|(_, v)| v.clone())
    }

    fn window(&self, window_ms: i64) -> Vec<Kept> {
        let since = now_millis() - window_ms;
        crate::util::locked(&self.samples).iter().filter(|(t, _)| *t >= since).cloned().collect()
    }
}

fn health_of(latest: Option<&Snapshot>) -> Health {
    latest.map_or_else(Health::starting, |s| s.health.clone())
}

/// A stretch of the window (`ResourceTelemetryHistoryBucket`), with the most
/// its samples show of each measure.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoryBucket {
    started_at: String,
    ended_at: String,
    avg_cpu_percent: f64,
    max_cpu_percent: f64,
    max_rss_bytes: i64,
    io_read_bytes: i64,
    io_write_bytes: i64,
    max_process_count: usize,
}

/// What the window's samples show of one process.
struct ProcStats<'a> {
    first: &'a Process,
    last: &'a Process,
    cpu_sum: f64,
    cpu_max: f64,
    rss_max: i64,
    samples: u64,
}

impl ProcStats<'_> {
    fn avg_cpu_percent(&self) -> f64 {
        self.cpu_sum / self.samples.max(1) as f64
    }
}

/// What history takes as a sample's CPU use over all processes. It has always
/// been 0: the samples were read for a `cpuPercent` their totals do not have
/// (the field is `currentCpuPercent`), so every bucket's average and maximum,
/// and the total of CPU seconds, are 0. Kept so here: reading the right field
/// changes what clients are shown, which is its own change to make.
fn sample_cpu_percent(_sample: &Snapshot) -> f64 {
    0.0
}

/// Per-bucket totals and per-process statistics over the window.
fn summarize(samples: &[Kept], window_ms: i64, bucket_ms: i64) -> (Vec<HistoryBucket>, Vec<ProcStats<'_>>) {
    let bucket_ms = bucket_ms.max(1_000);
    let end = now_millis();
    let start = end - window_ms;
    let count = ((window_ms + bucket_ms - 1) / bucket_ms).clamp(1, 1_000) as usize;
    let mut buckets = vec![];
    let mut per_proc: HashMap<String, ProcStats> = HashMap::new();
    for b in 0..count {
        let (from, to) = (start + b as i64 * bucket_ms, (start + (b as i64 + 1) * bucket_ms).min(end));
        let inside: Vec<&Snapshot> = samples.iter().filter(|(t, _)| *t >= from && *t < to).map(|(_, v)| &**v).collect();
        let cpu: Vec<f64> = inside.iter().map(|s| sample_cpu_percent(s)).collect();
        let io = |of: fn(&Aggregate) -> i64| -> i64 {
            let first = inside.first().map_or(0, |s| of(&s.groups.all_t3));
            let last = inside.last().map_or(0, |s| of(&s.groups.all_t3));
            (last - first).max(0)
        };
        buckets.push(HistoryBucket {
            started_at: iso_from_millis(from),
            ended_at: iso_from_millis(to),
            avg_cpu_percent: if cpu.is_empty() { 0.0 } else { cpu.iter().sum::<f64>() / cpu.len() as f64 },
            max_cpu_percent: cpu.iter().cloned().fold(0.0, f64::max),
            max_rss_bytes: inside.iter().map(|s| s.groups.all_t3.current_rss_bytes).max().unwrap_or(0),
            io_read_bytes: io(|totals| totals.io_read_bytes),
            io_write_bytes: io(|totals| totals.io_write_bytes),
            max_process_count: inside.iter().map(|s| s.processes.len()).max().unwrap_or(0),
        });
    }
    for (_, s) in samples {
        for p in &s.processes {
            let key = format!("{}:{}", p.identity.pid, p.identity.start_time_ms);
            let e = per_proc.entry(key).or_insert_with(|| ProcStats { first: p, last: p, cpu_sum: 0.0, cpu_max: 0.0, rss_max: 0, samples: 0 });
            e.last = p;
            e.cpu_sum += p.cpu_percent;
            e.cpu_max = e.cpu_max.max(p.cpu_percent);
            e.rss_max = e.rss_max.max(p.resident_bytes);
            e.samples += 1;
        }
    }
    let mut procs: Vec<ProcStats> = per_proc.into_values().collect();
    procs.sort_by(|a, b| b.cpu_sum.partial_cmp(&a.cpu_sum).unwrap_or(std::cmp::Ordering::Equal));
    procs.truncate(20);
    (buckets, procs)
}

/// What a window's samples show of one process (`ResourceTelemetryProcessSummary`).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessSummary {
    identity: ProcessIdentity,
    ppid: u32,
    depth: usize,
    name: String,
    command: String,
    category: ProcessCategory,
    first_seen_at: String,
    last_seen_at: String,
    current_cpu_percent: f64,
    avg_cpu_percent: f64,
    max_cpu_percent: f64,
    cpu_time_ms: i64,
    current_rss_bytes: i64,
    peak_rss_bytes: i64,
    io_read_bytes: i64,
    io_write_bytes: i64,
    io_semantics: IoSemantics,
    sample_count: u64,
}

/// `server.getResourceTelemetryHistory` (`ResourceTelemetryHistory`): the
/// window in buckets, and the processes that used the most CPU in it.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TelemetryHistory {
    read_at: String,
    window_ms: i64,
    /// As asked for; buckets are never shorter than a second.
    bucket_ms: i64,
    sample_interval_ms: u64,
    retained_sample_count: usize,
    buckets: Vec<HistoryBucket>,
    top_processes: Vec<ProcessSummary>,
    health: Health,
}

/// `server.getResourceTelemetryHistory`.
pub fn telemetry_history(window_ms: i64, bucket_ms: i64) -> TelemetryHistory {
    let h = history();
    let samples = h.window(window_ms);
    let (buckets, procs) = summarize(&samples, window_ms, bucket_ms);
    let top_processes = procs
        .iter()
        .map(|p| ProcessSummary {
            identity: p.last.identity,
            ppid: p.last.ppid,
            depth: p.last.depth,
            name: p.last.name.clone(),
            command: p.last.command.clone(),
            category: p.last.category,
            first_seen_at: p.first.first_seen_at.clone(),
            last_seen_at: p.last.last_seen_at.clone(),
            current_cpu_percent: p.last.cpu_percent,
            avg_cpu_percent: p.avg_cpu_percent(),
            max_cpu_percent: p.cpu_max,
            cpu_time_ms: p.last.cpu_time_ms,
            current_rss_bytes: p.last.resident_bytes,
            peak_rss_bytes: p.rss_max,
            io_read_bytes: p.last.io_read_bytes,
            io_write_bytes: p.last.io_write_bytes,
            io_semantics: IoSemantics::Storage,
            sample_count: p.samples,
        })
        .collect();
    TelemetryHistory {
        read_at: iso_from_millis(now_millis()),
        window_ms,
        bucket_ms,
        sample_interval_ms: SAMPLE_INTERVAL_MS,
        retained_sample_count: samples.len(),
        buckets,
        top_processes,
        health: health_of(h.latest().as_deref()),
    }
}

/// A stretch of the window in the older diagnostics view (`ServerProcessResourceHistoryBucket`).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessHistoryBucket {
    started_at: String,
    ended_at: String,
    avg_cpu_percent: f64,
    max_cpu_percent: f64,
    max_rss_bytes: i64,
    max_process_count: usize,
}

/// What a window's samples show of one process, in the older diagnostics view
/// (`ServerProcessResourceHistorySummary`).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessHistorySummary {
    /// The pid and the start time, joined by a colon.
    process_key: String,
    pid: u32,
    ppid: u32,
    command: String,
    depth: usize,
    is_server_root: bool,
    first_seen_at: String,
    last_seen_at: String,
    current_cpu_percent: f64,
    avg_cpu_percent: f64,
    max_cpu_percent: f64,
    cpu_seconds_approx: f64,
    current_rss_bytes: i64,
    max_rss_bytes: i64,
    sample_count: u64,
}

/// `server.getProcessResourceHistory` (`ServerProcessResourceHistoryResult`).
/// Reading the samples kept in memory cannot fail, so there is never an error.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessHistory {
    read_at: String,
    window_ms: i64,
    bucket_ms: i64,
    sample_interval_ms: u64,
    retained_sample_count: usize,
    total_cpu_seconds_approx: f64,
    buckets: Vec<ProcessHistoryBucket>,
    top_processes: Vec<ProcessHistorySummary>,
    error: TaggedOption<Never>,
}

/// `server.getProcessResourceHistory` (the older diagnostics view of the same samples).
pub fn process_history(window_ms: i64, bucket_ms: i64) -> ProcessHistory {
    let samples = history().window(window_ms);
    let (buckets, procs) = summarize(&samples, window_ms, bucket_ms);
    let buckets = buckets
        .into_iter()
        .map(|b| ProcessHistoryBucket {
            started_at: b.started_at,
            ended_at: b.ended_at,
            avg_cpu_percent: b.avg_cpu_percent,
            max_cpu_percent: b.max_cpu_percent,
            max_rss_bytes: b.max_rss_bytes,
            max_process_count: b.max_process_count,
        })
        .collect();
    let interval = SAMPLE_INTERVAL_MS as f64 / 1000.0;
    let top_processes = procs
        .iter()
        .map(|p| ProcessHistorySummary {
            process_key: format!("{}:{}", p.last.identity.pid, p.last.identity.start_time_ms),
            pid: p.last.identity.pid,
            ppid: p.last.ppid,
            command: p.last.command.clone(),
            depth: p.last.depth,
            is_server_root: p.last.category == ProcessCategory::Server,
            first_seen_at: p.first.first_seen_at.clone(),
            last_seen_at: p.last.last_seen_at.clone(),
            current_cpu_percent: p.last.cpu_percent,
            avg_cpu_percent: p.avg_cpu_percent(),
            max_cpu_percent: p.cpu_max,
            cpu_seconds_approx: p.cpu_sum / 100.0 * interval,
            current_rss_bytes: p.last.resident_bytes,
            max_rss_bytes: p.rss_max,
            sample_count: p.samples,
        })
        .collect();
    let total_cpu: f64 = samples.iter().map(|(_, s)| sample_cpu_percent(s) / 100.0 * interval).sum();
    ProcessHistory {
        read_at: iso_from_millis(now_millis()),
        window_ms,
        bucket_ms,
        sample_interval_ms: SAMPLE_INTERVAL_MS,
        retained_sample_count: samples.len(),
        total_cpu_seconds_approx: total_cpu,
        buckets,
        top_processes,
        error: TaggedOption::None,
    }
}

/// One fresh sample of the server's process tree.
pub async fn sample_now() -> Option<Snapshot> {
    tokio::task::spawn_blocking(|| {
        let sys = System::new().ok()?;
        Some(sample(sys, &mut Model::default(), std::process::id()).0)
    })
    .await
    .ok()
    .flatten()
}

/// A snapshot with no processes, for when sampling is unavailable.
pub fn empty_snapshot() -> Snapshot {
    let (processes, groups) = Model::default().merge(&[], std::process::id(), now_millis());
    Snapshot::new(iso_from_millis(now_millis()), processes, groups, Health::starting())
}

/// `server.retryResourceTelemetry` (`ResourceTelemetryRetryResult`).
#[derive(Debug, Serialize)]
pub struct Retried {
    /// Whether a sample could be taken.
    accepted: bool,
    snapshot: Snapshot,
}

/// `server.retryResourceTelemetry`: a fresh sample, or an empty one where sampling is unavailable.
pub async fn retry() -> Retried {
    match sample_now().await {
        Some(snapshot) => Retried { accepted: true, snapshot },
        None => Retried { accepted: false, snapshot: empty_snapshot() },
    }
}

/// A measure as the client's record holds it. What is measured here is finite.
fn number(v: f64) -> Number {
    Number::from_f64(v).unwrap_or_else(|| Number::from(0))
}

/// `server.getProcessDiagnostics`: the server's process tree right now.
pub async fn process_diagnostics() -> ServerProcessDiagnosticsResult {
    let server_pid = u64::from(std::process::id());
    let read_at = Timestamp::from(iso_from_millis(now_millis()));
    let Some(snapshot) = sample_now().await else {
        let error = ServerProcessDiagnosticsError { message: "Process diagnostics are unavailable on this platform.".into() };
        return ServerProcessDiagnosticsResult {
            server_pid, read_at, process_count: 0, total_rss_bytes: 0, total_cpu_percent: number(0.0), processes: vec![], error: TaggedOption::Some { value: error },
        };
    };
    let processes: Vec<ServerProcessDiagnosticsEntry> = snapshot
        .processes
        .iter()
        .map(|p| {
            let pgid = process_group(i64::from(p.identity.pid));
            let secs = p.run_time_ms / 1000;
            ServerProcessDiagnosticsEntry {
                pid: u64::from(p.identity.pid),
                start_time_ms: p.identity.start_time_ms as u64,
                ppid: u64::from(p.ppid),
                pgid: if pgid >= 0 { TaggedOption::Some { value: pgid } } else { TaggedOption::None },
                status: p.status.clone(),
                cpu_percent: number(p.cpu_percent),
                rss_bytes: p.resident_bytes as u64,
                elapsed: format!("{:02}:{:02}:{:02}", secs / 3600, (secs / 60) % 60, secs % 60),
                command: if p.command.is_empty() { p.name.clone() } else { p.command.clone() },
                depth: p.depth as u64,
                child_pids: p.child_pids.iter().map(|pid| u64::from(*pid)).collect(),
            }
        })
        .collect();
    ServerProcessDiagnosticsResult {
        server_pid,
        read_at,
        process_count: processes.len() as u64,
        total_rss_bytes: snapshot.processes.iter().map(|p| p.resident_bytes).sum::<i64>() as u64,
        total_cpu_percent: number(snapshot.processes.iter().map(|p| p.cpu_percent).sum::<f64>()),
        processes,
        error: TaggedOption::None,
    }
}

/// The signal `server.signalProcess` sends (`ServerProcessSignal`). One it
/// does not know is sent as an interrupt and named in the answer as it came.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Signal {
    #[serde(rename = "SIGINT")]
    Interrupt,
    #[serde(rename = "SIGKILL")]
    Kill,
    #[serde(untagged)]
    Other(String),
}

/// `server.signalProcess` (`ServerSignalProcessResult`): whether the signal was sent, and why not.
#[derive(Debug, Serialize)]
pub struct Signaled {
    pid: i64,
    signal: Signal,
    signaled: bool,
    message: TaggedOption<String>,
}

/// `server.signalProcess`: only descendants of the server, identified by pid and start time.
pub async fn signal_process(pid: i64, start_time_ms: i64, signal: Signal) -> Signaled {
    let refused = |signal: Signal, why: &str| Signaled { pid, signal, signaled: false, message: TaggedOption::Some { value: why.to_owned() } };
    if pid == std::process::id() as i64 {
        return refused(signal, "The server process cannot be signaled from here.");
    }
    let Some(snapshot) = sample_now().await else { return refused(signal, "Process diagnostics are unavailable.") };
    let found = snapshot
        .processes
        .iter()
        .any(|p| i64::from(p.identity.pid) == pid && p.identity.start_time_ms == start_time_ms && p.category != ProcessCategory::Server);
    if !found {
        return refused(signal, "The process is not a descendant of the server (or has exited).");
    }
    match send_signal(pid, signal == Signal::Kill) {
        Ok(()) => Signaled { pid, signal, signaled: true, message: TaggedOption::None },
        Err(message) => refused(signal, &message),
    }
}

#[cfg(unix)]
fn process_group(pid: i64) -> i64 {
    unsafe { libc::getpgid(pid as libc::pid_t) as i64 }
}

/// Windows has no process groups.
#[cfg(not(unix))]
fn process_group(_: i64) -> i64 {
    -1
}

#[cfg(unix)]
fn send_signal(pid: i64, kill: bool) -> Result<(), String> {
    let sig = if kill { libc::SIGKILL } else { libc::SIGINT };
    if unsafe { libc::kill(pid as libc::pid_t, sig) } == 0 { Ok(()) } else { Err(std::io::Error::last_os_error().to_string()) }
}

/// Windows has no SIGINT for another process: both end it, `kill` by force.
#[cfg(not(unix))]
fn send_signal(pid: i64, kill: bool) -> Result<(), String> {
    let mut cmd = crate::process::blocking_command("taskkill");
    cmd.args(["/PID", &pid.to_string()]);
    if kill {
        cmd.arg("/F");
    }
    match cmd.output() {
        Ok(out) if out.status.success() => Ok(()),
        Ok(out) => Err(String::from_utf8_lossy(&out.stderr).trim().to_owned()),
        Err(e) => Err(e.to_string()),
    }
}

/// `server.getHostResources`.
pub async fn host_resources() -> HostResourcesSnapshot {
    tokio::task::spawn_blocking(|| {
        let mut sys = System::new().ok()?;
        sys.refresh_cpu_usage();
        std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
        sys.refresh_cpu_usage();
        sys.refresh_memory();
        Some(HostResourcesSnapshot {
            sampled_at: now_millis() as u64,
            cpu_utilization: Number::from_f64(f64::from(sys.global_cpu_usage()) / 100.0),
            cpu_count: sys.cpus().len() as u64,
            available_memory_bytes: sys.available_memory(),
            total_memory_bytes: sys.total_memory(),
        })
    })
    .await
    .ok()
    .flatten()
    .unwrap_or_else(|| HostResourcesSnapshot { sampled_at: now_millis() as u64, cpu_utilization: None, cpu_count: 0, available_memory_bytes: 0, total_memory_bytes: 0 })
}

pub fn register(r: Router<ConnCtx>) -> Router<ConnCtx> {
    r.stream("subscribeResourceTelemetry", |_, _: IgnoredAny| telemetry_stream())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    fn raw(pid: u32, ppid: u32, cpu_time_ms: i64, rss: i64) -> RawProcess {
        RawProcess { pid, ppid, start_time_ms: 1_000, name: format!("p{pid}"), cpu_time_ms, resident_bytes: rss, ..Default::default() }
    }

    #[test]
    fn tree_order_depth_and_rates() {
        let mut m = Model::default();
        let all = vec![raw(1, 0, 0, 0), raw(10, 1, 100, 50), raw(30, 10, 0, 5), raw(20, 10, 0, 7), raw(21, 20, 0, 1), raw(99, 1, 0, 1)];
        let (procs, groups) = m.merge(&all, 10, 10_000);
        let order: Vec<(u32, usize)> = procs.iter().map(|p| (p.identity.pid, p.depth)).collect();
        assert_eq!(order, vec![(10, 0), (20, 1), (21, 2), (30, 1)]);
        assert_eq!(procs[0].category, ProcessCategory::Server);
        assert_eq!(procs[0].child_pids, [20, 30]);
        assert_eq!(groups.backend.process_starts, 4);
        assert_eq!(groups.backend.current_rss_bytes, 63);
        // One second later the server used 500ms of CPU (50%) and 30 exited.
        let all = vec![raw(10, 1, 600, 40), raw(20, 10, 0, 7), raw(21, 20, 0, 1)];
        let (procs, groups) = m.merge(&all, 10, 11_000);
        assert_eq!(procs[0].cpu_percent, 50.0);
        assert_eq!(procs[0].peak_resident_bytes, 50);
        assert_eq!(groups.all_t3.cpu_time_ms, 500);
        assert_eq!(groups.backend.process_exits, 1);
        assert_eq!(groups.electron.process_count, 0);
        // As the client is sent them.
        assert_eq!(
            json!(procs[1]),
            json!({ "identity": { "pid": 20, "startTimeMs": 1000 }, "ppid": 10, "childPids": [21], "depth": 1, "name": "p20", "command": "", "status": "",
                "category": "server-child", "cpuPercent": 0.0, "cpuTimeMs": 0, "residentBytes": 7, "peakResidentBytes": 7, "virtualBytes": 0, "ioReadBytes": 0,
                "ioWriteBytes": 0, "ioReadBytesPerSecond": 0.0, "ioWriteBytesPerSecond": 0.0, "ioSemantics": "storage", "runTimeMs": 0,
                "firstSeenAt": "1970-01-01T00:00:10.000Z", "lastSeenAt": "1970-01-01T00:00:11.000Z" })
        );
        assert_eq!(
            json!(groups.backend),
            json!({ "processCount": 3, "currentCpuPercent": 50.0, "cpuTimeMs": 500, "currentRssBytes": 48, "peakRssBytes": 58, "ioReadBytes": 0, "ioWriteBytes": 0,
                "ioReadBytesPerSecond": 0.0, "ioWriteBytesPerSecond": 0.0, "processStarts": 4, "processExits": 1 })
        );
        let mut names: Vec<String> = json!(groups).as_object().unwrap().keys().cloned().collect();
        names.sort();
        assert_eq!(names, ["allT3", "backend", "electron", "monitor"]);
    }

    #[test]
    fn samples_this_process() {
        let mut m = Model::default();
        let (snapshot, _) = sample(System::new().unwrap(), &mut m, std::process::id());
        assert_eq!(snapshot.processes[0].category, ProcessCategory::Server);
        assert!(snapshot.processes[0].resident_bytes > 0);
    }

    #[test]
    fn a_snapshot_without_processes_is_written_in_full() {
        let mut snapshot = json!(empty_snapshot());
        let at = snapshot["readAt"].clone();
        let none = json!({ "_tag": "None" });
        let nothing = json!({ "processCount": 0, "currentCpuPercent": -0.0, "cpuTimeMs": 0, "currentRssBytes": 0, "peakRssBytes": 0, "ioReadBytes": 0, "ioWriteBytes": 0,
            "ioReadBytesPerSecond": -0.0, "ioWriteBytesPerSecond": -0.0, "processStarts": 0, "processExits": 0 });
        // A sum of no measures is written `-0.0`, as it always was.
        assert_eq!(snapshot["groups"]["backend"]["currentCpuPercent"].to_string(), "-0.0");
        assert_eq!(snapshot["groups"].take(), json!({ "backend": nothing, "electron": nothing, "monitor": nothing, "allT3": nothing }));
        assert_eq!(
            snapshot,
            json!({ "readAt": at, "sampleIntervalMs": 1000, "processes": [], "groups": null,
                "power": { "source": "unknown", "idle": "unknown", "idleSeconds": null, "locked": "unknown", "suspended": false, "onBattery": "unknown",
                    "lowPowerMode": "unknown", "thermalState": "unknown", "stale": true, "updatedAt": at },
                "speedLimitPercent": none, "attribution": { "readAt": at, "entries": [] },
                "health": { "native": { "status": "starting", "lastSampleAt": none, "lastError": none }, "desktop": { "status": "unavailable", "lastSampleAt": none, "lastError": none },
                    "sidecarVersion": none, "sidecarPid": none, "restartCount": 0, "collectionDurationMicros": 0, "scannedProcessCount": 0, "retainedProcessCount": 0,
                    "inaccessibleProcessCount": 0 } })
        );
        let sampled = json!(Health::sampled("2026-01-02T03:04:05.678Z", 7, 9, 3));
        assert_eq!(sampled["native"], json!({ "status": "healthy", "lastSampleAt": { "_tag": "Some", "value": "2026-01-02T03:04:05.678Z" }, "lastError": none }));
        assert_eq!((&sampled["collectionDurationMicros"], &sampled["scannedProcessCount"], &sampled["retainedProcessCount"]), (&json!(7), &json!(9), &json!(3)));
    }

    #[tokio::test]
    async fn signals_are_refused_for_the_server_and_for_strangers() {
        let me = i64::from(std::process::id());
        let own = signal_process(me, 0, Signal::Interrupt).await;
        assert_eq!(
            json!(own),
            json!({ "pid": me, "signal": "SIGINT", "signaled": false, "message": { "_tag": "Some", "value": "The server process cannot be signaled from here." } })
        );
        let stranger = signal_process(999_999, 5, Signal::Kill).await;
        assert_eq!(
            json!(stranger),
            json!({ "pid": 999_999, "signal": "SIGKILL", "signaled": false,
                "message": { "_tag": "Some", "value": "The process is not a descendant of the server (or has exited)." } })
        );
        assert_eq!(json!(Signaled { pid: 7, signal: Signal::Kill, signaled: true, message: TaggedOption::None }), json!({ "pid": 7, "signal": "SIGKILL", "signaled": true, "message": { "_tag": "None" } }));
        let signal = |sent: Value| serde_json::from_value::<Signal>(sent).unwrap();
        assert_eq!((signal(json!("SIGINT")), signal(json!("SIGKILL")), signal(json!("SIGHUP"))), (Signal::Interrupt, Signal::Kill, Signal::Other("SIGHUP".into())));
        assert_eq!(json!(Signal::Other("SIGHUP".into())), json!("SIGHUP"));
    }

    /// A process with every measure given, and a run time of 01:02:05.
    fn measured(pid: u32, ppid: u32, cpu_time_ms: i64, rss: i64, read: i64, write: i64) -> RawProcess {
        RawProcess {
            pid, ppid, start_time_ms: 1_000, name: format!("p{pid}"), command: if pid == 20 { String::new() } else { format!("p{pid} --flag") }, status: "Run".into(),
            cpu_percent: 1.5, cpu_time_ms, resident_bytes: rss, virtual_bytes: rss * 10, io_read_bytes: read, io_write_bytes: write, run_time_ms: 3_725_000,
        }
    }

    // The sampler `history()` starts is never polled here: nothing awaits, so the samples are the two pushed.
    #[tokio::test(flavor = "current_thread")]
    async fn history_is_written_as_the_client_reads_it() {
        let none = json!({ "_tag": "None" });
        let empty = json!(process_history(60_000, 10_000));
        assert_eq!((&empty["retainedSampleCount"], &empty["topProcesses"], &empty["error"]), (&json!(0), &json!([]), &none));
        assert_eq!(empty["totalCpuSecondsApprox"].to_string(), "-0.0", "a sum of no samples, as it always was written");
        assert_eq!(empty["buckets"].as_array().unwrap().len(), 6);
        assert_eq!(json!(telemetry_history(0, 500))["buckets"].as_array().unwrap().len(), 1, "an empty window is one bucket");

        let samples = [
            (vec![measured(1, 0, 0, 0, 0, 0), measured(10, 1, 100, 50, 10, 20), measured(20, 10, 0, 7, 0, 0)], 10_000),
            (vec![measured(10, 1, 600, 40, 1_010, 2_020), measured(20, 10, 30, 7, 0, 0)], 11_000),
        ];
        let mut m = Model::default();
        for (all, at) in samples {
            let (processes, groups) = m.merge(&all, 10, at);
            let read_at = iso_from_millis(at);
            let health = Health::sampled(&read_at, 7, all.len(), processes.len());
            history().push(Snapshot::new(read_at, processes, groups, health));
        }
        // Buckets end before now: the samples are in the last one once the clock has moved on.
        std::thread::sleep(Duration::from_millis(5));

        let telemetry = json!(telemetry_history(60_000, 20_000));
        let (first_seen, last_seen) = ("1970-01-01T00:00:10.000Z", "1970-01-01T00:00:11.000Z");
        assert_eq!(
            telemetry["topProcesses"],
            json!([
                { "identity": { "pid": 10, "startTimeMs": 1000 }, "ppid": 1, "depth": 0, "name": "p10", "command": "p10 --flag", "category": "server",
                  "firstSeenAt": first_seen, "lastSeenAt": last_seen, "currentCpuPercent": 50.0, "avgCpuPercent": 25.75, "maxCpuPercent": 50.0, "cpuTimeMs": 600,
                  "currentRssBytes": 40, "peakRssBytes": 50, "ioReadBytes": 1010, "ioWriteBytes": 2020, "ioSemantics": "storage", "sampleCount": 2 },
                { "identity": { "pid": 20, "startTimeMs": 1000 }, "ppid": 10, "depth": 1, "name": "p20", "command": "", "category": "server-child",
                  "firstSeenAt": first_seen, "lastSeenAt": last_seen, "currentCpuPercent": 3.0, "avgCpuPercent": 2.25, "maxCpuPercent": 3.0, "cpuTimeMs": 30,
                  "currentRssBytes": 7, "peakRssBytes": 7, "ioReadBytes": 0, "ioWriteBytes": 0, "ioSemantics": "storage", "sampleCount": 2 },
            ])
        );
        let mut last = telemetry["buckets"][2].clone();
        let (started_at, ended_at) = (last["startedAt"].take(), last["endedAt"].take());
        assert!(started_at.is_string() && ended_at.is_string());
        // The CPU of a bucket has always been 0 (see `sample_cpu_percent`).
        assert_eq!(
            last,
            json!({ "startedAt": null, "endedAt": null, "avgCpuPercent": 0.0, "maxCpuPercent": 0.0, "maxRssBytes": 57, "ioReadBytes": 1000, "ioWriteBytes": 2000, "maxProcessCount": 2 })
        );
        assert_eq!((&telemetry["windowMs"], &telemetry["bucketMs"], &telemetry["sampleIntervalMs"], &telemetry["retainedSampleCount"]), (&json!(60_000), &json!(20_000), &json!(1000), &json!(2)));
        assert_eq!(telemetry["health"]["native"], json!({ "status": "healthy", "lastSampleAt": { "_tag": "Some", "value": last_seen }, "lastError": none }));

        let older = json!(process_history(60_000, 20_000));
        assert_eq!(
            older["topProcesses"],
            json!([
                { "processKey": "10:1000", "pid": 10, "ppid": 1, "command": "p10 --flag", "depth": 0, "isServerRoot": true, "firstSeenAt": first_seen, "lastSeenAt": last_seen,
                  "currentCpuPercent": 50.0, "avgCpuPercent": 25.75, "maxCpuPercent": 50.0, "cpuSecondsApprox": 0.515, "currentRssBytes": 40, "maxRssBytes": 50, "sampleCount": 2 },
                { "processKey": "20:1000", "pid": 20, "ppid": 10, "command": "", "depth": 1, "isServerRoot": false, "firstSeenAt": first_seen, "lastSeenAt": last_seen,
                  "currentCpuPercent": 3.0, "avgCpuPercent": 2.25, "maxCpuPercent": 3.0, "cpuSecondsApprox": 0.045, "currentRssBytes": 7, "maxRssBytes": 7, "sampleCount": 2 },
            ])
        );
        let mut last = older["buckets"][2].clone();
        (last["startedAt"], last["endedAt"]) = (json!(null), json!(null));
        assert_eq!(last, json!({ "startedAt": null, "endedAt": null, "avgCpuPercent": 0.0, "maxCpuPercent": 0.0, "maxRssBytes": 57, "maxProcessCount": 2 }));
        assert_eq!((&older["totalCpuSecondsApprox"], &older["error"]), (&json!(0.0), &none));
    }

    #[tokio::test]
    async fn diagnostics_list_this_process_first() {
        let diagnostics = process_diagnostics().await;
        let me = u64::from(std::process::id());
        assert_eq!((diagnostics.server_pid, diagnostics.processes[0].pid, diagnostics.processes[0].depth), (me, me, 0));
        assert_eq!(diagnostics.process_count, diagnostics.processes.len() as u64);
        let first = json!(diagnostics.processes[0]);
        let mut keys: Vec<&str> = first.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(keys, ["childPids", "command", "cpuPercent", "depth", "elapsed", "pgid", "pid", "ppid", "rssBytes", "startTimeMs", "status"]);
        assert!(first["cpuPercent"].is_f64() && first["rssBytes"].is_u64(), "{first}");
        assert_eq!(first["pgid"]["_tag"], if cfg!(unix) { "Some" } else { "None" });
        assert_eq!(json!(diagnostics.error), json!({ "_tag": "None" }));
        let host = json!(host_resources().await);
        assert!(host["sampledAt"].is_u64() && host["cpuCount"].as_u64().unwrap() > 0 && host["cpuUtilization"].is_f64(), "{host}");
        let retried = json!(retry().await);
        assert_eq!(retried["accepted"], true);
        assert_eq!(retried["snapshot"]["processes"][0]["category"], "server");
    }
}
