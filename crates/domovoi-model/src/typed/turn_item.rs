//! Turn items: the entries of a thread's timeline.
//!
//! The schema is a union of 25 records discriminated by `type`, each starting
//! with the same nineteen fields. [`TurnItem`] is an enum with one boxed record
//! per type, and the shared fields are repeated in every record by the
//! `turn_item!` macro and read through accessors on the enum (`item.id()`,
//! `item.ordinal()`, …), so no `#[serde(flatten)]` is involved.
//!
//! The alternative, one struct with every type-specific field optional, cannot
//! type this union: `input` is a string on a command and open JSON on a dynamic
//! tool, `source` is a notification's origin or a fork's, `results` and
//! `failure` likewise differ by type, and `providerThreadId` is nullable
//! everywhere except on forks, where it may be absent instead.
//!
//! Decoding is written by hand rather than derived. The derived decoder for a
//! tagged enum with an untagged fallback buffers the record twice and falls
//! back on any error, so a known type with a missing field would silently
//! become `Other`. Here a known `type` must decode in full, as t3code's
//! `ForwardCompatibleUnion` requires, and only an unknown `type` becomes
//! [`TurnItem::Other`], which keeps the whole record and re-encodes it
//! unchanged.

use super::attachment::ChatAttachment;
use super::checkpoint::{CheckpointFileSummary, CheckpointFileTotals};
use super::common::{
    Actor, ApprovalDecision, CreationSource, HandoffStrategy, IdleCompaction, LinkedThread, McpClientAccess, ModelSelection,
    ProviderFailureClass, ProviderRef, ProviderRequestKind, SubagentOrigin, Timestamp, double_option, present,
};
use super::ids::{
    CheckpointId, CheckpointScopeId, ContextHandoffId, EnvironmentId, MessageId, NodeId, PlanId, ProviderDriverKind,
    ProviderInstanceId, ProviderThreadId, ProviderTurnId, RunId, RuntimeRequestId, ScheduledTaskId, ThreadId, TurnItemId,
};
use super::macros::{string_enum, tagged_union_deserialize};
use super::message::{MessageContext, NotificationOutcome, NotificationSource};
use super::plan::{PlanStep, UserInputQuestion};
use super::run::{CompletionWake, RemoteTaskChild};
use super::thread::ForkSource;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::BTreeMap;
use std::sync::Arc;

/// Declares a turn item record: the fields every turn item carries, then its
/// own. An attribute in brackets after the name replaces the default handling
/// of `providerThreadId` (present, possibly `null`).
macro_rules! turn_item {
    ($(#[$meta:meta])* $vis:vis struct $name:ident $([$provider_thread_id:meta])? { $($fields:tt)* }) => {
        $(#[$meta])*
        #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
        #[serde(rename_all = "camelCase")]
        $vis struct $name {
            /// Wire-only: this item stands for a run of collapsed work that can be fetched.
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub deferred_work: Option<DeferredWork>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub tool_non_execution_kind: Option<String>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub tool_surface: Option<ToolSurface>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub tool_icon: Option<ToolIcon>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub tool_source: Option<ToolSource>,
            pub id: TurnItemId,
            pub thread_id: ThreadId,
            pub run_id: Option<RunId>,
            pub node_id: Option<NodeId>,
            $(#[$provider_thread_id])?
            pub provider_thread_id: Option<ProviderThreadId>,
            pub provider_turn_id: Option<ProviderTurnId>,
            pub native_item_ref: Option<ProviderRef>,
            pub parent_item_id: Option<TurnItemId>,
            pub ordinal: u64,
            pub status: TurnItemStatus,
            pub title: Option<String>,
            pub started_at: Option<Timestamp>,
            pub completed_at: Option<Timestamp>,
            pub updated_at: Timestamp,
            $($fields)*
        }
    };
}

/// Defines an accessor on [`TurnItem`] for a field every record carries.
macro_rules! shared_field {
    ([$($variant:ident)+] $(#[$meta:meta])* $name:ident -> $returns:ty, $item:ident => $value:expr) => {
        $(#[$meta])*
        pub fn $name(&self) -> $returns {
            match self {
                $(Self::$variant($item) => $value,)+
                Self::Other(unknown) => {
                    let $item = &unknown.shared;
                    $value
                }
            }
        }
    };
}

/// Defines [`TurnItem`] from its wire types and the record each one decodes as.
macro_rules! turn_items {
    ($($tag:literal => $variant:ident($record:ident),)+) => {
        /// One entry of a thread's timeline (`OrchestrationV2TurnItem`).
        #[derive(Clone, Debug, PartialEq, Serialize)]
        #[serde(tag = "type")]
        pub enum TurnItem {
            $(#[serde(rename = $tag)] $variant(Box<$record>),)+
            /// An item of a type this build does not know, kept whole.
            #[serde(untagged)]
            Other(Box<UnknownTurnItem>),
        }

        tagged_union_deserialize! {
            TurnItem by "type" {
                $($tag => $variant,)+
                _ => Other,
            }
        }

        impl TurnItem {
            /// The wire name of this item's type, such as `user_message`.
            pub fn item_type(&self) -> &str {
                match self {
                    $(Self::$variant(_) => $tag,)+
                    Self::Other(unknown) => unknown.item_type(),
                }
            }

            shared_field!([$($variant)+] id -> &TurnItemId, item => &item.id);
            shared_field!([$($variant)+] thread_id -> &ThreadId, item => &item.thread_id);
            shared_field!([$($variant)+] run_id -> Option<&RunId>, item => item.run_id.as_ref());
            shared_field!([$($variant)+] node_id -> Option<&NodeId>, item => item.node_id.as_ref());
            shared_field!([$($variant)+] provider_thread_id -> Option<&ProviderThreadId>, item => item.provider_thread_id.as_ref());
            shared_field!([$($variant)+] provider_turn_id -> Option<&ProviderTurnId>, item => item.provider_turn_id.as_ref());
            shared_field!([$($variant)+] native_item_ref -> Option<&ProviderRef>, item => item.native_item_ref.as_ref());
            shared_field!([$($variant)+] parent_item_id -> Option<&TurnItemId>, item => item.parent_item_id.as_ref());
            shared_field!([$($variant)+]
                /// The item's place in its thread's timeline; items sort by `(ordinal, id)`.
                ordinal -> u64, item => item.ordinal);
            shared_field!([$($variant)+] status -> &TurnItemStatus, item => &item.status);
            shared_field!([$($variant)+] title -> Option<&str>, item => item.title.as_deref());
            shared_field!([$($variant)+] started_at -> Option<&Timestamp>, item => item.started_at.as_ref());
            shared_field!([$($variant)+] completed_at -> Option<&Timestamp>, item => item.completed_at.as_ref());
            shared_field!([$($variant)+] updated_at -> &Timestamp, item => &item.updated_at);
            shared_field!([$($variant)+] deferred_work -> Option<&DeferredWork>, item => item.deferred_work.as_ref());
            shared_field!([$($variant)+] tool_non_execution_kind -> Option<&str>, item => item.tool_non_execution_kind.as_deref());
            shared_field!([$($variant)+] tool_surface -> Option<&ToolSurface>, item => item.tool_surface.as_ref());
            shared_field!([$($variant)+] tool_icon -> Option<&ToolIcon>, item => item.tool_icon.as_ref());
            shared_field!([$($variant)+] tool_source -> Option<&ToolSource>, item => item.tool_source.as_ref());

            /// Moves the item to `status`, whatever its type.
            pub fn set_status(&mut self, status: TurnItemStatus) {
                match self {
                    $(Self::$variant(item) => item.status = status,)+
                    Self::Other(unknown) => unknown.set("status", |shared| &mut shared.status, status),
                }
            }

            /// Says when the item ended (or, with `None`, that it has not).
            pub fn set_completed_at(&mut self, at: Option<Timestamp>) {
                match self {
                    $(Self::$variant(item) => item.completed_at = at,)+
                    Self::Other(unknown) => unknown.set("completedAt", |shared| &mut shared.completed_at, at),
                }
            }

            /// Says when the item last changed.
            pub fn set_updated_at(&mut self, at: Timestamp) {
                match self {
                    $(Self::$variant(item) => item.updated_at = at,)+
                    Self::Other(unknown) => unknown.set("updatedAt", |shared| &mut shared.updated_at, at),
                }
            }
        }
    };
}

turn_items! {
    "notification" => Notification(NotificationItem),
    "user_message" => UserMessage(UserMessageItem),
    "assistant_message" => AssistantMessage(AssistantMessageItem),
    "reasoning" => Reasoning(ReasoningItem),
    "proposed_plan" => ProposedPlan(ProposedPlanItem),
    "todo_list" => TodoList(TodoListItem),
    "user_input_request" => UserInputRequest(UserInputRequestItem),
    "file_change" => FileChange(FileChangeItem),
    "command_execution" => CommandExecution(CommandExecutionItem),
    "file_search" => FileSearch(FileSearchItem),
    "web_search" => WebSearch(WebSearchItem),
    "approval_request" => ApprovalRequest(ApprovalRequestItem),
    "checkpoint" => Checkpoint(CheckpointItem),
    "run_interrupt_request" => RunInterruptRequest(RunInterruptRequestItem),
    "run_interrupt_result" => RunInterruptResult(RunInterruptResultItem),
    "system_notice" => SystemNotice(SystemNoticeItem),
    "error" => Error(ErrorItem),
    "compaction" => Compaction(CompactionItem),
    "handoff" => Handoff(HandoffItem),
    "fork" => Fork(ForkItem),
    "thread_created" => ThreadCreated(ThreadCreatedItem),
    "secret_request" => SecretRequest(SecretRequestItem),
    "link_request" => LinkRequest(LinkRequestItem),
    "subagent" => Subagent(SubagentItem),
    "dynamic_tool" => DynamicTool(DynamicToolItem),
}

impl TurnItem {
    /// The conversation message this item shows, for the items that show one.
    pub fn message_id(&self) -> Option<&MessageId> {
        match self {
            Self::UserMessage(item) => Some(&item.message_id),
            Self::AssistantMessage(item) => Some(&item.message_id),
            _ => None,
        }
    }

    /// Says an item that grows while its reply streams has stopped growing.
    /// An item of a type that does not stream is left as it is; one of a type
    /// this build does not know is told so if it says it streams.
    pub fn stop_streaming(&mut self) {
        match self {
            Self::AssistantMessage(item) => item.streaming = false,
            Self::Reasoning(item) => item.streaming = false,
            Self::ProposedPlan(item) => item.streaming = false,
            Self::Other(unknown) => {
                if let Some(streaming) = unknown.record.get_mut("streaming") {
                    *streaming = Value::Bool(false);
                }
            }
            _ => {}
        }
    }
}

turn_item! {
    /// The shared fields of a turn item whose type is not known.
    struct SharedFields {}
}

/// A turn item of a type this build does not know: its record as it was sent,
/// with the fields every turn item carries read out of it.
#[derive(Clone, Debug, PartialEq)]
pub struct UnknownTurnItem {
    shared: SharedFields,
    record: Map<String, Value>,
}

impl UnknownTurnItem {
    /// The wire name of the item's type.
    pub fn item_type(&self) -> &str {
        self.record.get("type").and_then(Value::as_str).unwrap_or_default()
    }

    /// The whole record, `type` and shared fields included.
    pub fn record(&self) -> &Map<String, Value> {
        &self.record
    }

    /// Sets a field every turn item carries, in the record and in what was read out of it.
    fn set<T: Serialize + Clone>(&mut self, key: &str, field: fn(&mut SharedFields) -> &mut T, value: T) {
        if let Ok(written) = serde_json::to_value(&value) {
            self.record.insert(key.to_owned(), written);
            *field(&mut self.shared) = value;
        }
    }
}

impl Serialize for UnknownTurnItem {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.record.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for UnknownTurnItem {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let record = Value::Object(Map::deserialize(deserializer)?);
        let shared = SharedFields::deserialize(&record).map_err(serde::de::Error::custom)?;
        match record {
            Value::Object(record) => Ok(Self { shared, record }),
            _ => Err(serde::de::Error::custom("a turn item is an object")),
        }
    }
}

string_enum! {
    /// Where a turn item is in its life.
    TurnItemStatus {
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

/// A turn item as one thread's timeline shows it (`OrchestrationV2ProjectedTurnItem`):
/// its own, one inherited from the thread it was forked from, or a synthetic one.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectedTurnItem {
    pub position: u64,
    pub visibility: ItemVisibility,
    pub source_thread_id: ThreadId,
    pub source_item_id: TurnItemId,
    /// Shared with the thread's own records, so showing an item copies no text.
    pub item: Arc<TurnItem>,
}

string_enum! {
    /// Why a turn item appears in a thread's timeline.
    ItemVisibility {
        Local = "local",
        Inherited = "inherited",
        Synthetic = "synthetic",
    }
}

/// Where to fetch the collapsed work a turn item stands for.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeferredWork {
    pub cursor: String,
    pub item_count: u64,
    pub last_ordinal: u64,
}

string_enum! {
    /// The surface a tool acted on.
    ToolSurface {
        Browser = "browser",
        Computer = "computer",
    }
}

/// The icon shown for a tool's activity.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "_tag", rename_all = "kebab-case", rename_all_fields = "camelCase")]
pub enum ToolIcon {
    Website {
        page_url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        favicon_url: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        favicon_url_dark: Option<String>,
    },
    NativeApp {
        app: NativeAppReference,
    },
    ThemedLogo {
        logo_url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        logo_url_dark: Option<String>,
    },
}

/// The desktop application a tool acted on.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "_tag", rename_all = "kebab-case", rename_all_fields = "camelCase")]
pub enum NativeAppReference {
    AppId { app_id: String },
    DisplayName { display_name: String },
}

/// The site, application or integration a tool's activity belongs to.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolSource {
    pub key: String,
    pub name: String,
    pub kind: ToolSourceKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<ToolIcon>,
}

string_enum! {
    /// What kind of thing a tool's activity belongs to.
    ToolSourceKind {
        Browser = "browser",
        Computer = "computer",
        Integration = "integration",
    }
}

turn_item! {
    /// Word that background work finished or changed, shown in the timeline.
    pub struct NotificationItem {
        pub source: NotificationSource,
        pub outcome: NotificationOutcome,
        pub summary: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub detail: Option<String>,
    }
}

turn_item! {
    /// A message sent to the agent: by the user, another agent, or the server.
    pub struct UserMessageItem {
        pub created_by: Actor,
        pub creation_source: CreationSource,
        pub message_id: MessageId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub scheduled_task_id: Option<ScheduledTaskId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub sender_thread_id: Option<ThreadId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub linked_sender: Option<LinkedThread>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub idle_compaction: Option<IdleCompaction>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub workflow_script: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub fresh_context: Option<bool>,
        pub input_intent: InputIntent,
        pub text: String,
        /// Wire-only: `text` was blanked because message `messageId` holds it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub text_omitted: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub context: Option<MessageContext>,
        pub attachments: Vec<ChatAttachment>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub model_switch: Option<ModelSwitch>,
    }
}

string_enum! {
    /// How a user message reached the agent.
    InputIntent {
        TurnStart = "turn_start",
        QueuedTurn = "queued_turn",
        Steer = "steer",
        PromotedQueuedToSteer = "promoted_queued_to_steer",
    }
}

/// A model or effort change that took effect with a message.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelSwitch {
    pub from: ModelSelection,
    pub to: ModelSelection,
}

turn_item! {
    /// Text the agent wrote to the user.
    pub struct AssistantMessageItem {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub phase: Option<AssistantMessagePhase>,
        pub message_id: MessageId,
        pub text: String,
        /// Wire-only: `text` was blanked because message `messageId` holds it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub text_omitted: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub attachments: Option<Vec<ChatAttachment>>,
        pub streaming: bool,
    }
}

string_enum! {
    /// Whether an assistant message is a remark along the way or the answer.
    AssistantMessagePhase {
        Commentary = "commentary",
        PartialAnswer = "partial_answer",
        FinalAnswer = "final_answer",
    }
}

turn_item! {
    /// The agent's reasoning, as far as the provider shows it.
    pub struct ReasoningItem {
        pub text: String,
        pub streaming: bool,
    }
}

turn_item! {
    /// A plan the agent proposes for the user to accept.
    pub struct ProposedPlanItem {
        pub plan_id: PlanId,
        pub markdown: String,
        pub streaming: bool,
    }
}

turn_item! {
    /// The agent's todo list as of this point.
    pub struct TodoListItem {
        pub plan_id: PlanId,
        pub steps: Vec<PlanStep>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub explanation: Option<String>,
    }
}

turn_item! {
    /// Questions the agent asks the user, and the answer once given.
    pub struct UserInputRequestItem {
        pub request_id: RuntimeRequestId,
        pub questions: Vec<UserInputQuestion>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub question_answer: Option<UserInputAnswer>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub response_mode: Option<ResponseMode>,
    }
}

/// The user's answer to an agent's questions.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserInputAnswer {
    pub request_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question_text_by_id: Option<BTreeMap<String, String>>,
    /// Answers keyed by question id. The schema leaves each answer's shape open.
    pub answers: Map<String, Value>,
    pub attachments_by_question_id: BTreeMap<String, Vec<ChatAttachment>>,
}

string_enum! {
    /// How an answer to a question reaches the agent when not through the live request.
    ResponseMode {
        Message = "message",
    }
}

turn_item! {
    /// An edit the agent made to a file.
    pub struct FileChangeItem {
        pub file_name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub additions: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub deletions: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub diff_str: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub old_str: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub new_str: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub changes: Option<Vec<FileChangeDetail>>,
    }
}

/// One file touched by a file change that covers several.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileChangeDetail {
    pub operation: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
}

turn_item! {
    /// A shell command the agent ran.
    pub struct CommandExecutionItem {
        pub input: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub output: Option<String>,
        /// Wire-only: the output was withheld and can be fetched with `getTurnItem`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub output_omitted: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub output_indicates_failure: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub exit_code: Option<i64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub monitor: Option<bool>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub completion_wake: Option<CompletionWake>,
    }
}

turn_item! {
    /// A search of the workspace's files.
    pub struct FileSearchItem {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub pattern: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub results: Option<Vec<FileSearchResult>>,
    }
}

/// One match of a file search.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileSearchResult {
    pub file_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub column: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview: Option<String>,
}

turn_item! {
    /// A web search the agent made.
    pub struct WebSearchItem {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub patterns: Option<Vec<String>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub results: Option<Vec<WebSearchResult>>,
    }
}

/// One result of a web search.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebSearchResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snippet: Option<String>,
}

turn_item! {
    /// The agent asking the user's permission to do something.
    pub struct ApprovalRequestItem {
        pub request_id: RuntimeRequestId,
        pub request_kind: ProviderRequestKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub prompt: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub reason: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub full_access_override: Option<FullAccessOverride>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub app_name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub options: Option<Vec<ApprovalOption>>,
    }
}

string_enum! {
    /// Why an approval reached the user although the thread has full access.
    FullAccessOverride {
        AskRule = "ask_rule",
        ProviderCheck = "provider_check",
    }
}

/// One answer an approval request offers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalOption {
    pub decision: ApprovalDecision,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

turn_item! {
    /// A checkpoint taken at this point, with the files it changed.
    pub struct CheckpointItem {
        pub checkpoint_id: CheckpointId,
        pub scope_id: CheckpointScopeId,
        pub files: Vec<CheckpointFileSummary>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub files_total: Option<CheckpointFileTotals>,
        /// Wire-only: `files` was blanked because checkpoint `checkpointId` holds it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub files_omitted: Option<bool>,
    }
}

turn_item! {
    /// The user asked to stop the run.
    pub struct RunInterruptRequestItem {
        pub message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub unresponsive_at: Option<Timestamp>,
    }
}

turn_item! {
    /// The run stopped after the user asked it to.
    pub struct RunInterruptResultItem {
        pub message: String,
    }
}

turn_item! {
    /// A note from the server in the timeline.
    pub struct SystemNoticeItem {
        pub message: String,
    }
}

turn_item! {
    /// A provider failure, and the retry under way if there is one.
    pub struct ErrorItem {
        pub failure: ProviderFailure,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub retry: Option<ProviderRetry>,
    }
}

/// A provider failure as shown to the user, with credentials already redacted.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderFailure {
    pub class: ProviderFailureClass,
    pub message: String,
    pub code: Option<String>,
    pub retryable: Option<bool>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub reset_at: Option<Option<Timestamp>>,
}

/// Which retry of a failed provider call is under way.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderRetry {
    pub attempt: u64,
    pub max_attempts: Option<u64>,
    pub retry_delay_ms: Option<u64>,
}

turn_item! {
    /// The provider compacted its conversation to free context.
    pub struct CompactionItem {
        pub driver: Option<ProviderDriverKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub summary: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub before_token_count: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub after_token_count: Option<u64>,
    }
}

turn_item! {
    /// The thread moved to another provider thread, which was given a summary.
    pub struct HandoffItem {
        pub context_handoff_id: ContextHandoffId,
        pub from_provider_thread_ids: Vec<ProviderThreadId>,
        pub to_provider_thread_id: ProviderThreadId,
        pub from_provider_instance_ids: Vec<ProviderInstanceId>,
        pub to_provider_instance_id: ProviderInstanceId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub from_model_selections: Option<Vec<ModelSelection>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub to_model: Option<String>,
        pub strategy: HandoffStrategy,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub summary: Option<String>,
    }
}

turn_item! {
    /// Another thread was forked from this one. Unlike on every other item,
    /// `providerThreadId` is absent rather than `null` when there is none.
    pub struct ForkItem [serde(default, skip_serializing_if = "Option::is_none")] {
        pub source: ForkSource,
        pub target_thread_id: ThreadId,
    }
}

turn_item! {
    /// The agent started a new thread.
    pub struct ThreadCreatedItem {
        pub target_thread_id: ThreadId,
        pub target_run_id: Option<RunId>,
        pub target_provider_instance_id: ProviderInstanceId,
        pub target_model: String,
    }
}

turn_item! {
    /// The agent asked the user for a secret, which never passes through the timeline.
    pub struct SecretRequestItem {
        pub label: String,
        pub reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub url: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub placeholder: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub delivery: Option<SecretDelivery>,
        pub secret_status: SecretRequestStatus,
    }
}

string_enum! {
    /// How a requested secret is handed to the agent.
    SecretDelivery {
        /// Written to an owner-only file the agent's shell commands can read.
        File = "file",
    }
}

string_enum! {
    /// How a secret request ended.
    SecretRequestStatus {
        Pending = "pending",
        Saved = "saved",
        Declined = "declined",
        Cancelled = "cancelled",
    }
}

turn_item! {
    /// The agent asked the user to link another environment.
    pub struct LinkRequestItem {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub url: Option<String>,
        pub reason: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub environment_id: Option<EnvironmentId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub label: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub hint: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub requested_access: Option<McpClientAccess>,
        pub link_status: LinkRequestStatus,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub linked_environment_id: Option<EnvironmentId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub linked_label: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub linked_access: Option<McpClientAccess>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub failure: Option<String>,
    }
}

string_enum! {
    /// How a link request ended.
    LinkRequestStatus {
        Pending = "pending",
        Linked = "linked",
        Declined = "declined",
        Cancelled = "cancelled",
        Failed = "failed",
    }
}

turn_item! {
    /// A task the agent handed to a subagent, with its progress and result.
    pub struct SubagentItem {
        pub subagent_id: NodeId,
        pub origin: SubagentOrigin,
        pub driver: ProviderDriverKind,
        pub provider_instance_id: ProviderInstanceId,
        pub child_thread_id: Option<ThreadId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub remote_child: Option<RemoteTaskChild>,
        pub prompt: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub progress: Option<String>,
        pub result: Option<String>,
        /// Wire-only: `prompt` was blanked because subagent `subagentId` holds it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub prompt_omitted: Option<bool>,
        /// Wire-only: `result` was blanked because subagent `subagentId` holds it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub result_omitted: Option<bool>,
    }
}

turn_item! {
    /// A call to a tool that has no record of its own, such as an MCP tool.
    pub struct DynamicToolItem {
        pub tool_name: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub viewed_image_path: Option<String>,
        /// The tool's arguments, in whatever shape the tool defines (`Schema.Unknown`).
        #[serde(default, deserialize_with = "present", skip_serializing_if = "Option::is_none")]
        pub input: Option<Value>,
        /// The tool's result, in whatever shape the tool defines (`Schema.Unknown`).
        #[serde(default, deserialize_with = "present", skip_serializing_if = "Option::is_none")]
        pub output: Option<Value>,
        /// Wire-only: the output was withheld and can be fetched with `getTurnItem`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pub output_omitted: Option<bool>,
    }
}
