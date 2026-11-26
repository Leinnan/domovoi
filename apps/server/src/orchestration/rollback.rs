//! `checkpoint.rollback` effect: revert the provider conversation, optionally
//! restore files, then mark later checkpoints stale and later runs rolled back
//! (event order as recorded from t3code).

use super::commands::Orchestrator;
use super::effects::ProviderHost;
use super::events::Origin;
use domovoi_model::typed::{Checkpoint, CheckpointStatus, EventBody, ExecutionNode, NodeStatus, RollbackFailure, Run, RunStatus, ThreadChange, Timestamp};

pub async fn run<H: ProviderHost>(orch: &Orchestrator, host: &H, thread_id: &str, checkpoint_id: &str, restore_files: bool, command_id: &str) {
    let store = orch.store();
    let fail = |message: String| {
        let failure = RollbackFailure { request_id: command_id.into(), message };
        let _ = store.update_thread(thread_id, ThreadChange::MetadataUpdated, &Origin::default(), |thread| {
            thread.rollback_failure = Some(Some(failure));
            true
        });
    };
    // The thread as it is when the rollback starts: what it marks is decided on this.
    let Ok(outline) = store.thread_outline(thread_id) else { return };
    let Some(target) = outline.checkpoints.iter().find(|c| c.id.as_str() == checkpoint_id).cloned() else {
        return fail("Unknown checkpoint.".into());
    };
    if let Err(m) = host.rollback_conversation(thread_id, checkpoint_id).await {
        return fail(format!("Could not rewind the provider conversation: {m}"));
    }
    if restore_files {
        let scope = outline.checkpoint_scopes.iter().find(|scope| scope.id == target.scope_id);
        let cwd = scope.map(|scope| scope.cwd.clone()).unwrap_or_default();
        if let Err(err) = crate::checkpoint::restore(&cwd, target.git_ref.as_str()).await {
            return fail(format!("Could not restore files: {err}"));
        }
    }
    let o = Origin::default();
    let ts = Timestamp::from(crate::time_fmt::iso_now());
    for later in outline.checkpoints.iter().filter(|c| c.scope_id == target.scope_id && c.ordinal_within_scope > target.ordinal_within_scope) {
        let mut stale = Checkpoint::clone(later);
        stale.status = CheckpointStatus::Stale;
        let _ = store.append_event(thread_id, EventBody::CheckpointCaptured(Box::new(stale)), &o);
    }
    let target_run_ordinal = target.app_run_ordinal.unwrap_or(0);
    for run in outline.runs.iter().filter(|run| run.ordinal > target_run_ordinal && run.status != RunStatus::RolledBack) {
        let mut rolled_back = Run::clone(run);
        rolled_back.status = RunStatus::RolledBack;
        if rolled_back.completed_at.is_none() {
            rolled_back.completed_at = Some(ts.clone());
        }
        let root = rolled_back.root_node_id.clone();
        let _ = store.append_event(thread_id, EventBody::RunUpdated(Box::new(rolled_back)), &o);
        if let Some(mut node) = root.and_then(|root| outline.nodes.iter().find(|node| node.id == root)).map(|node| ExecutionNode::clone(node)) {
            node.status = NodeStatus::RolledBack;
            node.completed_at = Some(ts.clone());
            let _ = store.append_event(thread_id, EventBody::NodeUpdated(Box::new(node)), &o);
        }
    }
}
