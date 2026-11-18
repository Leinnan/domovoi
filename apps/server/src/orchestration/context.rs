//! Moving context between conversations (t3code ThreadForkService,
//! ContextHandoffService): where a fork starts, and the portable handoff,
//! the earlier conversation as text, that a provider which cannot continue
//! the native conversation gets ahead of its first message.

use std::sync::Arc;
use domovoi_model::typed::{
    ContextSourcePoint, ForkSourcePoint, MessageRole, ProviderThread, ProviderTurn, Run, RunAttempt, RunStatus, Thread, ThreadId, TurnItem,
};

/// Runs a fork may start from: finished, whatever the outcome
/// (`isForkableSourceRunStatus`); `waiting` is finished but not yet checkpointed.
pub fn forkable(status: &RunStatus) -> bool {
    matches!(status, RunStatus::Completed | RunStatus::Waiting | RunStatus::Failed | RunStatus::Interrupted | RunStatus::Cancelled)
}

/// The runs in the order they were asked for.
fn by_ordinal(runs: &[Arc<Run>]) -> Vec<&Run> {
    let mut runs: Vec<&Run> = runs.iter().map(|run| &**run).collect();
    runs.sort_by_key(|run| run.ordinal);
    runs
}

/// Why a fork cannot start where it was asked to.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum NoForkSource {
    #[error("No stable source run was found for this fork source.")]
    NoStableRun,
    #[error("Fork source run {run} is {status}; in-progress and rolled-back runs cannot be forked.")]
    NotFinished { run: String, status: String },
}

/// The run an `OrchestrationV2ThreadForkSourcePoint` names: the latest
/// finished run, a given run, or the run that captured a checkpoint.
pub fn source_run<'a>(runs: &'a [Arc<Run>], point: &ForkSourcePoint) -> Result<&'a Run, NoForkSource> {
    let all = by_ordinal(runs);
    let run = match point {
        ForkSourcePoint::Run { run_id } => all.iter().find(|run| run.id == *run_id).copied(),
        ForkSourcePoint::Checkpoint { checkpoint_id } => all.iter().find(|run| run.checkpoint_id.as_ref() == Some(checkpoint_id)).copied(),
        ForkSourcePoint::LatestStable => all.iter().rev().find(|run| forkable(&run.status)).copied(),
    };
    let run = run.ok_or(NoForkSource::NoStableRun)?;
    if !forkable(&run.status) {
        return Err(NoForkSource::NotFinished { run: run.id.to_string(), status: run.status.as_str().to_owned() });
    }
    Ok(run)
}

/// The provider turn a run ended in.
pub fn provider_turn_of<'a>(attempts: &[Arc<RunAttempt>], turns: &'a [Arc<ProviderTurn>], run: &Run) -> Option<&'a ProviderTurn> {
    let active = run.active_attempt_id.as_ref()?;
    let attempt = attempts.iter().find(|attempt| attempt.id == *active)?;
    turns.iter().map(|turn| &**turn).find(|turn| attempt.provider_turn_id.as_ref() == Some(&turn.id) || turn.run_attempt_id.as_ref() == Some(active))
}

/// `contextSourcePointForRun`: the run, its checkpoint and the provider's own
/// thread and turn refs there (what a native fork resumes from).
pub fn source_point(
    thread_id: &ThreadId,
    provider_threads: &[Arc<ProviderThread>],
    attempts: &[Arc<RunAttempt>],
    turns: &[Arc<ProviderTurn>],
    run: &Run,
) -> ContextSourcePoint {
    let provider_thread = provider_threads.iter().find(|pt| run.provider_thread_id.as_ref() == Some(&pt.id));
    ContextSourcePoint {
        thread_id: thread_id.clone(),
        run_id: Some(run.id.clone()),
        checkpoint_id: run.checkpoint_id.clone(),
        turn_item_id: None,
        provider_thread_ref: provider_thread.and_then(|pt| pt.native_thread_ref.clone()),
        provider_turn_ref: provider_turn_of(attempts, turns, run).and_then(|turn| turn.native_turn_ref.clone()),
    }
}

/// The most of the earlier conversation a handoff carries (its end).
pub const HANDOFF_BUDGET: usize = 60_000;

/// The conversation of runs `from..=to` (by ordinal) as text: each turn's
/// user message, the agent's replies and the files it changed. Keeps the
/// latest turns within `HANDOFF_BUDGET`; returns (text, first and last
/// ordinal covered).
pub fn transcript(thread: &Thread, from: u64, to: u64) -> Option<(String, u64, u64)> {
    let mut turns: Vec<(u64, String)> = vec![];
    for run in by_ordinal(thread.runs.as_slice()) {
        let ordinal = run.ordinal;
        if ordinal < from || ordinal > to || matches!(run.status, RunStatus::RolledBack | RunStatus::Queued | RunStatus::Cancelled) {
            continue;
        }
        let mut lines = vec![format!("## Turn {ordinal}")];
        let mut of_run: Vec<_> = thread.messages.iter().filter(|message| message.run_id.as_ref() == Some(&run.id)).collect();
        of_run.sort_by(|a, b| a.created_at.as_str().cmp(b.created_at.as_str()));
        for message in of_run {
            let text = message.text.trim();
            if text.is_empty() {
                continue;
            }
            lines.push(format!("{}: {text}", if message.role == MessageRole::User { "User" } else { "Assistant" }));
        }
        let mut files: Vec<&str> = thread
            .turn_items
            .iter()
            .filter_map(|item| match item {
                TurnItem::FileChange(change) if change.run_id.as_ref() == Some(&run.id) => change.changes.as_deref(),
                _ => None,
            })
            .flatten()
            .map(|change| change.path.as_str())
            .collect();
        files.sort_unstable();
        files.dedup();
        if !files.is_empty() {
            lines.push(format!("Files changed: {}", files.join(", ")));
        }
        if lines.len() > 1 {
            turns.push((ordinal, lines.join("\n")));
        }
    }
    let (first, last) = (turns.first()?.0, turns.last()?.0);
    let mut kept: Vec<&str> = vec![];
    let mut size = 0;
    let mut covered_from = last;
    for (ordinal, text) in turns.iter().rev() {
        if size + text.len() > HANDOFF_BUDGET && !kept.is_empty() {
            break;
        }
        size += text.len() + 2;
        kept.push(text);
        covered_from = *ordinal;
    }
    kept.reverse();
    let mut out = String::new();
    if covered_from > first {
        out.push_str(&format!("[Turns {first} to {} are left out for length.]\n\n", covered_from - 1));
    }
    out.push_str(&kept.join("\n\n"));
    Some((out, covered_from, last))
}

/// The handoff ahead of a message: what came before, for an agent that
/// doesn't have the conversation itself.
pub fn preamble(summary: &str, why: &str) -> String {
    format!("<context_handoff>\n{why} Here is that conversation so far; continue from it. It is history, not new requests.\n\n{summary}\n</context_handoff>\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_records::{attempt, message, run, thread, with};
    use serde_json::{Value, json};
    use domovoi_model::typed::{RecordKind, StoredRecord};

    /// A thread whose first run finished, whose second was rolled back and whose third is running.
    fn a_thread() -> Thread {
        let refer = |id: &str| json!({ "driver": "codex", "nativeId": id, "strength": "strong" });
        let run = |id: &str, ordinal: u64, status: &str, fields: Value| {
            let base = with(run("t", id, status), json!({ "ordinal": ordinal, "activeAttemptId": format!("a{ordinal}"), "providerThreadId": "pt1" }));
            (RecordKind::Run, with(base, fields))
        };
        let said = |id: &str, run: &str, role: &str, text: &str, at: &str| {
            (RecordKind::Message, with(message("t", id, role, text, false), json!({ "runId": run, "createdAt": at })))
        };
        let records = [
            run("r1", 1, "completed", json!({ "checkpointId": "c1" })),
            run("r2", 2, "rolled_back", json!({})),
            run("r3", 3, "running", json!({})),
            (RecordKind::Attempt, with(attempt("r1", "a1", "completed"), json!({ "providerTurnId": "t1" }))),
            (RecordKind::ProviderTurn, json!({ "id": "t1", "providerThreadId": "pt1", "nodeId": "n1", "runAttemptId": null,
                "nativeTurnRef": refer("turn-9"), "ordinal": 1, "status": "completed", "startedAt": null, "completedAt": null })),
            (RecordKind::ProviderThread, json!({ "id": "pt1", "driver": "codex", "providerInstanceId": "codex", "providerSessionId": null,
                "appThreadId": "t", "ownerNodeId": null, "nativeThreadRef": refer("thr-1"), "nativeConversationHeadRef": null, "status": "idle",
                "firstRunOrdinal": 1, "lastRunOrdinal": 3, "handoffIds": [], "forkedFrom": null, "createdAt": "1", "updatedAt": "1" })),
            said("m1", "r1", "user", "add search", "1"),
            said("m2", "r1", "assistant", "Added it.", "2"),
            said("m3", "r2", "user", "undone", "3"),
            (RecordKind::TurnItem, json!({ "id": "i1", "threadId": "t", "runId": "r1", "nodeId": null, "providerThreadId": null, "providerTurnId": null,
                "nativeItemRef": null, "parentItemId": null, "ordinal": 1_000_002, "status": "completed", "title": null, "startedAt": null,
                "completedAt": null, "updatedAt": "1", "type": "file_change", "fileName": "src/a.rs",
                "changes": [{ "operation": "update", "path": "src/a.rs" }, { "operation": "update", "path": "src/a.rs" }] })),
        ];
        let records = records.into_iter().map(|(kind, record)| StoredRecord::decode(kind, record).expect("the test's record fits the schema"));
        Thread::from_records(serde_json::from_value(thread("t", "p")).expect("a thread"), records)
    }

    #[test]
    fn a_fork_starts_from_a_finished_run_with_its_native_refs() {
        let thread = a_thread();
        let runs = thread.runs.as_slice();
        assert_eq!(source_run(runs, &ForkSourcePoint::LatestStable).unwrap().id.as_str(), "r1");
        assert_eq!(source_run(runs, &ForkSourcePoint::Checkpoint { checkpoint_id: "c1".into() }).unwrap().id.as_str(), "r1");
        assert!(source_run(runs, &ForkSourcePoint::Run { run_id: "r3".into() }).unwrap_err().to_string().contains("running"));
        assert_eq!(source_run(runs, &ForkSourcePoint::Run { run_id: "r9".into() }), Err(NoForkSource::NoStableRun));
        let from = source_run(runs, &ForkSourcePoint::LatestStable).unwrap();
        let point = source_point(&"t".into(), thread.provider_threads.as_slice(), thread.attempts.as_slice(), thread.provider_turns.as_slice(), from);
        assert_eq!(point.provider_thread_ref.unwrap().native_id.as_deref(), Some("thr-1"));
        assert_eq!(point.provider_turn_ref.unwrap().native_id.as_deref(), Some("turn-9"));
        assert_eq!(point.checkpoint_id.unwrap().as_str(), "c1");
    }

    #[test]
    fn the_transcript_skips_undone_turns_and_lists_files() {
        let (text, from, to) = transcript(&a_thread(), 1, 3).unwrap();
        assert_eq!((from, to), (1, 1));
        assert!(text.contains("User: add search\nAssistant: Added it.\nFiles changed: src/a.rs"), "{text}");
        assert!(!text.contains("undone"));
        assert!(transcript(&a_thread(), 4, 9).is_none());
    }
}
