//! The effect worker: performs side effects after their intent is committed,
//! then feeds results back as events (t3code EffectWorker).

use super::commands::{Answers, Effect, Orchestrator, next_item_ordinal};
use super::events::{Origin, enc};
use crate::adapters::turn::{Shared, turn_item};
use crate::store::Store;
use std::sync::Arc;
use domovoi_model::typed::{
    EventBody, ExecutionNode, NodeStatus, ProviderFailure, ProviderFailureClass, Run, RunAttempt, RunAttemptStatus, RunStatus, Timestamp, TurnItemStatus,
};
use tokio::sync::mpsc;

/// Why provider work did not happen. The text is what a failed run shows.
#[derive(Debug, thiserror::Error)]
pub enum ProviderError {
    #[error("No active turn for this thread.")]
    NoActiveTurn,
    #[error("The run's turn has ended.")]
    TurnEnded,
    #[error("{0}")]
    Failed(String),
}

impl From<String> for ProviderError {
    fn from(message: String) -> Self {
        Self::Failed(message)
    }
}

impl From<&str> for ProviderError {
    fn from(message: &str) -> Self {
        Self::Failed(message.to_owned())
    }
}

/// Provider work the worker delegates (implemented by `crate::adapters::Host`).
/// Generic rather than `dyn`: its one implementation is known where the worker
/// starts, and the futures are sent across tasks.
pub trait ProviderHost: Send + Sync + 'static {
    fn start_turn(&self, thread_id: &str, run_id: &str) -> impl Future<Output = Result<(), ProviderError>> + Send;
    /// `answers` is what the user gave a `user_input` request.
    fn respond(&self, thread_id: &str, request_id: &str, decision: &str, answers: Option<Answers>) -> impl Future<Output = Result<(), ProviderError>> + Send;
    fn interrupt(&self, thread_id: &str, run_id: &str) -> impl Future<Output = Result<(), ProviderError>> + Send;
    fn runtime_mode_changed(&self, thread_id: &str) -> impl Future<Output = ()> + Send;
    fn rollback_conversation(&self, thread_id: &str, checkpoint_id: &str) -> impl Future<Output = Result<(), ProviderError>> + Send;
    /// Adds a message to the run's live provider turn.
    fn steer(&self, thread_id: &str, run_id: &str, message_id: &str) -> impl Future<Output = Result<(), ProviderError>> + Send;
    /// Stops one subagent of the thread's live turn.
    fn stop_subagent(&self, thread_id: &str, native_id: &str) -> impl Future<Output = Result<(), ProviderError>> + Send;
}

/// Marks a run failed with a root `error` turn item the client renders.
pub fn fail_run(store: &Store, thread_id: &str, run_id: &str, message: &str, class: ProviderFailureClass) {
    fail_run_with(store, thread_id, run_id, message, class, None);
}

/// [`fail_run`], with when the limit that stopped it resets (a `usage_limit` failure).
pub fn fail_run_with(store: &Store, thread_id: &str, run_id: &str, message: &str, class: ProviderFailureClass, reset_at: Option<Timestamp>) {
    let Ok(Some(run)) = store.record::<Run>(thread_id, run_id) else { return };
    let ts = Timestamp::from(domovoi_contracts::iso_now());
    let o = Origin { provider_instance_id: Some(run.provider_instance_id.as_str().to_owned()), ..Default::default() };
    let ordinal = next_item_ordinal(store, thread_id, run.ordinal);
    if let Some(attempt) = &run.active_attempt_id {
        let _ = store.update_record(thread_id, attempt.as_str(), &o, |attempt: &mut RunAttempt| {
            attempt.status = RunAttemptStatus::Failed;
            attempt.completed_at = Some(ts.clone());
        });
    }
    if let Some(root) = &run.root_node_id {
        let _ = store.update_record(thread_id, root.as_str(), &o, |node: &mut ExecutionNode| {
            node.status = NodeStatus::Failed;
            node.completed_at = Some(ts.clone());
        });
    }
    let shared = Shared {
        id: format!("turn-item:run:{}:signal:error", enc(run_id)).into(),
        thread_id: thread_id.into(),
        run_id: Some(run_id.into()),
        node_id: run.root_node_id.clone(),
        provider_thread_id: run.provider_thread_id.clone(),
        provider_turn_id: None,
        native_item_ref: None,
        parent_item_id: None,
        ordinal,
        status: TurnItemStatus::Failed,
        title: None,
        started_at: Some(ts.clone()),
        completed_at: Some(ts.clone()),
        updated_at: ts.clone(),
    };
    let item = turn_item!(Error(ErrorItem), shared, {
        failure: ProviderFailure { class, message: message.to_owned(), code: None, retryable: Some(false), reset_at: reset_at.map(Some) },
        retry: None,
    });
    let _ = store.append_event(thread_id, EventBody::TurnItemUpdated(item), &o);
    let _ = store.update_record(thread_id, run_id, &o, |run: &mut Run| {
        run.status = RunStatus::Failed;
        run.completed_at = Some(ts.clone());
    });
}

/// Runs effects as they are queued. A thread's ordered effects
/// (`Effect::is_ordered`) run one after another in the order queued, on a
/// lane of their own; other threads' lanes, and unordered effects, run
/// alongside. A lane ends once it has nothing queued or running.
pub fn spawn<H: ProviderHost>(orchestrator: Arc<Orchestrator>, host: Arc<H>, rx: mpsc::UnboundedReceiver<Effect>) {
    tokio::spawn(dispatch_effects(orchestrator, host, rx));
}

/// A thread's lane: its queue, and how many effects are queued or running.
struct Lane {
    tx: mpsc::UnboundedSender<Effect>,
    pending: Arc<std::sync::atomic::AtomicUsize>,
}

async fn dispatch_effects<H: ProviderHost>(orch: Arc<Orchestrator>, host: Arc<H>, mut rx: mpsc::UnboundedReceiver<Effect>) {
    use std::sync::atomic::Ordering;
    let mut lanes: std::collections::HashMap<String, Lane> = std::collections::HashMap::new();
    let mut tasks = tokio::task::JoinSet::new();
    while let Some(effect) = rx.recv().await {
        while tasks.try_join_next().is_some() {}
        // Only this loop sends, so a lane with nothing pending is idle for
        // good: dropping its sender ends its task.
        lanes.retain(|_, lane| lane.pending.load(Ordering::Acquire) > 0);
        if !effect.is_ordered() {
            let (orch, host) = (orch.clone(), host.clone());
            tasks.spawn(async move { perform(&orch, host.as_ref(), effect).await });
            continue;
        }
        let lane = lanes.entry(effect.thread_id().to_owned()).or_insert_with(|| {
            #[expect(clippy::disallowed_methods, reason = "a thread's lane takes its effects from the dispatcher, which must not wait on one thread")]
            let (tx, mut lane_rx) = mpsc::unbounded_channel::<Effect>();
            let pending = Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let (orch, host, left) = (orch.clone(), host.clone(), pending.clone());
            tasks.spawn(async move {
                while let Some(effect) = lane_rx.recv().await {
                    perform(&orch, host.as_ref(), effect).await;
                    left.fetch_sub(1, Ordering::AcqRel);
                }
            });
            Lane { tx, pending }
        });
        lane.pending.fetch_add(1, Ordering::AcqRel);
        if lane.tx.send(effect).is_err() {
            lane.pending.fetch_sub(1, Ordering::AcqRel);
            tracing::error!("an effect lane ended with work queued");
        }
    }
    // The orchestrator is gone: let what was queued finish.
    drop(lanes);
    while tasks.join_next().await.is_some() {}
}

/// Runs one effect, then clears its outbox entry.
async fn perform<H: ProviderHost>(orch: &Orchestrator, host: &H, effect: Effect) {
    let key = effect.outbox_key();
    run_effect(orch, host, effect).await;
    if let Some(k) = key {
        if let Err(e) = orch.store().outbox_remove(&k) {
            tracing::error!(key = %k, "could not clear a done effect from the outbox, it will run again at the next start: {e}");
        }
    }
}

async fn run_effect<H: ProviderHost>(orch: &Orchestrator, host: &H, effect: Effect) {
    let store = orch.store();
    match effect {
        Effect::StartTurn { thread_id, run_id } => {
            if let Some(run) = run_record(store, &thread_id, &run_id) {
                super::checkpoints::ensure_baseline(store, &thread_id, &run).await;
            }
            if let Err(e) = host.start_turn(&thread_id, &run_id).await {
                tracing::warn!(%thread_id, %run_id, "turn did not start: {e}");
                fail_run(store, &thread_id, &run_id, &e.to_string(), ProviderFailureClass::ProviderError);
                finalize_run(orch, &thread_id, &run_id).await;
            }
        }
        Effect::RespondToRequest { thread_id, request_id, decision, answers } => {
            if let Err(e) = host.respond(&thread_id, &request_id, &decision, answers).await {
                tracing::warn!(%thread_id, %request_id, "respond failed: {e}");
            }
        }
        Effect::Interrupt { thread_id, run_id } => {
            if let Err(e) = host.interrupt(&thread_id, &run_id).await {
                tracing::warn!(%thread_id, %run_id, "interrupt failed: {e}");
            }
        }
        Effect::RuntimeModeChanged { thread_id } => host.runtime_mode_changed(&thread_id).await,
        Effect::Steer { thread_id, run_id, message_id } => {
            if let Err(e) = host.steer(&thread_id, &run_id, &message_id).await {
                tracing::info!(%thread_id, %run_id, "steer not taken, queueing the message: {e}");
                orch.requeue_steer(&thread_id, &message_id).await;
            }
        }
        Effect::GenerateTitle { thread_id, request_id, regenerate } => super::naming::title(orch, &thread_id, &request_id, regenerate).await,
        Effect::NameBranch { thread_id } => super::naming::branch(orch, &thread_id).await,
        Effect::ThreadSettled { thread_id } => crate::project_scripts::thread_settled(&thread_id).await,
        Effect::StopSubagent { thread_id, native_id } => {
            if let Err(e) = host.stop_subagent(&thread_id, &native_id).await {
                tracing::info!(%thread_id, %native_id, "subagent not stopped: {e}");
            }
        }
        Effect::CloseTerminals { thread_id } => {
            if let Some(state) = crate::state::STATE.get() {
                state.terminals.close_thread(&thread_id);
            }
        }
        Effect::Rollback { thread_id, checkpoint_id, restore_files, command_id } => {
            super::rollback::run(orch, host, &thread_id, &checkpoint_id, restore_files, &command_id).await;
        }
    }
}

fn run_record(store: &Store, thread_id: &str, run_id: &str) -> Option<Run> {
    store.record(thread_id, run_id).ok()?
}

/// Run finalization (t3code RunFinalizationService): once a run reaches a
/// terminal status, capture its checkpoint, link it from the run, and start
/// the next queued run. Provider hosts call this when a turn settles.
pub async fn finalize_run(orch: &Orchestrator, thread_id: &str, run_id: &str) {
    let store = orch.store();
    if let Some(run) = run_record(store, thread_id, run_id) {
        let checkpoint = super::checkpoints::capture_after_run(store, thread_id, &run).await;
        let o = Origin { provider_instance_id: Some(run.provider_instance_id.as_str().to_owned()), ..Default::default() };
        let ts = Timestamp::from(domovoi_contracts::iso_now());
        // Decided on the run as it is now (the checkpoint took a while), not as it was read before.
        let mut completed_root = None;
        let _ = store.update_record_if(thread_id, run_id, &o, |run: &mut Run| {
            let completes = run.status == RunStatus::Waiting;
            if checkpoint.is_none() && !completes {
                return false;
            }
            if let Some(id) = &checkpoint {
                run.checkpoint_id = Some(id.clone());
            }
            if completes {
                run.status = RunStatus::Completed;
                run.completed_at = Some(ts.clone());
                completed_root = run.root_node_id.clone();
            }
            true
        });
        if let Some(root) = completed_root {
            let _ = store.update_record(thread_id, root.as_str(), &o, |node: &mut ExecutionNode| {
                node.status = NodeStatus::Completed;
                node.completed_at = Some(ts.clone());
            });
        }
    }
    let _ = orch.start_next_queued(thread_id).await;
    crate::mcp::deliver_completion(orch, thread_id, run_id).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use std::sync::Mutex;

    /// Records what it was asked to do; starting a turn takes a while.
    #[derive(Default)]
    struct Recording(Mutex<Vec<String>>);

    impl Recording {
        fn note(&self, what: String) {
            self.0.lock().unwrap().push(what);
        }
    }

    impl ProviderHost for Recording {
        async fn start_turn(&self, thread_id: &str, run_id: &str) -> Result<(), ProviderError> {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            self.note(format!("start {thread_id} {run_id}"));
            Ok(())
        }
        async fn respond(&self, _: &str, _: &str, _: &str, _: Option<Answers>) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn interrupt(&self, thread_id: &str, run_id: &str) -> Result<(), ProviderError> {
            self.note(format!("interrupt {thread_id} {run_id}"));
            Ok(())
        }
        async fn runtime_mode_changed(&self, _: &str) {}
        async fn rollback_conversation(&self, _: &str, _: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn steer(&self, _: &str, _: &str, _: &str) -> Result<(), ProviderError> {
            Ok(())
        }
        async fn stop_subagent(&self, _: &str, _: &str) -> Result<(), ProviderError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn a_threads_effects_run_in_order_and_other_threads_go_alongside() {
        let (orch, rx) = Orchestrator::new(Store::new(Db::open_in_memory().unwrap()));
        let (orch, host) = (Arc::new(orch), Arc::new(Recording::default()));
        spawn(orch.clone(), host.clone(), rx);
        let start = |t: &str| Effect::StartTurn { thread_id: t.into(), run_id: "r".into() };
        let stop = |t: &str| Effect::Interrupt { thread_id: t.into(), run_id: "r".into() };
        for _ in 0..3 {
            orch.send_effect(start("a"));
            orch.send_effect(stop("a"));
        }
        orch.send_effect(stop("b"));
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while host.0.lock().unwrap().len() < 7 {
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("every effect ran");
        let seen = host.0.lock().unwrap().clone();
        let a: Vec<_> = seen.iter().filter(|s| s.contains(" a ")).cloned().collect();
        assert_eq!(a, ["start a r", "interrupt a r"].repeat(3), "thread a's effects in the order sent");
        assert_eq!(seen[0], "interrupt b r", "thread b did not wait for thread a");
    }
}
