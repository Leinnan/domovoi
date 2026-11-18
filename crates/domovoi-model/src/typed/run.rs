//! Runs and the execution they consist of: attempts, nodes and subagents.

use super::common::{Actor, ModelSelection, ProviderRef, SubagentOrigin, Timestamp, double_option};
use super::ids::{
    CheckpointId, CheckpointScopeId, ContextHandoffId, EnvironmentId, MessageId, NodeId, PlanId, ProviderDriverKind,
    ProviderInstanceId, ProviderThreadId, ProviderTurnId, RunAttemptId, RunId, RuntimeRequestId, ThreadId,
};
use super::macros::string_enum;
use serde::{Deserialize, Serialize};

/// One user-to-agent cycle of a thread (`OrchestrationV2Run`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Run {
    pub id: RunId,
    pub thread_id: ThreadId,
    pub ordinal: u64,
    pub provider_instance_id: ProviderInstanceId,
    pub model_selection: ModelSelection,
    pub provider_thread_id: Option<ProviderThreadId>,
    pub user_message_id: MessageId,
    pub root_node_id: Option<NodeId>,
    pub active_attempt_id: Option<RunAttemptId>,
    pub status: RunStatus,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub queue_position: Option<Option<u64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_held: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub steer_when_ready_run_id: Option<RunId>,
    pub requested_at: Timestamp,
    pub started_at: Option<Timestamp>,
    pub completed_at: Option<Timestamp>,
    pub checkpoint_id: Option<CheckpointId>,
    pub context_handoff_id: Option<ContextHandoffId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restart_continuation_of_run_id: Option<RunId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_limit_continuation_of_run_id: Option<RunId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegated_task_id: Option<NodeId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_started_at: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restart_cancelled_background_work: Option<Vec<CancelledBackgroundWork>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_plan_ref: Option<SourcePlanRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegated_completion: Option<DelegatedCompletionCohort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_preparation: Option<WorkspaceStrategy>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fresh_context: Option<bool>,
}

string_enum! {
    /// Where a run is in its life.
    RunStatus {
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

/// Background work a server restart cancelled, which the next run tells the agent about.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelledBackgroundWork {
    pub kind: CancelledWorkKind,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub awaited: Option<bool>,
}

string_enum! {
    /// The kind of background work a restart cancelled.
    CancelledWorkKind {
        Subagent = "subagent",
        Shell = "shell",
        Monitor = "monitor",
        Task = "task",
    }
}

/// The plan a run was started to carry out.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourcePlanRef {
    pub thread_id: ThreadId,
    pub plan_id: PlanId,
}

/// The delegated tasks of a run whose results are reported back together.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DelegatedCompletionCohort {
    pub disposition: CohortDisposition,
    pub next_generation: u64,
    pub delivery: Option<DelegatedCompletionDelivery>,
}

string_enum! {
    /// Whether a run still collects its delegated tasks' results.
    CohortDisposition {
        Open = "open",
        Stopped = "stopped",
        Disposed = "disposed",
    }
}

/// The message that reported a batch of delegated tasks' results.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DelegatedCompletionDelivery {
    pub generation: u64,
    pub message_id: MessageId,
    pub task_ids: Vec<NodeId>,
}

/// Where a new thread's run works: the project root or a git worktree.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum WorkspaceStrategy {
    Root {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        branch: Option<String>,
    },
    ExistingWorktree {
        worktree_path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        branch: Option<String>,
    },
    Worktree {
        base_ref: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        branch: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        start_from_origin: Option<bool>,
    },
}

/// One attempt at carrying out a run (`OrchestrationV2RunAttempt`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunAttempt {
    pub id: RunAttemptId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_thread_id: Option<String>,
    pub run_id: RunId,
    pub attempt_ordinal: u64,
    pub root_node_id: NodeId,
    pub provider_instance_id: ProviderInstanceId,
    pub provider_thread_id: ProviderThreadId,
    pub provider_turn_id: Option<ProviderTurnId>,
    pub reason: RunAttemptReason,
    pub status: RunAttemptStatus,
    pub started_at: Option<Timestamp>,
    pub completed_at: Option<Timestamp>,
}

string_enum! {
    /// Why a run needed this attempt.
    RunAttemptReason {
        Initial = "initial",
        SteeringRestart = "steering_restart",
        Retry = "retry",
        ProviderRecovery = "provider_recovery",
    }
}

string_enum! {
    /// Where a run attempt is in its life.
    RunAttemptStatus {
        Pending = "pending",
        Running = "running",
        Completed = "completed",
        Interrupted = "interrupted",
        Failed = "failed",
        Cancelled = "cancelled",
        Superseded = "superseded",
    }
}

/// One step of a run's execution tree (`OrchestrationV2ExecutionNode`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionNode {
    pub id: NodeId,
    pub thread_id: ThreadId,
    pub run_id: Option<RunId>,
    pub parent_node_id: Option<NodeId>,
    pub root_node_id: NodeId,
    pub kind: NodeKind,
    pub status: NodeStatus,
    pub counts_for_run: bool,
    pub provider_thread_id: Option<ProviderThreadId>,
    pub provider_turn_id: Option<ProviderTurnId>,
    pub native_item_ref: Option<ProviderRef>,
    pub runtime_request_id: Option<RuntimeRequestId>,
    pub checkpoint_scope_id: Option<CheckpointScopeId>,
    pub started_at: Option<Timestamp>,
    pub completed_at: Option<Timestamp>,
}

string_enum! {
    /// What an execution node stands for.
    NodeKind {
        RootTurn = "root_turn",
        AssistantMessage = "assistant_message",
        Reasoning = "reasoning",
        Plan = "plan",
        TodoList = "todo_list",
        ToolCall = "tool_call",
        ApprovalRequest = "approval_request",
        UserInputRequest = "user_input_request",
        Subagent = "subagent",
        Hook = "hook",
        System = "system",
    }
}

string_enum! {
    /// Where an execution node is in its life.
    NodeStatus {
        Idle = "idle",
        Pending = "pending",
        Running = "running",
        Waiting = "waiting",
        Completed = "completed",
        Interrupted = "interrupted",
        Failed = "failed",
        Cancelled = "cancelled",
        RolledBack = "rolled_back",
    }
}

/// A task an agent handed to another agent (`OrchestrationV2Subagent`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Subagent {
    pub id: NodeId,
    pub thread_id: ThreadId,
    pub run_id: Option<RunId>,
    pub parent_node_id: NodeId,
    pub origin: SubagentOrigin,
    pub created_by: Actor,
    pub driver: ProviderDriverKind,
    pub provider_instance_id: ProviderInstanceId,
    pub provider_thread_id: Option<ProviderThreadId>,
    pub child_thread_id: Option<ThreadId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_child: Option<RemoteTaskChild>,
    pub native_task_ref: Option<ProviderRef>,
    pub prompt: String,
    pub title: Option<String>,
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_selection: Option<ModelSelection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_wake: Option<CompletionWake>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion_delivery: Option<CompletionTaskDelivery>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancelled_run_id: Option<RunId>,
    pub status: SubagentStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<SubagentWorkflow>,
    pub result: Option<String>,
    pub started_at: Option<Timestamp>,
    pub completed_at: Option<Timestamp>,
    pub updated_at: Timestamp,
    /// Wire-only: `prompt` was blanked because an earlier record on the stream holds it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_omitted: Option<bool>,
    /// Wire-only: `result` was blanked because an earlier record on the stream holds it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result_omitted: Option<bool>,
}

string_enum! {
    /// Where a subagent's task is in its life.
    SubagentStatus {
        Idle = "idle",
        Pending = "pending",
        Running = "running",
        Waiting = "waiting",
        Completed = "completed",
        Failed = "failed",
        Cancelled = "cancelled",
        Interrupted = "interrupted",
    }
}

string_enum! {
    /// When finished background work wakes the agent that started it. Commands
    /// only ever say `always`.
    CompletionWake {
        Always = "always",
        SettledOnly = "settled_only",
    }
}

/// Whether a delegated task's result has reached the agent that asked for it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompletionTaskDelivery {
    pub state: CompletionDeliveryState,
    pub observed_by_run_id: Option<RunId>,
}

string_enum! {
    /// How far a delegated task's result is on its way to the delegating agent.
    CompletionDeliveryState {
        Pending = "pending",
        Claimed = "claimed",
        Acknowledged = "acknowledged",
        Delivered = "delivered",
        Disposed = "disposed",
    }
}

/// The thread, in a linked environment, that a delegated task runs as.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteTaskChild {
    pub environment_id: EnvironmentId,
    pub thread_id: ThreadId,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<RunId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stopped_there: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stop_count: Option<u64>,
}

/// The progress of a provider-native workflow a subagent runs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentWorkflow {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_handles: Option<WorkflowRunHandles>,
    pub phases: Vec<WorkflowPhase>,
    pub agents: Vec<WorkflowAgent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

/// Where a workflow run keeps its script and transcripts.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowRunHandles {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transcript_dir: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script_path: Option<String>,
}

/// One phase of a workflow.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowPhase {
    pub index: u64,
    pub title: String,
}

/// One agent of a workflow. Its times are epoch milliseconds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkflowAgent {
    pub index: u64,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_thread_id: Option<ThreadId>,
    pub state: WorkflowAgentState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase_index: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queued_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<String>,
}

string_enum! {
    /// Where a workflow agent is in its life.
    WorkflowAgentState {
        Queued = "queued",
        Running = "running",
        Completed = "completed",
        Failed = "failed",
    }
}
