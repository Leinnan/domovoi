//! `server.getTraceDiagnostics` (t3code `diagnostics/TraceDiagnostics.ts`):
//! a summary of the trace file `crate::trace_writer` writes, read with its
//! rotated files oldest first (`server.trace.ndjson.10` ... `.1`, then the
//! file itself).
//!
//! Each line is a span record. One without a name, trace and span ids, a
//! finite `durationMs` and an end time counts as a parse error. A span failed
//! when its exit is a `Failure` (or its OTLP status code is `"2"`), and was
//! interrupted when its exit is `Interrupted` (or it says so). The summary:
//! counts, the first start and last end, spans of a second or more (slow), the
//! 10 names seen most (with their failures and durations), the 10 slowest
//! spans, the 10 commonest failures by name and cause, the 20 latest failures,
//! log events counted by `effect.logLevel`, and the 20 latest warnings and
//! errors. No file at all is `trace-file-not-found`; one that can't be read is
//! `trace-file-read-failed`, a partial failure when another was read.

use crate::rpc::shell::{if_fits, those_that_fit};
use domovoi_model::typed::TaggedOption;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::io::BufRead;
use std::path::{Path, PathBuf};

/// `DEFAULT_SLOW_SPAN_THRESHOLD_MS`.
pub const SLOW_SPAN_THRESHOLD_MS: u64 = 1_000;
const TOP_LIMIT: usize = 10;
const RECENT_LIMIT: usize = 20;

/// `ServerTraceDiagnosticsErrorKind`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ErrorKind {
    TraceFileNotFound,
    TraceFileReadFailed,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TraceError {
    pub kind: ErrorKind,
    pub message: String,
}

/// `ServerTraceDiagnosticsSpanSummary`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpanSummary {
    pub name: String,
    pub count: u64,
    pub failure_count: u64,
    pub total_duration_ms: f64,
    pub average_duration_ms: f64,
    pub max_duration_ms: f64,
}

/// `ServerTraceDiagnosticsSpanOccurrence`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SpanOccurrence {
    pub name: String,
    pub duration_ms: f64,
    pub ended_at: String,
    pub trace_id: String,
    pub span_id: String,
}

/// `ServerTraceDiagnosticsFailureSummary`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FailureSummary {
    pub name: String,
    pub cause: String,
    pub count: u64,
    pub last_seen_at: String,
    pub trace_id: String,
    pub span_id: String,
}

/// `ServerTraceDiagnosticsRecentFailure`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecentFailure {
    pub name: String,
    pub cause: String,
    pub duration_ms: f64,
    pub ended_at: String,
    pub trace_id: String,
    pub span_id: String,
}

/// `ServerTraceDiagnosticsLogEvent`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LogEvent {
    pub span_name: String,
    pub level: String,
    pub message: String,
    pub seen_at: String,
    pub trace_id: String,
    pub span_id: String,
}

/// `ServerTraceDiagnosticsResult`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TraceDiagnostics {
    pub trace_file_path: String,
    pub scanned_file_paths: Vec<String>,
    pub read_at: String,
    pub record_count: u64,
    pub parse_error_count: u64,
    pub first_span_at: TaggedOption<String>,
    pub last_span_at: TaggedOption<String>,
    pub failure_count: u64,
    pub interruption_count: u64,
    pub slow_span_threshold_ms: u64,
    pub slow_span_count: u64,
    pub log_level_counts: BTreeMap<String, u64>,
    pub top_spans_by_count: Vec<SpanSummary>,
    pub slowest_spans: Vec<SpanOccurrence>,
    pub common_failures: Vec<FailureSummary>,
    pub latest_failures: Vec<RecentFailure>,
    pub latest_warning_and_error_logs: Vec<LogEvent>,
    pub partial_failure: TaggedOption<bool>,
    pub error: TaggedOption<TraceError>,
}

// ---- one line, read leniently ----

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Line {
    #[serde(default, deserialize_with = "if_fits")]
    name: Option<String>,
    #[serde(default, deserialize_with = "if_fits")]
    trace_id: Option<String>,
    #[serde(default, deserialize_with = "if_fits")]
    span_id: Option<String>,
    #[serde(default, deserialize_with = "if_fits")]
    duration_ms: Option<f64>,
    #[serde(default, deserialize_with = "if_fits")]
    start_time_unix_nano: Option<String>,
    #[serde(default, deserialize_with = "if_fits")]
    end_time_unix_nano: Option<String>,
    #[serde(default, deserialize_with = "if_fits")]
    exit: Option<LineExit>,
    #[serde(default, deserialize_with = "if_fits")]
    status: Option<LineStatus>,
    #[serde(default, deserialize_with = "if_fits")]
    attributes: Option<LineAttributes>,
    #[serde(default, deserialize_with = "those_that_fit")]
    events: Vec<LineEvent>,
}

#[derive(Deserialize, Default)]
struct LineExit {
    #[serde(rename = "_tag", default, deserialize_with = "if_fits")]
    tag: Option<String>,
    #[serde(default, deserialize_with = "if_fits")]
    cause: Option<String>,
}

#[derive(Deserialize, Default)]
struct LineStatus {
    #[serde(default, deserialize_with = "if_fits")]
    code: Option<String>,
    #[serde(default, deserialize_with = "if_fits")]
    message: Option<String>,
}

#[derive(Deserialize, Default)]
struct LineAttributes {
    #[serde(rename = "effect.fiber.interrupted", default, deserialize_with = "if_fits")]
    interrupted: Option<bool>,
    #[serde(rename = "effect.logLevel", default, deserialize_with = "if_fits")]
    log_level: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct LineEvent {
    #[serde(default, deserialize_with = "if_fits")]
    name: Option<String>,
    #[serde(default, deserialize_with = "if_fits")]
    time_unix_nano: Option<String>,
    #[serde(default, deserialize_with = "if_fits")]
    attributes: Option<LineAttributes>,
}

/// `toStringValue`: text that is not blank.
fn text(v: Option<String>) -> Option<String> {
    v.filter(|s| !s.trim().is_empty())
}

/// `unixNanoToDateTime`, in milliseconds.
fn nanos_to_millis(v: Option<&str>) -> Option<i64> {
    let n: i128 = v?.trim().parse().ok()?;
    let ms = n / 1_000_000;
    // `DateTime.make`'s range.
    (ms.abs() <= 8_640_000_000_000_000).then_some(ms as i64)
}

fn iso(ms: i64) -> String {
    crate::time_fmt::iso_from_millis(ms)
}

/// `insertBounded`: keeps the `limit` first of `items` in `order`.
fn insert_bounded<A>(items: &mut Vec<A>, item: A, limit: usize, order: impl Fn(&A, &A) -> std::cmp::Ordering) {
    if items.len() >= limit && items.last().is_some_and(|last| order(&item, last) != std::cmp::Ordering::Less) {
        return;
    }
    items.push(item);
    items.sort_by(&order);
    items.truncate(limit);
}

#[derive(Default)]
struct ByName {
    count: u64,
    failure_count: u64,
    total_duration_ms: f64,
    max_duration_ms: f64,
}

/// `makeTraceDiagnosticsAggregator`.
struct Aggregator {
    slow_ms: f64,
    parse_errors: u64,
    records: u64,
    failures: u64,
    interruptions: u64,
    slow: u64,
    first_ms: Option<i64>,
    last_ms: Option<i64>,
    names: Vec<(String, ByName)>,
    name_index: HashMap<String, usize>,
    failure_index: HashMap<(String, String), usize>,
    common: Vec<(FailureSummary, i64)>,
    latest_failures: Vec<(RecentFailure, i64)>,
    slowest: Vec<SpanOccurrence>,
    logs: Vec<(LogEvent, i64)>,
    levels: BTreeMap<String, u64>,
}

impl Aggregator {
    fn new(slow_ms: u64) -> Aggregator {
        Aggregator {
            slow_ms: slow_ms as f64,
            parse_errors: 0,
            records: 0,
            failures: 0,
            interruptions: 0,
            slow: 0,
            first_ms: None,
            last_ms: None,
            names: vec![],
            name_index: HashMap::new(),
            failure_index: HashMap::new(),
            common: vec![],
            latest_failures: vec![],
            slowest: vec![],
            logs: vec![],
            levels: BTreeMap::new(),
        }
    }

    fn add_line(&mut self, line: &str) {
        if line.trim().is_empty() {
            return;
        }
        let Ok(l) = serde_json::from_str::<Line>(line) else {
            self.parse_errors += 1;
            return;
        };
        let (name, trace_id, span_id) = (text(l.name), text(l.trace_id), text(l.span_id));
        let duration = l.duration_ms.filter(|d| d.is_finite());
        let ended = nanos_to_millis(l.end_time_unix_nano.as_deref());
        let started = nanos_to_millis(l.start_time_unix_nano.as_deref());
        let (Some(name), Some(trace_id), Some(span_id), Some(duration_ms), Some(ended)) = (name, trace_id, span_id, duration, ended) else {
            self.parse_errors += 1;
            return;
        };
        self.records += 1;
        if let Some(s) = started
            && self.first_ms.is_none_or(|f| s < f)
        {
            self.first_ms = Some(s);
        }
        if self.last_ms.is_none_or(|l| ended > l) {
            self.last_ms = Some(ended);
        }
        let exit = l.exit.unwrap_or_default();
        let status = l.status.unwrap_or_default();
        let exit_tag = text(exit.tag);
        let failed = exit_tag.as_deref() == Some("Failure") || status.code.as_deref() == Some("2");
        let interrupted = exit_tag.as_deref() == Some("Interrupted") || l.attributes.as_ref().and_then(|a| a.interrupted) == Some(true) || status.message.as_deref() == Some("Interrupted");
        if failed {
            self.failures += 1;
        }
        if interrupted {
            self.interruptions += 1;
        }
        let i = *self.name_index.entry(name.clone()).or_insert_with(|| {
            self.names.push((name.clone(), ByName::default()));
            self.names.len() - 1
        });
        let by = &mut self.names[i].1;
        by.count += 1;
        by.total_duration_ms += duration_ms;
        by.max_duration_ms = by.max_duration_ms.max(duration_ms);
        if failed {
            by.failure_count += 1;
        }
        if duration_ms >= self.slow_ms {
            self.slow += 1;
        }
        let ended_at = iso(ended);
        let occurrence = SpanOccurrence { name: name.clone(), duration_ms, ended_at: ended_at.clone(), trace_id: trace_id.clone(), span_id: span_id.clone() };
        insert_bounded(&mut self.slowest, occurrence, TOP_LIMIT, |a, b| b.duration_ms.total_cmp(&a.duration_ms));
        if failed {
            let cause = if exit_tag.as_deref() == Some("Failure") {
                text(exit.cause).map_or_else(|| "Failure".to_owned(), |c| c.trim().to_owned())
            } else {
                text(status.message).map_or_else(|| "Failure".to_owned(), |c| c.trim().to_owned())
            };
            let recent = RecentFailure { name: name.clone(), cause: cause.clone(), duration_ms, ended_at: ended_at.clone(), trace_id: trace_id.clone(), span_id: span_id.clone() };
            insert_bounded(&mut self.latest_failures, (recent, ended), RECENT_LIMIT, |a, b| b.1.cmp(&a.1));
            match self.failure_index.get(&(name.clone(), cause.clone())) {
                Some(&k) => {
                    let (held, seen) = &mut self.common[k];
                    held.count += 1;
                    if ended > *seen {
                        *seen = ended;
                        held.last_seen_at = ended_at.clone();
                        held.trace_id = trace_id.clone();
                        held.span_id = span_id.clone();
                    }
                }
                None => {
                    self.failure_index.insert((name.clone(), cause.clone()), self.common.len());
                    self.common.push((FailureSummary { name: name.clone(), cause, count: 1, last_seen_at: ended_at.clone(), trace_id: trace_id.clone(), span_id: span_id.clone() }, ended));
                }
            }
        }
        for event in l.events {
            let Some(level) = text(event.attributes.and_then(|a| a.log_level)) else { continue };
            *self.levels.entry(level.clone()).or_default() += 1;
            if !matches!(level.to_lowercase().as_str(), "warning" | "warn" | "error" | "fatal") {
                continue;
            }
            let seen = nanos_to_millis(event.time_unix_nano.as_deref()).unwrap_or(ended);
            let message = text(event.name).map_or_else(|| "Log event".to_owned(), |m| m.trim().to_owned());
            let log = LogEvent { span_name: name.clone(), level, message, seen_at: iso(seen), trace_id: trace_id.clone(), span_id: span_id.clone() };
            insert_bounded(&mut self.logs, (log, seen), RECENT_LIMIT, |a, b| b.1.cmp(&a.1));
        }
    }

    fn finish(self, trace_file_path: String, scanned: Vec<String>, read_at: String, error: Option<TraceError>) -> TraceDiagnostics {
        let mut top: Vec<SpanSummary> = self
            .names
            .into_iter()
            .map(|(name, s)| SpanSummary {
                name,
                count: s.count,
                failure_count: s.failure_count,
                total_duration_ms: s.total_duration_ms,
                average_duration_ms: if s.count > 0 { s.total_duration_ms / s.count as f64 } else { 0.0 },
                max_duration_ms: s.max_duration_ms,
            })
            .collect();
        top.sort_by(|a, b| b.count.cmp(&a.count).then(b.max_duration_ms.total_cmp(&a.max_duration_ms)));
        top.truncate(TOP_LIMIT);
        let mut common = self.common;
        common.sort_by(|a, b| b.0.count.cmp(&a.0.count).then(b.1.cmp(&a.1)));
        common.truncate(TOP_LIMIT);
        TraceDiagnostics {
            trace_file_path,
            scanned_file_paths: scanned,
            read_at,
            record_count: self.records,
            parse_error_count: self.parse_errors,
            first_span_at: self.first_ms.map_or(TaggedOption::None, |ms| TaggedOption::Some { value: iso(ms) }),
            last_span_at: self.last_ms.map_or(TaggedOption::None, |ms| TaggedOption::Some { value: iso(ms) }),
            failure_count: self.failures,
            interruption_count: self.interruptions,
            slow_span_threshold_ms: self.slow_ms as u64,
            slow_span_count: self.slow,
            log_level_counts: self.levels,
            top_spans_by_count: top,
            slowest_spans: self.slowest,
            common_failures: common.into_iter().map(|(f, _)| f).collect(),
            latest_failures: self.latest_failures.into_iter().map(|(f, _)| f).collect(),
            latest_warning_and_error_logs: self.logs.into_iter().map(|(l, _)| l).collect(),
            partial_failure: if error.is_some() { TaggedOption::Some { value: true } } else { TaggedOption::None },
            error: error.map_or(TaggedOption::None, |value| TaggedOption::Some { value }),
        }
    }
}

/// `toRotatedTracePaths`: the rotated files oldest first, then the file.
pub fn rotated_paths(path: &Path, max_files: usize) -> Vec<PathBuf> {
    (1..=max_files).rev().map(|i| PathBuf::from(format!("{}.{i}", path.display()))).chain(std::iter::once(path.to_path_buf())).collect()
}

/// `TraceDiagnostics.read`.
pub fn read(path: &Path, max_files: usize, slow_ms: u64) -> TraceDiagnostics {
    let read_at = crate::time_fmt::iso_now();
    let paths = rotated_paths(path, max_files);
    let scanned: Vec<String> = paths.iter().map(|p| p.to_string_lossy().into_owned()).collect();
    let mut agg = Aggregator::new(slow_ms);
    let mut found = false;
    let mut failure: Option<TraceError> = None;
    for p in &paths {
        let file = match std::fs::File::open(p) {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                tracing::warn!(trace_file_path = %p.display(), error = %e, "Failed to read local trace file.");
                failure.get_or_insert(TraceError { kind: ErrorKind::TraceFileReadFailed, message: format!("Failed to read local trace file '{}'.", p.display()) });
                continue;
            }
        };
        let mut reader = std::io::BufReader::new(file);
        let mut line = Vec::new();
        let mut ok = true;
        loop {
            line.clear();
            match reader.read_until(b'\n', &mut line) {
                Ok(0) => break,
                Ok(_) => agg.add_line(&String::from_utf8_lossy(&line)),
                Err(_) => {
                    ok = false;
                    break;
                }
            }
        }
        if ok {
            found = true;
        } else {
            failure.get_or_insert(TraceError { kind: ErrorKind::TraceFileReadFailed, message: format!("Failed to read local trace file '{}'.", p.display()) });
        }
    }
    let path_text = path.to_string_lossy().into_owned();
    if !found {
        let error = failure.unwrap_or(TraceError { kind: ErrorKind::TraceFileNotFound, message: "No local trace files were found.".into() });
        return empty(path, max_files, slow_ms, error);
    }
    agg.finish(path_text, scanned, read_at, failure)
}

/// `makeEmptyDiagnostics`: nothing read, and why.
fn empty(path: &Path, max_files: usize, slow_ms: u64, error: TraceError) -> TraceDiagnostics {
    let scanned = rotated_paths(path, max_files).iter().map(|p| p.to_string_lossy().into_owned()).collect();
    let mut d = Aggregator::new(slow_ms).finish(path.to_string_lossy().into_owned(), scanned, crate::time_fmt::iso_now(), None);
    d.error = TaggedOption::Some { value: error };
    d
}

/// The files could not be read at all (the reading task failed).
pub fn unreadable(path: &Path, max_files: usize, message: String) -> TraceDiagnostics {
    empty(path, max_files, SLOW_SPAN_THRESHOLD_MS, TraceError { kind: ErrorKind::TraceFileReadFailed, message })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn span(name: &str, id: &str, start_ms: i64, duration_ms: f64, exit: serde_json::Value, events: serde_json::Value) -> String {
        let ns = |ms: f64| format!("{}", (ms * 1e6) as i128);
        json!({ "type": "effect-span", "name": name, "kind": "internal", "traceId": format!("t{id}"), "spanId": id, "sampled": true,
            "startTimeUnixNano": ns(start_ms as f64), "endTimeUnixNano": ns(start_ms as f64 + duration_ms), "durationMs": duration_ms,
            "attributes": {}, "events": events, "links": [], "exit": exit }).to_string()
    }

    #[test]
    fn summarizes_the_rotated_files() {
        let dir = std::env::temp_dir().join(format!("domovoi-tracediag-{}", crate::util::new_uuid()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("server.trace.ndjson");
        let ok = json!({ "_tag": "Success" });
        let fail = |c: &str| json!({ "_tag": "Failure", "cause": c });
        let base = 1_790_000_000_000i64;
        std::fs::write(PathBuf::from(format!("{}.1", path.display())), [
            span("ws.rpc.a", "1", base, 5.0, ok.clone(), json!([])),
            span("ws.rpc.a", "2", base + 10, 1500.0, fail("Boom"), json!([])),
            "not json".to_owned(),
            json!({ "name": "no ids" }).to_string(),
        ].join("\n")).unwrap();
        std::fs::write(&path, [
            span("ws.rpc.b", "3", base + 20, 7.0, json!({ "_tag": "Interrupted", "cause": "Interrupted" }), json!([
                { "name": "careful", "timeUnixNano": format!("{}", (base + 21) as i128 * 1_000_000), "attributes": { "effect.logLevel": "Warn" } },
                { "name": "fine", "timeUnixNano": "x", "attributes": { "effect.logLevel": "Info" } },
                { "name": "no level", "attributes": {} },
            ])),
            span("ws.rpc.a", "4", base + 30, 2.0, fail("Boom"), json!([])),
            span("ws.rpc.c", "5", base + 40, 3.0, fail(" Other "), json!([])),
            String::new(),
        ].join("\n")).unwrap();
        let d = read(&path, 3, SLOW_SPAN_THRESHOLD_MS);
        let v = serde_json::to_value(&d).unwrap();
        assert_eq!(v["scannedFilePaths"].as_array().unwrap().len(), 4);
        assert!(v["scannedFilePaths"][0].as_str().unwrap().ends_with(".3"));
        assert_eq!((d.record_count, d.parse_error_count, d.failure_count, d.interruption_count, d.slow_span_count), (5, 2, 3, 1, 1));
        assert_eq!(v["firstSpanAt"], json!({ "_tag": "Some", "value": iso(base) }));
        assert_eq!(v["lastSpanAt"], json!({ "_tag": "Some", "value": iso(base + 1510) }));
        assert_eq!(v["topSpansByCount"][0], json!({ "name": "ws.rpc.a", "count": 3, "failureCount": 2, "totalDurationMs": 1507.0, "averageDurationMs": 1507.0 / 3.0, "maxDurationMs": 1500.0 }));
        assert_eq!(v["slowestSpans"][0]["spanId"], "2");
        assert_eq!(v["commonFailures"][0], json!({ "name": "ws.rpc.a", "cause": "Boom", "count": 2, "lastSeenAt": iso(base + 1510), "traceId": "t2", "spanId": "2" }));
        assert_eq!(v["commonFailures"][1]["cause"], "Other");
        assert_eq!(v["latestFailures"].as_array().unwrap().iter().map(|f| f["spanId"].as_str().unwrap()).collect::<Vec<_>>(), ["2", "5", "4"]);
        assert_eq!(v["logLevelCounts"], json!({ "Info": 1, "Warn": 1 }));
        assert_eq!(v["latestWarningAndErrorLogs"], json!([{ "spanName": "ws.rpc.b", "level": "Warn", "message": "careful", "seenAt": iso(base + 21), "traceId": "t3", "spanId": "3" }]));
        assert_eq!((v["partialFailure"].clone(), v["error"].clone()), (json!({ "_tag": "None" }), json!({ "_tag": "None" })));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn no_file_is_not_found() {
        let dir = std::env::temp_dir().join(format!("domovoi-tracediag-none-{}", crate::util::new_uuid()));
        let v = serde_json::to_value(read(&dir.join("server.trace.ndjson"), 10, SLOW_SPAN_THRESHOLD_MS)).unwrap();
        assert_eq!(v["error"], json!({ "_tag": "Some", "value": { "kind": "trace-file-not-found", "message": "No local trace files were found." } }));
        assert_eq!((v["recordCount"].clone(), v["scannedFilePaths"].as_array().map(Vec::len)), (json!(0), Some(11)));
    }
}
