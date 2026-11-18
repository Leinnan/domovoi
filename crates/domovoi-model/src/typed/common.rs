//! Value types shared by several records.

use super::ids::{EnvironmentId, ProviderDriverKind, ProviderInstanceId, ThreadId};
use super::macros::string_enum;
use serde::{Deserialize, Deserializer, Serialize};
use std::fmt;
use std::sync::Arc;

/// A moment in time as t3code writes it: an ISO-8601 UTC string, kept verbatim.
/// Two timestamps in t3code's own format (`2026-01-02T03:04:05.678Z`) order
/// chronologically.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Timestamp(Arc<str>);

impl Timestamp {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Timestamp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<&str> for Timestamp {
    fn from(value: &str) -> Self {
        Self(Arc::from(value))
    }
}

impl From<String> for Timestamp {
    fn from(value: String) -> Self {
        Self(Arc::from(value))
    }
}

/// Reads a field that may be absent or `null` into `Some(None)` for `null`,
/// leaving `None` (the field's default) for absent, so each is written back as
/// it came.
pub(crate) fn double_option<'de, T, D>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

/// Reads an open-shaped field that may be absent into `Some`, `null` included,
/// so an absent value and a `null` one are each written back as they came.
pub(crate) fn present<'de, T, D>(deserializer: D) -> Result<Option<T>, D::Error>
where
    T: Deserialize<'de>,
    D: Deserializer<'de>,
{
    T::deserialize(deserializer).map(Some)
}

string_enum! {
    /// Who caused a record to exist.
    Actor {
        User = "user",
        Agent = "agent",
        System = "system",
    }
}

string_enum! {
    /// The surface a thread or message was created from.
    CreationSource {
        Web = "web",
        Mobile = "mobile",
        Mcp = "mcp",
        Provider = "provider",
        Server = "server",
    }
}

string_enum! {
    /// How far the agent may act without asking the user.
    RuntimeMode {
        ApprovalRequired = "approval-required",
        AutoAcceptEdits = "auto-accept-edits",
        Auto = "auto",
        FullAccess = "full-access",
    }
}

string_enum! {
    /// Whether the agent works normally or only plans.
    InteractionMode {
        Default = "default",
        Plan = "plan",
    }
}

string_enum! {
    /// The access an agent in a linked environment was paired with.
    McpClientAccess {
        ReadOnly = "read-only",
        ApprovalRequired = "approval-required",
        AutoAcceptEdits = "auto-accept-edits",
        Auto = "auto",
        FullAccess = "full-access",
    }
}

string_enum! {
    /// How reliably a provider's native id identifies the thing it names.
    NativeRefStrength {
        Strong = "strong",
        Weak = "weak",
        None = "none",
    }
}

string_enum! {
    /// The kind of permission a provider asks the user for.
    ProviderRequestKind {
        Command = "command",
        FileRead = "file-read",
        FileChange = "file-change",
        McpElicitation = "mcp-elicitation",
        Permission = "permission",
    }
}

string_enum! {
    /// The user's answer to an approval request.
    ApprovalDecision {
        Accept = "accept",
        AcceptForSession = "acceptForSession",
        AcceptAlways = "acceptAlways",
        Decline = "decline",
        Cancel = "cancel",
    }
}

string_enum! {
    /// How a provider failure is classified for the user.
    ProviderFailureClass {
        UsageLimit = "usage_limit",
        ProviderError = "provider_error",
        TransportError = "transport_error",
        PermissionError = "permission_error",
        ValidationError = "validation_error",
        Unknown = "unknown",
    }
}

string_enum! {
    /// How a handoff summarised the runs its target provider thread did not see.
    HandoffStrategy {
        DeltaSinceTargetLastSeen = "delta_since_target_last_seen",
        ForkDeltaSummary = "fork_delta_summary",
        FullThreadSummary = "full_thread_summary",
        CheckpointSummary = "checkpoint_summary",
        ManualContext = "manual_context",
    }
}

string_enum! {
    /// Whether a subagent is the provider's own or a thread t3code runs for it.
    SubagentOrigin {
        ProviderNative = "provider_native",
        AppOwned = "app_owned",
    }
}

string_enum! {
    /// Why t3code compacted a thread on its own.
    IdleCompaction {
        BeforeCacheExpires = "beforeCacheExpires",
        OnReturn = "onReturn",
    }
}

/// A provider's native identifier for a thread, turn, item or request.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderRef {
    pub driver: ProviderDriverKind,
    pub native_id: Option<String>,
    pub strength: NativeRefStrength,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ordinal: Option<u64>,
}

/// The model a thread or run uses, with the options chosen for it.
///
/// This is the form the server writes. Its decoder also accepts a legacy
/// `provider` key in place of `instanceId` and an options object in place of
/// the list, and rewrites both, so neither appears in records it sends.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelSelection {
    pub instance_id: ProviderInstanceId,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<OptionSelection>>,
}

/// One provider option set on a model selection, such as reasoning effort.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OptionSelection {
    pub id: String,
    pub value: OptionValue,
}

/// The value of a provider option: a choice or a switch.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum OptionValue {
    Choice(String),
    Switch(bool),
}

/// The thread, in a linked environment, that sent a message or delegated work here.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkedThread {
    pub environment_id: EnvironmentId,
    pub thread_id: ThreadId,
    pub title: String,
}
