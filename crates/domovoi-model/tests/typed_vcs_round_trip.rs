//! Version control and pull request payloads go through their typed records
//! unchanged: every one the TypeScript server sent in the recorded sessions,
//! and a sample of each shape no recording holds, written as the Domovoi server
//! builds it.

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Debug;
use domovoi_model::typed::{
    GitActionProgressEvent, GitActionToastCta, GitPreparePullRequestThreadResult, GitResolvePullRequestResult,
    GitRunStackedActionResult, GitStackedAction, PullRequestAction, PullRequestActionResult, PullRequestCheck,
    PullRequestCheckStatus, PullRequestChecks, PullRequestChecksResult, PullRequestListEntry, PullRequestListResult,
    PullRequestProviderSummary, PullRequestRef, PullRequestState, PullRequestSummary, VcsBranchStatus, VcsCreateRefResult,
    VcsListRefsResult, VcsPullResult, VcsPullStatus, VcsStatus, VcsStatusLocal, VcsStatusRemote, VcsStatusStreamEvent,
    VcsSwitchRefResult,
};

const FIXTURES: [&str; 1] = ["session1"];

/// Kinds the recorded sessions are known to contain. Checking none of one of
/// these means the walk over the fixtures broke, not that the types are right.
const KINDS_IN_FIXTURES: [&str; 5] = [
    "subscribeVcsStatus snapshot",
    "subscribeVcsStatus localUpdated",
    "vcs.listRefs",
    "vcs.refreshStatus",
    "pullRequests.list",
];

fn fixture(name: &str) -> Vec<Value> {
    let path = format!("{}/../../fixtures/native/{name}/wire.ndjson", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap().lines().map(|line| serde_json::from_str(line).unwrap()).collect()
}

/// Whether an RPC is one this file's records are for.
fn is_ours(rpc: &str) -> bool {
    ["subscribeVcs", "vcs.", "git.", "pullRequests."].iter().any(|prefix| rpc.starts_with(prefix))
}

/// Every stream item and every successful result the server sent for one of
/// our RPCs in the fixtures, with the RPC it answered.
fn recorded() -> Vec<(String, Value)> {
    let mut payloads = Vec::new();
    for name in FIXTURES {
        let mut requests: BTreeMap<(i64, String), String> = BTreeMap::new();
        for line in fixture(name) {
            let frame = &line["frame"];
            if line["kind"] != "ws" || !frame.is_object() {
                continue;
            }
            let connection = line["conn"].as_i64().unwrap_or(0);
            if frame["_tag"] == "Request" {
                requests.insert((connection, frame["id"].to_string()), frame["tag"].as_str().unwrap_or_default().to_owned());
                continue;
            }
            let Some(rpc) = requests.get(&(connection, frame["requestId"].to_string())).filter(|rpc| is_ours(rpc)) else { continue };
            if frame["_tag"] == "Chunk" {
                payloads.extend(frame["values"].as_array().unwrap().iter().map(|value| (rpc.clone(), value.clone())));
            } else if frame["_tag"] == "Exit" && frame["exit"]["_tag"] == "Success" {
                // A stream that ends well exits with no value; only a unary call's value is a payload.
                if !rpc.starts_with("subscribe") {
                    payloads.push((rpc.clone(), frame["exit"]["value"].clone()));
                }
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

/// Decodes `record` as `T` and encodes it again; `Err` says how the round trip failed.
fn round_trip<T: DeserializeOwned + Serialize + Debug>(record: &Value) -> Result<(), String> {
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
    Ok(())
}

/// An RPC that answers with nothing sends `null`.
fn nothing(value: &Value) -> Result<(), String> {
    if value.is_null() { Ok(()) } else { Err(format!("answers {value} where the schema says nothing")) }
}

/// The kind a payload of `rpc` is counted under: the RPC, and for a status
/// stream item its `_tag`, for a git action event its `kind`.
fn kind_of(rpc: &str, payload: &Value) -> String {
    match rpc {
        "subscribeVcsStatus" => format!("{rpc} {}", payload["_tag"].as_str().unwrap_or("<no _tag>")),
        "git.runStackedAction" => format!("{rpc} {}", payload["kind"].as_str().unwrap_or("<no kind>")),
        _ => rpc.to_owned(),
    }
}

/// Round-trips a stream item or a result of `rpc` through the type it is declared with.
fn round_trip_as(rpc: &str, payload: &Value) -> Result<(), String> {
    match rpc {
        "subscribeVcsStatus" => round_trip::<VcsStatusStreamEvent>(payload),
        "subscribeVcsBranch" => round_trip::<VcsBranchStatus>(payload),
        "vcs.refreshBranch" | "vcs.removeWorktree" | "vcs.init" => nothing(payload),
        "vcs.listRefs" => round_trip::<VcsListRefsResult>(payload),
        "vcs.pull" => round_trip::<VcsPullResult>(payload),
        "vcs.refreshStatus" => round_trip::<VcsStatus>(payload),
        "vcs.switchRef" => round_trip::<VcsSwitchRefResult>(payload),
        "vcs.createRef" => round_trip::<VcsCreateRefResult>(payload),
        "git.runStackedAction" => round_trip::<GitActionProgressEvent>(payload),
        "git.resolvePullRequest" => round_trip::<GitResolvePullRequestResult>(payload),
        "git.preparePullRequestThread" => round_trip::<GitPreparePullRequestThreadResult>(payload),
        "pullRequests.list" => round_trip::<PullRequestListResult>(payload),
        "pullRequests.summary" => round_trip::<PullRequestSummary>(payload),
        "pullRequests.runAction" => round_trip::<PullRequestActionResult>(payload),
        "pullRequests.checks" => round_trip::<PullRequestChecksResult>(payload),
        "pullRequests.submitReview" | "pullRequests.comment" | "pullRequests.update" => nothing(payload),
        other => Err(format!("no typed record for `{other}`")),
    }
}

/// Round-trips `payloads`, counting them by kind; the failures come back one
/// entry per distinct failure with how many payloads failed that way.
fn check(payloads: &[(String, Value)]) -> (BTreeMap<String, usize>, BTreeMap<String, usize>) {
    let (mut checked, mut failures) = (BTreeMap::new(), BTreeMap::new());
    for (rpc, payload) in payloads {
        let kind = kind_of(rpc, payload);
        *checked.entry(kind.clone()).or_default() += 1;
        if let Err(why) = round_trip_as(rpc, payload) {
            *failures.entry(format!("{kind} {why}")).or_default() += 1;
        }
    }
    (checked, failures)
}

fn report(failures: &BTreeMap<String, usize>) -> String {
    failures.iter().map(|(failure, count)| format!("{failure}\n    ({count} payloads)")).collect::<Vec<_>>().join("\n")
}

#[test]
fn recorded_vcs_payloads_round_trip_through_their_types() {
    let (checked, failures) = check(&recorded());
    println!("recorded payloads checked, by kind:");
    for (kind, count) in &checked {
        println!("  {kind:<34} {count}");
    }
    assert!(failures.is_empty(), "payloads that do not round-trip:\n{}", report(&failures));
    for kind in KINDS_IN_FIXTURES {
        assert!(checked.get(kind).copied().unwrap_or(0) > 0, "the fixtures contain `{kind}` payloads, but none was checked");
    }
}

/// The sample files of this area: `(file name, RPC, the payloads it holds)`.
/// A file named `<rpc>.json` or `<rpc>.<variant>.json` holds one result of a
/// unary RPC, or the items of a stream in an array.
fn samples() -> Vec<(String, String, Vec<Value>)> {
    const STREAMS: [&str; 2] = ["subscribeVcsStatus", "git.runStackedAction"];
    const RPCS: [&str; 16] = [
        "subscribeVcsStatus", "vcs.listRefs", "vcs.pull", "vcs.refreshStatus", "vcs.switchRef", "vcs.createRef",
        "git.runStackedAction", "git.resolvePullRequest", "git.preparePullRequestThread", "pullRequests.list",
        "pullRequests.summary", "pullRequests.runAction", "pullRequests.checks", "pullRequests.submitReview",
        "pullRequests.comment", "pullRequests.update",
    ];
    let directory = format!("{}/tests/samples", env!("CARGO_MANIFEST_DIR"));
    let mut names: Vec<String> = std::fs::read_dir(&directory)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| is_ours(name))
        .collect();
    names.sort();
    names
        .into_iter()
        .map(|name| {
            let stem = name.strip_suffix(".json").unwrap_or_else(|| panic!("{name} is not a .json file"));
            let rpc = RPCS
                .into_iter()
                .filter(|rpc| stem == *rpc || stem.starts_with(&format!("{rpc}.")))
                .max_by_key(|rpc| rpc.len())
                .unwrap_or_else(|| panic!("{name} is named after no RPC this test knows"));
            let document: Value = serde_json::from_str(&std::fs::read_to_string(format!("{directory}/{name}")).unwrap())
                .unwrap_or_else(|error| panic!("{name} is not JSON: {error}"));
            let payloads = if STREAMS.contains(&rpc) {
                document.as_array().unwrap_or_else(|| panic!("{name} holds a stream's items in an array")).clone()
            } else {
                vec![document]
            };
            (name, rpc.to_owned(), payloads)
        })
        .collect()
}

#[test]
fn samples_round_trip_through_their_types() {
    let samples = samples();
    let mut payloads = Vec::new();
    println!("sample payloads checked, by file:");
    for (name, rpc, items) in &samples {
        println!("  {name:<40} {}", items.len());
        assert!(!items.is_empty(), "{name} holds no payload");
        payloads.extend(items.iter().map(|item| (rpc.clone(), item.clone())));
    }
    let (checked, failures) = check(&payloads);
    println!("sample payloads checked, by kind:");
    for (kind, count) in &checked {
        println!("  {kind:<40} {count}");
    }
    assert!(failures.is_empty(), "samples that do not round-trip:\n{}", report(&failures));
    // What the samples are there to show.
    for kind in [
        "subscribeVcsStatus snapshot", "subscribeVcsStatus localUpdated", "subscribeVcsStatus remoteUpdated", "vcs.listRefs",
        "vcs.pull", "vcs.refreshStatus", "vcs.switchRef", "vcs.createRef", "git.runStackedAction action_started",
        "git.runStackedAction phase_started", "git.runStackedAction hook_started", "git.runStackedAction hook_output",
        "git.runStackedAction hook_finished", "git.runStackedAction action_finished", "git.runStackedAction action_failed",
        "git.resolvePullRequest", "git.preparePullRequestThread", "pullRequests.list", "pullRequests.summary",
        "pullRequests.runAction", "pullRequests.checks",
    ] {
        assert!(checked.contains_key(kind), "no sample holds a `{kind}` payload");
    }
}

fn sample(name: &str) -> Value {
    let path = format!("{}/tests/samples/{name}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

/// The samples say what their task asks of them, read through the types.
#[test]
fn samples_hold_what_the_client_reads() {
    let status: Vec<VcsStatusStreamEvent> = serde_json::from_value(sample("subscribeVcsStatus.domovoi-server.json")).unwrap();
    let [VcsStatusStreamEvent::Snapshot(first), VcsStatusStreamEvent::LocalUpdated(local), VcsStatusStreamEvent::RemoteUpdated(remote), VcsStatusStreamEvent::Snapshot(outside)] =
        &status[..]
    else {
        panic!("the status sample is a snapshot, a local update, a remote update and a snapshot: {status:?}");
    };
    let pr = first.remote.as_ref().unwrap().pr.as_ref().unwrap();
    assert_eq!((pr.number, pr.state.as_str(), pr.is_draft), (128, "open", Some(false)));
    assert_eq!(local.local.working_tree.files.len(), 3);
    assert_eq!(local.local.working_tree.files.iter().map(|file| file.insertions).sum::<u64>(), local.local.working_tree.insertions);
    assert!(remote.remote.as_ref().unwrap().pr.is_none());
    assert!(outside.remote.is_none() && !outside.local.is_repo);

    let pull: VcsPullResult = serde_json::from_value(sample("vcs.pull.json")).unwrap();
    assert_eq!(pull.status, VcsPullStatus::SkippedUpToDate);

    let events: Vec<GitActionProgressEvent> = serde_json::from_value(sample("git.runStackedAction.json")).unwrap();
    let Some(GitActionProgressEvent::ActionFinished(finished)) = events.last() else { panic!("the action sample ends finished") };
    assert_eq!(finished.result.toast.title, "Created PR #129");
    assert!(matches!(finished.result.toast.cta, GitActionToastCta::OpenPr(_)));
    let failed: Vec<GitActionProgressEvent> = serde_json::from_value(sample("git.runStackedAction.failed.json")).unwrap();
    let Some(GitActionProgressEvent::ActionFailed(failure)) = failed.last() else { panic!("the failed sample ends failed") };
    assert!(failure.message.starts_with("Cannot push"));

    let list: PullRequestListResult = serde_json::from_value(sample("pullRequests.list.json")).unwrap();
    assert_eq!(list.entries.len(), 3);
    assert_eq!(list.entries.iter().filter(|entry| entry.is_draft).count(), 1);
    assert_eq!(list.entries.iter().map(|entry| entry.state.as_str()).collect::<BTreeSet<_>>().len(), 2);
    let unconfigured: Vec<&PullRequestProviderSummary> = list.providers.iter().filter(|provider| !provider.configured).collect();
    assert_eq!((list.providers.len(), unconfigured.len()), (2, 1));
    assert_eq!(unconfigured[0].detail.as_deref(), Some("This host cannot be browsed here yet."));
    assert_eq!(list.providers[0].actions.as_ref().unwrap().len(), 11);
}

#[test]
fn an_unknown_enum_value_survives_a_round_trip() {
    let pull = json!({ "status": "rebased", "refName": "main", "upstreamRef": null });
    let typed: VcsPullResult = serde_json::from_value(pull.clone()).unwrap();
    assert_eq!(typed.status.as_str(), "rebased");
    assert_eq!(fallbacks(&typed).len(), 1);
    assert_eq!(serde_json::to_value(&typed).unwrap(), pull);

    let check = json!({ "name": "build", "status": "queued", "description": null, "url": null });
    let typed: PullRequestCheck = serde_json::from_value(check.clone()).unwrap();
    assert_eq!(typed.status, PullRequestCheckStatus::Other("queued".into()));
    assert_eq!(serde_json::to_value(&typed).unwrap(), check);

    // An action a newer host offers stays in the list, in its place.
    let provider = json!({
        "host": "github.com", "kind": "github", "searchesOnHost": true, "actions": ["merge", "queue", "close"],
        "projectCount": 1, "configured": true, "detail": null,
    });
    let typed: PullRequestProviderSummary = serde_json::from_value(provider.clone()).unwrap();
    assert_eq!(typed.actions.as_ref().unwrap()[1], PullRequestAction::Other("queue".into()));
    assert_eq!(serde_json::to_value(&typed).unwrap(), provider);
}

#[test]
fn an_unknown_union_tag_survives_a_round_trip() {
    let item = json!({ "_tag": "stashUpdated", "stashes": [{ "name": "wip" }] });
    let typed: VcsStatusStreamEvent = serde_json::from_value(item.clone()).unwrap();
    assert!(matches!(typed, VcsStatusStreamEvent::Other(_)));
    assert_eq!(serde_json::to_value(&typed).unwrap(), item);

    let event = json!({ "actionId": "a", "cwd": "/repo", "action": "commit", "kind": "phase_finished", "phase": "commit" });
    let typed: GitActionProgressEvent = serde_json::from_value(event.clone()).unwrap();
    assert!(matches!(typed, GitActionProgressEvent::Other(_)));
    assert_eq!(serde_json::to_value(&typed).unwrap(), event);

    // A button this build does not know does not cost the result it sits in.
    let result = json!({
        "action": "amend",
        "branch": { "status": "skipped_not_requested" },
        "commit": { "status": "created" },
        "push": { "status": "skipped_not_requested" },
        "pr": { "status": "skipped_not_requested" },
        "toast": { "title": "Done", "cta": { "kind": "open_commit", "label": "View commit", "sha": "abc" } },
    });
    let typed: GitRunStackedActionResult = serde_json::from_value(result.clone()).unwrap();
    assert!(matches!(typed.toast.cta, GitActionToastCta::Other(_)));
    assert_eq!(typed.action, GitStackedAction::Other("amend".into()));
    assert_eq!(serde_json::to_value(&typed).unwrap(), result);
}

#[test]
fn every_toast_button_round_trips() {
    for cta in [
        json!({ "kind": "none" }),
        json!({ "kind": "open_pr", "label": "View PR", "url": "https://github.com/o/r/pull/1" }),
        json!({ "kind": "run_action", "label": "Create PR", "action": { "kind": "create_pr" } }),
    ] {
        round_trip::<GitActionToastCta>(&cta).unwrap_or_else(|why| panic!("{cta} {why}"));
    }
}

#[test]
fn a_record_missing_a_required_field_is_an_error() {
    let missing = |error: serde_json::Error| error.to_string();
    // A known tag whose record is malformed is an error, not a fallback.
    let item = json!({ "_tag": "localUpdated" });
    assert!(missing(serde_json::from_value::<VcsStatusStreamEvent>(item).unwrap_err()).contains("missing field `local`"));
    let item = json!({ "_tag": "snapshot", "local": { "isRepo": true }, "remote": null });
    assert!(missing(serde_json::from_value::<VcsStatusStreamEvent>(item).unwrap_err()).contains("missing field `hasPrimaryRemote`"));
    let mut snapshot = sample("subscribeVcsStatus.domovoi-server.json")[0].clone();
    snapshot["remote"].as_object_mut().unwrap().remove("aheadCount");
    assert!(missing(serde_json::from_value::<VcsStatusStreamEvent>(snapshot).unwrap_err()).contains("missing field `aheadCount`"));
    assert!(serde_json::from_value::<VcsStatusStreamEvent>(json!({ "local": {} })).is_err(), "an item says what it is");

    let event = json!({ "actionId": "a", "cwd": "/repo", "action": "commit", "kind": "action_failed", "phase": null });
    assert!(missing(serde_json::from_value::<GitActionProgressEvent>(event).unwrap_err()).contains("missing field `message`"));

    let mut list = sample("pullRequests.list.json");
    list["entries"][0].as_object_mut().unwrap().remove("projectId");
    assert!(missing(serde_json::from_value::<PullRequestListResult>(list).unwrap_err()).contains("missing field `projectId`"));

    let mut summary = sample("pullRequests.summary.json");
    summary["number"] = json!("128");
    assert!(serde_json::from_value::<PullRequestSummary>(summary).is_err(), "a number sent as a string is not a number");

    let refs = json!({ "refs": [{ "name": "main", "isDefault": true, "worktreePath": null }], "isRepo": true, "hasPrimaryRemote": false, "nextCursor": null, "totalCount": 1 });
    assert!(missing(serde_json::from_value::<VcsListRefsResult>(refs).unwrap_err()).contains("missing field `current`"));
}

/// Fields of the schemas the Domovoi server never sends, as a t3code server would.
#[test]
fn what_only_a_t3code_server_sends_round_trips() {
    let mut list = sample("pullRequests.list.json");
    list["entries"][0]["stack"] = json!({ "number": 4, "position": 2, "size": 3, "base": "main" });
    list["providers"][0]["projectIds"] = json!(["project-t5code", "project-playground"]);
    list["nextCursors"] = json!({ "github.com/acme/t5code": "Y3Vyc29yOjUw" });
    round_trip::<PullRequestListResult>(&list).unwrap();

    let mut summary = sample("pullRequests.summary.json");
    summary["stack"] = Value::Null;
    summary["observedAt"] = json!(1794938400456.5);
    round_trip::<PullRequestSummary>(&summary).unwrap();
    summary["stack"] = json!({ "number": 4, "position": 1, "size": 3, "base": "main" });
    round_trip::<PullRequestSummary>(&summary).unwrap();

    let mut checks = sample("pullRequests.checks.json");
    checks["checks"][0]["placeholder"] = json!(true);
    checks.as_object_mut().unwrap().remove("observedAt");
    round_trip::<PullRequestChecks>(&checks).unwrap();

    // A t3code server answers an action with nothing unless asked for the summary, and checks with `null` at times.
    assert_eq!(serde_json::from_value::<PullRequestActionResult>(Value::Null).unwrap(), None);
    assert_eq!(serde_json::from_value::<PullRequestChecksResult>(Value::Null).unwrap(), None);

    // An older server's branch status and prepared thread leave out what was added since.
    round_trip::<VcsBranchStatus>(&json!({ "refName": null })).unwrap();
    let mut prepared = sample("git.preparePullRequestThread.json");
    prepared.as_object_mut().unwrap().remove("isOnPullRequestHead");
    prepared["worktreePath"] = Value::Null;
    round_trip::<GitPreparePullRequestThreadResult>(&prepared).unwrap();
    let typed: GitPreparePullRequestThreadResult = serde_json::from_value(prepared).unwrap();
    assert_eq!(typed.is_on_pull_request_head, None);
}

/// Where the Domovoi server has no value for an optional field of a step it sends
/// the key as `null`; that reads as absent, as t3code's own codec reads it.
#[test]
fn a_null_optional_field_reads_as_absent() {
    let step = json!({ "status": "created", "url": null, "number": null, "baseBranch": null, "headBranch": null, "title": null });
    let typed: domovoi_model::typed::GitPrStep = serde_json::from_value(step).unwrap();
    assert_eq!(serde_json::to_value(&typed).unwrap(), json!({ "status": "created" }));
    let step = json!({ "status": "created", "commitSha": null, "subject": "Add x" });
    let typed: domovoi_model::typed::GitCommitStep = serde_json::from_value(step).unwrap();
    assert_eq!(serde_json::to_value(&typed).unwrap(), json!({ "status": "created", "subject": "Add x" }));
}

/// The halves of `vcs.refreshStatus` are the records the status stream sends.
#[test]
fn a_refreshed_status_holds_both_halves() {
    let status = sample("vcs.refreshStatus.json");
    round_trip::<VcsStatus>(&status).unwrap();
    let local: VcsStatusLocal = serde_json::from_value(status.clone()).unwrap();
    let remote: VcsStatusRemote = serde_json::from_value(status.clone()).unwrap();
    let mut merged = serde_json::to_value(&local).unwrap();
    merged.as_object_mut().unwrap().extend(serde_json::to_value(&remote).unwrap().as_object().unwrap().clone());
    assert_eq!(merged, status);
}

#[test]
fn a_list_entry_names_its_pull_request() {
    let list: PullRequestListResult = serde_json::from_value(sample("pullRequests.list.json")).unwrap();
    let entry: &PullRequestListEntry = &list.entries[0];
    let reference: PullRequestRef = entry.reference();
    assert_eq!(
        serde_json::to_value(&reference).unwrap(),
        json!({ "projectId": "project-t5code", "repository": "acme/t5code", "number": 128 })
    );
    assert_eq!(entry.state, PullRequestState::Open);
}
