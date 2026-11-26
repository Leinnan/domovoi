//! A server's configuration and the streams that keep it current
//! (`server.ts`): its providers with their models and limits
//! (`model.ts`, `providerUsageLimits.ts`), the editors it can open
//! (`editor.ts`), an administrator's policy (`managedPolicy.ts`), and its
//! lifecycle events.

use super::common::{RuntimeMode, Timestamp, double_option};
use super::environment::ExecutionEnvironmentDescriptor;
use super::ids::{ProjectId, ProviderDriverKind, ProviderInstanceId, ThreadId};
use super::keybinding::ResolvedKeybindingsConfig;
use super::list::ForwardList;
use super::macros::{string_enum, tagged_union_deserialize};
use super::remote_access::ServerAuthDescriptor;
use super::settings::ServerSettings;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Number, Value};
use std::collections::BTreeMap;

// ---------------------------------------------------------------------------
// Config issues
// ---------------------------------------------------------------------------

/// Something wrong with the server's configuration files (`ServerConfigIssue`).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind")]
pub enum ServerConfigIssue {
    #[serde(rename = "keybindings.malformed-config")]
    KeybindingsMalformedConfig(KeybindingsMalformedConfigIssue),
    #[serde(rename = "keybindings.invalid-entry")]
    KeybindingsInvalidEntry(KeybindingsInvalidEntryIssue),
    /// An issue of a kind this build does not know, kept as it was sent.
    #[serde(untagged)]
    Other(Map<String, Value>),
}

tagged_union_deserialize! {
    ServerConfigIssue by "kind" {
        "keybindings.malformed-config" => KeybindingsMalformedConfig,
        "keybindings.invalid-entry" => KeybindingsInvalidEntry,
        _ => Other,
    }
}

/// The keybindings file could not be read as a list of rules (`KeybindingsMalformedConfigIssue`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeybindingsMalformedConfigIssue {
    pub message: String,
}

/// One rule of the keybindings file could not be read (`KeybindingsInvalidEntryIssue`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KeybindingsInvalidEntryIssue {
    pub message: String,
    /// The rule's place in the file. The Domovoi server counts the rules it could
    /// not read into one issue and always sends `0`.
    pub index: Number,
}

// ---------------------------------------------------------------------------
// Models and their options (`model.ts`)
// ---------------------------------------------------------------------------

/// One value a select option can take (`ProviderOptionChoice`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderOptionChoice {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_default: Option<bool>,
}

string_enum! {
    /// What a select option stands for when its id is the provider's own (`SelectProviderOptionDescriptor.role`).
    ProviderOptionRole {
        Reasoning = "reasoning",
    }
}

/// An option of a model chosen from a list, such as reasoning effort (`SelectProviderOptionDescriptor`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SelectProviderOptionDescriptor {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub options: Vec<ProviderOptionChoice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_value: Option<String>,
    /// The choices sent to the model as a word in the prompt instead of as a setting.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_injected_values: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<ProviderOptionRole>,
}

/// An option of a model that is on or off, such as fast mode (`BooleanProviderOptionDescriptor`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BooleanProviderOptionDescriptor {
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_value: Option<bool>,
}

/// An option a model offers (`ProviderOptionDescriptor`).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ProviderOptionDescriptor {
    Select(SelectProviderOptionDescriptor),
    Boolean(BooleanProviderOptionDescriptor),
    /// An option of a type this build does not know, kept as it was sent.
    #[serde(untagged)]
    Other(Map<String, Value>),
}

tagged_union_deserialize! {
    ProviderOptionDescriptor by "type" {
        "select" => Select,
        "boolean" => Boolean,
        _ => Other,
    }
}

impl ProviderOptionDescriptor {
    /// The option's id, which a model selection names it by.
    pub fn id(&self) -> Option<&str> {
        match self {
            Self::Select(option) => Some(&option.id),
            Self::Boolean(option) => Some(&option.id),
            Self::Other(fields) => fields.get("id").and_then(Value::as_str),
        }
    }

    /// The option's name as shown.
    pub fn label(&self) -> Option<&str> {
        match self {
            Self::Select(option) => Some(&option.label),
            Self::Boolean(option) => Some(&option.label),
            Self::Other(fields) => fields.get("label").and_then(Value::as_str),
        }
    }

    /// The choice a select option is set to, when the provider says.
    pub fn current_value(&self) -> Option<&str> {
        match self {
            Self::Select(option) => option.current_value.as_deref(),
            Self::Boolean(_) | Self::Other(_) => None,
        }
    }

    /// The choices of a select option; no other option has any.
    pub fn options(&self) -> &[ProviderOptionChoice] {
        match self {
            Self::Select(option) => &option.options,
            Self::Boolean(_) | Self::Other(_) => &[],
        }
    }
}

/// What can be set on a model (`ModelCapabilities`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCapabilities {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub option_descriptors: Option<Vec<ProviderOptionDescriptor>>,
}

// ---------------------------------------------------------------------------
// Providers
// ---------------------------------------------------------------------------

string_enum! {
    /// Whether a provider can be used (`ServerProviderState`).
    ServerProviderState {
        Ready = "ready",
        Warning = "warning",
        Error = "error",
        Disabled = "disabled",
    }
}

string_enum! {
    /// Whether a provider is signed in (`ServerProviderAuthStatus`).
    ServerProviderAuthStatus {
        Authenticated = "authenticated",
        Unauthenticated = "unauthenticated",
        Unknown = "unknown",
    }
}

/// A sign-in a provider is waiting for the user to finish in a browser (`AcpRegistryUrlAuthAction`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AcpRegistryUrlAuthAction {
    pub elicitation_id: String,
    pub url: String,
    pub message: String,
    /// A time as the agent wrote it, in no fixed format.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    /// A time as the agent wrote it, in no fixed format.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
}

/// The account a provider is signed in with (`ServerProviderAuth`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerProviderAuth {
    pub status: ServerProviderAuthStatus,
    /// The kind of account, in the provider's own words (`chatgpt`, `apiKey`, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<AcpRegistryUrlAuthAction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub can_logout: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subscription_sharing: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
}

string_enum! {
    /// A badge shown beside a model's name (`ServerProviderModel.badge`).
    ServerProviderModelBadge {
        New = "new",
    }
}

/// A model a provider offers (`ServerProviderModel`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerProviderModel {
    pub slug: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sub_provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aliases: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub badge: Option<ServerProviderModelBadge>,
    pub is_custom: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_default: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_legacy: Option<bool>,
    /// `false` when the model runs in sessions but cannot generate text for the application.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supports_text_generation: Option<bool>,
    pub capabilities: Option<ModelCapabilities>,
}

/// A model the installed provider is too old to run (`ServerProviderUpdateRequiredModel`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerProviderUpdateRequiredModel {
    pub slug: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub badge: Option<ServerProviderModelBadge>,
    pub min_version: String,
}

/// What a slash command takes after its name (`ServerProviderSlashCommandInput`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerProviderSlashCommandInput {
    pub hint: String,
}

/// A slash command a provider runs (`ServerProviderSlashCommand`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerProviderSlashCommand {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<ServerProviderSlashCommandInput>,
}

/// A skill a provider can use (`ServerProviderSkill`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerProviderSkill {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub path: String,
    /// Where the skill comes from, in the provider's own words (`repo`, `user`, `system`, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short_description: Option<String>,
    /// Only a user can invoke the skill, so a composer must offer it as a slash command.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_invocation_only: Option<bool>,
    /// `false` when only an agent can invoke the skill, so a composer must not offer it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_invocable: Option<bool>,
}

/// What a provider offers in one working directory (`ServerProviderWorkspaceSnapshot`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerProviderWorkspaceSnapshot {
    pub cwd: String,
    pub checked_at: Timestamp,
    pub slash_commands: Vec<ServerProviderSlashCommand>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub slash_commands_pending: Option<bool>,
    pub skills: Vec<ServerProviderSkill>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub option_descriptors: Option<Vec<ProviderOptionDescriptor>>,
    /// The provider's own `models` apply when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub models: Option<Vec<ServerProviderModel>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub models_pending: Option<bool>,
}

string_enum! {
    /// Whether a provider exists on the server's machine at all (`ServerProviderAvailability`).
    ServerProviderAvailability {
        Available = "available",
        Unavailable = "unavailable",
    }
}

/// Which providers can continue each other's conversations: those with the same key (`ServerProviderContinuation`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerProviderContinuation {
    pub group_key: String,
}

string_enum! {
    /// How well the server works with a version of a provider (`ServerProviderCompatibilityStatus`).
    ServerProviderCompatibilityStatus {
        Unknown = "unknown",
        Supported = "supported",
        Graceful = "graceful",
        Unsupported = "unsupported",
        Broken = "broken",
    }
}

/// Whether the installed version of a provider works with the server (`ServerProviderCompatibilityAdvisory`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerProviderCompatibilityAdvisory {
    pub status: ServerProviderCompatibilityStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_version_status: Option<ServerProviderCompatibilityStatus>,
    pub message: Option<String>,
    pub recommended_version: Option<String>,
    pub recommended_range: Option<String>,
}

string_enum! {
    /// Whether a provider is up to date (`ServerProviderVersionAdvisoryStatus`).
    ServerProviderVersionAdvisoryStatus {
        Unknown = "unknown",
        Current = "current",
        BehindLatest = "behind_latest",
    }
}

/// The installed version of a provider against the latest (`ServerProviderVersionAdvisory`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerProviderVersionAdvisory {
    pub status: ServerProviderVersionAdvisoryStatus,
    pub current_version: Option<String>,
    pub latest_version: Option<String>,
    pub update_command: Option<String>,
    /// The schema reads an absent flag as `false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub can_update: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub can_install_version: Option<bool>,
    pub checked_at: Option<Timestamp>,
    pub message: Option<String>,
}

string_enum! {
    /// How far an update of a provider has got (`ServerProviderUpdateStatus`).
    ServerProviderUpdateStatus {
        Idle = "idle",
        Queued = "queued",
        Running = "running",
        Succeeded = "succeeded",
        Failed = "failed",
        Unchanged = "unchanged",
    }
}

/// An update of a provider the server ran or is running (`ServerProviderUpdateState`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerProviderUpdateState {
    pub status: ServerProviderUpdateStatus,
    pub started_at: Option<Timestamp>,
    pub finished_at: Option<Timestamp>,
    pub message: Option<String>,
    pub output: Option<String>,
}

/// What the server can set up for a provider (`ServerProvider.setup`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerProviderSetup {
    pub can_authenticate: bool,
    pub can_install: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub documentation_url: Option<String>,
}

/// What the server can do with a provider's own sessions (`ServerProvider.nativeSessions`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerProviderNativeSessions {
    pub can_list: bool,
    pub can_load: bool,
    pub can_resume: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub can_delete: Option<bool>,
}

/// Where a provider keeps its state (`ServerProvider.runtimePaths`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerProviderRuntimePaths {
    pub home_path: String,
    pub shadow_home_path: Option<String>,
}

/// Defines a provider record: [`ServerProvider`] and [`ServerProviderDelta`]
/// have the same fields and differ in how three lists are read.
macro_rules! server_provider_record {
    (
        $(#[$meta:meta])*
        $name:ident {
            $(#[$models_meta:meta])*
            models: $models:ty,
            $(#[$slash_commands_meta:meta])*
            slash_commands,
            $(#[$skills_meta:meta])*
            skills,
        }
    ) => {
        $(#[$meta])*
        #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
        #[serde(rename_all = "camelCase")]
        pub struct $name {
            /// The one stable name of the provider: threads and model selections refer to it.
            pub instance_id: ProviderInstanceId,
            pub driver: ProviderDriverKind,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub display_name: Option<String>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub accent_color: Option<String>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub icon_url: Option<String>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub badge_label: Option<String>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub continuation: Option<ServerProviderContinuation>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub show_interaction_mode_toggle: Option<bool>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub reports_context_window: Option<bool>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub supported_runtime_modes: Option<ForwardList<RuntimeMode>>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub supports_conversation_rollback: Option<bool>,
            /// Only `false` says the provider cannot generate text for the application.
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub supports_text_generation: Option<bool>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub setup: Option<ServerProviderSetup>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub native_sessions: Option<ServerProviderNativeSessions>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub configurable_providers: Option<bool>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub runtime_paths: Option<ServerProviderRuntimePaths>,
            pub enabled: bool,
            pub installed: bool,
            pub version: Option<String>,
            /// The version the installed CLI reports, for a provider whose
            /// server is separate from it; `null` when unknown.
            #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
            pub installed_version: Option<Option<String>>,
            pub status: ServerProviderState,
            pub auth: ServerProviderAuth,
            pub checked_at: Timestamp,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub message: Option<String>,
            /// Available when absent.
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub availability: Option<ServerProviderAvailability>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub unavailable_reason: Option<String>,
            $(#[$models_meta])*
            pub models: $models,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub update_required_models: Option<Vec<ServerProviderUpdateRequiredModel>>,
            $(#[$slash_commands_meta])*
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub slash_commands: Option<Vec<ServerProviderSlashCommand>>,
            $(#[$skills_meta])*
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub skills: Option<Vec<ServerProviderSkill>>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub workspace_snapshots: Option<Vec<ServerProviderWorkspaceSnapshot>>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub usage_limits: Option<ServerProviderUsageLimits>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub version_advisory: Option<ServerProviderVersionAdvisory>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub compatibility_advisory: Option<ServerProviderCompatibilityAdvisory>,
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub update_state: Option<ServerProviderUpdateState>,
            /// Stands for the provider's own slash commands and skills, which
            /// are then left out, for a client that asked for digests.
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub catalog_digest: Option<String>,
            /// Set by a client and never sent: the digest its held lists belong to.
            #[serde(default, skip_serializing_if = "Option::is_none")]
            pub loaded_catalog_digest: Option<String>,
        }
    };
}

server_provider_record! {
    /// A provider instance and its state on the server (`ServerProvider`).
    ServerProvider {
        models: Vec<ServerProviderModel>,
        /// The schema reads an absent list as empty.
        slash_commands,
        /// The schema reads an absent list as empty.
        skills,
    }
}

server_provider_record! {
    /// A provider without the lists its reader already holds
    /// (`ServerProviderDelta`): an absent list is the one last received for
    /// the same `instance_id`.
    ServerProviderDelta {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        models: Option<Vec<ServerProviderModel>>,
        slash_commands,
        skills,
    }
}

// ---------------------------------------------------------------------------
// Usage limits (`providerUsageLimits.ts`)
// ---------------------------------------------------------------------------

string_enum! {
    /// The period a usage limit covers (`ServerProviderUsageWindow.kind`).
    ServerProviderUsageWindowKind {
        Session = "session",
        Weekly = "weekly",
        Monthly = "monthly",
        /// The literal `other`: a period t3code does not name.
        Unnamed = "other",
    }
}

/// One usage limit of a provider account and how much of it is used (`ServerProviderUsageWindow`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerProviderUsageWindow {
    /// Stable per provider, so a partial update can replace the window.
    pub id: String,
    pub kind: ServerProviderUsageWindowKind,
    pub label: String,
    /// The limit applies only to these models when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_ids: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_ids: Option<Vec<String>>,
    /// Between 0 and 100.
    pub used_percent: Number,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resets_at: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_duration_mins: Option<u64>,
}

/// One kind of banked reset an account can redeem (`ServerProviderResetCreditOption`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerProviderResetCreditOption {
    pub credit_id: String,
    pub label: String,
    pub description: String,
    pub available_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<Timestamp>,
}

/// The resets of its limits an account has banked (`ServerProviderResetCredits`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerProviderResetCredits {
    pub available_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_expires_at: Option<Timestamp>,
    /// The credit shown, so that redeeming it twice redeems one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_credit_id: Option<String>,
    /// Present when resets differ in what they refill.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<ForwardList<ServerProviderResetCreditOption>>,
}

/// Where an account's usage is shown outside the application (`ServerProviderUsageLimits.externalUsage`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerProviderExternalUsage {
    pub label: String,
    pub url: String,
}

string_enum! {
    /// Why a provider's usage limits could not be read (`ServerProviderUsageLimits.unavailable.reason`).
    ServerProviderUsageUnavailableReason {
        Unsupported = "unsupported",
        ProbeFailed = "probeFailed",
    }
}

/// That a provider's usage limits could not be read, and why (`ServerProviderUsageLimits.unavailable`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerProviderUsageUnavailable {
    pub reason: ServerProviderUsageUnavailableReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

/// The usage limits of a provider account as last read (`ServerProviderUsageLimits`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerProviderUsageLimits {
    pub checked_at: Timestamp,
    pub windows: ForwardList<ServerProviderUsageWindow>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reset_credits: Option<ServerProviderResetCredits>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub external_usage: Option<ServerProviderExternalUsage>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unavailable: Option<ServerProviderUsageUnavailable>,
}

/// One account a usage limit source reports on (`UsageLimitSourceAccount`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageLimitSourceAccount {
    pub id: String,
    pub driver: ProviderDriverKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    pub usage_limits: ServerProviderUsageLimits,
}

string_enum! {
    /// The kind of service a usage limit source is (`UsageLimitSourceSnapshot.kind`).
    UsageLimitSourceKind {
        Cliproxy = "cliproxy",
    }
}

/// The accounts of one usage limit source as last read (`UsageLimitSourceSnapshot`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageLimitSourceSnapshot {
    pub id: String,
    pub kind: UsageLimitSourceKind,
    pub label: String,
    pub checked_at: Timestamp,
    pub accounts: ForwardList<UsageLimitSourceAccount>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

// ---------------------------------------------------------------------------
// Editors (`editor.ts`)
// ---------------------------------------------------------------------------

string_enum! {
    /// An editor the server can open a path in (`EditorId`).
    EditorId {
        Cursor = "cursor",
        Trae = "trae",
        Kiro = "kiro",
        Vscode = "vscode",
        VscodeInsiders = "vscode-insiders",
        Vscodium = "vscodium",
        Zed = "zed",
        Antigravity = "antigravity",
        Idea = "idea",
        Aqua = "aqua",
        Clion = "clion",
        Datagrip = "datagrip",
        Dataspell = "dataspell",
        Goland = "goland",
        Phpstorm = "phpstorm",
        Pycharm = "pycharm",
        Rider = "rider",
        Rubymine = "rubymine",
        Rustrover = "rustrover",
        Webstorm = "webstorm",
        FileManager = "file-manager",
    }
}

// ---------------------------------------------------------------------------
// Observability
// ---------------------------------------------------------------------------

string_enum! {
    /// Where an OTLP endpoint was configured (`OtlpEndpointSource`).
    OtlpEndpointSource {
        Environment = "environment",
        Bootstrap = "bootstrap",
        Settings = "settings",
    }
}

/// Where the server writes its logs and sends its telemetry (`ServerObservability`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerObservability {
    pub logs_directory_path: String,
    pub local_tracing_enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub otlp_traces_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub otlp_traces_source: Option<OtlpEndpointSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub otlp_traces_variable: Option<String>,
    pub otlp_traces_enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub otlp_metrics_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub otlp_metrics_source: Option<OtlpEndpointSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub otlp_metrics_variable: Option<String>,
    pub otlp_metrics_enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub otlp_logs_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub otlp_logs_source: Option<OtlpEndpointSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub otlp_logs_variable: Option<String>,
    /// The schema reads an absent flag as `false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub otlp_logs_enabled: Option<bool>,
}

// ---------------------------------------------------------------------------
// Managed policy (`managedPolicy.ts`)
// ---------------------------------------------------------------------------

string_enum! {
    /// A kind of client a managed server may accept (`ManagedPolicyClient`).
    ManagedPolicyClient {
        Desktop = "desktop",
        LocalBrowser = "local-browser",
        Remote = "remote",
    }
}

string_enum! {
    /// Whether a managed server checks for its own updates (`ManagedUpdatesMode`).
    ManagedUpdatesMode {
        Auto = "auto",
        Off = "off",
    }
}

/// What a policy says about updates, as clients see it (`ManagedPolicySummaryLimits.updates`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedPolicySummaryUpdates {
    pub mode: ManagedUpdatesMode,
    /// `false` makes the nightly channel unavailable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_nightly: Option<bool>,
}

/// What a policy says about providers, as clients see it (`ManagedPolicySummaryLimits.providers`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedPolicySummaryProviders {
    /// Every driver is allowed when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_drivers: Option<Vec<ProviderDriverKind>>,
    /// Per driver, the instance fields users cannot change, as dot paths
    /// (`config.binaryPath`, `environment.<NAME>`, or `environment` for every variable).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locked_instance_fields: Option<BTreeMap<ProviderDriverKind, Vec<String>>>,
}

/// The limits a policy makes the server enforce (`ManagedPolicySummaryLimits`): an absent one is no limit.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedPolicySummaryLimits {
    /// The broadest runtime mode a thread may use.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_runtime_mode: Option<RuntimeMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clients: Option<Vec<ManagedPolicyClient>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connect: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhook_tasks: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub telemetry: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser_cookie_import: Option<bool>,
    /// `false` turns the preview tunnel off: no client reaches this machine's loopback ports.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preview_tunnel: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub providers: Option<ManagedPolicySummaryProviders>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updates: Option<ManagedPolicySummaryUpdates>,
}

/// What clients learn of an administrator's policy (`ManagedPolicySummary`):
/// enough to explain a locked control.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagedPolicySummary {
    pub organization: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub support_url: Option<String>,
    /// Dot paths into the server's settings that users cannot change.
    pub locked_settings: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits: Option<ManagedPolicySummaryLimits>,
}

// ---------------------------------------------------------------------------
// Themes, endpoints, stalls
// ---------------------------------------------------------------------------

string_enum! {
    /// Whether a theme is light or dark (`EnvironmentTheme.appearance`).
    EnvironmentThemeAppearance {
        Light = "light",
        Dark = "dark",
    }
}

/// The colors a theme overrides in each appearance (`EnvironmentTheme.variants`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentThemeVariants {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub light: Option<BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dark: Option<BTreeMap<String, String>>,
}

/// A color theme the server publishes to its clients (`EnvironmentTheme`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvironmentTheme {
    pub id: String,
    /// Always 1 when present.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<u64>,
    pub name: String,
    pub appearance: EnvironmentThemeAppearance,
    /// With `accent`, the color the palette is derived from, as `#rgb` or `#rrggbb`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canvas: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accent: Option<String>,
    /// Colors by token name, laid over the derived palette, or the whole palette without one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colors: Option<BTreeMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variants: Option<EnvironmentThemeVariants>,
}

// ---------------------------------------------------------------------------
// The config and its stream
// ---------------------------------------------------------------------------

/// Everything a client needs to know about a server to work with it (`ServerConfig`).
pub type ServerConfig = ServerConfigOf<ServerSettings>;

/// [`ServerConfig`] with its settings held as `S`. A client reads them as
/// [`ServerSettings`]; the Domovoi server writes its own settings type, which also
/// carries what its settings file holds that the record does not read.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerConfigOf<S> {
    pub environment: ExecutionEnvironmentDescriptor,
    pub auth: ServerAuthDescriptor,
    pub cwd: String,
    pub keybindings_config_path: String,
    pub keybindings: ResolvedKeybindingsConfig,
    pub issues: ForwardList<ServerConfigIssue>,
    pub providers: ForwardList<ServerProvider>,
    /// Domovoi's live and saved terminal metadata.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_details: Option<bool>,
    /// Supports opening a bounded transcript and loading its earlier pages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_windows: Option<bool>,
    pub observability: ServerObservability,
    pub settings: S,
    /// Present when an administrator's policy manages the server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub managed_policy: Option<ManagedPolicySummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_projects_root: Option<String>,
    /// Never sent in a snapshot: themes arrive as their own stream event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_themes: Option<Vec<EnvironmentTheme>>,
    /// Never sent in a snapshot: sources arrive as their own stream event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_limit_sources: Option<ForwardList<UsageLimitSourceSnapshot>>,
}

/// The keybindings after a change to them (`ServerConfigKeybindingsUpdatedPayload`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerConfigKeybindingsUpdatedPayload {
    pub keybindings: ResolvedKeybindingsConfig,
    pub issues: ForwardList<ServerConfigIssue>,
}

/// Every provider after a change to one (`ServerConfigProviderStatusesPayload`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerConfigProviderStatusesPayload {
    pub providers: ForwardList<ServerProvider>,
}

/// Every provider in order, each without the lists the client already holds (`ServerConfigProviderStatusDeltasPayload`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerConfigProviderStatusDeltasPayload {
    pub providers: ForwardList<ServerProviderDelta>,
}

/// The settings after a change to them (`ServerConfigSettingsUpdatedPayload`).
pub type ServerConfigSettingsUpdatedPayload = ServerConfigSettingsUpdatedPayloadOf<ServerSettings>;

/// [`ServerConfigSettingsUpdatedPayload`] with the settings held as `S` (see [`ServerConfigOf`]).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerConfigSettingsUpdatedPayloadOf<S> {
    pub settings: S,
}

/// The themes the server publishes (`ServerConfigEnvironmentThemesUpdatedPayload`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerConfigEnvironmentThemesUpdatedPayload {
    pub themes: Vec<EnvironmentTheme>,
}

/// The usage limit sources as last read (`ServerConfigUsageLimitSourcesUpdatedPayload`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerConfigUsageLimitSourcesUpdatedPayload {
    pub sources: ForwardList<UsageLimitSourceSnapshot>,
}

/// The whole config, which a subscription starts with (`ServerConfigStreamSnapshotEvent`).
pub type ServerConfigStreamSnapshotEvent = ServerConfigStreamSnapshotEventOf<ServerSettings>;

/// [`ServerConfigStreamSnapshotEvent`] with the settings held as `S` (see [`ServerConfigOf`]).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerConfigStreamSnapshotEventOf<S> {
    /// Always 1.
    pub version: u64,
    pub config: ServerConfigOf<S>,
}

/// Defines a config stream event that carries a payload.
macro_rules! config_stream_event {
    ($(#[$meta:meta])* $name:ident($payload:ty)) => {
        $(#[$meta])*
        #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
        #[serde(rename_all = "camelCase")]
        pub struct $name {
            /// Always 1.
            pub version: u64,
            pub payload: $payload,
        }
    };
}

config_stream_event! {
    /// The keybindings changed (`ServerConfigStreamKeybindingsUpdatedEvent`).
    ServerConfigStreamKeybindingsUpdatedEvent(ServerConfigKeybindingsUpdatedPayload)
}

config_stream_event! {
    /// A provider changed (`ServerConfigStreamProviderStatusesEvent`).
    ServerConfigStreamProviderStatusesEvent(ServerConfigProviderStatusesPayload)
}

config_stream_event! {
    /// A provider changed, told to a client that asked for deltas (`ServerConfigStreamProviderStatusDeltasEvent`).
    ServerConfigStreamProviderStatusDeltasEvent(ServerConfigProviderStatusDeltasPayload)
}

/// The settings changed (`ServerConfigStreamSettingsUpdatedEvent`).
pub type ServerConfigStreamSettingsUpdatedEvent = ServerConfigStreamSettingsUpdatedEventOf<ServerSettings>;

/// [`ServerConfigStreamSettingsUpdatedEvent`] with the settings held as `S` (see [`ServerConfigOf`]).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerConfigStreamSettingsUpdatedEventOf<S> {
    /// Always 1.
    pub version: u64,
    pub payload: ServerConfigSettingsUpdatedPayloadOf<S>,
}

config_stream_event! {
    /// The published themes, sent on subscribing and when they change (`ServerConfigStreamEnvironmentThemesUpdatedEvent`).
    ServerConfigStreamEnvironmentThemesUpdatedEvent(ServerConfigEnvironmentThemesUpdatedPayload)
}

config_stream_event! {
    /// The usage limit sources, sent on subscribing and when they change (`ServerConfigStreamUsageLimitSourcesUpdatedEvent`).
    ServerConfigStreamUsageLimitSourcesUpdatedEvent(ServerConfigUsageLimitSourcesUpdatedPayload)
}

/// One item of a subscription to the server's config (`ServerConfigStreamEvent`, `subscribeServerConfig`).
pub type ServerConfigStreamEvent = ServerConfigStreamEventOf<ServerSettings>;

/// [`ServerConfigStreamEvent`] with the settings held as `S` (see [`ServerConfigOf`]).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ServerConfigStreamEventOf<S> {
    Snapshot(Box<ServerConfigStreamSnapshotEventOf<S>>),
    KeybindingsUpdated(ServerConfigStreamKeybindingsUpdatedEvent),
    ProviderStatuses(ServerConfigStreamProviderStatusesEvent),
    ProviderStatusDeltas(ServerConfigStreamProviderStatusDeltasEvent),
    SettingsUpdated(Box<ServerConfigStreamSettingsUpdatedEventOf<S>>),
    EnvironmentThemesUpdated(ServerConfigStreamEnvironmentThemesUpdatedEvent),
    UsageLimitSourcesUpdated(ServerConfigStreamUsageLimitSourcesUpdatedEvent),
    /// An event of a type this build does not know, kept as it was sent.
    #[serde(untagged)]
    Other(Map<String, Value>),
}

tagged_union_deserialize! {
    ServerConfigStreamEvent by "type" {
        "snapshot" => Snapshot,
        "keybindingsUpdated" => KeybindingsUpdated,
        "providerStatuses" => ProviderStatuses,
        "providerStatusDeltas" => ProviderStatusDeltas,
        "settingsUpdated" => SettingsUpdated,
        "environmentThemesUpdated" => EnvironmentThemesUpdated,
        "usageLimitSourcesUpdated" => UsageLimitSourcesUpdated,
        _ => Other,
    }
}

// ---------------------------------------------------------------------------
// Lifecycle
// ---------------------------------------------------------------------------

string_enum! {
    /// How an update of the server itself ended (`ServerSelfUpdateOutcome.status`).
    ServerSelfUpdateOutcomeStatus {
        Committed = "committed",
        RolledBack = "rolled-back",
        Failed = "failed",
    }
}

/// The end of an update of the server itself (`ServerSelfUpdateOutcome`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerSelfUpdateOutcome {
    pub id: String,
    pub from_version: String,
    pub target_version: String,
    pub status: ServerSelfUpdateOutcomeStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// That the server is ready for work, and since when (`ServerLifecycleReadyPayload`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerLifecycleReadyPayload {
    pub at: Timestamp,
    pub environment: ExecutionEnvironmentDescriptor,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update_outcome: Option<ServerSelfUpdateOutcome>,
}

string_enum! {
    /// Whether the server has made the project and thread it starts with (`ServerLifecycleWelcomePayload.bootstrapStatus`).
    ServerBootstrapStatus {
        Pending = "pending",
        Complete = "complete",
    }
}

/// What the server greets a client with (`ServerLifecycleWelcomePayload`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerLifecycleWelcomePayload {
    pub environment: ExecutionEnvironmentDescriptor,
    pub cwd: String,
    pub project_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bootstrap_status: Option<ServerBootstrapStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bootstrap_project_id: Option<ProjectId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bootstrap_thread_id: Option<ThreadId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bootstrap_project_created: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bootstrap_thread_created: Option<bool>,
}

string_enum! {
    /// Whether threads of an older format are still being converted (`ServerLifecycleLegacyThreadMigrationPayload.status`).
    LegacyThreadMigrationStatus {
        Running = "running",
        Complete = "complete",
    }
}

/// The conversion of threads of an older format (`ServerLifecycleLegacyThreadMigrationPayload`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerLifecycleLegacyThreadMigrationPayload {
    pub status: LegacyThreadMigrationStatus,
    pub total_thread_count: u64,
}

/// Defines a lifecycle stream event: each has a place in the stream and a payload.
macro_rules! lifecycle_stream_event {
    ($(#[$meta:meta])* $name:ident($payload:ty)) => {
        $(#[$meta])*
        #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
        #[serde(rename_all = "camelCase")]
        pub struct $name {
            /// Always 1.
            pub version: u64,
            pub sequence: u64,
            pub payload: $payload,
        }
    };
}

lifecycle_stream_event! {
    /// The first event of a subscription (`ServerLifecycleStreamWelcomeEvent`).
    ServerLifecycleStreamWelcomeEvent(ServerLifecycleWelcomePayload)
}

lifecycle_stream_event! {
    /// The server is ready for work (`ServerLifecycleStreamReadyEvent`).
    ServerLifecycleStreamReadyEvent(ServerLifecycleReadyPayload)
}

lifecycle_stream_event! {
    /// Progress of converting threads of an older format (`ServerLifecycleStreamLegacyThreadMigrationEvent`).
    ServerLifecycleStreamLegacyThreadMigrationEvent(ServerLifecycleLegacyThreadMigrationPayload)
}

/// One item of a subscription to the server's lifecycle (`ServerLifecycleStreamEvent`, `subscribeServerLifecycle`).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ServerLifecycleStreamEvent {
    Welcome(Box<ServerLifecycleStreamWelcomeEvent>),
    Ready(Box<ServerLifecycleStreamReadyEvent>),
    LegacyThreadMigration(ServerLifecycleStreamLegacyThreadMigrationEvent),
    /// An event of a type this build does not know, kept as it was sent.
    #[serde(untagged)]
    Other(Map<String, Value>),
}

tagged_union_deserialize! {
    ServerLifecycleStreamEvent by "type" {
        "welcome" => Welcome,
        "ready" => Ready,
        "legacyThreadMigration" => LegacyThreadMigration,
        _ => Other,
    }
}

// ---------------------------------------------------------------------------
// RPC results
// ---------------------------------------------------------------------------

/// The providers after the server probed them again (`ServerProviderUpdatedPayload`):
/// the result of `server.refreshProviders`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerProviderUpdatedPayload {
    pub providers: ForwardList<ServerProvider>,
}
