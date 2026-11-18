//! The records behind the settings pages (scheduled tasks, access, storage,
//! processes, usage, agent sessions) and remote access must take what a server
//! sends and give it back unchanged: every recorded payload of these shapes,
//! and, for the shapes no recording contains, a sample written as the Domovoi
//! server builds them.

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Debug;
use domovoi_model::typed::{
    AgentSessionImportResult, AgentSessionScanResult, AgentSessionSource, AuthAccessStreamEvent, AuthAccessTokenResult,
    AuthBrowserSessionResult, AuthClientDeviceType, AuthClientSession, AuthSessionState, AuthWebSocketTicketResult,
    DatabaseStorageItem, HostResourcesSnapshot, RemoteAccessState, RemoteAccessStatus, RemotePairingLink, RequestRejection,
    ScheduledTask, ScheduledTaskList, ScheduledTaskRunStatus, ScheduledTaskSchedule, ServerAuthSessionMethod,
    ServerProcessDiagnosticsResult, StorageCleanupReport, StorageCleanupReportItem, TaggedOption, UsageBucket, UsagePricingStatus,
    UsageSummary, WebhookFilter,
};

const FIXTURES: [&str; 2] = ["ts-session1", "ts-session2-boot"];

/// The HTTP routes of these shapes the recorded sessions are known to contain.
const SESSION: &str = "GET /api/auth/session";
const BROWSER_SESSION: &str = "POST /api/auth/browser-session";

/// The RPCs of these shapes. No recorded session calls one; a recording that
/// does is checked like the rest.
const RPCS: [&str; 9] = [
    "scheduledTasks.subscribe",
    "subscribeAuthAccess",
    "server.getDatabaseStorage",
    "server.getStorageCleanupReport",
    "server.getProcessDiagnostics",
    "server.getHostResources",
    "server.getUsageSummary",
    "agentSessions.scan",
    "agentSessions.import",
];

fn fixture(name: &str) -> Vec<Value> {
    let path = format!("{}/../../fixtures/golden/{name}/wire.ndjson", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap().lines().map(|line| serde_json::from_str(line).unwrap()).collect()
}

fn sample(name: &str) -> Value {
    let path = format!("{}/tests/samples/{name}.json", env!("CARGO_MANIFEST_DIR"));
    let text = std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{path}: {error}"));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("{path}: {error}"))
}

/// Every payload of these shapes in the recordings, with the route or RPC it
/// answered: successful HTTP bodies, stream items and unary results.
fn recorded() -> Vec<(String, Value)> {
    let mut payloads = Vec::new();
    for name in FIXTURES {
        let mut requests: BTreeMap<(i64, String), String> = BTreeMap::new();
        for line in fixture(name) {
            if line["kind"] == "http" {
                let path = line["path"].as_str().unwrap_or_default();
                let route = format!("{} {}", line["method"].as_str().unwrap_or_default(), path.split('?').next().unwrap_or_default());
                if line["status"] == 200 && [SESSION, BROWSER_SESSION].contains(&route.as_str()) {
                    payloads.push((route, line["res"].clone()));
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
            if !RPCS.contains(&rpc.as_str()) {
                continue;
            }
            if frame["_tag"] == "Chunk" {
                payloads.extend(frame["values"].as_array().unwrap().iter().map(|value| (rpc.clone(), value.clone())));
            } else if frame["_tag"] == "Exit" && frame["exit"]["_tag"] == "Success" && !frame["exit"]["value"].is_null() {
                payloads.push((rpc.clone(), frame["exit"]["value"].clone()));
            }
        }
    }
    payloads
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

/// Decodes `payload` as `T` and encodes it again; `Err` says how the round trip failed.
fn round_trip<T: DeserializeOwned + Serialize + Debug>(payload: &Value) -> Result<(), String> {
    let typed: T = serde_json::from_value(payload.clone()).map_err(|error| format!("does not decode: {error}"))?;
    let unknown = fallbacks(&typed);
    if !unknown.is_empty() {
        return Err(format!("decodes into a fallback variant:\n    {}", unknown.join("\n    ")));
    }
    let encoded = serde_json::to_value(&typed).map_err(|error| format!("does not encode: {error}"))?;
    if encoded != *payload {
        let mut lines = Vec::new();
        differences("", payload, &encoded, &mut lines);
        return Err(format!("encodes differently:\n    {}", lines.join("\n    ")));
    }
    Ok(())
}

/// As [`round_trip`], and no element of a forward-compatible list may have been left unread.
fn round_trip_read_in_full<T: DeserializeOwned + Serialize + Debug>(payload: &Value, unread: impl Fn(&T) -> usize) -> Result<(), String> {
    round_trip::<T>(payload)?;
    match unread(&serde_json::from_value(payload.clone()).map_err(|error| error.to_string())?) {
        0 => Ok(()),
        left_out => Err(format!("leaves {left_out} list element(s) unread")),
    }
}

/// Round-trips one payload of the route or RPC `kind` through the type for it.
fn round_trip_as(kind: &str, payload: &Value) -> Result<(), String> {
    match kind {
        SESSION | "auth.session" => round_trip::<AuthSessionState>(payload),
        BROWSER_SESSION | "auth.browser-session" => round_trip::<AuthBrowserSessionResult>(payload),
        "scheduledTasks.subscribe" => round_trip_read_in_full::<ScheduledTaskList>(payload, |list| list.tasks.unread()),
        "subscribeAuthAccess" => round_trip::<AuthAccessStreamEvent>(payload),
        "server.getDatabaseStorage" => round_trip::<DatabaseStorageItem>(payload),
        "server.getStorageCleanupReport" => round_trip::<StorageCleanupReportItem>(payload),
        "server.getProcessDiagnostics" => round_trip::<ServerProcessDiagnosticsResult>(payload),
        "server.getHostResources" => round_trip::<HostResourcesSnapshot>(payload),
        "server.getUsageSummary" => {
            round_trip_read_in_full::<UsageSummary>(payload, |summary| summary.buckets.unread() + summary.sources.unread())
        }
        "agentSessions.scan" => round_trip::<AgentSessionScanResult>(payload),
        "agentSessions.import" => round_trip::<AgentSessionImportResult>(payload),
        "api.domovoi.remote" => round_trip::<RemoteAccessStatus>(payload),
        "api.domovoi.remote.pairing" => round_trip::<RemotePairingLink>(payload),
        "auth.access-token" => round_trip::<AuthAccessTokenResult>(payload),
        "auth.ws-ticket" => round_trip::<AuthWebSocketTicketResult>(payload),
        "rejected" => round_trip::<RequestRejection>(payload),
        other => Err(format!("no typed record for `{other}`")),
    }
}

/// The samples: the file, the kind its payloads are of, and whether it holds
/// a stream's items (an array) or one document.
const SAMPLES: [(&str, &str, bool); 18] = [
    ("scheduledTasks.subscribe", "scheduledTasks.subscribe", true),
    ("subscribeAuthAccess", "subscribeAuthAccess", true),
    ("server.getDatabaseStorage", "server.getDatabaseStorage", true),
    ("server.getStorageCleanupReport", "server.getStorageCleanupReport", true),
    ("server.getProcessDiagnostics", "server.getProcessDiagnostics", false),
    ("server.getHostResources", "server.getHostResources", false),
    ("server.getUsageSummary", "server.getUsageSummary", false),
    ("server.getUsageSummary.unpriced", "server.getUsageSummary", false),
    ("server.getUsageSummary.hourly", "server.getUsageSummary", false),
    ("agentSessions.scan", "agentSessions.scan", false),
    ("agentSessions.import", "agentSessions.import", false),
    ("api.domovoi.remote", "api.domovoi.remote", false),
    ("api.domovoi.remote.off", "api.domovoi.remote", false),
    ("api.domovoi.remote.pairing", "api.domovoi.remote.pairing", false),
    ("api.domovoi.remote.rejected", "rejected", false),
    ("auth.access-token", "auth.access-token", false),
    ("auth.ws-ticket", "auth.ws-ticket", false),
    ("auth.rejected", "rejected", false),
];

#[test]
fn recorded_payloads_round_trip_through_their_types() {
    let mut checked: BTreeMap<String, usize> = BTreeMap::new();
    let mut failures: BTreeMap<String, usize> = BTreeMap::new();
    for (kind, payload) in recorded() {
        *checked.entry(kind.clone()).or_default() += 1;
        if let Err(why) = round_trip_as(&kind, &payload) {
            *failures.entry(format!("{kind} {why}")).or_default() += 1;
        }
    }

    println!("recorded payloads checked, by kind:");
    for kind in [SESSION, BROWSER_SESSION].into_iter().chain(RPCS) {
        println!("  {kind:<32} {}", checked.get(kind).copied().unwrap_or(0));
    }
    let report: Vec<String> = failures.iter().map(|(failure, count)| format!("{failure}\n    ({count} payloads)")).collect();
    assert!(failures.is_empty(), "payloads that do not round-trip:\n{}", report.join("\n"));
    for kind in [SESSION, BROWSER_SESSION] {
        assert!(checked.get(kind).copied().unwrap_or(0) > 0, "the fixtures contain `{kind}` bodies, but none was checked");
    }
}

#[test]
fn samples_round_trip_through_their_types() {
    let mut failures = Vec::new();
    println!("sample payloads checked, by file:");
    for (file, kind, is_stream) in SAMPLES {
        let document = sample(file);
        let payloads: Vec<Value> = if is_stream { document.as_array().unwrap_or_else(|| panic!("{file}: not an array")).clone() } else { vec![document] };
        assert!(!payloads.is_empty(), "{file}: no payload");
        println!("  {file:<34} {}", payloads.len());
        for (index, payload) in payloads.iter().enumerate() {
            if let Err(why) = round_trip_as(kind, payload) {
                failures.push(format!("{file}[{index}] {why}"));
            }
        }
    }
    assert!(failures.is_empty(), "samples that do not round-trip:\n{}", failures.join("\n"));
}

/// The samples carry what the pages show, under the names the types give it.
#[test]
fn samples_decode_into_what_the_pages_read() {
    let lists: Vec<ScheduledTaskList> = serde_json::from_value(sample("scheduledTasks.subscribe")).unwrap();
    let first = &lists[0];
    assert_eq!(first.time_zone, Some(Some("Europe/Warsaw".to_owned())));
    let types: Vec<&str> = first.tasks.iter().map(|task| task.schedule.schedule_type()).collect();
    assert_eq!(types, ["webhook", "once", "fixed_time", "interval", "fixed_time"]);
    assert!(first.tasks.iter().any(|task| task.last_run_status == ScheduledTaskRunStatus::Never && task.last_run_at.is_none()));
    assert!(first.tasks.iter().any(|task| task.last_run_status == ScheduledTaskRunStatus::Succeeded && task.last_run_at.is_some()));
    let ScheduledTaskSchedule::Webhook(webhook) = &first.tasks[0].schedule else { panic!("the first task runs on a webhook") };
    assert_eq!(webhook.max_delivery_age_minutes, Some(Some(90)));
    assert_eq!(webhook.filters.as_ref().map(Vec::len), Some(5));
    let ScheduledTaskSchedule::Webhook(cleared) = &lists[1].tasks[0].schedule else { panic!("the first task runs on a webhook") };
    assert_eq!((cleared.signature.is_none(), cleared.max_delivery_age_minutes, cleared.filters.is_none()), (true, Some(None), true));

    let events: Vec<AuthAccessStreamEvent> = serde_json::from_value(sample("subscribeAuthAccess")).unwrap();
    let AuthAccessStreamEvent::Snapshot(snapshot) = &events[0] else { panic!("the stream starts with a snapshot") };
    assert_eq!((snapshot.payload.pairing_links.len(), snapshot.payload.client_sessions.len()), (1, 2));
    let methods: Vec<&ServerAuthSessionMethod> = snapshot.payload.client_sessions.iter().map(|session| &session.method).collect();
    assert_eq!(methods, [&ServerAuthSessionMethod::BrowserSessionCookie, &ServerAuthSessionMethod::BearerAccessToken]);
    let phone = &snapshot.payload.client_sessions[1];
    assert_eq!((phone.client.label.as_deref(), phone.client.os.as_deref(), &phone.client.device_type), (Some("Pixel 9"), Some("Android 16"), &AuthClientDeviceType::Mobile));
    let paired = events.iter().find_map(|event| match event {
        AuthAccessStreamEvent::ClientUpserted(event) => Some(&event.payload),
        _ => None,
    });
    assert_eq!(paired.map(|session| (&session.method, session.connected)), Some((&ServerAuthSessionMethod::BearerAccessToken, false)));
    let mut seen = BTreeSet::new();
    for event in &events {
        seen.insert(match event {
            AuthAccessStreamEvent::Snapshot(_) => "snapshot",
            AuthAccessStreamEvent::PairingLinkUpserted(_) => "pairingLinkUpserted",
            AuthAccessStreamEvent::PairingLinkRemoved(_) => "pairingLinkRemoved",
            AuthAccessStreamEvent::ClientUpserted(_) => "clientUpserted",
            AuthAccessStreamEvent::ClientRemoved(_) => "clientRemoved",
            AuthAccessStreamEvent::Other(_) => "other",
        });
    }
    assert_eq!(seen.len(), 5, "one of each event: {seen:?}");

    let processes: ServerProcessDiagnosticsResult = serde_json::from_value(sample("server.getProcessDiagnostics")).unwrap();
    assert_eq!(processes.processes.iter().map(|process| process.depth).collect::<Vec<_>>(), [0, 1, 2, 1]);
    assert_eq!(processes.processes[2].pgid.as_option(), Some(&4388));
    assert!(processes.error.as_option().is_none());

    let report: Vec<StorageCleanupReport> = serde_json::from_value(sample("server.getStorageCleanupReport")).unwrap();
    assert_eq!((report[0].counts.removed.as_u64(), report[0].bytes_freed.as_ref().and_then(|bytes| bytes.as_u64())), (Some(2), Some(414_720_000)));
    let stray = report[0].entries.last().unwrap();
    assert_eq!((&stray.bytes, &stray.files, &stray.project_path), (&None, &None, &None), "domovoi leaves all three out");

    let unpriced: UsageSummary = serde_json::from_value(sample("server.getUsageSummary.unpriced")).unwrap();
    assert_eq!((unpriced.pricing.status, unpriced.buckets.len()), (UsagePricingStatus::Unavailable, 5));
    assert!(unpriced.buckets.iter().all(|bucket| bucket.hour_start == Some(None)), "domovoi writes `null` for a summary by the day");
    let days: BTreeSet<&str> = unpriced.buckets.iter().map(|bucket| bucket.day.as_str()).collect();
    let providers: BTreeSet<&str> = unpriced.buckets.iter().map(|bucket| bucket.provider.as_str()).collect();
    assert_eq!((days.len(), providers.len()), (2, 2));
    let priced: UsageSummary = serde_json::from_value(sample("server.getUsageSummary")).unwrap();
    assert_eq!((priced.pricing.status, priced.buckets.len(), priced.buckets[0].hour_start.is_none()), (UsagePricingStatus::Cached, 5, true));

    let scan: AgentSessionScanResult = serde_json::from_value(sample("agentSessions.scan")).unwrap();
    assert_eq!(scan.candidates.len(), 4);
    assert_eq!(scan.candidates[0].sources, [AgentSessionSource::ClaudeAgent, AgentSessionSource::Codex]);
    assert!(scan.candidates[0].already_imported && scan.candidates[0].project_id.is_some());
    assert_eq!((scan.candidates[3].thread_count, &scan.candidates[3].git), (0, &Some(None)));

    let remote: RemoteAccessStatus = serde_json::from_value(sample("api.domovoi.remote")).unwrap();
    assert_eq!((remote.state, remote.running, remote.push.configured, remote.relays.len()), (RemoteAccessState::Online, true, true, 1));
    let off: RemoteAccessStatus = serde_json::from_value(sample("api.domovoi.remote.off")).unwrap();
    assert_eq!((off.state, off.enabled, off.home_relay), (RemoteAccessState::Off, false, None));

    let token: AuthAccessTokenResult = serde_json::from_value(sample("auth.access-token")).unwrap();
    assert!(!token.access_token.is_empty());
    let rejected: RequestRejection = serde_json::from_value(sample("auth.rejected")).unwrap();
    assert_eq!((rejected.message(), rejected.reason()), (None, Some("invalid_credential")));
    let refused: RequestRejection = serde_json::from_value(sample("api.domovoi.remote.rejected")).unwrap();
    assert_eq!((refused.message(), refused.reason()), (Some("relay relay.example.net: relative URL without a base"), None));
}

fn a_task() -> Value {
    json!({
        "id": "scheduled-task:1", "title": "Daily", "prompt": "Summarize", "enabled": true,
        "schedule": { "type": "interval", "everyMs": 3_600_000 }, "projectId": "p1", "threadId": null, "threadMode": "new_thread",
        "workspaceStrategy": { "type": "root" }, "modelSelection": { "instanceId": "codex", "model": "gpt-5" },
        "runtimeMode": "full-access", "interactionMode": "default", "createdBy": "user", "creationSource": "web",
        "createdAt": "2026-01-01T00:00:00.000Z", "updatedAt": "2026-01-01T00:00:00.000Z", "nextRunAt": null, "lastRunAt": null,
        "lastRunStatus": "never", "lastRunError": null, "runCount": 0,
    })
}

fn a_session() -> Value {
    json!({
        "sessionId": "s1", "subject": "one-time-token", "scopes": ["orchestration:read"], "method": "bearer-access-token",
        "client": { "deviceType": "mobile" }, "issuedAt": "2026-01-01T00:00:00.000Z", "expiresAt": "2026-02-01T00:00:00.000Z",
        "lastConnectedAt": null, "connected": false, "current": false,
    })
}

/// A newer server's schedule type, filter comparison, event type, error tag
/// and enum values this build does not know must all survive a round trip.
#[test]
fn unknown_values_survive_a_round_trip() {
    let mut task = a_task();
    task["schedule"] = json!({ "type": "sunrise", "offsetMinutes": -15 });
    task["lastRunStatus"] = "skipped".into();
    task["threadMode"] = "rotating".into();
    let typed: ScheduledTask = serde_json::from_value(task.clone()).unwrap();
    let ScheduledTaskSchedule::Other(schedule) = &typed.schedule else { panic!("an unknown schedule type is kept") };
    assert_eq!((typed.schedule.schedule_type(), schedule.record()["offsetMinutes"].as_i64()), ("sunrise", Some(-15)));
    assert_eq!(typed.last_run_status.as_str(), "skipped");
    assert_eq!(fallbacks(&typed).len(), 3, "the schedule, the run status and the thread mode");
    assert_eq!(serde_json::to_value(&typed).unwrap(), task);

    let filter = json!({ "path": "body.title", "op": "matches", "value": "^fix" });
    let typed: WebhookFilter = serde_json::from_value(filter.clone()).unwrap();
    assert!(matches!(&typed, WebhookFilter::Other(unknown) if unknown.tag() == "matches"));
    assert_eq!(serde_json::to_value(&typed).unwrap(), filter);

    let event = json!({ "version": 2, "revision": 9, "type": "passkeyUpserted", "payload": { "id": "k1" } });
    let typed: AuthAccessStreamEvent = serde_json::from_value(event.clone()).unwrap();
    assert!(matches!(&typed, AuthAccessStreamEvent::Other(unknown) if unknown.tag() == "passkeyUpserted"));
    assert_eq!(serde_json::to_value(&typed).unwrap(), event);

    let mut session = a_session();
    session["method"] = "passkey".into();
    session["client"]["deviceType"] = "watch".into();
    session["permissions"] = json!(["orchestration:read", "holograms:project"]);
    let typed: AuthClientSession = serde_json::from_value(session.clone()).unwrap();
    assert_eq!((typed.method.as_str(), typed.client.device_type.as_str()), ("passkey", "watch"));
    assert_eq!(serde_json::to_value(&typed).unwrap(), session);

    let rejection = json!({ "_tag": "EnvironmentHttpForbiddenError", "message": "Remote clients are not allowed here." });
    let typed: RequestRejection = serde_json::from_value(rejection.clone()).unwrap();
    assert!(matches!(&typed, RequestRejection::Other(unknown) if unknown.tag() == "EnvironmentHttpForbiddenError"));
    assert_eq!(typed.message(), Some("Remote clients are not allowed here."));
    assert_eq!(serde_json::to_value(&typed).unwrap(), rejection);

    let mut remote = sample("api.domovoi.remote");
    remote["state"] = "relayOnly".into();
    let typed: RemoteAccessStatus = serde_json::from_value(remote.clone()).unwrap();
    assert_eq!(typed.state.as_str(), "relayOnly");
    assert_eq!(serde_json::to_value(&typed).unwrap(), remote);
}

/// A record without a field its schema requires does not decode, and a known
/// union member that is malformed is an error rather than a fallback.
#[test]
fn a_missing_required_field_is_an_error() {
    let missing = |mut record: Value, field: &str| {
        record.as_object_mut().unwrap().remove(field).unwrap_or_else(|| panic!("the record has no `{field}`"));
        record
    };
    let error = serde_json::from_value::<ScheduledTask>(missing(a_task(), "prompt")).unwrap_err();
    assert!(error.to_string().contains("prompt"), "{error}");
    let error = serde_json::from_value::<ScheduledTask>(missing(a_task(), "lastRunStatus")).unwrap_err();
    assert!(error.to_string().contains("lastRunStatus"), "{error}");

    let mut task = a_task();
    task["schedule"] = json!({ "type": "interval" });
    let error = serde_json::from_value::<ScheduledTask>(task).unwrap_err();
    assert!(error.to_string().contains("everyMs"), "{error}");
    let error = serde_json::from_value::<WebhookFilter>(json!({ "path": "body.action", "op": "in", "value": "opened" })).unwrap_err();
    assert!(error.to_string().contains("WebhookFilter `in`"), "{error}");
    let error = serde_json::from_value::<ScheduledTaskSchedule>(json!({ "everyMs": 60_000 })).unwrap_err();
    assert!(error.to_string().contains("type"), "{error}");

    let error = serde_json::from_value::<AuthClientSession>(missing(a_session(), "method")).unwrap_err();
    assert!(error.to_string().contains("method"), "{error}");
    let event = json!({ "version": 1, "revision": 2, "type": "clientRemoved", "payload": { "id": "s1" } });
    let error = serde_json::from_value::<AuthAccessStreamEvent>(event).unwrap_err();
    assert!(error.to_string().contains("sessionId"), "{error}");

    let error = serde_json::from_value::<RemoteAccessStatus>(missing(sample("api.domovoi.remote"), "push")).unwrap_err();
    assert!(error.to_string().contains("push"), "{error}");
    let error = serde_json::from_value::<RemotePairingLink>(missing(sample("api.domovoi.remote.pairing"), "code")).unwrap_err();
    assert!(error.to_string().contains("code"), "{error}");
    let error = serde_json::from_value::<AuthAccessTokenResult>(missing(sample("auth.access-token"), "access_token")).unwrap_err();
    assert!(error.to_string().contains("access_token"), "{error}");
    let error = serde_json::from_value::<RequestRejection>(json!({ "_tag": "EnvironmentAuthInvalidError", "code": "auth_invalid" })).unwrap_err();
    assert!(error.to_string().contains("reason"), "{error}");
    let error = serde_json::from_value::<HostResourcesSnapshot>(missing(sample("server.getHostResources"), "cpuCount")).unwrap_err();
    assert!(error.to_string().contains("cpuCount"), "{error}");
    let mut bucket = sample("server.getUsageSummary")["buckets"][0].clone();
    bucket["totals"].as_object_mut().unwrap().remove("reasoningTokens");
    let error = serde_json::from_value::<UsageBucket>(bucket).unwrap_err();
    assert!(error.to_string().contains("reasoningTokens"), "{error}");
}

/// Absent and `null` are different on the wire and stay different, where the
/// schema allows both or the Domovoi server writes `null` for a field the schema leaves out.
#[test]
fn absent_and_null_stay_distinct() {
    for (zone, expected) in [(None, None), (Some(Value::Null), Some(None)), (Some(json!("UTC")), Some(Some("UTC".to_owned())))] {
        let mut list = json!({ "tasks": [] });
        if let Some(zone) = zone {
            list["timeZone"] = zone;
        }
        let typed: ScheduledTaskList = serde_json::from_value(list.clone()).unwrap();
        assert_eq!(typed.time_zone, expected);
        assert_eq!(serde_json::to_value(&typed).unwrap(), list);
    }

    let mut bucket = sample("server.getUsageSummary.unpriced")["buckets"][0].clone();
    assert_eq!(serde_json::from_value::<UsageBucket>(bucket.clone()).unwrap().hour_start, Some(None));
    bucket.as_object_mut().unwrap().remove("hourStart");
    let typed: UsageBucket = serde_json::from_value(bucket.clone()).unwrap();
    assert_eq!(typed.hour_start, None);
    assert_eq!(serde_json::to_value(&typed).unwrap(), bucket);

    let mut scan = sample("agentSessions.scan");
    scan["candidates"][0].as_object_mut().unwrap().remove("git");
    let typed: AgentSessionScanResult = serde_json::from_value(scan.clone()).unwrap();
    assert_eq!(typed.candidates[0].git, None);
    assert_eq!(serde_json::to_value(&typed).unwrap(), scan);

    // A stream that has nothing to report yet sends `null`.
    assert_eq!(serde_json::from_value::<StorageCleanupReportItem>(Value::Null).unwrap(), None);
    assert_eq!(serde_json::from_value::<DatabaseStorageItem>(Value::Null).unwrap(), None);
}

/// An element of the task list this build cannot read (`null`, or a task
/// missing a field) is left out and kept, and the tasks around it are read.
#[test]
fn a_task_that_does_not_fit_is_left_out_of_the_list() {
    let mut broken = a_task();
    broken.as_object_mut().unwrap().remove("schedule");
    let list = json!({ "tasks": [a_task(), null, broken, a_task()], "timeZone": "UTC" });
    let typed: ScheduledTaskList = serde_json::from_value(list.clone()).unwrap();
    assert_eq!((typed.tasks.len(), typed.tasks.unread()), (2, 2));
    assert_eq!(serde_json::to_value(&typed).unwrap(), list);
}

/// `Schema.Option` values keep their encoding, and the bodies the Domovoi server
/// writes short of the schema still decode.
#[test]
fn options_and_domovoi_rejections_keep_their_shape() {
    for sent in [json!({ "_tag": "None" }), json!({ "_tag": "Some", "value": -1 })] {
        let typed: TaggedOption<i64> = serde_json::from_value(sent.clone()).unwrap();
        assert_eq!(typed.as_option().is_some(), sent["_tag"] == "Some");
        assert_eq!(serde_json::to_value(&typed).unwrap(), sent);
    }
    assert!(serde_json::from_value::<TaggedOption<i64>>(json!({ "_tag": "Some" })).is_err());

    let unavailable = json!({
        "serverPid": 77, "readAt": "2026-01-01T00:00:00.000Z", "processCount": 0, "totalRssBytes": 0, "totalCpuPercent": 0.0,
        "processes": [], "error": { "_tag": "Some", "value": { "message": "Process diagnostics are unavailable on this platform." } },
    });
    let typed: ServerProcessDiagnosticsResult = serde_json::from_value(unavailable.clone()).unwrap();
    assert!(typed.error.as_option().is_some_and(|error| error.message.starts_with("Process diagnostics")));
    assert_eq!(serde_json::to_value(&typed).unwrap(), unavailable);

    // As `internal`, `forbidden`, `scope_required`, `invalid_request` and `rate_limited` in the Domovoi server's `http.rs` write them.
    let bodies = [
        (json!({ "_tag": "EnvironmentInternalError", "code": "internal_error" }), None),
        (json!({ "_tag": "EnvironmentOperationForbiddenError", "code": "forbidden", "reason": "remote_access_is_changed_locally", "traceId": "t" }), Some("remote_access_is_changed_locally")),
        (json!({ "_tag": "EnvironmentOperationForbiddenError", "code": "forbidden", "reason": "current_session_revoke_not_allowed", "traceId": "t" }), Some("current_session_revoke_not_allowed")),
        (json!({ "_tag": "EnvironmentScopeRequiredError", "code": "insufficient_scope", "requiredScope": "access:write", "requiredPermission": "access:read", "traceId": "t" }), None),
        (json!({ "_tag": "EnvironmentRequestInvalidError", "code": "invalid_request", "reason": "scope_not_granted", "traceId": "t" }), Some("scope_not_granted")),
        (json!({ "_tag": "EnvironmentRateLimitedError", "code": "rate_limited", "retryAfterSeconds": 60, "traceId": "t" }), None),
    ];
    for (body, reason) in bodies {
        round_trip::<RequestRejection>(&body).unwrap_or_else(|why| panic!("{body} {why}"));
        let typed: RequestRejection = serde_json::from_value(body).unwrap();
        assert_eq!((typed.reason(), typed.message()), (reason, None));
    }
}
