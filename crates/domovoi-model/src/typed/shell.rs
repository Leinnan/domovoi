//! The thread shell: the summary of a thread that sidebars and lists show.

use super::common::{
    Actor, CreationSource, InteractionMode, LinkedThread, ModelSelection, ProviderFailureClass, RuntimeMode, Timestamp, double_option,
};
use super::ids::{MessageId, ProjectId, ProviderInstanceId, ProviderThreadId, RunId, RuntimeRequestId, ThreadId};
use super::macros::string_enum;
use super::message::MessageRole;
use super::provider::{PendingBackgroundTask, ProviderGoal, RuntimeRequestKind};
use super::pull_request::{LinkedPullRequest, PullRequestLink};
use super::thread::{
    ForkSource, HistoryOrigin, LastTitleRegeneration, LimitRecovery, LinkOrigin, SettledOverride, ThreadLineage, ThreadOpens,
    TitleRegeneration,
};
use serde::{Deserialize, Serialize};

/// A thread as lists show it (`OrchestrationV2ThreadShell`): the thread record
/// plus what its latest run, pending request and messages derive.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadShell {
    pub created_by: Actor,
    pub creation_source: CreationSource,
    pub id: ThreadId,
    pub project_id: ProjectId,
    pub title: String,
    pub provider_instance_id: ProviderInstanceId,
    pub model_selection: ModelSelection,
    pub runtime_mode: RuntimeMode,
    pub interaction_mode: InteractionMode,
    pub branch: Option<String>,
    pub worktree_path: Option<String>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub linked_pull_request: Option<Option<LinkedPullRequest>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_requests: Option<Vec<PullRequestLink>>,
    /// Issues linked to the thread; absent from servers that predate linking.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issues: Option<Vec<super::issue::ThreadIssueLink>>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub branch_pull_request: Option<Option<LinkedPullRequest>>,
    pub lineage: ThreadLineage,
    pub forked_from: Option<ForkSource>,
    pub active_provider_thread_id: Option<ProviderThreadId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_origin: Option<HistoryOrigin>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_origin: Option<LinkOrigin>,
    // Optional in the schema, but t3code stores `null` on threads launched over MCP.
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub delegated_from: Option<Option<LinkedThread>>,
    pub latest_run_id: Option<RunId>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub latest_run_requested_at: Option<Option<Timestamp>>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub latest_run_started_at: Option<Option<Timestamp>>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub latest_run_completed_at: Option<Option<Timestamp>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_run_stop_requested: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_run_background_wake: Option<bool>,
    pub active_run_id: Option<RunId>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub activity_run_started_at: Option<Option<Timestamp>>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub activity_run_status: Option<Option<ActivityRunStatus>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_on_thread_wait: Option<bool>,
    pub status: ShellThreadStatus,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub last_error: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub last_error_class: Option<Option<ProviderFailureClass>>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub last_error_code: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub usage_limit_reset_at: Option<Option<Timestamp>>,
    pub pending_runtime_request: Option<PendingRuntimeRequestSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_guest_message_count: Option<u64>,
    pub latest_visible_message: Option<LatestVisibleMessageSummary>,
    pub latest_user_message_at: Option<Timestamp>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub latest_user_authored_message_at: Option<Option<Timestamp>>,
    pub has_actionable_proposed_plan: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_background_tasks: Option<Vec<PendingBackgroundTask>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_instance_history: Option<Vec<ProviderInstanceId>>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub goal: Option<Option<ProviderGoal>>,
    pub item_count: u64,
    pub visible_item_count: u64,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub archived_at: Option<Timestamp>,
    pub settled_override: Option<SettledOverride>,
    pub settled_at: Option<Timestamp>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub unsettled_at: Option<Option<Timestamp>>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub snoozed_until: Option<Option<Timestamp>>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub snoozed_at: Option<Option<Timestamp>>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub limit_recovery: Option<Option<LimitRecovery>>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub pinned_at: Option<Option<Timestamp>>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub auto_settle_disabled_at: Option<Option<Timestamp>>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub pin_order_key: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub active_order_key: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tags: Option<Vec<String>>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub last_visited_at: Option<Option<Timestamp>>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub opens: Option<Option<ThreadOpens>>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub title_regeneration: Option<Option<TitleRegeneration>>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub last_title_regeneration: Option<Option<LastTitleRegeneration>>,
    pub deleted_at: Option<Timestamp>,
}

string_enum! {
    /// What a thread is doing: its latest run's status, or idle before any run.
    ShellThreadStatus {
        Idle = "idle",
        Preparing = "preparing",
        Queued = "queued",
        Starting = "starting",
        Running = "running",
        Waiting = "waiting",
        Completed = "completed",
        Interrupted = "interrupted",
        Failed = "failed",
        Cancelled = "cancelled",
        RolledBack = "rolled_back",
    }
}

string_enum! {
    /// The status of the run that makes a thread show as working.
    ActivityRunStatus {
        Preparing = "preparing",
        Starting = "starting",
        Running = "running",
        Waiting = "waiting",
    }
}

/// The request a thread is waiting on the user for.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingRuntimeRequestSummary {
    pub id: RuntimeRequestId,
    pub kind: RuntimeRequestKind,
    pub created_at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocking: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_only: Option<bool>,
}

/// The newest message a thread's list row previews.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LatestVisibleMessageSummary {
    pub id: MessageId,
    pub role: MessageRole,
    pub text: String,
    pub updated_at: Timestamp,
}
