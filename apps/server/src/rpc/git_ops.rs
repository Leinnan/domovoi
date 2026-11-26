//! Git writes, review diffs, worktree setup progress and opening a folder in
//! an editor (t3code `GitWorkflowService`, `ReviewService`,
//! `WorktreeSetupTracker`, `externalLauncher`).

use super::Never;
use crate::state::ConnCtx;
use crate::vcs_ops::{self, DiffFileContentsInput, DiffPreviewInput, sent};
use crate::worktree_setup::Snapshot;
use effect_rpc::Router;
use futures::StreamExt;
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::broadcast::error::RecvError;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RefInput {
    cwd: String,
    ref_name: String,
    #[serde(default)]
    switch_ref: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CreateWorktreeInput {
    cwd: String,
    ref_name: String,
    new_ref_name: Option<String>,
    path: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RemoveWorktreeInput {
    cwd: String,
    path: String,
    #[serde(default)]
    force: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThreadOnly {
    thread_id: String,
}

/// `VcsInitInput`.
#[derive(Deserialize)]
struct InitInput {
    #[serde(default, deserialize_with = "sent::text")]
    cwd: String,
    #[serde(default, deserialize_with = "sent::text_if_any")]
    kind: Option<String>,
}

/// `VcsPullInput`.
#[derive(Deserialize)]
struct PullInput {
    #[serde(default, deserialize_with = "sent::text")]
    cwd: String,
}

/// What `vcs.refreshStatus` is asked with: a checkout, and whether to leave its upstream out.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RefreshStatusInput {
    #[serde(default, deserialize_with = "sent::text")]
    cwd: String,
    #[serde(default, deserialize_with = "sent::is_true")]
    local_only: bool,
}

/// `WorktreeSetupCancelResult`.
#[derive(Serialize)]
struct Cancelled {
    cancelled: bool,
}

/// `subscribeWorktreeSetup`: the thread's current setup (or null), then updates.
fn worktree_setup_stream(input: ThreadOnly) -> BoxStream<'static, Result<Option<Snapshot>, Never>> {
    let tracker = crate::worktree_setup::tracker();
    async_stream::stream! {
        let mut rx = tracker.subscribe();
        let current = tracker.current(&input.thread_id);
        let mut last = current.as_ref().map_or(0, |r| r.sequence);
        yield Ok(current);
        loop {
            match rx.recv().await {
                Ok((tid, record)) if tid == input.thread_id => {
                    let seq = record.sequence;
                    if seq > last || seq == 1 {
                        last = seq;
                        yield Ok(Some(record));
                    }
                }
                Ok(_) => {}
                Err(RecvError::Lagged(_)) => {
                    if let Some(r) = tracker.current(&input.thread_id) {
                        last = r.sequence;
                        yield Ok(Some(r));
                    }
                }
                Err(RecvError::Closed) => return,
            }
        }
    }
    .boxed()
}

// ---- editors ----

pub fn register(r: Router<ConnCtx>) -> Router<ConnCtx> {
    r.unary("vcs.createRef", |_ctx: Arc<ConnCtx>, i: RefInput| async move { vcs_ops::create_ref(&i.cwd, &i.ref_name, i.switch_ref.unwrap_or(false)).await })
        .unary("vcs.switchRef", |_ctx: Arc<ConnCtx>, i: RefInput| async move { vcs_ops::switch_ref(&i.cwd, &i.ref_name).await })
        .unary("vcs.createWorktree", |_ctx: Arc<ConnCtx>, i: CreateWorktreeInput| async move {
            vcs_ops::create_worktree(&i.cwd, &i.ref_name, i.new_ref_name.as_deref(), i.path.as_deref()).await
        })
        .unary("vcs.removeWorktree", |_ctx: Arc<ConnCtx>, i: RemoveWorktreeInput| async move {
            vcs_ops::remove_worktree(&i.cwd, &i.path, i.force.unwrap_or(false)).await
        })
        .unary("vcs.init", |_ctx: Arc<ConnCtx>, i: InitInput| async move { vcs_ops::init(&i.cwd, i.kind.as_deref()).await })
        .unary("vcs.pull", |_ctx: Arc<ConnCtx>, i: PullInput| async move { vcs_ops::pull(&i.cwd).await })
        .unary("vcs.refreshStatus", |_ctx: Arc<ConnCtx>, i: RefreshStatusInput| async move {
            Ok::<_, Never>(vcs_ops::refresh_status(&i.cwd, i.local_only).await)
        })
        .unary("review.getDiffPreview", |_ctx: Arc<ConnCtx>, i: DiffPreviewInput| async move { vcs_ops::diff_preview(&i).await })
        .unary("review.getDiffFileContents", |_ctx: Arc<ConnCtx>, i: DiffFileContentsInput| async move { vcs_ops::diff_file_contents(&i).await })
        .stream("subscribeWorktreeSetup", |_ctx: Arc<ConnCtx>, i: ThreadOnly| worktree_setup_stream(i))
        .unary("worktreeSetup.cancel", |_ctx: Arc<ConnCtx>, i: ThreadOnly| async move {
            Ok::<_, Never>(Cancelled { cancelled: crate::worktree_setup::tracker().cancel(&i.thread_id) })
        })
}
