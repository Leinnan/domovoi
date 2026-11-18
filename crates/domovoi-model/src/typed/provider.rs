//! What the server knows about providers: their sessions, native threads and
//! turns, and the requests they wait on.

use super::common::{ApprovalDecision, ModelSelection, NativeRefStrength, ProviderRef, Timestamp, double_option};
use super::ids::{
    CheckpointId, ContextHandoffId, NodeId, ProviderDriverKind, ProviderInstanceId, ProviderSessionId, ProviderThreadId,
    ProviderTurnId, RunAttemptId, RuntimeRequestId, ThreadId, TurnItemId,
};
use super::macros::string_enum;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value};

/// A live provider process (`OrchestrationV2ProviderSession`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderSession {
    pub id: ProviderSessionId,
    pub driver: ProviderDriverKind,
    pub provider_instance_id: ProviderInstanceId,
    pub status: ProviderSessionStatus,
    pub cwd: String,
    pub model: Option<String>,
    pub capabilities: ProviderCapabilities,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extension_statuses: Option<Vec<ExtensionStatus>>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub last_error: Option<String>,
}

string_enum! {
    /// Where a provider session is in its life.
    ProviderSessionStatus {
        Starting = "starting",
        Ready = "ready",
        Running = "running",
        Waiting = "waiting",
        Stopped = "stopped",
        Error = "error",
    }
}

/// A status line a provider extension shows for its session.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExtensionStatus {
    pub key: String,
    pub text: String,
}

/// Everything a provider session can and cannot do.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderCapabilities {
    pub sessions: SessionCapabilities,
    pub threads: ThreadCapabilities,
    pub turns: TurnCapabilities,
    pub streaming: StreamingCapabilities,
    pub tools: ToolCapabilities,
    pub approvals: ApprovalCapabilities,
    pub planning: PlanningCapabilities,
    pub subagents: SubagentCapabilities,
    pub context: ContextCapabilities,
    pub checkpointing: CheckpointCapabilities,
    pub identity: IdentityCapabilities,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_policy: Option<RuntimePolicyCapabilities>,
}

/// What a provider session can do across threads, models and restarts.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionCapabilities {
    pub supports_multiple_provider_threads_per_session: bool,
    pub supports_model_switch_in_session: bool,
    pub supports_provider_switching_via_handoff: bool,
    pub supports_runtime_mode_switch_in_session: bool,
    pub pending_requests_survive_restart: bool,
}

/// What a provider can do with its native threads.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadCapabilities {
    pub can_create_empty_thread: bool,
    pub can_read_thread_snapshot: bool,
    pub can_rollback_thread: bool,
    pub can_fork_thread: bool,
    pub can_fork_from_turn: bool,
    pub can_fork_from_subagent_thread: bool,
    pub exposes_native_thread_id: bool,
}

/// How a provider's turns start, end, and take more input.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnCapabilities {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_native_goals: Option<bool>,
    pub exposes_native_turn_id: bool,
    pub emits_turn_started: bool,
    pub emits_turn_completed: bool,
    pub supports_interrupt: bool,
    pub supports_active_steering: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_steering_interrupts_tools: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_steering_accepts_attachments: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_steering_during_compaction: Option<bool>,
    pub supports_steering_by_interrupt_restart: bool,
    pub supports_queued_messages: bool,
    pub terminal_status_quality: TerminalStatusQuality,
}

string_enum! {
    /// How far a provider's report that a turn ended can be trusted.
    TerminalStatusQuality {
        Strong = "strong",
        Weak = "weak",
        None = "none",
    }
}

/// What a provider streams while it works.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamingCapabilities {
    pub streams_assistant_text: bool,
    pub streams_reasoning: bool,
    pub streams_tool_output: bool,
    pub streams_plan_text: bool,
    pub emits_message_completed: bool,
}

/// What a provider reports about its tool calls.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCapabilities {
    pub exposes_tool_item_ids: bool,
    pub emits_tool_started: bool,
    pub emits_tool_completed: bool,
    pub emits_tool_output: bool,
    pub supports_mcp_tools: bool,
    pub supports_dynamic_tool_callbacks: bool,
}

/// Which approvals a provider can ask for, and how.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalCapabilities {
    pub supports_command_approval: bool,
    pub supports_file_read_approval: bool,
    pub supports_file_change_approval: bool,
    pub supports_apply_patch_approval: bool,
    pub approvals_have_native_request_ids: bool,
    pub approval_callbacks_are_live_only: bool,
    pub approvals_can_originate_from_subagents: bool,
}

/// What a provider reports about plans, todo lists and questions.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanningCapabilities {
    pub emits_plan_updated: bool,
    pub emits_todo_list: bool,
    pub emits_proposed_plan: bool,
    pub supports_structured_questions: bool,
    pub plan_deltas_have_item_ids: bool,
}

/// What a provider can do with its own subagents.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SubagentCapabilities {
    pub supports_subagents: bool,
    pub exposes_subagent_thread_ids: bool,
    pub emits_subagent_lifecycle: bool,
    pub can_wait_for_subagents: bool,
    pub can_close_subagents: bool,
    pub can_fork_subagent_thread: bool,
}

/// Which kinds of handed-over context a provider accepts.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextCapabilities {
    pub accepts_system_context: bool,
    pub accepts_developer_context: bool,
    pub accepts_synthetic_user_context: bool,
    pub can_generate_summaries: bool,
    pub can_consume_handoff_summaries: bool,
    pub supports_delta_handoff: bool,
    pub supports_full_thread_handoff: bool,
    pub max_recommended_handoff_chars: Option<u64>,
}

/// What can be checkpointed and rolled back with a provider.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CheckpointCapabilities {
    pub app_can_checkpoint_filesystem: bool,
    pub supports_nested_checkpoint_scopes: bool,
    pub provider_can_rollback_conversation: bool,
    pub provider_rollback_returns_snapshot: bool,
    pub provider_can_read_conversation_snapshot: bool,
}

/// How reliable a provider's native ids are, per kind of thing.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IdentityCapabilities {
    pub native_thread_ids: NativeRefStrength,
    pub native_turn_ids: NativeRefStrength,
    pub native_item_ids: NativeRefStrength,
    pub native_request_ids: NativeRefStrength,
}

/// Where a provider's runtime mode is enforced.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimePolicyCapabilities {
    pub enforcement: RuntimePolicyEnforcement,
}

string_enum! {
    /// Whether the provider itself or the server's boundary enforces the runtime mode.
    RuntimePolicyEnforcement {
        Native = "native",
        ClientBoundary = "client-boundary",
    }
}

/// A provider's own conversation backing a thread (`OrchestrationV2ProviderThread`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderThread {
    pub id: ProviderThreadId,
    pub driver: ProviderDriverKind,
    pub provider_instance_id: ProviderInstanceId,
    pub provider_session_id: Option<ProviderSessionId>,
    pub app_thread_id: Option<ThreadId>,
    pub owner_node_id: Option<NodeId>,
    pub native_thread_ref: Option<ProviderRef>,
    pub native_conversation_head_ref: Option<ProviderRef>,
    pub status: ProviderThreadStatus,
    pub first_run_ordinal: Option<u64>,
    pub last_run_ordinal: Option<u64>,
    pub handoff_ids: Vec<ContextHandoffId>,
    pub forked_from: Option<ProviderThreadFork>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_background_tasks: Option<Vec<PendingBackgroundTask>>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub context_usage: Option<Option<ContextUsage>>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub native_metadata: Option<Option<ProviderThreadNativeMetadata>>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub goal: Option<Option<ProviderGoal>>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

impl ProviderThread {
    /// Whether this is still the stand-in made when a run was queued: not
    /// loaded, never run, and tied to no native thread or session. A stand-in
    /// does not become its thread's active provider thread.
    pub fn is_placeholder(&self) -> bool {
        self.status == ProviderThreadStatus::NotLoaded
            && self.first_run_ordinal.is_none()
            && self.native_thread_ref.is_none()
            && self.provider_session_id.is_none()
    }
}

string_enum! {
    /// Where a provider thread is in its life.
    ProviderThreadStatus {
        NotLoaded = "not_loaded",
        Idle = "idle",
        Active = "active",
        Archived = "archived",
        Closed = "closed",
        Error = "error",
    }
}

/// The provider thread, and point in it, another provider thread was forked from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderThreadFork {
    pub provider_thread_id: ProviderThreadId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_turn_id: Option<ProviderTurnId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_id: Option<CheckpointId>,
}

/// Work a provider left running in the background: a subagent, command or monitor.
///
/// One record for every kind: `providerInstanceId` and `childThreadId` belong
/// to subagents, and a kind this build does not know keeps the shared fields.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingBackgroundTask {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_id: Option<TurnItemId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_thread_id: Option<ThreadId>,
    pub task_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_task_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_item_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub awaited: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<BackgroundTaskKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_instance_id: Option<ProviderInstanceId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_thread_id: Option<ThreadId>,
}

string_enum! {
    /// What kind of background work is pending.
    BackgroundTaskKind {
        Subagent = "subagent",
        Command = "command",
        Monitor = "monitor",
        BackgroundTask = "background_task",
    }
}

/// How full a provider thread's context window is, and what it has cost.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextUsage {
    pub used_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total_processed_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_used_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_cached_input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_reasoning_output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_uses: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compacts_automatically: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_compact_threshold: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<UsageCost>,
}

/// What a provider thread's usage has cost so far.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageCost {
    pub amount: Number,
    pub currency: String,
}

/// Provider-specific facts the server keeps about a native thread.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderThreadNativeMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor_store_cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_selection: Option<ModelSelection>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub title: Option<Option<String>>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<Option<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub item_identity_version: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fork_head_message_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_attachment_budget: Option<AttachmentBudget>,
}

/// How much of Claude's per-conversation attachment allowance is used.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentBudget {
    pub bytes_used: u64,
    pub pages_used: u64,
}

/// A goal a provider pursues across turns on its own.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderGoal {
    pub objective: String,
    pub status: ProviderGoalStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens_used: Option<u64>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub token_budget: Option<Option<u64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_used_seconds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checks: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_check: Option<String>,
}

string_enum! {
    /// Where a provider's goal stands.
    ProviderGoalStatus {
        Active = "active",
        Paused = "paused",
        Blocked = "blocked",
        UsageLimited = "usage_limited",
        BudgetLimited = "budget_limited",
        Complete = "complete",
    }
}

/// One native turn of a provider thread (`OrchestrationV2ProviderTurn`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderTurn {
    pub id: ProviderTurnId,
    pub provider_thread_id: ProviderThreadId,
    pub node_id: NodeId,
    pub run_attempt_id: Option<RunAttemptId>,
    pub native_turn_ref: Option<ProviderRef>,
    pub ordinal: u64,
    pub status: ProviderTurnStatus,
    pub started_at: Option<Timestamp>,
    pub completed_at: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_usage: Option<ContextWindowUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_token_usage: Option<TurnTokenUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

string_enum! {
    /// Where a provider turn is in its life.
    ProviderTurnStatus {
        Pending = "pending",
        Running = "running",
        Completed = "completed",
        Interrupted = "interrupted",
        Failed = "failed",
        Cancelled = "cancelled",
    }
}

/// The context window as a provider reported it during a turn
/// (`OrchestrationV2ProviderTurnTokenUsage`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextWindowUsage {
    pub used_tokens: u64,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<Option<u64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_cache_ttl_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_compact_threshold: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_cache_expires_at: Option<Timestamp>,
    pub updated_at: Timestamp,
}

/// The tokens one turn's main agent spent.
///
/// `inputTokens` and `outputTokens` are always present when `usageStatus` is
/// `complete`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnTokenUsage {
    pub usage_scope: UsageScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cached_input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_creation_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_tokens: Option<u64>,
    pub has_subagents: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub models: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_command: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub other_usage: Option<OtherUsage>,
    pub usage_status: UsageStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tokens: Option<u64>,
}

string_enum! {
    /// Whose work a turn's token counts describe.
    UsageScope {
        MainAgent = "main_agent",
    }
}

string_enum! {
    /// Whether a turn's token counts are all there.
    UsageStatus {
        Complete = "complete",
        Partial = "partial",
        Unavailable = "unavailable",
    }
}

/// Tokens a turn spent beyond its main agent, such as on native subagents.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OtherUsage {
    pub input_tokens: u64,
    pub output_tokens: u64,
}

/// A question a provider waits on (`OrchestrationV2RuntimeRequest`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimeRequest {
    pub id: RuntimeRequestId,
    pub node_id: NodeId,
    pub provider_turn_id: Option<ProviderTurnId>,
    pub native_request_ref: Option<ProviderRef>,
    pub kind: RuntimeRequestKind,
    pub status: RuntimeRequestStatus,
    pub response_capability: ResponseCapability,
    pub created_at: Timestamp,
    pub resolved_at: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<ApprovalDecision>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub feedback: Option<String>,
    /// Answers keyed by question id. The schema leaves each answer's shape open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answers: Option<Map<String, Value>>,
}

string_enum! {
    /// What a runtime request asks for.
    RuntimeRequestKind {
        Command = "command",
        FileRead = "file-read",
        FileChange = "file-change",
        McpElicitation = "mcp-elicitation",
        Permission = "permission",
        DynamicToolCall = "dynamic_tool_call",
        UserInput = "user_input",
        AuthRefresh = "auth_refresh",
    }
}

string_enum! {
    /// Whether a runtime request still waits for its answer.
    RuntimeRequestStatus {
        Pending = "pending",
        Resolved = "resolved",
        Expired = "expired",
        Cancelled = "cancelled",
    }
}

/// How a runtime request can still be answered.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum ResponseCapability {
    /// The provider session that asked is still waiting.
    Live {
        provider_session_id: ProviderSessionId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        supports_feedback: Option<bool>,
    },
    /// The answer is sent to the provider as a message.
    Message,
    NotResumable {
        reason: String,
    },
}
