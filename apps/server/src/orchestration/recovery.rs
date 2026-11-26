//! Startup recovery: a server that stopped mid-turn (crash, SIGTERM, reboot)
//! leaves runs the store still calls active, though no provider process is
//! behind them any more. Clients would show them working for ever, Stop
//! would find no turn to stop, and every new message would queue behind
//! them. This settles them the way an interrupt does, as recorded events, so
//! clients replaying with `afterSequence` see it too.

use super::commands::Orchestrator;
use super::events::{Kept, Origin, Outline};
use crate::store::Store;
use domovoi_model::typed::command::MessageDispatch;
use domovoi_model::typed::{
    Actor, ConversationMessage, CreationSource, ExecutionNode, NodeStatus, ProviderFailureClass, ProviderThread, ProviderThreadStatus, ProviderTurn, ProviderTurnStatus, Run,
    RunAttempt, RunAttemptStatus, RunStatus, RuntimeRequest, RuntimeRequestStatus, Timestamp, TurnItem, TurnItemStatus,
};

/// Runs with a provider turn under way when the server stopped.
const LIVE: [&str; 3] = ["preparing", "starting", "running"];
/// Item states that only a live turn ends.
const OPEN: [&str; 4] = ["running", "waiting", "pending", "starting"];

/// What startup found: runs it interrupted, and runs whose turn had finished
/// but whose finalization (checkpoint, then `completed`) had not.
#[derive(Debug, Default, PartialEq)]
pub struct Recovered {
    pub interrupted: Vec<(String, String)>,
    pub unfinalized: Vec<(String, String)>,
}

/// Marks every live run interrupted, with its open items, attempt, provider
/// turn, root node and pending requests. Call before clients can subscribe.
pub fn interrupt_orphaned_runs(store: &Store) -> Recovered {
    let mut out = Recovered::default();
    for (tid, _) in store.thread_ids().unwrap_or_default() {
        // What a run is settled from: read without the conversation's text,
        // which only a thread with a live run needs.
        let Ok(outline) = store.thread_outline(&tid) else { continue };
        let mut conversation = None;
        for run in &outline.runs {
            if LIVE.contains(&run.status.as_str()) {
                let read = || Some((store.records::<TurnItem>(&tid).ok()?, store.records::<ConversationMessage>(&tid).ok()?));
                let Some((items, messages)) = conversation.get_or_insert_with(read) else { break };
                interrupt(store, &tid, run, &outline, items, messages);
                out.interrupted.push((tid.clone(), run.id.as_str().to_owned()));
            } else if run.status == RunStatus::Waiting {
                out.unfinalized.push((tid.clone(), run.id.as_str().to_owned()));
            }
        }
        if out.interrupted.iter().any(|(t, _)| *t == tid) {
            settle_thread(store, &tid, &outline);
        }
    }
    out
}

/// A run whose turn task ended without settling it (it panicked) while the
/// server goes on: settled as startup recovery would, then failed with
/// `message`. Only a run still live is touched (one whose turn completed
/// meanwhile is left to its own finalization); returns whether it was.
pub fn settle_dead_run(store: &Store, tid: &str, run_id: &str, message: &str) -> bool {
    let Ok(outline) = store.thread_outline(tid) else { return false };
    let Some(run) = outline.runs.iter().find(|run| run.id.as_str() == run_id && LIVE.contains(&run.status.as_str())) else { return false };
    let (Ok(items), Ok(messages)) = (store.records::<TurnItem>(tid), store.records::<ConversationMessage>(tid)) else { return false };
    interrupt(store, tid, run, &outline, &items, &messages);
    settle_thread(store, tid, &outline);
    super::effects::fail_run(store, tid, run_id, message, ProviderFailureClass::ProviderError);
    true
}

fn interrupt(store: &Store, tid: &str, run: &Run, outline: &Outline, items: &[std::sync::Arc<TurnItem>], messages: &[std::sync::Arc<ConversationMessage>]) {
    let ts = Timestamp::from(crate::time_fmt::iso_now());
    let o = Origin { provider_instance_id: Some(run.provider_instance_id.as_str().to_owned()), ..Default::default() };
    fn emit<R: Kept>(store: &Store, tid: &str, o: &Origin, record: R) {
        let event = record.updated();
        let event_type = event.event_type().to_owned();
        if let Err(err) = store.append_event(tid, event, o) {
            tracing::error!(thread = %tid, "recovery could not record {event_type}: {err}");
        }
    }

    for item in items.iter().filter(|item| item.run_id() == Some(&run.id) && OPEN.contains(&item.status().as_str())) {
        let mut item = TurnItem::clone(item);
        item.set_status(TurnItemStatus::Interrupted);
        item.set_completed_at(Some(ts.clone()));
        item.set_updated_at(ts.clone());
        item.stop_streaming();
        // A reply cut off while it streamed has no message yet: it gets the
        // one its turn would have written at its end, with what was said.
        let unfinished_reply = match &item {
            TurnItem::AssistantMessage(shown) if !messages.iter().any(|message| message.id == shown.message_id) => {
                Some(crate::adapters::turn::message_of_reply(shown, &ts))
            }
            _ => None,
        };
        emit(store, tid, &o, item);
        if let Some(message) = unfinished_reply {
            emit(store, tid, &o, message);
        }
    }
    for message in messages.iter().filter(|message| message.run_id.as_ref() == Some(&run.id) && message.streaming) {
        let mut message = ConversationMessage::clone(message);
        message.streaming = false;
        message.updated_at = ts.clone();
        emit(store, tid, &o, message);
    }
    for turn in outline.provider_turns.iter().filter(|turn| turn.completed_at.is_none() && turn.status != ProviderTurnStatus::Completed) {
        let mut turn = ProviderTurn::clone(turn);
        turn.status = ProviderTurnStatus::Interrupted;
        turn.completed_at = Some(ts.clone());
        emit(store, tid, &o, turn);
    }
    if let Some(attempt) = outline.attempts.iter().find(|attempt| Some(&attempt.id) == run.active_attempt_id.as_ref() && attempt.completed_at.is_none()) {
        let mut attempt = RunAttempt::clone(attempt);
        attempt.status = RunAttemptStatus::Interrupted;
        attempt.completed_at = Some(ts.clone());
        emit(store, tid, &o, attempt);
    }
    if let Some(node) = outline.nodes.iter().find(|node| Some(&node.id) == run.root_node_id.as_ref()) {
        let mut node = ExecutionNode::clone(node);
        node.status = NodeStatus::Interrupted;
        node.completed_at = Some(ts.clone());
        emit(store, tid, &o, node);
    }
    let mut run = run.clone();
    run.status = RunStatus::Interrupted;
    run.completed_at = Some(ts);
    emit(store, tid, &o, run);
}

/// What belongs to the thread rather than one run: requests no process will
/// answer, and provider threads still marked active.
fn settle_thread(store: &Store, tid: &str, outline: &Outline) {
    let ts = Timestamp::from(crate::time_fmt::iso_now());
    let o = Origin::default();
    for request in outline.runtime_requests.iter().filter(|request| request.status == RuntimeRequestStatus::Pending
        && !(request.kind == domovoi_model::typed::RuntimeRequestKind::UserInput
            && matches!(request.response_capability, domovoi_model::typed::ResponseCapability::Message))) {
        let mut request = RuntimeRequest::clone(request);
        request.status = RuntimeRequestStatus::Cancelled;
        request.resolved_at = Some(ts.clone());
        let _ = store.append_event(tid, request.updated(), &o);
    }
    for provider_thread in outline.provider_threads.iter().filter(|provider_thread| provider_thread.status == ProviderThreadStatus::Active) {
        let mut provider_thread = ProviderThread::clone(provider_thread);
        provider_thread.status = ProviderThreadStatus::Idle;
        provider_thread.updated_at = ts.clone();
        let _ = store.append_event(tid, provider_thread.updated(), &o);
    }
}

/// Startup: interrupts orphaned runs at once, then in the background
/// finalizes turns that had finished and moves each touched thread's queue
/// on, as a Stop would have.
pub fn recover(orch: std::sync::Arc<Orchestrator>) {
    let found = interrupt_orphaned_runs(orch.store());
    if found.interrupted.is_empty() && found.unfinalized.is_empty() {
        RECOVERED.store(true, std::sync::atomic::Ordering::Release);
        return;
    }
    tracing::info!(interrupted = found.interrupted.len(), unfinalized = found.unfinalized.len(), "settled runs a server stop left open");
    tokio::spawn(async move {
        for (tid, rid) in &found.unfinalized {
            super::effects::finalize_run(&orch, tid, rid).await;
        }
        for (tid, _) in &found.interrupted {
            let _ = orch.start_next_queued(tid).await;
        }
        RECOVERED.store(true, std::sync::atomic::Ordering::Release);
    });
}

/// Set once `recover` has settled every run a stop left open (`/ready`).
static RECOVERED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Startup recovery has finished.
pub fn recovered() -> bool {
    RECOVERED.load(std::sync::atomic::Ordering::Acquire)
}

const RESTART_CONTINUATION: &str = "restart_continuation";

/// What the agent is told when its turn was cut off by a server restart.
pub const CONTINUE_PROMPT: &str = "The Domovoi server restarted while you were working, which stopped your last turn. Continue where you left off.";

/// On a deliberate stop: remembers the turns it cuts off (thread, run).
pub fn record_restart_continuation(store: &Store, live: &[(String, String)]) {
    if live.is_empty() {
        return;
    }
    let Ok(live) = serde_json::to_string(live) else { return };
    let _ = store.db().meta_set(RESTART_CONTINUATION, &live);
}

/// The turns the last deliberate stop cut off, forgetting them (so a crash
/// later never continues them twice).
pub fn take_restart_continuation(store: &Store) -> Vec<(String, String)> {
    let Ok(Some(raw)) = store.db().meta_get(RESTART_CONTINUATION) else { return vec![] };
    let _ = store.db().with(|c| c.execute("DELETE FROM meta WHERE key = ?1", [RESTART_CONTINUATION]).map(|_| ()));
    serde_json::from_str(&raw).unwrap_or_default()
}

/// t3code RestartContinuation: each interrupted run from `cut_off` gets a
/// server message asking the agent to go on (queued like any message).
pub fn continue_after_restart(orch: std::sync::Arc<Orchestrator>, cut_off: Vec<(String, String)>) {
    if cut_off.is_empty() {
        return;
    }
    tokio::spawn(async move {
        for (tid, rid) in cut_off {
            let interrupted = orch.store().record::<Run>(&tid, &rid).ok().flatten().is_some_and(|run| run.status == RunStatus::Interrupted);
            let deleted = orch.store().app_thread(&tid).ok().flatten().is_none_or(|thread| thread.deleted_at.is_some());
            if !interrupted || deleted {
                continue;
            }
            let message_id = format!("message:restart-continuation:{}", super::events::enc(&rid));
            let mut resume =
                MessageDispatch::new(format!("restart-continuation:{rid}"), tid.as_str(), message_id, CONTINUE_PROMPT, Actor::System, CreationSource::Server);
            resume.restart_continuation_of_run_id = Some(rid.as_str().into());
            match orch.dispatch(resume.into()).await {
                Ok(_) => tracing::info!(%tid, %rid, "continuing a turn the restart cut off"),
                Err(e) => tracing::warn!(%tid, %rid, "could not continue after the restart: {e}"),
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use serde_json::json;
    use domovoi_model::typed::RecordKind;

    fn seed(store: &Store, tid: &str, run_status: &str) {
        use crate::test_records::{append, assistant_item, attempt, message, node, run, runtime_request, thread, with};
        let o = Origin::default();
        let rid = format!("r-{tid}");
        append(store, tid, "thread.created", thread(tid, "p"), &o).unwrap();
        let live_run = with(run(tid, &rid, run_status), json!({ "rootNodeId": format!("n-{tid}"), "activeAttemptId": format!("a-{tid}") }));
        append(store, tid, "run.created", live_run, &o).unwrap();
        append(store, tid, "node.updated", node(tid, &format!("n-{tid}"), "running"), &o).unwrap();
        append(store, tid, "run-attempt.created", attempt(&rid, &format!("a-{tid}"), "running"), &o).unwrap();
        let open_item = with(assistant_item(tid, &format!("i-{tid}"), &format!("m-{tid}"), 1, "so far", true), json!({ "runId": rid }));
        append(store, tid, "turn-item.updated", open_item, &o).unwrap();
        let done_item = with(assistant_item(tid, &format!("done-{tid}"), "m-done", 2, "done", false), json!({ "runId": rid }));
        append(store, tid, "turn-item.updated", done_item, &o).unwrap();
        // An item of a type a newer server wrote, still open.
        let newer = json!({ "id": format!("newer-{tid}"), "threadId": tid, "runId": rid, "ordinal": 3, "status": "running", "updatedAt": crate::test_records::AT,
            "type": "a_newer_type", "streaming": true, "kept": [1, 2] });
        append(store, tid, "turn-item.updated", newer, &o).unwrap();
        append(store, tid, "message.updated", with(message(tid, &format!("m-{tid}"), "assistant", "so far", true), json!({ "runId": rid })), &o).unwrap();
        append(store, tid, "runtime-request.updated", runtime_request(&format!("q-{tid}"), "pending"), &o).unwrap();
    }

    #[test]
    fn a_run_left_running_is_interrupted_with_what_it_left_open() {
        let store = Store::new(Db::open_in_memory().unwrap());
        seed(&store, "t1", "running");
        let found = interrupt_orphaned_runs(&store);
        assert_eq!(found.interrupted, vec![("t1".to_owned(), "r-t1".to_owned())]);
        let thread = store.load_thread("t1").unwrap().unwrap();
        let run = thread.runs.get("r-t1").unwrap();
        assert_eq!((&run.status, run.completed_at.is_some()), (&RunStatus::Interrupted, true));
        assert_eq!(thread.nodes.get("n-t1").unwrap().status, NodeStatus::Interrupted);
        assert_eq!(thread.attempts.get("a-t1").unwrap().status, RunAttemptStatus::Interrupted);
        let TurnItem::AssistantMessage(item) = thread.turn_items.get("i-t1").unwrap() else { panic!("an assistant message") };
        assert_eq!((&item.status, item.streaming), (&TurnItemStatus::Interrupted, false));
        assert_eq!(*thread.turn_items.get("done-t1").unwrap().status(), TurnItemStatus::Completed, "finished items stay as they were");
        // An item this build does not know is closed all the same, and keeps what it said.
        let newer = crate::test_records::stored(&store, "t1", RecordKind::TurnItem, "newer-t1");
        assert_eq!((newer["status"].as_str(), newer["streaming"].as_bool(), &newer["kept"]), (Some("interrupted"), Some(false), &json!([1, 2])));
        assert!(!thread.messages.get("m-t1").unwrap().streaming);
        assert_eq!(thread.runtime_requests.get("q-t1").unwrap().status, RuntimeRequestStatus::Cancelled);
        // The thread no longer reads as working.
        assert_eq!(domovoi_model::typed::shell_status(&thread.shell()).as_str(), "interrupted");
    }

    #[test]
    fn a_dead_turns_run_is_failed_with_what_it_left_open() {
        let store = Store::new(Db::open_in_memory().unwrap());
        seed(&store, "t1", "running");
        seed(&store, "fin", "waiting");
        assert!(settle_dead_run(&store, "t1", "r-t1", "The server hit an internal error."));
        let thread = store.load_thread("t1").unwrap().unwrap();
        let run = thread.runs.get("r-t1").unwrap();
        assert_eq!((&run.status, run.completed_at.is_some()), (&RunStatus::Failed, true));
        assert_eq!(thread.attempts.get("a-t1").unwrap().status, RunAttemptStatus::Failed);
        assert_eq!(*thread.turn_items.get("i-t1").unwrap().status(), TurnItemStatus::Interrupted);
        assert!(!thread.messages.get("m-t1").unwrap().streaming);
        assert_eq!(thread.runtime_requests.get("q-t1").unwrap().status, RuntimeRequestStatus::Cancelled);
        let error = thread.turn_items.iter().find_map(|item| match item {
            TurnItem::Error(e) => Some(e.failure.message.clone()),
            _ => None,
        });
        assert_eq!(error.as_deref(), Some("The server hit an internal error."));
        // Settled once; a turn that completed (finalization pending) is not touched.
        let before = store.latest_sequence().unwrap();
        assert!(!settle_dead_run(&store, "t1", "r-t1", "again"));
        assert!(!settle_dead_run(&store, "fin", "r-fin", "late"));
        assert!(!settle_dead_run(&store, "t1", "unknown", "x"));
        assert_eq!(store.latest_sequence().unwrap(), before);
    }

    #[test]
    fn settled_runs_are_left_alone_and_finished_turns_are_finalized_later() {
        let store = Store::new(Db::open_in_memory().unwrap());
        seed(&store, "done", "completed");
        seed(&store, "fin", "waiting");
        let before = store.latest_sequence().unwrap();
        let found = interrupt_orphaned_runs(&store);
        assert!(found.interrupted.is_empty());
        assert_eq!(found.unfinalized, vec![("fin".to_owned(), "r-fin".to_owned())]);
        assert_eq!(store.latest_sequence().unwrap(), before, "nothing recorded for settled threads");
        // A second start finds nothing more to do.
        assert_eq!(interrupt_orphaned_runs(&store).interrupted.len(), 0);
    }
}
