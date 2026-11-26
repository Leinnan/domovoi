//! Commands decode into their typed records and encode back to the JSON they
//! came from.
//!
//! Two sources: every `orchestration.dispatchCommand` and
//! `orchestration.launchThread` request in the recorded session, and one
//! example of every command type, with every field it defines.

use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use domovoi_model::typed::{Command, ThreadLaunchInput, ThreadLaunchResult};


const FIXTURES: [&str; 1] = ["session1"];

/// The command types the recorded sessions are known to contain.
const RECORDED_TYPES: [&str; 3] = ["message.dispatch", "runtime-request.respond", "thread.visit"];

fn workspace_root() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The payload of every request to `rpc` in the recorded sessions.
fn recorded(rpc: &str) -> Vec<Value> {
    let mut payloads = Vec::new();
    for name in FIXTURES {
        let path = workspace_root().join(format!("fixtures/native/{name}/wire.ndjson"));
        for line in std::fs::read_to_string(&path).unwrap().lines() {
            let line: Value = serde_json::from_str(line).unwrap();
            let frame = &line["frame"];
            if line["kind"] == "ws" && frame["_tag"] == "Request" && frame["tag"] == rpc {
                payloads.push(frame["payload"].clone());
            }
        }
    }
    payloads
}

/// Decodes `sent` as a command of a known type and encodes it again.
fn round_trip(sent: &Value) -> Result<Command, String> {
    let command = Command::from_value(sent.clone()).map_err(|error| format!("does not decode: {error}"))?;
    if matches!(command, Command::Other(_)) {
        return Err("decodes as a command of an unknown type".into());
    }
    if sent["type"] != command.type_name() {
        return Err(format!("decodes as `{}`", command.type_name()));
    }
    let encoded = serde_json::to_value(&command).map_err(|error| format!("does not encode: {error}"))?;
    if encoded != *sent {
        return Err(format!("encodes differently:\n  sent    {sent}\n  encoded {encoded}"));
    }
    Ok(command)
}

#[test]
fn recorded_commands_round_trip_through_their_types() {
    let commands = recorded("orchestration.dispatchCommand");
    let mut by_type: BTreeMap<String, usize> = BTreeMap::new();
    let mut problems = Vec::new();
    for sent in &commands {
        match round_trip(sent) {
            Ok(command) => *by_type.entry(command.type_name().to_owned()).or_default() += 1,
            Err(problem) => problems.push(format!("{}: {problem}", sent["type"])),
        }
    }
    assert!(problems.is_empty(), "{} of {} recorded commands:\n{}", problems.len(), commands.len(), problems.join("\n"));
    assert_eq!(by_type.keys().map(String::as_str).collect::<Vec<_>>(), RECORDED_TYPES, "the walk over the fixtures found other types");
    let unrecorded = Command::TYPES.len() - by_type.len();
    eprintln!("{} recorded commands of {} types round-trip {by_type:?}; {unrecorded} types are not in the recordings", commands.len(), by_type.len());
}

#[test]
fn recorded_launches_round_trip_through_their_type() {
    let launches = recorded("orchestration.launchThread");
    assert!(!launches.is_empty(), "the fixtures hold launches");
    for sent in &launches {
        let input: ThreadLaunchInput = serde_json::from_value(sent.clone()).unwrap_or_else(|error| panic!("a recorded launch does not decode: {error}\n{sent}"));
        assert_eq!(json!(input), *sent, "a recorded launch encodes differently");
    }
    eprintln!("{} recorded launches round-trip", launches.len());
}

/// What the server answered the recorded launches and commands with.
#[test]
fn recorded_answers_round_trip_through_their_types() {
    let (mut launched, mut dispatched) = (0, 0);
    for name in FIXTURES {
        let path = workspace_root().join(format!("fixtures/native/{name}/wire.ndjson"));
        let mut requests: BTreeMap<(i64, String), String> = BTreeMap::new();
        for line in std::fs::read_to_string(&path).unwrap().lines() {
            let line: Value = serde_json::from_str(line).unwrap();
            let (frame, connection) = (&line["frame"], line["conn"].as_i64().unwrap_or(0));
            if line["kind"] != "ws" {
                continue;
            }
            if frame["_tag"] == "Request" {
                requests.insert((connection, frame["id"].to_string()), frame["tag"].as_str().unwrap_or_default().to_owned());
            } else if frame["_tag"] == "Exit" && frame["exit"]["_tag"] == "Success" {
                let value = &frame["exit"]["value"];
                match requests.get(&(connection, frame["requestId"].to_string())).map(String::as_str) {
                    Some("orchestration.dispatchCommand") => {
                        let result: domovoi_model::typed::DispatchCommandResult = serde_json::from_value(value.clone()).expect("a dispatch result");
                        assert_eq!(json!(result), *value);
                        dispatched += 1;
                    }
                    Some("orchestration.launchThread") => {
                        let result: ThreadLaunchResult = serde_json::from_value(value.clone()).expect("a launch result");
                        assert_eq!(json!(result), *value);
                        launched += 1;
                    }
                    _ => {}
                }
            }
        }
    }
    assert!(launched > 0 && dispatched > 0, "the fixtures hold answers ({launched} launches, {dispatched} commands)");
    eprintln!("{launched} launch results and {dispatched} dispatch results round-trip");
}

fn model() -> Value {
    json!({ "instanceId": "codex", "model": "gpt-6-luna", "options": [{ "id": "reasoningEffort", "value": "low" }, { "id": "fast", "value": true }] })
}

fn image() -> Value {
    json!({ "type": "image", "id": "att-1", "name": "shot.png", "mimeType": "image/png", "sizeBytes": 12 })
}

fn notification() -> Value {
    json!({ "source": { "kind": "monitor", "pullRequest": { "host": "github.com", "repository": "a/b", "number": 7,
        "watchStartedAt": "2026-01-02T03:04:05.678Z", "headSha": "abc" } }, "outcome": "updated", "summary": "Checks passed", "detail": "All green." })
}

fn watch() -> Value {
    json!({ "startedAt": "2026-01-02T03:04:05.678Z", "headSha": "abc", "failedChecks": ["lint"], "passed": false, "passedChecks": ["build"],
        "remarksThrough": "2026-01-02T03:04:05.678Z", "remarkIds": ["r1"], "conflicting": false, "wakes": 2, "checksPending": true })
}

fn linked_pull_request() -> Value {
    json!({ "projectId": "p1", "repository": "a/b", "number": 7, "url": "https://github.com/a/b/pull/7" })
}

/// One command of every type, each with every field its schema defines.
fn examples() -> Vec<Value> {
    let at = "2026-01-02T03:04:05.678Z";
    let on = |ty: &str, fields: Value| {
        let mut command = json!({ "type": ty, "commandId": "c1", "threadId": "t1" });
        command.as_object_mut().unwrap().extend(fields.as_object().unwrap().clone());
        command
    };
    let under = |ty: &str, fields: Value| {
        let mut command = json!({ "type": ty, "commandId": "c1", "parentThreadId": "t1" });
        command.as_object_mut().unwrap().extend(fields.as_object().unwrap().clone());
        command
    };
    let key = |ty: &str, fields: Value| {
        let mut command = on(ty, json!({ "host": "github.com", "repository": "a/b", "number": 7 }));
        command.as_object_mut().unwrap().extend(fields.as_object().unwrap().clone());
        command
    };
    let snapshot = json!({ "state": "open", "title": "Add search", "headBranch": "feature/x", "baseBranch": "main", "isDraft": false,
        "updatedAt": at, "syncedAt": at, "mergedAt": null, "author": { "login": "octo", "name": null, "avatarUrl": null }, "additions": 3,
        "deletions": 1, "changedFiles": 2, "reviewDecision": null, "checksState": null });
    let stack = json!({ "kind": "native", "id": "s1", "number": 1, "url": "https://github.com/a/b/stack/1", "base": "main",
        "layers": [{ "number": 7, "headBranch": "feature/x", "state": "open" }] });
    let wake = json!({ "messageId": "m1", "text": "It is linked.", "notification": notification() });
    vec![
        on("thread.stop", json!({ "reason": "user", "stopDelegatedTasks": false })),
        on("thread.create", json!({ "createdBy": "agent", "creationSource": "mcp", "projectId": "p1", "title": "New", "modelSelection": model(),
            "runtimeMode": "full-access", "interactionMode": "default", "branch": null, "worktreePath": "/work/tree",
            "linkOrigin": { "sessionId": "s1", "label": "Laptop", "access": "read-only" },
            "delegatedFrom": { "environmentId": "e1", "threadId": "t0", "title": "Parent" },
            "importedNativeThread": { "ref": { "driver": "codex", "nativeId": "n1", "strength": "strong" },
                "metadata": { "title": "Imported", "itemIdentityVersion": 2 } } })),
        on("thread.archive", json!({})),
        on("thread.guest-share.close", json!({})),
        on("thread.guest-message.dismiss", json!({ "guestMessageId": "g1" })),
        on("thread.unarchive", json!({})),
        on("thread.delete", json!({ "onlyIfUnused": true })),
        on("thread.settle", json!({ "expectedLatestRunId": "r1", "settledAt": at })),
        on("thread.auto-settle", json!({ "snapshotAt": at, "settledAt": at })),
        on("thread.unsettle", json!({ "reason": "user" })),
        on("thread.snooze", json!({ "snoozedUntil": at })),
        on("thread.unsnooze", json!({ "reason": "user" })),
        on("thread.auto-settle.set", json!({ "enabled": false })),
        on("thread.pin", json!({ "orderKey": "a0" })),
        on("thread.unpin", json!({})),
        on("thread.tag.set", json!({ "tag": "bug", "tagged": true })),
        on("thread.pin.reorder", json!({ "orderKey": "a1" })),
        on("thread.active.reorder", json!({ "orderKey": "b1" })),
        on("thread.visit", json!({ "visitedAt": at, "opened": true, "searchQuery": "search" })),
        on("thread.mark-unread", json!({})),
        on("thread.metadata.update", json!({ "title": "Renamed", "regenerateTitle": false, "branch": "main", "worktreePath": null,
            "expectedWorktreePath": "/work/tree", "expectedBranch": null, "expectedEmpty": true,
            "limitRecovery": { "runId": "r1", "resetAt": at, "autoResume": true, "snooze": false }, "linkedPullRequest": linked_pull_request() })),
        on("thread.metadata.update", json!({ "limitRecovery": null, "linkedPullRequest": null })),
        key("thread.pull-request.link", json!({ "url": "https://github.com/a/b/pull/7", "source": "manual", "repositoryWebUrl": "https://github.com/a/b" })),
        key("thread.pull-request.unlink", json!({})),
        on("thread.issue.link", json!({ "tracker": "github", "host": "github.com", "scope": "a/b", "key": "12", "projectId": "p1",
            "url": "https://github.com/a/b/issues/12", "displayKey": "#12", "title": "Crash on start", "source": "agent" })),
        on("thread.issue.link", json!({ "tracker": "github", "host": "github.com", "scope": "a/b", "key": "13", "projectId": "p1",
            "url": "https://github.com/a/b/issues/13", "displayKey": "#13", "title": "Crash on exit", "source": "manual", "role": "focus" })),
        on("thread.issue.unlink", json!({ "tracker": "linear", "host": "linear.app", "scope": "ENG", "key": "ENG-4" })),
        key("thread.pull-request-link.sync", json!({ "snapshot": snapshot, "stack": stack })),
        key("thread.pull-request.watch", json!({ "watching": true,
            "link": { "url": "https://github.com/a/b/pull/7", "source": "agent", "repositoryWebUrl": "https://github.com/a/b" } })),
        // The watch of a pull request made from the thread: what it wakes the agent for.
        key("thread.pull-request.watch", json!({ "watching": true, "until": "merged", "checksPending": true,
            "link": { "url": "https://github.com/a/b/pull/7", "source": "created" },
            "policy": { "wakeOnFailedChecks": true, "wakeOnPassed": false, "wakeOnMerged": false, "maxFixAttempts": 3 } })),
        on("thread.pull-request.sync", json!({ "projectId": "p1", "snapshotSequence": 12,
            "expected": { "workspaceRoot": "/work", "branch": "main", "worktreePath": null, "linkedPullRequest": null, "branchPullRequest": linked_pull_request() },
            "branchPullRequest": null, "linkedPullRequest": linked_pull_request() })),
        on("thread.title.regeneration.complete", json!({ "requestId": "c0", "title": "Search bar", "outcome": { "status": "failed", "message": "no model" } })),
        on("thread.runtime-mode.set", json!({ "runtimeMode": "auto-accept-edits" })),
        on("thread.interaction-mode.set", json!({ "interactionMode": "plan" })),
        on("thread.model-selection.set", json!({ "modelSelection": model() })),
        on("provider-session.detach", json!({ "providerSessionId": "ps1", "reason": "idle" })),
        on("message.dispatch", json!({ "notification": notification(), "guestMessageId": "g1", "createdBy": "user", "creationSource": "web",
            "scheduledTaskId": "st1", "senderThreadId": "t0", "linkedSender": { "environmentId": "e1", "threadId": "t9", "title": "Elsewhere" },
            "idleCompaction": "onReturn", "freshContext": true, "messageId": "m1", "text": "hello",
            "context": { "version": 1, "guest": { "guestMessageId": "g1", "name": "Sam", "text": "hi", "shareId": "sh1" }, "records": [] },
            "attachments": [image()], "titleSeed": "hello", "modelSelection": model(), "sourcePlanRef": { "threadId": "t1", "planId": "pl1" },
            "restartContinuationOfRunId": "r1", "usageLimitContinuationOfRunId": "r2", "manualContinuationOfRunId": "r3", "steerFollowUpOfRunId": "r4",
            "steerFollowUpAfterRunOrdinal": 4, "usageLimitRecoveryRequestId": "c0", "deliveryIntent": "steer",
            "delegatedCompletion": { "parentRunId": "r1", "generation": 1, "taskIds": ["n1"] },
            "dispatchMode": { "type": "defer_start", "workspaceStrategy": { "type": "worktree", "baseRef": "main", "branch": "feature/x", "startFromOrigin": true } } })),
        on("message.dispatch", json!({ "createdBy": "user", "creationSource": "web", "messageId": "m1", "text": "hello", "attachments": [],
            "dispatchMode": { "type": "steer_active", "targetRunId": "r1" } })),
        on("message.dispatch", json!({ "createdBy": "user", "creationSource": "web", "messageId": "m1", "text": "hello", "attachments": [],
            "dispatchMode": { "type": "restart_active", "targetRunId": "r1" } })),
        on("message.dispatch", json!({ "createdBy": "system", "creationSource": "server", "messageId": "m1", "text": "hello", "attachments": [],
            "dispatchMode": { "type": "queue_after_active" } })),
        on("message.dispatch", json!({ "createdBy": "user", "creationSource": "mobile", "messageId": "m1", "text": "hello", "attachments": [],
            "dispatchMode": { "type": "defer_start" } })),
        on("prepared-run.release", json!({ "runId": "r1" })),
        on("notification.delivery.accept", json!({ "messageId": "m1" })),
        on("prepared-run.progress", json!({ "runId": "r1", "phase": "worktree" })),
        on("prepared-run.fail", json!({ "runId": "r1",
            "failure": { "class": "provider_error", "message": "It broke.", "code": null, "retryable": false, "resetAt": null } })),
        on("prepared-run.retry", json!({ "runId": "r1" })),
        on("subagent.stop", json!({ "runId": "r1", "subagentId": "n1" })),
        on("run.interrupt", json!({ "runId": "r1", "reason": "user", "holdQueue": true })),
        on("queued-message.promote-to-steer", json!({ "queuedRunId": "r2", "targetRunId": "r1" })),
        on("queue.resume", json!({})),
        on("queued-run.start", json!({ "runId": "r2" })),
        on("queued-run.reorder", json!({ "runId": "r2", "beforeRunId": "r3" })),
        on("queued-run.reorder", json!({ "runId": "r2", "beforeRunId": null })),
        on("queued-run.cancel", json!({ "runId": "r2" })),
        on("queued-run.edit", json!({ "context": { "version": 1, "records": [{ "kind": "file", "path": "a.rs" }] }, "runId": "r2", "text": "edited",
            "attachments": [image()] })),
        on("runtime-request.respond", json!({ "requestId": "q1", "decision": "acceptForSession", "feedback": "fine",
            "answers": { "question-1": ["a", "b"], "question-2": "text" }, "attachmentsByQuestionId": { "question-1": [image()] } })),
        on("thread.user-input.dismiss", json!({ "requestId": "q1" })),
        on("checkpoint.rollback", json!({ "restoreFiles": true, "scopeId": "s1", "checkpointId": "cp1" })),
        json!({ "type": "thread.fork", "createdBy": "user", "creationSource": "web", "commandId": "c1", "sourceThreadId": "t1", "targetThreadId": "t2",
            "sourcePoint": { "type": "run", "runId": "r1" }, "title": "A fork", "sideChat": true, "createdAt": at }),
        json!({ "type": "thread.fork", "createdBy": "user", "creationSource": "web", "commandId": "c1", "sourceThreadId": "t1", "targetThreadId": "t2",
            "sourcePoint": { "type": "checkpoint", "checkpointId": "cp1" } }),
        json!({ "type": "thread.merge_back", "createdBy": "user", "creationSource": "web", "commandId": "c1", "sourceThreadId": "t2", "targetThreadId": "t1",
            "sourcePoint": { "type": "latest_stable" }, "createdAt": at }),
        under("delegated_task.request", json!({ "createdBy": "agent", "creationSource": "mcp", "parentRunId": "r1", "parentNodeId": "n1", "task": "Look into it",
            "title": "Look", "modelSelection": model(), "runtimeMode": "auto", "interactionMode": "default",
            "workspaceStrategy": { "type": "existing_worktree", "worktreePath": "/work/tree", "branch": "main" }, "completionWake": "always", "createdAt": at })),
        under("delegated_task.wake-policy", json!({ "taskId": "n2", "completionWake": "settled_only" })),
        under("delegated_task.completion-delivery.acknowledge", json!({ "taskId": "n2", "observedByRunId": null })),
        under("delegated_task.completion-delivery.dispose", json!({ "taskId": "n2" })),
        under("thread.created.record", json!({ "parentRunId": "r1", "parentNodeId": "n1", "targetThreadId": "t2", "targetRunId": "r9" })),
        on("provider.switch", json!({ "modelSelection": model() })),
        // Server-only commands.
        on("thread.guest-share.open", json!({ "shareId": "sh1", "host": "relay.example", "expiresAt": at })),
        on("thread.guest-share.redeem", json!({ "shareId": "sh1", "guestName": "Sam" })),
        on("thread.guest-message.submit", json!({ "shareId": "sh1", "guestMessageId": "g1", "text": "hi", "attachments": [image()] })),
        on("thread.project.adopt-folder", json!({ "projectId": "p2" })),
        on("thread.pull-request-watch.checks-reopen", json!({})),
        key("thread.pull-request-watch.sync", json!({ "startedAt": at, "watch": watch(), "checksPendingBefore": true, "endedWatch": watch(),
            "others": [{ "host": "github.com", "repository": "a/b", "number": 8, "startedAt": at, "watch": null, "checksPendingBefore": false,
                "endedWatch": watch(), "includesRemarks": true }],
            "wake": { "messageId": "m1", "text": "Checks passed.", "notification": notification(), "includesRemarks": false,
                "sections": [{ "text": "PR 7", "outcome": "completed", "summary": "Passed" }] } })),
        under("delegated_task.remote.request", json!({ "parentRunId": "r1", "parentNodeId": "n1", "task": "Look into it", "title": "Look",
            "driver": "codex", "modelSelection": model(),
            "remoteChild": { "environmentId": "e1", "threadId": "t9", "label": "Laptop", "title": "Look", "runId": "r1", "stoppedThere": false, "stopCount": 1 },
            "completionWake": "always" })),
        under("delegated_task.remote.complete", json!({ "taskId": "n2", "status": "interrupted", "result": "Stopped.", "stoppedThere": "ran-out", "stopCount": 2 })),
        under("delegated_task.remote.stop", json!({ "taskId": "n2" })),
        on("checkpoint.rollback.fail", json!({ "requestId": "c0", "message": "Could not restore files." })),
        on("runtime-request.delivery.fail", json!({ "requestId": "q1" })),
        on("thread.interrupt.unresponsive", json!({ "providerThreadId": "pt1", "providerTurnId": "ptn1" })),
        on("thread.background-work.settle", json!({ "providerThreadId": "pt1", "providerTurnId": "ptn1", "reason": "force-stop",
            "subagent": { "id": "n2", "stoppedAt": at, "outcome": { "status": "cancelled", "completedAt": at, "result": null } } })),
        on("secret_request.record", json!({ "runId": "r1", "nodeId": "n1", "turnItemId": "i1", "label": "API key", "reason": "To deploy.",
            "url": "https://example.com/keys", "placeholder": "sk-…", "delivery": "file", "secretStatus": "pending" })),
        on("link_request.record", json!({ "runId": "r1", "nodeId": "n1", "turnItemId": "i1", "url": "https://example.com", "reason": "To reach the laptop.",
            "environmentId": "e1", "label": "Laptop", "hint": "Open settings", "requestedAccess": "read-only", "linkStatus": "linked",
            "linkedEnvironmentId": "e1", "linkedLabel": "Laptop", "linkedAccess": "read-only", "failure": "none", "wake": wake })),
    ]
}

fn launch_examples() -> Vec<Value> {
    vec![
        json!({ "commandId": "c1", "creationSource": "mobile", "threadId": "t1", "reuseExistingThread": false, "projectId": "p1", "title": "New thread",
            "generateTitle": true, "modelSelection": model(), "runtimeMode": "approval-required", "interactionMode": "plan",
            "workspaceStrategy": { "type": "worktree", "baseRef": "main", "branch": "feature/x", "startFromOrigin": false },
            "initialMessage": { "messageId": "m1", "text": "hello", "context": { "version": 1, "records": [] }, "attachments": [image()] } }),
        json!({ "commandId": "c1", "projectId": "p1", "title": "New thread", "modelSelection": { "instanceId": "claudeAgent", "model": "claude-haiku-5-5" },
            "runtimeMode": "full-access", "interactionMode": "default", "workspaceStrategy": { "type": "root", "branch": "main" } }),
        json!({ "commandId": "c1", "projectId": "p1", "title": "New thread", "modelSelection": { "instanceId": "claudeAgent", "model": "claude-haiku-5-5" },
            "runtimeMode": "auto", "interactionMode": "default", "workspaceStrategy": { "type": "existing_worktree", "worktreePath": "/work/tree" },
            "initialMessage": { "text": "hello", "attachments": [] } }),
        // Started on an issue: the reference, and whether to take the issue.
        json!({ "commandId": "c1", "projectId": "p1", "title": "", "modelSelection": { "instanceId": "claudeAgent", "model": "claude-haiku-5-5" },
            "runtimeMode": "auto", "interactionMode": "default", "workspaceStrategy": { "type": "root" },
            "initialMessage": { "text": "", "attachments": [] },
            "issue": { "issue": { "projectId": "p1", "tracker": "github", "host": "github.com", "scope": "a/b", "key": "12" }, "assignToMe": true } }),
        json!({ "commandId": "c1", "projectId": "p1", "title": "", "modelSelection": { "instanceId": "claudeAgent", "model": "claude-haiku-5-5" },
            "runtimeMode": "auto", "interactionMode": "default", "workspaceStrategy": { "type": "root" },
            "issue": { "issue": { "projectId": "p1", "tracker": "linear", "scope": "ENG", "key": "ENG-4" } } }),
    ]
}

#[test]
fn every_command_type_has_an_example_that_round_trips() {
    let mut covered = BTreeSet::new();
    let mut problems = Vec::new();
    for sent in examples() {
        match round_trip(&sent) {
            Ok(command) => {
                covered.insert(command.type_name().to_owned());
            }
            Err(problem) => problems.push(format!("{}: {problem}", sent["type"])),
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
    let known: BTreeSet<String> = Command::TYPES.iter().map(|ty| (*ty).to_owned()).collect();
    assert_eq!(covered, known, "an example for every command type, and no other");
    assert_eq!(known.len(), Command::TYPES.len(), "no type is declared twice");
    for sent in launch_examples() {
        let input: ThreadLaunchInput = serde_json::from_value(sent.clone()).unwrap_or_else(|error| panic!("a launch example does not decode: {error}"));
        assert_eq!(json!(input), sent);
    }
    let unrecorded: Vec<&str> = Command::TYPES.iter().copied().filter(|ty| !RECORDED_TYPES.contains(ty)).collect();
    eprintln!("{} command types round-trip from examples; {} of them are not in the recordings: {}", known.len(), unrecorded.len(), unrecorded.join(", "));
}
