//! Checkpoints: the workspace as it was after a run, and the scopes they are taken in.

use super::common::Timestamp;
use super::ids::{CheckpointId, CheckpointRef, CheckpointScopeId, NodeId, ProviderThreadId, RunId, ThreadId};
use super::macros::string_enum;
use serde::{Deserialize, Serialize};

/// The directory and execution scope a series of checkpoints is taken in
/// (`OrchestrationV2CheckpointScope`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckpointScope {
    pub id: CheckpointScopeId,
    pub thread_id: ThreadId,
    pub run_id: Option<RunId>,
    pub node_id: NodeId,
    pub parent_scope_id: Option<CheckpointScopeId>,
    pub provider_thread_id: Option<ProviderThreadId>,
    pub kind: CheckpointScopeKind,
    pub ordinal_within_parent: u64,
    pub advances_app_run_count: bool,
    pub cwd: String,
    pub created_at: Timestamp,
}

string_enum! {
    /// What a checkpoint scope covers.
    CheckpointScopeKind {
        RootRun = "root_run",
        Subagent = "subagent",
        Tool = "tool",
        ProviderThread = "provider_thread",
        Manual = "manual",
    }
}

/// The workspace as captured at one point of a thread (`OrchestrationV2Checkpoint`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Checkpoint {
    pub id: CheckpointId,
    pub thread_id: ThreadId,
    pub scope_id: CheckpointScopeId,
    pub run_id: Option<RunId>,
    pub node_id: NodeId,
    pub parent_checkpoint_id: Option<CheckpointId>,
    pub ordinal_within_scope: u64,
    pub app_run_ordinal: Option<u64>,
    #[serde(rename = "ref")]
    pub git_ref: CheckpointRef,
    pub status: CheckpointStatus,
    pub files: Vec<CheckpointFileSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files_total: Option<CheckpointFileTotals>,
    pub captured_at: Timestamp,
}

string_enum! {
    /// Whether a checkpoint can still be restored.
    CheckpointStatus {
        Ready = "ready",
        Missing = "missing",
        Error = "error",
        Stale = "stale",
    }
}

/// One file a checkpoint changed against its parent.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckpointFileSummary {
    pub path: String,
    pub kind: String,
    pub additions: u64,
    pub deletions: u64,
}

/// Totals over all of a checkpoint's files, sent when `files` is only a prefix.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckpointFileTotals {
    pub count: u64,
    pub additions: u64,
    pub deletions: u64,
}
