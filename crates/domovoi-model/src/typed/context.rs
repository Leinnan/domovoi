//! Moving context between provider threads: handoff summaries and transfers.

use super::common::{Actor, HandoffStrategy, ProviderRef, Timestamp, double_option};
use super::ids::{
    CheckpointId, ContextHandoffId, ContextTransferId, MessageId, ProviderInstanceId, ProviderThreadId, RunId, ThreadId, TurnItemId,
};
use super::macros::string_enum;
use super::provider::ContextWindowUsage;
use serde::{Deserialize, Serialize};

/// A summary of earlier runs given to a provider thread that did not see them
/// (`OrchestrationV2ContextHandoff`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextHandoff {
    pub id: ContextHandoffId,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub transfer_id: Option<Option<ContextTransferId>>,
    pub thread_id: ThreadId,
    pub target_run_id: RunId,
    pub from_provider_thread_ids: Vec<ProviderThreadId>,
    pub to_provider_thread_id: ProviderThreadId,
    pub covered_run_ordinals: RunOrdinalRange,
    pub strategy: HandoffStrategy,
    pub status: HandoffStatus,
    pub summary_message_id: Option<MessageId>,
    pub summary_text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history: Option<HandoffHistory>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delivery: Option<HandoffDelivery>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail_in_turn_item: Option<bool>,
    pub created_by_provider_instance_id: Option<ProviderInstanceId>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}

string_enum! {
    /// Where a handoff is in its life.
    HandoffStatus {
        Pending = "pending",
        Ready = "ready",
        Failed = "failed",
        Superseded = "superseded",
    }
}

/// The runs a handoff covers, by ordinal, both ends included.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RunOrdinalRange {
    pub from: u64,
    pub to: u64,
}

/// The earlier messages a handoff carries verbatim.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HandoffHistory {
    pub messages: Vec<HistoricalMessage>,
    pub coverage: String,
    pub omitted_items: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub omitted_item_ids: Option<Vec<TurnItemId>>,
}

/// One earlier message carried by a handoff.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HistoricalMessage {
    pub role: HistoricalRole,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_status: Option<String>,
    pub thread_id: ThreadId,
    pub run_id: Option<RunId>,
    pub item_id: TurnItemId,
    pub provider_thread_id: Option<ProviderThreadId>,
    pub status: String,
    pub kind: String,
}

string_enum! {
    /// Who a historical message speaks as.
    HistoricalRole {
        User = "user",
        Assistant = "assistant",
    }
}

/// How a handoff's history reached the target provider thread.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HandoffDelivery {
    pub native_thread_id: String,
    pub status: HandoffDeliveryStatus,
    pub item_ids: Vec<TurnItemId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub omitted_item_ids: Option<Vec<TurnItemId>>,
}

string_enum! {
    /// Whether a handoff's history has been given to the provider.
    HandoffDeliveryStatus {
        Pending = "pending",
        Injected = "injected",
        Inline = "inline",
    }
}

/// A transfer of context from one thread or provider to another
/// (`OrchestrationV2ContextTransfer`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextTransfer {
    pub id: ContextTransferId,
    #[serde(rename = "type")]
    pub kind: ContextTransferType,
    pub source_thread_id: ThreadId,
    pub target_thread_id: ThreadId,
    pub source_point: ContextSourcePoint,
    pub base_point: Option<ContextSourcePoint>,
    pub source_provider_instance_id: Option<ProviderInstanceId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_token_usage: Option<ContextWindowUsage>,
    pub target_provider_instance_id: Option<ProviderInstanceId>,
    pub target_run_id: Option<RunId>,
    pub status: ContextTransferStatus,
    pub resolution: Option<ContextTransferResolution>,
    pub created_by: Actor,
    pub error: Option<String>,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub consumed_at: Option<Timestamp>,
}

string_enum! {
    /// Why context is being transferred.
    ContextTransferType {
        Fork = "fork",
        ProviderHandoff = "provider_handoff",
        MergeBack = "merge_back",
        SubagentSpawn = "subagent_spawn",
        SubagentResult = "subagent_result",
    }
}

string_enum! {
    /// Where a context transfer is in its life.
    ContextTransferStatus {
        Pending = "pending",
        ResolvedNative = "resolved_native",
        ResolvedPortable = "resolved_portable",
        Failed = "failed",
        Consumed = "consumed",
        Superseded = "superseded",
    }
}

/// The point in a thread that transferred context is taken from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextSourcePoint {
    pub thread_id: ThreadId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<RunId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_id: Option<CheckpointId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_item_id: Option<TurnItemId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_thread_ref: Option<ProviderRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_turn_ref: Option<ProviderRef>,
}

/// How a context transfer was carried out.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "strategy", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum ContextTransferResolution {
    /// The provider forked its own conversation.
    NativeFork { provider_thread_ref: ProviderRef },
    PortableContext { context_handoff_id: ContextHandoffId },
    DeltaContext { context_handoff_id: ContextHandoffId },
    ForkDeltaContext { context_handoff_id: ContextHandoffId },
    CheckpointContext { context_handoff_id: ContextHandoffId },
}
