//! Conversation messages, and the notifications that wake an agent.

use super::attachment::ChatAttachment;
use super::common::{Actor, CreationSource, IdleCompaction, LinkedThread, Timestamp};
use super::ids::{GuestMessageId, GuestShareId, MessageId, NodeId, RunId, ScheduledTaskId, ThreadId};
use super::macros::string_enum;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A message of a thread's conversation (`OrchestrationV2ConversationMessage`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationMessage {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notification: Option<Notification>,
    pub created_by: Actor,
    pub creation_source: CreationSource,
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
    pub id: MessageId,
    pub thread_id: ThreadId,
    pub run_id: Option<RunId>,
    pub node_id: Option<NodeId>,
    pub role: MessageRole,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<MessageContext>,
    pub attachments: Vec<ChatAttachment>,
    pub streaming: bool,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegated_completion: Option<DelegatedCompletionReport>,
}

string_enum! {
    /// Who a message speaks as.
    MessageRole {
        User = "user",
        Assistant = "assistant",
        System = "system",
    }
}

/// The delegated tasks whose results a message reports to their parent run.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DelegatedCompletionReport {
    pub parent_run_id: RunId,
    pub generation: u64,
    pub task_ids: Vec<NodeId>,
}

/// Structured context a user message carries besides its text.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageContext {
    pub version: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guest: Option<GuestAttribution>,
    /// Composer context records (files, terminal output, mentions, …). The
    /// schema's wire form is an array of unknown values: each reader drops the
    /// records it cannot decode.
    pub records: Vec<Value>,
}

/// Marks a user message as written by a guest and sent by the owner.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuestAttribution {
    pub guest_message_id: GuestMessageId,
    pub name: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub share_id: Option<GuestShareId>,
}

/// Word that background work finished or changed, delivered to the agent as a message.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Notification {
    pub source: NotificationSource,
    pub outcome: NotificationOutcome,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

string_enum! {
    /// How the work a notification reports on turned out.
    NotificationOutcome {
        Completed = "completed",
        Failed = "failed",
        Cancelled = "cancelled",
        Updated = "updated",
        Unknown = "unknown",
    }
}

/// The background work a notification is about.
///
/// One record for every kind: `taskIds` belongs to delegated tasks,
/// `childThreadId` to delegated tasks and subagents, `work` to the legacy
/// `background_task` spelling of a subagent, and the pull requests to monitors.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NotificationSource {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<NotificationSourceKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_ids: Option<Vec<NodeId>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work: Option<BackgroundWork>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_thread_id: Option<ThreadId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_request: Option<MonitorPullRequestStamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub other_pull_requests: Option<Vec<MonitorPullRequestStamp>>,
}

string_enum! {
    /// What kind of background work a notification is about. The server writes
    /// subagents as `background_task` and commands as `background_command`,
    /// the spellings older clients know.
    NotificationSourceKind {
        DelegatedTask = "delegated_task",
        BackgroundTask = "background_task",
        BackgroundCommand = "background_command",
        Subagent = "subagent",
        Command = "command",
        Monitor = "monitor",
        System = "system",
    }
}

string_enum! {
    /// What a `background_task` notification source ran.
    BackgroundWork {
        Subagent = "subagent",
    }
}

/// The pull request, and the watch on it, a monitor's wake is about.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MonitorPullRequestStamp {
    pub host: String,
    pub repository: String,
    pub number: u64,
    pub watch_started_at: Timestamp,
    pub head_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub watch_ended: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub section: Option<PullRequestWakeSection>,
}

/// One pull request's part of a wake that covers several.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PullRequestWakeSection {
    pub text: String,
    pub outcome: NotificationOutcome,
    pub summary: String,
}
