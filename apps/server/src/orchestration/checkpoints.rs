//! Checkpoint lifecycle for runs: a baseline (`name:0`) before a thread's first
//! run and one checkpoint per completed run, recorded as `checkpoint.captured`
//! plus a `checkpoint` turn item, and the turn-diff RPCs built on them.

use super::commands::{driver_for_instance, next_item_ordinal, scope_id};
use super::events::{Origin, enc};
use crate::adapters::turn::{Shared, turn_item};
use crate::checkpoint;
use crate::store::Store;
use serde::Serialize;
use std::sync::Arc;
use domovoi_model::typed::{Checkpoint, CheckpointFileSummary, CheckpointId, CheckpointScope, CheckpointStatus, EventBody, Run, Timestamp, TurnItemStatus};

pub fn checkpoint_id(scope: &str, n: u64) -> String {
    format!("checkpoint:scope:{}:name:{n}", enc(scope))
}

/// The folder the checkpoints of `scope` are taken in.
fn scope_cwd(store: &Store, thread_id: &str, scope: &str) -> Option<String> {
    store.record::<CheckpointScope>(thread_id, scope).ok().flatten().map(|scope| scope.cwd)
}

/// The newest non-stale checkpoint of the thread's root scope.
fn head(checkpoints: &[Arc<Checkpoint>], scope: &str) -> Option<Arc<Checkpoint>> {
    checkpoints.iter().filter(|c| c.scope_id.as_str() == scope && c.status != CheckpointStatus::Stale).max_by_key(|c| c.ordinal_within_scope).cloned()
}

/// Captures `name:0` before the first run if the workspace is a git repo.
pub async fn ensure_baseline(store: &Store, thread_id: &str, run: &Run) {
    let Ok(checkpoints) = store.records::<Checkpoint>(thread_id) else { return };
    let scope = scope_id(thread_id);
    if head(&checkpoints, &scope).is_some() {
        return;
    }
    let Some(cwd) = scope_cwd(store, thread_id, &scope) else { return };
    if !checkpoint::is_git_repository(&cwd).await {
        return;
    }
    let r = checkpoint::checkpoint_ref(&scope, 0);
    let status = if checkpoint::capture(&cwd, &r).await.is_ok() { CheckpointStatus::Ready } else { CheckpointStatus::Error };
    // A checkpoint is taken under its run's root node: a run without one records none.
    let Some(node_id) = run.root_node_id.clone() else { return };
    let baseline = Checkpoint {
        id: checkpoint_id(&scope, 0).into(),
        thread_id: thread_id.into(),
        scope_id: scope.as_str().into(),
        run_id: None,
        node_id,
        parent_checkpoint_id: None,
        ordinal_within_scope: 0,
        app_run_ordinal: None,
        git_ref: r.into(),
        status,
        files: Vec::new(),
        files_total: None,
        captured_at: Timestamp::from(domovoi_contracts::iso_now()),
    };
    let _ = store.append_event(thread_id, EventBody::CheckpointCaptured(Box::new(baseline)), &Origin::default());
}

/// Captures the checkpoint after `run`, emits its turn item, and returns its id.
pub async fn capture_after_run(store: &Store, thread_id: &str, run: &Run) -> Option<CheckpointId> {
    let checkpoints = store.records::<Checkpoint>(thread_id).ok()?;
    let scope = scope_id(thread_id);
    let cwd = scope_cwd(store, thread_id, &scope)?;
    let parent = head(&checkpoints, &scope)?;
    let n = parent.ordinal_within_scope + 1;
    let r = checkpoint::checkpoint_ref(&scope, n);
    let captured = checkpoint::capture(&cwd, &r).await.is_ok();
    let files: Vec<CheckpointFileSummary> = if captured {
        checkpoint::diff_files(&cwd, parent.git_ref.as_str(), &r)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|(path, kind, additions, deletions)| CheckpointFileSummary { path, kind, additions, deletions })
            .collect()
    } else {
        vec![]
    };
    // A checkpoint is taken under its run's root node: a run without one records none.
    let node_id = run.root_node_id.clone()?;
    let id = CheckpointId::from(checkpoint_id(&scope, n));
    let ts = Timestamp::from(domovoi_contracts::iso_now());
    let instance = run.provider_instance_id.as_str();
    let o = Origin { provider_instance_id: Some(instance.into()), ..Default::default() };
    let taken = Checkpoint {
        id: id.clone(),
        thread_id: thread_id.into(),
        scope_id: scope.as_str().into(),
        run_id: Some(run.id.clone()),
        node_id: node_id.clone(),
        parent_checkpoint_id: Some(parent.id.clone()),
        ordinal_within_scope: n,
        app_run_ordinal: Some(run.ordinal),
        git_ref: r.into(),
        status: if captured { CheckpointStatus::Ready } else { CheckpointStatus::Error },
        files: files.clone(),
        files_total: None,
        captured_at: ts.clone(),
    };
    store.append_event(thread_id, EventBody::CheckpointCaptured(Box::new(taken)), &o).ok()?;
    let driver = &driver_for_instance(instance);
    let shared = Shared {
        id: format!("turn-item:provider:{driver}:native-item:{}", enc(&format!("checkpoint:{id}"))).into(),
        thread_id: thread_id.into(),
        run_id: Some(run.id.clone()),
        node_id: Some(node_id),
        provider_thread_id: run.provider_thread_id.clone(),
        provider_turn_id: None,
        native_item_ref: None,
        parent_item_id: None,
        ordinal: next_item_ordinal(store, thread_id, run.ordinal),
        status: TurnItemStatus::Completed,
        title: None,
        started_at: Some(ts.clone()),
        completed_at: Some(ts.clone()),
        updated_at: ts,
    };
    let item = turn_item!(Checkpoint(CheckpointItem), shared, {
        checkpoint_id: id.clone(),
        scope_id: scope.as_str().into(),
        files: files,
        files_total: None,
        files_omitted: None,
    });
    store.append_event(thread_id, EventBody::TurnItemUpdated(item), &o).ok()?;
    Some(id)
}

/// Which diff RPC a turn diff answers: each fails with its own tagged error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum DiffCall {
    #[serde(rename = "OrchestrationGetTurnDiffError")]
    TurnDiff,
    #[serde(rename = "OrchestrationGetFullThreadDiffError")]
    FullThreadDiff,
}

/// Why a diff between two checkpoints could not be made.
#[derive(Debug, thiserror::Error, Serialize)]
#[error("{message}")]
pub struct DiffError {
    #[serde(rename = "_tag")]
    pub call: DiffCall,
    pub message: String,
}

/// The patch between two turns of a thread (`OrchestrationGetTurnDiffResult`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnDiff {
    pub thread_id: String,
    pub from_turn_count: u64,
    pub to_turn_count: u64,
    pub diff: String,
}

/// Patch between the checkpoints at turn counts `from` and `to` (turn count n =
/// checkpoint `name:n` of the root scope).
pub async fn turn_diff(store: &Store, thread_id: &str, from: u64, to: u64, call: DiffCall) -> Result<TurnDiff, DiffError> {
    let failed = |message: String| DiffError { call, message };
    let checkpoints = store.records::<Checkpoint>(thread_id).map_err(|err| failed(err.to_string()))?;
    let scope = scope_id(thread_id);
    let cwd = scope_cwd(store, thread_id, &scope).ok_or_else(|| failed("This thread has no checkpoints.".into()))?;
    let ref_for = |n: u64| -> Result<String, DiffError> {
        checkpoints
            .iter()
            .find(|c| c.scope_id.as_str() == scope && c.ordinal_within_scope == n && c.status != CheckpointStatus::Stale)
            .map(|c| c.git_ref.as_str().to_owned())
            .ok_or_else(|| failed(format!("No checkpoint for turn {n}.")))
    };
    let diff = checkpoint::diff_patch(&cwd, &ref_for(from)?, &ref_for(to)?).await.map_err(|err| failed(err.to_string()))?;
    Ok(TurnDiff { thread_id: thread_id.to_owned(), from_turn_count: from, to_turn_count: to, diff })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::Db;
    use crate::orchestration::commands::Orchestrator;
    use domovoi_contracts::shell::ProjectShell;

    #[tokio::test]
    async fn baseline_then_run_checkpoint_with_files_and_diff() {
        let dir = std::env::temp_dir().join(format!("domovoi-ck-{}", crate::util::new_uuid()));
        std::fs::create_dir_all(&dir).unwrap();
        let root = dir.to_string_lossy().to_string();
        let sh = |args: &[&str]| { std::process::Command::new("git").args(args).current_dir(&dir).output().unwrap(); };
        sh(&["init", "-q", "-b", "main"]);
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        sh(&["add", "-A"]);
        sh(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "init"]);

        let store = Store::new(Db::open_in_memory().unwrap());
        store.create_project(ProjectShell {
            id: "p".into(), title: "p".into(), has_custom_title: false, workspace_root: root.clone(), repository_identity: None,
            default_model_selection: None, default_thread_env_mode: None, auto_pull: false, favicon_path: None,
            project_icon: None, scripts: vec![], pinned_at: None, archived_at: None, created_at: "t".into(), updated_at: "t".into(),
        }, None).unwrap();
        let (o, _fx) = Orchestrator::new(store.clone());
        o.launch_thread(crate::test_records::launch("c", "t", "p", "codex", "hi")).await.unwrap();
        let run = store.records::<Run>("t").unwrap().remove(0);
        ensure_baseline(&store, "t", &run).await;
        std::fs::write(dir.join("a.txt"), "one\ntwo\n").unwrap();
        std::fs::write(dir.join("b.txt"), "new\n").unwrap();
        let id = capture_after_run(&store, "t", &run).await.unwrap();
        assert!(id.as_str().ends_with(":name:1"));
        let taken = store.record::<Checkpoint>("t", id.as_str()).unwrap().unwrap();
        let file = |path: &str, kind: &str| CheckpointFileSummary { path: path.into(), kind: kind.into(), additions: 1, deletions: 0 };
        assert_eq!(taken.files, [file("a.txt", "modified"), file("b.txt", "added")]);
        assert!(store.records::<domovoi_model::typed::TurnItem>("t").unwrap().iter().any(|item| item.item_type() == "checkpoint"));
        let d = turn_diff(&store, "t", 0, 1, DiffCall::TurnDiff).await.unwrap();
        assert!(d.diff.contains("+two"));
        let failed = turn_diff(&store, "t", 0, 9, DiffCall::FullThreadDiff).await.unwrap_err();
        assert_eq!(serde_json::json!(failed), serde_json::json!({ "_tag": "OrchestrationGetFullThreadDiffError", "message": "No checkpoint for turn 9." }));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
