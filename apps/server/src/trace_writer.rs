//! The server's trace file (t3code `@t3tools/shared/observability`:
//! `makeLocalFileTracer`, `makeTraceSink`): every closed `tracing` span of
//! Domovoi's own code, one JSON record per line in
//! `<home>/userdata/logs/server.trace.ndjson`, in t3code's `EffectTraceRecord`
//! form (`type: "effect-span"`, ids, start and end in Unix nanoseconds,
//! `durationMs`, attributes, events, `exit`), which `server.getTraceDiagnostics`
//! (`crate::trace_diagnostics`) reads back.
//!
//! - The layer only builds a record and hands it to a writer thread
//!   (`try_send` on a bounded queue: a full queue drops the record and counts
//!   it, it never blocks). The thread serializes, batches for a second (or
//!   256 records) and appends; past 10 MiB the file moves to `.1` (`.1` to
//!   `.2`, ..., the 10th dropped), as t3code's rotating sink does. A record
//!   larger than the cap is dropped.
//! - Spans are recorded at `info` and above from Domovoi's crates
//!   (`domovoi_server`, `domovoi_rpc`), with the events logged inside them
//!   (`effect.logLevel` `Info`/`Warn`/`Error`/..., at most 32 per span). A
//!   span names itself with `otel.name`; `otel.status_code = "ERROR"` with
//!   `otel.status_message` makes its exit a `Failure`, `otel.interrupted` an
//!   `Interrupted`; other fields are attributes (strings cut at 500 characters).
//! - With `observability.otlpTracesUrl` set, spans are also exported as
//!   OTLP/JSON (`{"resourceSpans": [...]}`, the body `server.checkOtlpEndpoint`
//!   sends empty) from a thread of its own every 10 seconds or 512 spans, at
//!   most 2048 waiting (more are dropped), 10 seconds per request. A failed
//!   export is dropped, not retried, and logged once until one succeeds.
//!
//! Limits: no span links; URL credentials in attributes are not redacted
//! (Domovoi's spans carry none); the export carries no `OTEL_*` resource
//! attributes or headers.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;

pub const TRACE_FILE: &str = "server.trace.ndjson";
/// `traceMaxBytes`.
pub const MAX_BYTES: u64 = 10 * 1024 * 1024;
/// `traceMaxFiles`: rotated files kept beside the current one.
pub const MAX_FILES: usize = 10;
/// `traceBatchWindowMs`.
const BATCH_WINDOW: Duration = Duration::from_secs(1);
/// `FLUSH_BUFFER_THRESHOLD`.
const FLUSH_RECORDS: usize = 256;
const QUEUE: usize = 8192;
const MAX_EVENTS: usize = 32;
/// `TRACE_ATTRIBUTE_MAX_LENGTH`.
const MAX_ATTRIBUTE_CHARS: usize = 500;
const OTLP_INTERVAL: Duration = Duration::from_secs(10);
const OTLP_BATCH: usize = 512;
const OTLP_QUEUE: usize = 2048;
const OTLP_TIMEOUT: Duration = Duration::from_secs(10);

/// An attribute's value.
#[derive(Clone, Debug, PartialEq)]
pub enum AttrValue {
    Str(String),
    I64(i64),
    U64(u64),
    F64(f64),
    Bool(bool),
}

impl serde::Serialize for AttrValue {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        match self {
            AttrValue::Str(v) => s.serialize_str(v),
            AttrValue::I64(v) => s.serialize_i64(*v),
            AttrValue::U64(v) => s.serialize_u64(*v),
            AttrValue::F64(v) => s.serialize_f64(*v),
            AttrValue::Bool(v) => s.serialize_bool(*v),
        }
    }
}

/// Attributes in the order they were recorded, written as an object.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Attrs(Vec<(&'static str, AttrValue)>);

impl Attrs {
    fn set(&mut self, key: &'static str, value: AttrValue) {
        match self.0.iter_mut().find(|(k, _)| *k == key) {
            Some(slot) => slot.1 = value,
            None => self.0.push((key, value)),
        }
    }
}

impl serde::Serialize for Attrs {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut m = s.serialize_map(Some(self.0.len()))?;
        for (k, v) in &self.0 {
            m.serialize_entry(k, v)?;
        }
        m.end()
    }
}

/// How a span ended (`EffectTraceRecord.exit`).
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[serde(tag = "_tag")]
pub enum Exit {
    Success,
    Failure { cause: String },
    Interrupted { cause: String },
}

/// A log event inside a span (`TraceRecordEvent`).
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceEvent {
    pub name: String,
    #[serde(serialize_with = "nanos_text")]
    pub time_unix_nano: u128,
    pub attributes: Attrs,
}

/// A closed span (`EffectTraceRecord`).
#[derive(Clone, Debug, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceRecord {
    #[serde(rename = "type")]
    pub ty: &'static str,
    pub name: String,
    pub kind: &'static str,
    #[serde(serialize_with = "hex_text")]
    pub trace_id: [u8; 16],
    #[serde(serialize_with = "hex_text")]
    pub span_id: [u8; 8],
    #[serde(skip_serializing_if = "Option::is_none", serialize_with = "hex_option")]
    pub parent_span_id: Option<[u8; 8]>,
    pub sampled: bool,
    #[serde(serialize_with = "nanos_text")]
    pub start_time_unix_nano: u128,
    #[serde(serialize_with = "nanos_text")]
    pub end_time_unix_nano: u128,
    pub duration_ms: f64,
    pub attributes: Attrs,
    pub events: Vec<TraceEvent>,
    pub links: [(); 0],
    pub exit: Exit,
}

fn nanos_text<S: serde::Serializer>(n: &u128, s: S) -> Result<S::Ok, S::Error> {
    s.collect_str(n)
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

fn hex_text<S: serde::Serializer, const N: usize>(b: &[u8; N], s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&hex(b))
}

fn hex_option<S: serde::Serializer>(b: &Option<[u8; 8]>, s: S) -> Result<S::Ok, S::Error> {
    match b {
        Some(b) => s.serialize_str(&hex(b)),
        None => s.serialize_none(),
    }
}

fn unix_nanos(t: SystemTime) -> u128 {
    t.duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.as_nanos())
}

/// Random ids without a system call per span: SplitMix64, seeded once per thread.
fn next_random() -> u64 {
    thread_local! {
        static STATE: std::cell::Cell<u64> = std::cell::Cell::new(u64::from_le_bytes(crate::util::random_bytes::<8>()));
    }
    STATE.with(|s| {
        let mut z = s.get().wrapping_add(0x9E37_79B9_7F4A_7C15);
        s.set(z);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    })
}

fn cut(mut s: String) -> String {
    if let Some((i, _)) = s.char_indices().nth(MAX_ATTRIBUTE_CHARS) {
        s.truncate(i);
        s.push_str("…[truncated]");
    }
    s
}

/// What a span holds while it is open.
struct Open {
    name: String,
    trace_id: [u8; 16],
    span_id: [u8; 8],
    parent_span_id: Option<[u8; 8]>,
    start: SystemTime,
    started: Instant,
    attributes: Attrs,
    events: Vec<TraceEvent>,
    failure: Option<String>,
    failed: bool,
    interrupted: bool,
}

impl Visit for Open {
    fn record_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            "otel.name" => self.name = value.to_owned(),
            "otel.status_code" => self.failed = value.eq_ignore_ascii_case("error"),
            "otel.status_message" => self.failure = Some(cut(value.to_owned())),
            name => self.attributes.set(name, AttrValue::Str(cut(value.to_owned()))),
        }
    }
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.record_str(field, &format!("{value:?}"));
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.attributes.set(field.name(), AttrValue::I64(value));
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.attributes.set(field.name(), AttrValue::U64(value));
    }
    fn record_f64(&mut self, field: &Field, value: f64) {
        self.attributes.set(field.name(), AttrValue::F64(value));
    }
    fn record_bool(&mut self, field: &Field, value: bool) {
        match field.name() {
            "otel.interrupted" => self.interrupted = value,
            name => self.attributes.set(name, AttrValue::Bool(value)),
        }
    }
}

/// An event's message and fields.
struct EventFields {
    message: Option<String>,
    attributes: Attrs,
}

impl Visit for EventFields {
    fn record_str(&mut self, field: &Field, value: &str) {
        match field.name() {
            "message" => self.message = Some(cut(value.to_owned())),
            name => self.attributes.set(name, AttrValue::Str(cut(value.to_owned()))),
        }
    }
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.record_str(field, &format!("{value:?}"));
    }
    fn record_i64(&mut self, field: &Field, value: i64) {
        self.attributes.set(field.name(), AttrValue::I64(value));
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.attributes.set(field.name(), AttrValue::U64(value));
    }
    fn record_f64(&mut self, field: &Field, value: f64) {
        self.attributes.set(field.name(), AttrValue::F64(value));
    }
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.attributes.set(field.name(), AttrValue::Bool(value));
    }
}

/// Effect's name for a level (`effect.logLevel`).
fn level_label(level: &tracing::Level) -> &'static str {
    match *level {
        tracing::Level::ERROR => "Error",
        tracing::Level::WARN => "Warn",
        tracing::Level::INFO => "Info",
        tracing::Level::DEBUG => "Debug",
        tracing::Level::TRACE => "Trace",
    }
}

/// Where closed spans go.
struct Sinks {
    file: SyncSender<Msg>,
    dropped: AtomicU64,
    otlp: OnceLock<SyncSender<Arc<TraceRecord>>>,
    /// An OTLP endpoint is set: spans are queued for it.
    exporting: AtomicBool,
    otlp_url: Mutex<Option<String>>,
}

static SINKS: OnceLock<Arc<Sinks>> = OnceLock::new();

enum Msg {
    Record(Arc<TraceRecord>),
    /// Write what is buffered, then say so.
    Flush(SyncSender<()>),
}

/// The `tracing` layer writing the trace file.
pub struct TraceLayer {
    sinks: Arc<Sinks>,
}

impl<S> Layer<S> for TraceLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else { return };
        let parent = span.parent().and_then(|p| p.extensions().get::<Open>().map(|o| (o.trace_id, o.span_id)));
        let (trace_id, parent_span_id) = match parent {
            Some((t, s)) => (t, Some(s)),
            None => {
                let mut t = [0u8; 16];
                t[..8].copy_from_slice(&next_random().to_le_bytes());
                t[8..].copy_from_slice(&next_random().to_le_bytes());
                (t, None)
            }
        };
        let mut open = Open {
            name: attrs.metadata().name().to_owned(),
            trace_id,
            span_id: next_random().to_le_bytes(),
            parent_span_id,
            start: SystemTime::now(),
            started: Instant::now(),
            attributes: Attrs::default(),
            events: vec![],
            failure: None,
            failed: false,
            interrupted: false,
        };
        attrs.record(&mut open);
        span.extensions_mut().insert(open);
    }

    fn on_record(&self, id: &Id, values: &Record<'_>, ctx: Context<'_, S>) {
        if let Some(span) = ctx.span(id)
            && let Some(open) = span.extensions_mut().get_mut::<Open>()
        {
            values.record(open);
        }
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let Some(span) = ctx.event_span(event) else { return };
        let mut ext = span.extensions_mut();
        let Some(open) = ext.get_mut::<Open>() else { return };
        if open.events.len() >= MAX_EVENTS {
            return;
        }
        let mut fields = EventFields { message: None, attributes: Attrs::default() };
        event.record(&mut fields);
        fields.attributes.0.insert(0, ("effect.logLevel", AttrValue::Str(level_label(event.metadata().level()).to_owned())));
        open.events.push(TraceEvent { name: fields.message.unwrap_or_else(|| event.metadata().name().to_owned()), time_unix_nano: unix_nanos(SystemTime::now()), attributes: fields.attributes });
    }

    fn on_close(&self, id: Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(&id) else { return };
        let Some(open) = span.extensions_mut().remove::<Open>() else { return };
        let elapsed = open.started.elapsed();
        let start = unix_nanos(open.start);
        let exit = if open.interrupted {
            Exit::Interrupted { cause: open.failure.unwrap_or_else(|| "Interrupted".into()) }
        } else if open.failed {
            Exit::Failure { cause: open.failure.unwrap_or_else(|| "Failure".into()) }
        } else {
            Exit::Success
        };
        let record = Arc::new(TraceRecord {
            ty: "effect-span",
            name: open.name,
            kind: "internal",
            trace_id: open.trace_id,
            span_id: open.span_id,
            parent_span_id: open.parent_span_id,
            sampled: true,
            start_time_unix_nano: start,
            end_time_unix_nano: start + elapsed.as_nanos(),
            duration_ms: elapsed.as_secs_f64() * 1000.0,
            attributes: open.attributes,
            events: open.events,
            links: [],
            exit,
        });
        self.sinks.push(record);
    }
}

impl Sinks {
    fn push(&self, record: Arc<TraceRecord>) {
        if self.exporting.load(Ordering::Relaxed)
            && let Some(otlp) = self.otlp.get()
        {
            let _ = otlp.try_send(record.clone());
        }
        if let Err(TrySendError::Full(_)) = self.file.try_send(Msg::Record(record)) {
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// The trace file's layer, its writer thread started; `None` when the folder can't be made.
pub fn layer(logs: &Path) -> Option<TraceLayer> {
    std::fs::create_dir_all(logs).ok()?;
    let path = logs.join(TRACE_FILE);
    let (tx, rx) = std::sync::mpsc::sync_channel(QUEUE);
    std::thread::Builder::new().name("trace-writer".into()).spawn(move || write_loop(&path, &rx, MAX_BYTES, MAX_FILES, BATCH_WINDOW)).ok()?;
    let sinks = Arc::new(Sinks { file: tx, dropped: AtomicU64::new(0), otlp: OnceLock::new(), exporting: AtomicBool::new(false), otlp_url: Mutex::new(None) });
    let _ = SINKS.set(sinks.clone());
    Some(TraceLayer { sinks })
}

/// Whether this process writes the trace file (`localTracingEnabled`).
pub fn enabled() -> bool {
    SINKS.get().is_some()
}

/// Writes what is buffered, waiting at most a second (at shutdown).
pub fn flush() {
    let Some(s) = SINKS.get() else { return };
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    if s.file.try_send(Msg::Flush(tx)).is_ok() {
        let _ = rx.recv_timeout(Duration::from_secs(1));
    }
}

/// Moves `path` to `.1`, `.1` to `.2` and so on, dropping the `max_files`th.
fn rotate(path: &Path, max_files: usize) {
    let with = |i: usize| PathBuf::from(format!("{}.{i}", path.display()));
    let _ = std::fs::remove_file(with(max_files));
    for i in (1..max_files).rev() {
        let _ = std::fs::rename(with(i), with(i + 1));
    }
    let _ = std::fs::rename(path, with(1));
}

fn write_loop(path: &Path, rx: &Receiver<Msg>, max_bytes: u64, max_files: usize, window: Duration) {
    let mut size = std::fs::metadata(path).map_or(0, |m| m.len());
    let mut buffer: Vec<Vec<u8>> = vec![];
    let mut deadline: Option<Instant> = None;
    let mut acks: Vec<SyncSender<()>> = vec![];
    let mut failing = false;
    loop {
        let msg = match deadline {
            None => rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
            Some(d) => rx.recv_timeout(d.saturating_duration_since(Instant::now())),
        };
        let closed = match msg {
            Ok(Msg::Record(r)) => {
                if let Ok(mut line) = serde_json::to_vec(&*r) {
                    line.push(b'\n');
                    if (line.len() as u64) <= max_bytes {
                        buffer.push(line);
                    }
                }
                deadline.get_or_insert_with(|| Instant::now() + window);
                false
            }
            Ok(Msg::Flush(ack)) => {
                acks.push(ack);
                false
            }
            Err(RecvTimeoutError::Timeout) => false,
            Err(RecvTimeoutError::Disconnected) => true,
        };
        let due = closed || !acks.is_empty() || buffer.len() >= FLUSH_RECORDS || deadline.is_some_and(|d| Instant::now() >= d);
        if !due {
            continue;
        }
        deadline = None;
        // In chunks that fit a file, rotating before one would not.
        let mut chunk: Vec<u8> = vec![];
        let records = std::mem::take(&mut buffer);
        let mut write = |chunk: &mut Vec<u8>, size: &mut u64| {
            if chunk.is_empty() {
                return;
            }
            if *size > 0 && *size + chunk.len() as u64 > max_bytes {
                rotate(path, max_files);
                *size = 0;
            }
            let wrote = std::fs::OpenOptions::new().create(true).append(true).open(path).and_then(|mut f| f.write_all(chunk));
            match wrote {
                Ok(()) => {
                    *size += chunk.len() as u64;
                    failing = false;
                }
                // Dropped rather than kept, so a full disk does not grow a backlog.
                Err(e) if !failing => {
                    failing = true;
                    eprintln!("trace file {}: {e}; dropping records until it can be written", path.display());
                }
                Err(_) => {}
            }
            chunk.clear();
        };
        for line in records {
            if !chunk.is_empty() && chunk.len() + line.len() > max_bytes as usize {
                write(&mut chunk, &mut size);
            }
            chunk.extend_from_slice(&line);
        }
        write(&mut chunk, &mut size);
        for ack in acks.drain(..) {
            let _ = ack.try_send(());
        }
        if closed {
            return;
        }
    }
}

// ---- OTLP/JSON export ----

/// Sets where spans are exported (`observability.otlpTracesUrl`); `None` stops it.
pub fn set_otlp_traces_url(url: Option<String>) {
    let Some(s) = SINKS.get() else { return };
    let url = url.map(|u| u.trim().to_owned()).filter(|u| !u.is_empty());
    if url.is_some() && s.otlp.get().is_none() {
        let (tx, rx) = std::sync::mpsc::sync_channel(OTLP_QUEUE);
        let sinks = s.clone();
        if std::thread::Builder::new().name("otlp-traces".into()).spawn(move || export_loop(&sinks, &rx)).is_ok() {
            let _ = s.otlp.set(tx);
        }
    }
    s.exporting.store(url.is_some(), Ordering::Relaxed);
    *crate::util::locked(&s.otlp_url) = url;
}

/// The OTLP endpoint spans go to, when there is one.
pub fn otlp_traces_url() -> Option<String> {
    SINKS.get().and_then(|s| crate::util::locked(&s.otlp_url).clone())
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct OtlpKeyValue<'a> {
    key: &'a str,
    value: OtlpAnyValue<'a>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
enum OtlpAnyValue<'a> {
    StringValue(&'a str),
    /// int64 as a decimal string, as OTLP/JSON writes it.
    IntValue(String),
    DoubleValue(f64),
    BoolValue(bool),
}

fn otlp_attributes(a: &Attrs) -> Vec<OtlpKeyValue<'_>> {
    a.0.iter()
        .map(|(k, v)| OtlpKeyValue {
            key: k,
            value: match v {
                AttrValue::Str(s) => OtlpAnyValue::StringValue(s),
                AttrValue::I64(n) => OtlpAnyValue::IntValue(n.to_string()),
                AttrValue::U64(n) => OtlpAnyValue::IntValue(n.to_string()),
                AttrValue::F64(n) => OtlpAnyValue::DoubleValue(*n),
                AttrValue::Bool(b) => OtlpAnyValue::BoolValue(*b),
            },
        })
        .collect()
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct OtlpEvent<'a> {
    time_unix_nano: String,
    name: &'a str,
    attributes: Vec<OtlpKeyValue<'a>>,
}

#[derive(serde::Serialize)]
struct OtlpStatus<'a> {
    /// 0 unset, 2 error.
    code: u8,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<&'a str>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct OtlpSpan<'a> {
    trace_id: String,
    span_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent_span_id: Option<String>,
    name: &'a str,
    /// `SPAN_KIND_INTERNAL`.
    kind: u8,
    start_time_unix_nano: String,
    end_time_unix_nano: String,
    attributes: Vec<OtlpKeyValue<'a>>,
    events: Vec<OtlpEvent<'a>>,
    status: OtlpStatus<'a>,
}

#[derive(serde::Serialize)]
struct OtlpScope {
    name: &'static str,
    version: &'static str,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct OtlpScopeSpans<'a> {
    scope: OtlpScope,
    spans: Vec<OtlpSpan<'a>>,
}

#[derive(serde::Serialize)]
struct OtlpResource<'a> {
    attributes: Vec<OtlpKeyValue<'a>>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct OtlpResourceSpans<'a> {
    resource: OtlpResource<'a>,
    scope_spans: Vec<OtlpScopeSpans<'a>>,
}

#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct OtlpExport<'a> {
    resource_spans: Vec<OtlpResourceSpans<'a>>,
}

/// The OTLP/JSON body of `records` (`ExportTraceServiceRequest`).
fn otlp_body(records: &[Arc<TraceRecord>]) -> Vec<u8> {
    let spans = records
        .iter()
        .map(|r| OtlpSpan {
            trace_id: hex(&r.trace_id),
            span_id: hex(&r.span_id),
            parent_span_id: r.parent_span_id.map(|p| hex(&p)),
            name: &r.name,
            kind: 1,
            start_time_unix_nano: r.start_time_unix_nano.to_string(),
            end_time_unix_nano: r.end_time_unix_nano.to_string(),
            attributes: otlp_attributes(&r.attributes),
            events: r.events.iter().map(|e| OtlpEvent { time_unix_nano: e.time_unix_nano.to_string(), name: &e.name, attributes: otlp_attributes(&e.attributes) }).collect(),
            status: match &r.exit {
                Exit::Failure { cause } => OtlpStatus { code: 2, message: Some(cause) },
                Exit::Interrupted { cause } => OtlpStatus { code: 2, message: Some(cause) },
                Exit::Success => OtlpStatus { code: 0, message: None },
            },
        })
        .collect();
    let export = OtlpExport {
        resource_spans: vec![OtlpResourceSpans {
            resource: OtlpResource {
                attributes: vec![
                    OtlpKeyValue { key: "service.name", value: OtlpAnyValue::StringValue("domovoi-server") },
                    OtlpKeyValue { key: "service.version", value: OtlpAnyValue::StringValue(env!("CARGO_PKG_VERSION")) },
                ],
            },
            scope_spans: vec![OtlpScopeSpans { scope: OtlpScope { name: "domovoi-server", version: env!("CARGO_PKG_VERSION") }, spans }],
        }],
    };
    serde_json::to_vec(&export).unwrap_or_default()
}

fn export_loop(sinks: &Sinks, rx: &Receiver<Arc<TraceRecord>>) {
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread().enable_all().build() else { return };
    // The client's connector needs a runtime around it.
    let _inside = runtime.enter();
    let _ = rustls::crypto::ring::default_provider().install_default();
    let Ok(client) = reqwest::Client::builder().timeout(OTLP_TIMEOUT).build() else { return };
    let mut batch: VecDeque<Arc<TraceRecord>> = VecDeque::new();
    let mut due = Instant::now() + OTLP_INTERVAL;
    let mut failing = false;
    loop {
        match rx.recv_timeout(due.saturating_duration_since(Instant::now())) {
            Ok(r) => {
                batch.push_back(r);
                if batch.len() > OTLP_QUEUE {
                    batch.pop_front();
                }
                if batch.len() < OTLP_BATCH {
                    continue;
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
        due = Instant::now() + OTLP_INTERVAL;
        let url = crate::util::locked(&sinks.otlp_url).clone();
        let Some(url) = url else {
            batch.clear();
            continue;
        };
        while !batch.is_empty() {
            let take: Vec<Arc<TraceRecord>> = batch.drain(..batch.len().min(OTLP_BATCH)).collect();
            let body = otlp_body(&take);
            let sent = runtime.block_on(client.post(&url).header(reqwest::header::CONTENT_TYPE, "application/json").body(body).send());
            match sent {
                Ok(r) if r.status().is_success() => failing = false,
                outcome => {
                    if !failing {
                        failing = true;
                        let why = outcome.map_or_else(|e| e.to_string(), |r| format!("status {}", r.status()));
                        eprintln!("OTLP trace export to {url} failed ({why}); dropping spans until it succeeds");
                    }
                    batch.clear();
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::layer::SubscriberExt;

    fn record(name: &str, exit: Exit) -> Arc<TraceRecord> {
        Arc::new(TraceRecord {
            ty: "effect-span",
            name: name.into(),
            kind: "internal",
            trace_id: [1; 16],
            span_id: [2; 8],
            parent_span_id: None,
            sampled: true,
            start_time_unix_nano: 1_700_000_000_000_000_000,
            end_time_unix_nano: 1_700_000_000_500_000_000,
            duration_ms: 500.0,
            attributes: Attrs(vec![("rpc.method", AttrValue::Str("x".into())), ("n", AttrValue::I64(-3))]),
            events: vec![],
            links: [],
            exit,
        })
    }

    #[test]
    fn records_spans_with_their_events_and_exits() {
        let (tx, rx) = std::sync::mpsc::sync_channel(64);
        let sinks = Arc::new(Sinks { file: tx, dropped: AtomicU64::new(0), otlp: OnceLock::new(), exporting: AtomicBool::new(false), otlp_url: Mutex::new(None) });
        let subscriber = tracing_subscriber::registry().with(TraceLayer { sinks });
        tracing::subscriber::with_default(subscriber, || {
            let outer = tracing::info_span!("ws.rpc", "otel.name" = "ws.rpc.server.getConfig", "rpc.method" = "server.getConfig", "otel.status_code" = tracing::field::Empty, "otel.status_message" = tracing::field::Empty);
            outer.in_scope(|| {
                tracing::warn!(attempt = 2, "slow disk");
                let inner = tracing::info_span!("inner", n = 5u64);
                inner.in_scope(|| tracing::info!("inside"));
            });
            outer.record("otel.status_code", "ERROR");
            outer.record("otel.status_message", "ServerError: no");
        });
        let got: Vec<Arc<TraceRecord>> = rx.try_iter().filter_map(|m| if let Msg::Record(r) = m { Some(r) } else { None }).collect();
        assert_eq!(got.len(), 2);
        let (inner, outer) = (&got[0], &got[1]);
        assert_eq!((inner.name.as_str(), inner.parent_span_id, inner.trace_id), ("inner", Some(outer.span_id), outer.trace_id));
        assert_eq!(inner.exit, Exit::Success);
        assert_eq!(outer.name, "ws.rpc.server.getConfig");
        assert_eq!(outer.exit, Exit::Failure { cause: "ServerError: no".into() });
        assert_eq!(outer.attributes.0, vec![("rpc.method", AttrValue::Str("server.getConfig".into()))]);
        assert_eq!(outer.events.len(), 1);
        assert_eq!(outer.events[0].name, "slow disk");
        assert_eq!(outer.events[0].attributes.0, vec![("effect.logLevel", AttrValue::Str("Warn".into())), ("attempt", AttrValue::I64(2))]);
        assert!(outer.end_time_unix_nano >= outer.start_time_unix_nano && outer.duration_ms >= 0.0);
        let line = serde_json::to_value(&**outer).unwrap();
        assert_eq!(line["type"], "effect-span");
        assert_eq!(line["traceId"].as_str().map(str::len), Some(32));
        assert_eq!(line["spanId"].as_str().map(str::len), Some(16));
        assert!(line["startTimeUnixNano"].is_string() && line.get("parentSpanId").is_none());
        assert_eq!(line["exit"], serde_json::json!({ "_tag": "Failure", "cause": "ServerError: no" }));
        assert_eq!(line["links"], serde_json::json!([]));
    }

    #[test]
    fn the_file_rotates_past_its_cap() {
        let dir = std::env::temp_dir().join(format!("domovoi-trace-{}", crate::util::new_uuid()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(TRACE_FILE);
        let (tx, rx) = std::sync::mpsc::sync_channel(1024);
        for i in 0..40 {
            tx.send(Msg::Record(record(&format!("span-{i:02}"), Exit::Success))).unwrap();
        }
        drop(tx);
        let one = serde_json::to_vec(&*record("span-00", Exit::Success)).unwrap().len() as u64 + 1;
        write_loop(&path, &rx, one * 10, 3, Duration::from_millis(10));
        let lines = |p: PathBuf| std::fs::read_to_string(p).map(|t| t.lines().count()).unwrap_or(0);
        assert_eq!(lines(path.clone()), 10);
        for i in 1..=3 {
            assert_eq!(lines(PathBuf::from(format!("{}.{i}", path.display()))), 10, ".{i}");
        }
        assert!(!PathBuf::from(format!("{}.4", path.display())).exists(), "only 3 rotated files are kept");
        let newest = std::fs::read_to_string(&path).unwrap();
        assert!(newest.lines().last().unwrap().contains("span-39"));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn exports_as_otlp_json() {
        let body: serde_json::Value = serde_json::from_slice(&otlp_body(&[record("a", Exit::Success), record("b", Exit::Failure { cause: "Boom".into() })])).unwrap();
        let spans = &body["resourceSpans"][0]["scopeSpans"][0]["spans"];
        assert_eq!(body["resourceSpans"][0]["resource"]["attributes"][0], serde_json::json!({ "key": "service.name", "value": { "stringValue": "domovoi-server" } }));
        assert_eq!(spans[0]["traceId"], "01010101010101010101010101010101");
        assert_eq!(spans[0]["startTimeUnixNano"], "1700000000000000000");
        assert_eq!(spans[0]["attributes"][1], serde_json::json!({ "key": "n", "value": { "intValue": "-3" } }));
        assert_eq!(spans[0]["status"], serde_json::json!({ "code": 0 }));
        assert_eq!(spans[1]["status"], serde_json::json!({ "code": 2, "message": "Boom" }));
    }

    #[test]
    fn exports_to_the_endpoint() {
        use std::io::{BufRead, BufReader, Read};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v1/traces", listener.local_addr().unwrap());
        let server = std::thread::spawn(move || {
            let (sock, _) = listener.accept().unwrap();
            sock.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
            let mut reader = BufReader::new(sock.try_clone().unwrap());
            let mut len = 0;
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap() > 0 && line != "\r\n" {
                if let Some(v) = line.to_lowercase().strip_prefix("content-length:") {
                    len = v.trim().parse().unwrap();
                }
                line.clear();
            }
            let mut body = vec![0; len];
            reader.read_exact(&mut body).unwrap();
            std::io::Write::write_all(&mut &sock, b"HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\n{}").unwrap();
            body
        });
        let (tx, rx) = std::sync::mpsc::sync_channel(OTLP_QUEUE);
        let (file, _keep) = std::sync::mpsc::sync_channel(1);
        let sinks = Arc::new(Sinks { file, dropped: AtomicU64::new(0), otlp: OnceLock::new(), exporting: AtomicBool::new(true), otlp_url: Mutex::new(Some(url)) });
        for i in 0..OTLP_BATCH {
            tx.send(record(&format!("s{i}"), Exit::Success)).unwrap();
        }
        let exporter = std::thread::spawn(move || export_loop(&sinks, &rx));
        let body: serde_json::Value = serde_json::from_slice(&server.join().unwrap()).unwrap();
        assert_eq!(body["resourceSpans"][0]["scopeSpans"][0]["spans"].as_array().map(Vec::len), Some(OTLP_BATCH));
        drop(tx);
        exporter.join().unwrap();
    }
}

#[cfg(test)]
mod bench {
    use super::*;
    use tracing_subscriber::layer::SubscriberExt;

    /// One RPC call's span as `domovoi_rpc` makes it: created, entered, a failure recorded, closed.
    fn call(tag: &str, fail: bool) {
        let span = tracing::info_span!(parent: None, "ws.rpc", "otel.name" = %format!("ws.rpc.{tag}"), "rpc.method" = %tag, "rpc.transport" = "websocket",
            "rpc.system" = "domovoi-rpc", "otel.status_code" = tracing::field::Empty, "otel.status_message" = tracing::field::Empty);
        span.in_scope(|| std::hint::black_box(tag.len()));
        if fail {
            span.record("otel.status_code", "ERROR");
            span.record("otel.status_message", "ServerError: no");
        }
    }

    /// `cargo test --release -p domovoi-server --bin domovoi-server trace_writer::bench -- --ignored --nocapture`:
    /// what a call's span costs with the trace file written (a writer thread
    /// appending to a temporary file) against the same code with no trace
    /// layer, in alternating rounds.
    #[test]
    #[ignore]
    fn span_overhead() {
        let dir = std::env::temp_dir().join(format!("domovoi-trace-bench-{}", crate::util::new_uuid()));
        std::fs::create_dir_all(&dir).unwrap();
        let (tx, rx) = std::sync::mpsc::sync_channel(QUEUE);
        let path = dir.join(TRACE_FILE);
        let writer = std::thread::spawn(move || write_loop(&path, &rx, MAX_BYTES, MAX_FILES, BATCH_WINDOW));
        let sinks = Arc::new(Sinks { file: tx, dropped: AtomicU64::new(0), otlp: OnceLock::new(), exporting: AtomicBool::new(false), otlp_url: Mutex::new(None) });
        let traced = tracing_subscriber::registry().with(TraceLayer { sinks: sinks.clone() }.with_filter(tracing_subscriber::filter::Targets::new().with_target("domovoi_server", tracing::Level::INFO)));
        let traced = tracing::Dispatch::new(traced);
        let plain = tracing::Dispatch::new(tracing_subscriber::registry().with(tracing_subscriber::filter::Targets::new().with_target("domovoi_server", tracing::Level::WARN)));
        const N: u32 = 100_000;
        let round = |d: &tracing::Dispatch| {
            tracing::dispatcher::with_default(d, || {
                let t = Instant::now();
                for i in 0..N {
                    call("orchestration.getThreadProjection", i % 50 == 0);
                }
                t.elapsed().as_nanos() as f64 / f64::from(N)
            })
        };
        for pass in 0..6 {
            let (off, on) = if pass % 2 == 0 { let off = round(&plain); (off, round(&traced)) } else { let on = round(&traced); (round(&plain), on) };
            println!("pass {pass}: no trace layer {off:>7.1} ns/call   trace file {on:>7.1} ns/call   (+{:.1} ns; dropped {})", on - off, sinks.dropped.load(Ordering::Relaxed));
            // Let the writer catch up between rounds.
            std::thread::sleep(Duration::from_millis(1500));
        }
        drop(traced);
        drop(sinks);
        let _ = writer.join();
        let _ = std::fs::remove_dir_all(dir);
    }
}
