//! The thread record of a projection, and the values it shares with the thread shell.

use super::attachment::ChatAttachment;
use super::common::{
    Actor, CreationSource, InteractionMode, LinkedThread, McpClientAccess, ModelSelection, RuntimeMode, Timestamp, double_option,
};
use super::ids::{
    CommandId, GuestMessageId, GuestShareId, NodeId, ProjectId, ProviderInstanceId, ProviderThreadId, ProviderTurnId, RunId, ThreadId,
};
use super::macros::string_enum;
use super::pull_request::{LinkedPullRequest, PullRequestLink};
use serde::{Deserialize, Serialize};
use serde_json::Number;

/// A thread as its own projection holds it (`OrchestrationV2AppThread`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppThread {
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
    pub active_provider_thread_id: Option<ProviderThreadId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_origin: Option<HistoryOrigin>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_origin: Option<LinkOrigin>,
    // Optional in the schema, but t3code stores `null` on threads launched over MCP.
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub delegated_from: Option<Option<LinkedThread>>,
    pub lineage: ThreadLineage,
    pub forked_from: Option<ForkSource>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub archived_at: Option<Timestamp>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub settled_override: Option<Option<SettledOverride>>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub settled_at: Option<Option<Timestamp>>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub unsettled_at: Option<Option<Timestamp>>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub snoozed_until: Option<Option<Timestamp>>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub snoozed_at: Option<Option<Timestamp>>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub last_snooze_wake_at: Option<Option<Timestamp>>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub limit_recovery: Option<Option<LimitRecovery>>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub guest_share: Option<Option<GuestShare>>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rollback_request_id: Option<CommandId>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub rollback_failure: Option<Option<RollbackFailure>>,
    pub deleted_at: Option<Timestamp>,
}

string_enum! {
    /// Whether a thread's history was written by this server or imported.
    HistoryOrigin {
        Native = "native",
        V1Import = "v1_import",
    }
}

string_enum! {
    /// The user's explicit choice to keep a thread settled or active.
    SettledOverride {
        Settled = "settled",
        Active = "active",
    }
}

string_enum! {
    /// How a thread relates to the thread it was created from.
    ParentRelationship {
        Fork = "fork",
        Subagent = "subagent",
    }
}

/// The linked environment whose agent started a thread here.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkOrigin {
    pub session_id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access: Option<McpClientAccess>,
}

/// Where a thread sits in its family of forks and subagents.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadLineage {
    pub parent_thread_id: Option<ThreadId>,
    pub relationship_to_parent: Option<ParentRelationship>,
    pub root_thread_id: ThreadId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub side_chat: Option<bool>,
}

/// The point a thread was forked from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum ForkSource {
    Run {
        thread_id: ThreadId,
        run_id: RunId,
    },
    Node {
        node_id: NodeId,
    },
    ProviderThread {
        provider_thread_id: ProviderThreadId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider_turn_id: Option<ProviderTurnId>,
    },
}

/// A run stopped by a usage limit, and when it may resume.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LimitRecovery {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<CommandId>,
    pub run_id: RunId,
    pub reset_at: Timestamp,
    pub auto_resume: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snooze: Option<bool>,
}

/// How often and how recently a thread was opened, for ranking search results.
/// Times are epoch milliseconds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadOpens {
    pub opened_at: Number,
    pub score: Number,
    pub picks: Vec<OpenPick>,
}

/// A palette query that ended in opening the thread.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenPick {
    pub query: String,
    pub at: Number,
}

/// A title regeneration in progress.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TitleRegeneration {
    pub request_id: CommandId,
    pub started_at: Timestamp,
}

/// How the last title regeneration ended.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LastTitleRegeneration {
    pub status: TitleRegenerationStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    pub request_id: CommandId,
    pub completed_at: Timestamp,
}

string_enum! {
    /// The outcome of a title regeneration.
    TitleRegenerationStatus {
        Renamed = "renamed",
        Unchanged = "unchanged",
        Failed = "failed",
    }
}

/// Why the last checkpoint rollback did not happen.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RollbackFailure {
    pub request_id: CommandId,
    pub message: String,
}

/// A link that lets one guest watch a thread and propose messages.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuestShare {
    pub id: GuestShareId,
    pub host: String,
    pub created_at: Timestamp,
    pub expires_at: Timestamp,
    pub guest_name: Option<String>,
    pub messages: Vec<GuestMessage>,
}

/// A message a guest proposed, for the owner to send or dismiss.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GuestMessage {
    pub id: GuestMessageId,
    pub text: String,
    pub status: GuestMessageStatus,
    pub submitted_at: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachments: Option<Vec<ChatAttachment>>,
}

string_enum! {
    /// What the owner did with a guest's message.
    GuestMessageStatus {
        Pending = "pending",
        Sent = "sent",
        Dismissed = "dismissed",
    }
}
