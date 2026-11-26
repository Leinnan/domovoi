//! The server's settings (`settings.ts`): what `subscribeServerConfig` carries
//! as `config.settings` and `payload.settings`, and what `server.updateSettings`
//! answers with.
//!
//! Almost every setting has a decoding default in the schema, so a server may
//! leave it out. Such a field is optional here and stays absent when it was not
//! sent; its doc comment gives the default a reader applies.

use super::common::{ModelSelection, OptionSelection, RuntimeMode, double_option, present};
use super::environment::{EnvironmentMachineKind, ThreadEnvMode, WorktreeSubmodules};
use super::ids::{ProjectId, ProviderDriverKind, ProviderInstanceId};
use super::list::ForwardList;
use super::macros::string_enum;
use super::pages::UsageModelFilter;
use super::project::ProjectScript;
use super::source_control::PullRequestMergeMethod;
use serde::de::DeserializeOwned;
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Number, Value};
use std::collections::BTreeMap;
use std::ops::Deref;

/// Everything a server lets its user configure (`ServerSettings`).
///
/// The default value is settings of which nothing was sent.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerSettings {
    /// The models the user starred. Defaults to `null`, which means the list was never set; an empty list is a choice.
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub favorites: Option<Option<Vec<ModelFavorite>>>,
    /// Hidden models and model order, by provider instance. Defaults to `null`, which means never set.
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub provider_model_preferences: Option<Option<BTreeMap<ProviderInstanceId, ProviderModelPreference>>>,
    /// Defaults to `null`, which follows `storageCleanup`.
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub worktree_cleanup: Option<Option<WorktreeCleanup>>,
    /// Defaults to a `StorageCleanupSettings` with every field at its own default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage_cleanup: Option<StorageCleanupSettings>,

    /// Defaults to `""`, which means the server's own worktrees directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktrees_directory: Option<String>,
    /// Defaults to `false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fast_worktree_checkout: Option<bool>,
    /// Earlier values of `worktreesDirectory`, kept by the server. Defaults to `[]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_worktrees_directories: Option<Vec<String>>,
    /// Defaults to `paragraph`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_streaming_mode: Option<ResponseStreamingMode>,
    /// Defaults to `true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enable_provider_update_checks: Option<bool>,
    /// Defaults to `true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continue_threads_after_server_update: Option<bool>,
    /// Domovoi's off-by-default retention of an inert terminal screen across restarts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub save_terminal_screens: Option<bool>,
    /// Defaults to `true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_awake_while_agents_work: Option<bool>,
    /// New T3 Connect device keys wait for approval from someone already trusted. Defaults to `false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub require_connect_device_approval: Option<bool>,

    /// Defaults to `true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enable_agent_browser_access: Option<bool>,
    /// Lets clients reach this machine's loopback ports over `/api/preview/tunnel`. Defaults to `true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enable_preview_tunnel: Option<bool>,
    /// Defaults to three empty credentials.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cloud_speech_credentials: Option<CloudSpeechCredentials>,
    /// Defaults to `handy-computer/parakeet-unified-en-0.6b-gguf`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speech_model_id: Option<String>,
    /// `auto`, `cpu` or `gpu:<device>` (`SpeechAcceleration`). Defaults to `auto`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speech_acceleration: Option<String>,
    /// Defaults to `min_15`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speech_model_unload_timeout: Option<SpeechModelUnloadTimeout>,
    /// Defaults to `[]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speech_project_custom_words: Option<Vec<SpeechCustomWord>>,
    /// Defaults to `null`, which follows `textGenerationModelSelection`.
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub speech_post_processing_model_selection: Option<Option<ModelSelection>>,
    /// Defaults to `{}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_agent_browser_access_overrides: Option<BTreeMap<ProjectId, bool>>,
    /// Defaults to `false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_auto_pull: Option<bool>,
    /// Defaults to `[]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_project_scripts: Option<Vec<ProjectScript>>,
    /// A project's own scripts, or `null` for a project that uses none. Defaults to `{}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_script_overrides: Option<BTreeMap<ProjectId, Option<Vec<ProjectScript>>>>,
    /// Defaults to `{}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_auto_pull_overrides: Option<BTreeMap<ProjectId, bool>>,
    /// Defaults to `null`.
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub default_model_selection: Option<Option<ModelSelection>>,
    /// Defaults to `null`.
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub fallback_model_selection: Option<Option<ModelSelection>>,
    /// Defaults to `full-access`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_runtime_mode: Option<RuntimeMode>,

    /// What each project sets differently from the rest of these settings. Defaults to `{}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_settings_overrides: Option<BTreeMap<ProjectId, ProjectSettingsOverrideEntry>>,
    /// Defaults to `false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_settings_folded: Option<bool>,

    /// Defaults to `false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enable_agent_device_access: Option<bool>,
    /// Defaults to `false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enable_device_support: Option<bool>,
    /// Defaults to `false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_onboarding_completed: Option<bool>,
    /// Defaults to `[]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_hosts: Option<Vec<SshDeviceHostConfig>>,
    /// Defaults to `[]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eas_device_hosts: Option<Vec<EasDeviceHostConfig>>,
    /// Defaults to `[]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub simlock_device_hosts: Option<Vec<SimlockDeviceHostConfig>>,
    /// Defaults to `false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub simlock_device_hosts_enabled: Option<bool>,
    /// Days from 1 to 90, or `null` for never. Defaults to `7`.
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub sidebar_auto_settle_after_days: Option<Option<Number>>,
    /// Defaults to `false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compact_idle_threads: Option<CompactIdleThreads>,
    /// When idle threads are compacted; `compactIdleThreads` is its older spelling.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_compaction: Option<IdleCompactionMode>,
    /// Defaults to `false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snooze_limited_threads: Option<bool>,
    /// Defaults to `false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_resume_limited_threads: Option<bool>,
    /// At most eight. Defaults to: after 60 minutes, after 180 minutes, today at 18:00, tomorrow at 09:00, Monday at 09:00.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snooze_presets: Option<Vec<SnoozePresetSetting>>,
    /// Tags whose threads clients hide; an entry that is not a string is left out when read. Defaults to `[]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hidden_thread_tags: Option<ForwardList<String>>,
    /// Defaults to `false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hide_tagged_threads: Option<bool>,
    /// Defaults to `false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guest_sharing_enabled: Option<bool>,
    /// Defaults to `false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linked_environments_enabled: Option<bool>,
    /// Defaults to `false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_suggestions_enabled: Option<bool>,
    /// Defaults to `false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sidebar_auto_settle_keep_pull_requests: Option<bool>,
    /// Defaults to `true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sidebar_auto_settle_on_merge: Option<bool>,
    /// Defaults to a `BackgroundActivitySettings` with every field at its own default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background_activity: Option<BackgroundActivitySettings>,

    /// Milliseconds. Defaults to `30000`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub automatic_git_fetch_interval: Option<Number>,
    /// Milliseconds. Defaults to `300000`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_health_refresh_interval: Option<Number>,
    /// Defaults to `balanced`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub background_activity_profile: Option<BackgroundActivityProfile>,
    /// The theme clients take up once each time it is set. Defaults to `""`, which sets none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_theme: Option<String>,
    /// Tells two sets of the same theme apart. Defaults to `""`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_theme_set_at: Option<String>,
    /// The appearance clients apply once each time it is set (`DefaultAppearancePreference`).
    /// Defaults to `""`, which sets none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_appearance: Option<DefaultAppearance>,
    /// Tells two sets of the same appearance apart. Defaults to `""`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_appearance_set_at: Option<String>,
    /// Defaults to `null`; a kind this build does not know reads as `null` too.
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub environment_icon: Option<Option<EnvironmentMachineKind>>,
    /// Absent inherits the project's own file, then `local`; a mode this build does not know reads as absent.
    /// The TypeScript server never writes `null` here; the Domovoi server keeps the `null` a patch set.
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub default_thread_env_mode: Option<Option<ThreadEnvMode>>,
    /// Defaults to `true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_worktrees_start_from_origin: Option<bool>,
    /// Defaults to `null`, which inherits the project's own file, then `recursive`; a value this build does not know reads as `null` too.
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub worktree_submodules: Option<Option<WorktreeSubmodules>>,
    /// Defaults to `""`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub add_project_base_directory: Option<String>,
    /// Defaults to `false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ghq_clone_layout: Option<bool>,
    /// Defaults to instance `codex`, model `gpt-6-luna`, option `reasoningEffort` set to `low`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_generation_model_selection: Option<TextGenerationModelSelection>,
    /// Defaults to `""`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_title_instructions: Option<String>,
    /// Defaults to `static`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_naming_mode: Option<BranchNamingMode>,
    /// Defaults to `t3`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_name_prefix: Option<String>,
    /// Defaults to `""`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_name_instructions: Option<String>,
    /// Defaults to `false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remove_agent_credits_on_merge: Option<bool>,
    /// Defaults to a `SourceControlWritingStyleSettings` with every field at its own default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_control_writing_style: Option<SourceControlWritingStyle>,
    /// Defaults to `null`.
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub source_control_writer_model_selection: Option<Option<ModelSelection>>,
    /// Defaults to `false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_guides_enabled: Option<bool>,
    /// Defaults to `null`.
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub review_guide_model_selection: Option<Option<ModelSelection>>,
    /// Defaults to `""`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_guide_instructions: Option<String>,

    /// Defaults to `null`.
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub pull_request_merge_method: Option<Option<PullRequestMergeMethod>>,

    /// Defaults to `{}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_instances: Option<BTreeMap<ProviderInstanceId, ProviderInstanceConfig>>,
    /// Defaults to three empty addresses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub observability: Option<ObservabilitySettings>,
    /// What each source control provider is configured with, by provider kind (`github`, `bitbucket`, …); the settings
    /// are the provider's own, so each is a free-form record (`Schema.Record(Schema.String, Schema.Unknown)`). Defaults to `{}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_control_hosts: Option<BTreeMap<String, Map<String, Value>>>,
    /// Whether the project pickers offer a host, by source control provider kind. Defaults to `{}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offered_source_control_providers: Option<BTreeMap<String, bool>>,
    /// By the id the user gave each source (`UsageLimitSourceId`). Defaults to `{}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_limit_sources: Option<BTreeMap<String, UsageLimitSourceConfig>>,
    /// By an id that shares its namespace with provider instances. Defaults to `{}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_generators: Option<BTreeMap<ProviderInstanceId, TextGeneratorConfig>>,
    /// By secret provider id (`SecretProviderId`). Defaults to `{}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_providers: Option<BTreeMap<String, SecretProviderConfig>>,

    /// How long token usage history is kept. Defaults to 92 days.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_history_retention_days: Option<UsageHistoryRetentionDays>,

    /// By model id. Defaults to `{}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_price_overrides: Option<BTreeMap<String, UsageModelPriceOverride>>,

    /// The model id each model id is counted as. Defaults to `{}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_model_aliases: Option<BTreeMap<String, String>>,

    /// Which models the Usage tab counts; a value the schema does not accept is ignored (`ForwardCompatibleOptional`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_model_filter: Option<Lenient<UsageModelFilter>>,

    // ---- Domovoi AI code review, failover pools & PR watcher ----

    /// Priority-ordered provider-instance failover pools.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failover_pools: Option<Vec<crate::typed::FailoverPoolConfig>>,
    /// Opt-in: hand off interactive threads to a healthy fallback instance on usage limit instead of waiting for reset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failover_limited_threads: Option<bool>,
    /// Enable evidence-checked AI PR reviews.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai_review_enabled: Option<bool>,
    /// Model selection for AI PR reviews; falls back to `reviewGuideModelSelection`, then `textGenerationModelSelection`.
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub ai_review_model_selection: Option<Option<ModelSelection>>,
    /// `auto` | `economic` | `deep`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai_review_depth_mode: Option<String>,
    /// Max reviewable files per chunk (clamped 2..=20, default 8).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai_review_chunk_size: Option<u32>,
    /// Max concurrent review chunks (clamped 1..=8, default 3).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai_review_parallel_chunks: Option<u32>,
    /// Run optional second-pass adversarial Lens review over primary chunk findings.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai_review_lens_enabled: Option<bool>,
    /// Additional global instructions merged with repository convention docs (`REVIEW.md`, `AGENTS.md`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai_review_instructions: Option<String>,
    /// Enable background GitHub/GitLab PR watcher polling (for `watch_pull_request` and auto-reviews).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_watcher_enabled: Option<bool>,
    /// PR watcher poll interval in seconds (default 60, minimum 15).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_watcher_interval_secs: Option<u64>,
}

/// A value read the way `ForwardCompatibleOptional` reads one: a value that
/// does not fit its schema is ignored by readers, and kept as it was sent.
#[derive(Clone, Debug, PartialEq)]
pub enum Lenient<T> {
    Read(T),
    /// A value this build cannot read, kept as it was sent.
    Other(Value),
}

impl<T> Lenient<T> {
    /// The value, unless it could not be read.
    pub fn read(&self) -> Option<&T> {
        match self {
            Self::Read(value) => Some(value),
            Self::Other(_) => None,
        }
    }
}

impl<T: Serialize> Serialize for Lenient<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Read(value) => value.serialize(serializer),
            Self::Other(sent) => sent.serialize(serializer),
        }
    }
}

impl<'de, T: DeserializeOwned> Deserialize<'de> for Lenient<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let sent = Value::deserialize(deserializer)?;
        Ok(match T::deserialize(&sent) {
            Ok(value) => Self::Read(value),
            Err(_) => Self::Other(sent),
        })
    }
}

/// The model that writes titles, commit messages and the like: the
/// `textGenerationModelSelection` setting, a `ModelSelection`.
///
/// The schema requires `instanceId` and `model`, and the TypeScript server
/// always sends both. The Domovoi server stores a patch of this setting as it was
/// sent, and a patch may name only what it changes (`ModelSelectionPatch`), so
/// from it either may be missing.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextGenerationModelSelection {
    /// Required by the schema; the Domovoi server leaves it out after a patch that did not name it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance_id: Option<ProviderInstanceId>,
    /// Required by the schema; the Domovoi server leaves it out after a patch that did not name it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<OptionSelection>>,
}

impl TextGenerationModelSelection {
    /// The selection, when it names both its instance and its model.
    pub fn selection(&self) -> Option<ModelSelection> {
        Some(ModelSelection { instance_id: self.instance_id.clone()?, model: self.model.clone()?, options: self.options.clone() })
    }
}

/// A model the user starred (`ModelFavorite`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelFavorite {
    pub provider: ProviderInstanceId,
    pub model: String,
}

/// The models of one provider instance the user hid, and the order the user put its models in (an entry of `ProviderModelPreferences`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderModelPreference {
    /// Defaults to `[]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hidden_models: Option<Vec<String>>,
    /// Defaults to `[]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_order: Option<Vec<String>>,
}

string_enum! {
    /// What in a worktree keeps it from being cleaned up (`WorktreeKeepWhen`).
    WorktreeKeepWhen {
        AnyLocalFiles = "any-local-files",
        UncommittedChanges = "uncommitted-changes",
        TrackedChanges = "tracked-changes",
    }
}

/// When the worktrees of settled, merged or deleted threads are removed (`WorktreeCleanupRules`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorktreeCleanupRules {
    /// Defaults to `uncommitted-changes`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_keep_when: Option<WorktreeKeepWhen>,
    /// Days from 1 to 3650, or `null` for never. Required by the schema; the Domovoi server leaves out a rule no patch has set.
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub worktree_after_days: Option<Option<u64>>,
    /// Days from 0 to 3650, or `null` for never. Defaults to `null`.
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub worktree_settled_after_days: Option<Option<u64>>,
    /// Required by the schema; the Domovoi server leaves out a rule no patch has set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_on_merge: Option<bool>,
    /// Required by the schema; the Domovoi server leaves out a rule no patch has set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_on_delete: Option<bool>,
    /// Required by the schema; the Domovoi server leaves out a rule no patch has set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_unchanged: Option<bool>,
}

/// Whether worktrees are cleaned up by rules of their own rather than by `storageCleanup` (`WorktreeCleanup`).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "mode", rename_all = "lowercase")]
pub enum WorktreeCleanup {
    Off,
    Custom {
        rules: WorktreeCleanupRules,
    },
    /// A mode this build does not know, kept as it was sent.
    #[serde(untagged)]
    Other(Map<String, Value>),
}

impl<'de> Deserialize<'de> for WorktreeCleanup {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error;

        #[derive(Deserialize)]
        struct Custom {
            rules: WorktreeCleanupRules,
        }

        let fields = Map::<String, Value>::deserialize(deserializer)?;
        match tag_of::<D::Error>(&fields, "mode")? {
            "off" => Ok(Self::Off),
            "custom" => Custom::deserialize(Value::Object(fields))
                .map(|custom| Self::Custom { rules: custom.rules })
                .map_err(|error| D::Error::custom(format_args!("WorktreeCleanup `custom`: {error}"))),
            _ => Ok(Self::Other(fields)),
        }
    }
}

/// The string a union's member is named by.
fn tag_of<'a, E: serde::de::Error>(fields: &'a Map<String, Value>, field: &'static str) -> Result<&'a str, E> {
    let tag = fields.get(field).ok_or_else(|| E::missing_field(field))?;
    tag.as_str().ok_or_else(|| E::custom(format_args!("`{field}` is not a string")))
}

/// What the server removes on its own to free disk space, and when (`StorageCleanupSettings`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageCleanupSettings {
    /// Defaults to `uncommitted-changes`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_keep_when: Option<WorktreeKeepWhen>,
    /// Days from 1 to 3650, or `null` for never. Defaults to `null`.
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub worktree_after_days: Option<Option<u64>>,
    /// Days from 0 to 3650, or `null` for never. Defaults to `null`.
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub worktree_settled_after_days: Option<Option<u64>>,
    /// Defaults to `false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_on_merge: Option<bool>,
    /// Defaults to `false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_on_delete: Option<bool>,
    /// Defaults to `false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_unchanged: Option<bool>,
    /// Days from 1 to 3650, or `null` for never. Defaults to `null`.
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub browser_artifacts_after_days: Option<Option<u64>>,
    /// Days from 1 to 3650, or `null` for never. Defaults to `null`.
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub logs_after_days: Option<Option<u64>>,
}

string_enum! {
    /// How much of an answer is sent at a time while the agent writes it (`ResponseStreamingMode`).
    ResponseStreamingMode {
        Turn = "turn",
        Paragraph = "paragraph",
    }
}

string_enum! {
    /// The appearance clients apply once per set, like the default theme (`DefaultAppearancePreference`).
    DefaultAppearance {
        Unset = "",
        Light = "light",
        Dark = "dark",
        System = "system",
    }
}

/// How long token usage history is kept (`UsageHistoryRetentionDays`): a
/// positive number of days, or `unlimited`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsageHistoryRetentionDays {
    Days(u32),
    Unlimited,
}

impl UsageHistoryRetentionDays {
    /// t3code's default.
    pub const DEFAULT: UsageHistoryRetentionDays = UsageHistoryRetentionDays::Days(92);
}

impl Serialize for UsageHistoryRetentionDays {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            UsageHistoryRetentionDays::Days(days) => serializer.serialize_u32(*days),
            UsageHistoryRetentionDays::Unlimited => serializer.serialize_str("unlimited"),
        }
    }
}

impl<'de> Deserialize<'de> for UsageHistoryRetentionDays {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Wire {
            Days(u32),
            Text(String),
        }
        match Wire::deserialize(deserializer)? {
            Wire::Days(0) => Err(serde::de::Error::custom("usageHistoryRetentionDays must be greater than 0")),
            Wire::Days(days) => Ok(UsageHistoryRetentionDays::Days(days)),
            Wire::Text(text) if text == "unlimited" => Ok(UsageHistoryRetentionDays::Unlimited),
            Wire::Text(text) => Err(serde::de::Error::custom(format!("unknown usageHistoryRetentionDays `{text}`"))),
        }
    }
}

/// A secret held by a configured secret provider, named rather than stored (`SecretReference`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SecretReference {
    /// The id of a secret provider in `secretProviders`.
    pub provider: String,
    pub reference: String,
}

/// A credential setting (`SettingsSecretValue`): the credential itself, which
/// the TypeScript server sends to clients emptied, or where to read it from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SettingsSecretValue {
    Literal(String),
    Reference(SecretReference),
}

/// The keys of the cloud services that transcribe speech (`CloudSpeechCredentials`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudSpeechCredentials {
    /// Defaults to `""`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub openai: Option<SettingsSecretValue>,
    /// Defaults to `""`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub xai: Option<SettingsSecretValue>,
    /// Defaults to `""`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub openrouter: Option<SettingsSecretValue>,
}

string_enum! {
    /// How long an unused speech model stays loaded (`SpeechModelUnloadTimeout`).
    SpeechModelUnloadTimeout {
        Never = "never",
        Immediately = "immediately",
        Min2 = "min_2",
        Min5 = "min_5",
        Min10 = "min_10",
        Min15 = "min_15",
        Hour1 = "hour_1",
    }
}

/// A word dictation should recognise, with other ways it is said (an entry of `SpeechCustomWords`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpeechCustomWord {
    pub term: String,
    pub aliases: Vec<String>,
}

/// What a project changes about one provider instance (`ProjectProviderInstanceOverride`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectProviderInstanceOverride {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

/// The settings one project sets differently (`ProjectSettingsOverrides`): an
/// absent setting is inherited, and `null` is a value of the settings that have one.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectSettingsOverrides {
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub worktree_cleanup: Option<Option<WorktreeCleanup>>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub default_model_selection: Option<Option<ModelSelection>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_runtime_mode: Option<RuntimeMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_thread_env_mode: Option<ThreadEnvMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_worktrees_start_from_origin: Option<bool>,
    /// A value this build does not know reads as absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_submodules: Option<WorktreeSubmodules>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_auto_pull: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_project_scripts: Option<Vec<ProjectScript>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enable_agent_browser_access: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enable_agent_device_access: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text_generation_model_selection: Option<ModelSelection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_title_instructions: Option<String>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub source_control_writer_model_selection: Option<Option<ModelSelection>>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub review_guide_model_selection: Option<Option<ModelSelection>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_guide_instructions: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_control_writing_style: Option<SourceControlWritingStyle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remove_agent_credits_on_merge: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_naming_mode: Option<BranchNamingMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_name_prefix: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch_name_instructions: Option<String>,
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub pull_request_merge_method: Option<Option<PullRequestMergeMethod>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sidebar_auto_settle_on_merge: Option<bool>,
    /// Days from 1 to 90, or `null` for never.
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub sidebar_auto_settle_after_days: Option<Option<Number>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sidebar_auto_settle_keep_pull_requests: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continue_threads_after_server_update: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_streaming_mode: Option<ResponseStreamingMode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speech_project_custom_words: Option<Vec<SpeechCustomWord>>,
    /// Unlike the other settings, overridden field by field for each instance.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_instances: Option<BTreeMap<ProviderInstanceId, ProjectProviderInstanceOverride>>,
    /// Automatically trigger incremental AI code reviews when new PRs or pushes appear on this project.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_review_prs: Option<bool>,
    /// PR comment trigger prefix (e.g. `/review`) that activates an AI code review on this project.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_comment_trigger: Option<String>,
    /// Publish validated AI code reviews automatically without waiting for manual triage (default false).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_publish_reviews: Option<bool>,
}

/// Every key of `ProjectSettingsOverrides`, to tell its settings from the
/// keys a newer server added.
const PROJECT_SETTINGS_OVERRIDE_KEYS: [&str; 31] = [
    "worktreeCleanup",
    "defaultModelSelection",
    "defaultRuntimeMode",
    "defaultThreadEnvMode",
    "newWorktreesStartFromOrigin",
    "worktreeSubmodules",
    "defaultAutoPull",
    "defaultProjectScripts",
    "enableAgentBrowserAccess",
    "enableAgentDeviceAccess",
    "textGenerationModelSelection",
    "threadTitleInstructions",
    "sourceControlWriterModelSelection",
    "reviewGuideModelSelection",
    "reviewGuideInstructions",
    "sourceControlWritingStyle",
    "removeAgentCreditsOnMerge",
    "branchNamingMode",
    "branchNamePrefix",
    "branchNameInstructions",
    "pullRequestMergeMethod",
    "sidebarAutoSettleOnMerge",
    "sidebarAutoSettleAfterDays",
    "sidebarAutoSettleKeepPullRequests",
    "continueThreadsAfterServerUpdate",
    "responseStreamingMode",
    "speechProjectCustomWords",
    "providerInstances",
    "autoReviewPrs",
    "reviewCommentTrigger",
    "autoPublishReviews",
];

/// One project's overrides as `projectSettingsOverrides` holds them
/// (`keepUnknownOverrideKeys(ProjectSettingsOverrides)`): the settings this
/// build knows, read, and the keys it does not, kept.
///
/// It derefs to the settings it read.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProjectSettingsOverrideEntry {
    pub overrides: ProjectSettingsOverrides,
    /// The keys this build does not know, each with its value as it was sent:
    /// the schema keeps them so that writing an entry back does not erase a
    /// newer server's settings (`Schema.Record(Schema.String, Schema.Unknown)`).
    pub unknown: Map<String, Value>,
}

impl Deref for ProjectSettingsOverrideEntry {
    type Target = ProjectSettingsOverrides;

    fn deref(&self) -> &ProjectSettingsOverrides {
        &self.overrides
    }
}

impl Serialize for ProjectSettingsOverrideEntry {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let known = serde_json::to_value(&self.overrides).map_err(serde::ser::Error::custom)?;
        let known = known.as_object().into_iter().flatten();
        let mut fields = serializer.serialize_map(None)?;
        for (key, value) in known.chain(&self.unknown) {
            fields.serialize_entry(key, value)?;
        }
        fields.end()
    }
}

impl<'de> Deserialize<'de> for ProjectSettingsOverrideEntry {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let (known, unknown): (Map<String, Value>, Map<String, Value>) = Map::<String, Value>::deserialize(deserializer)?
            .into_iter()
            .partition(|(key, _)| PROJECT_SETTINGS_OVERRIDE_KEYS.contains(&key.as_str()));
        let overrides = ProjectSettingsOverrides::deserialize(Value::Object(known)).map_err(serde::de::Error::custom)?;
        Ok(Self { overrides, unknown })
    }
}

/// A machine reached over SSH whose simulators and devices the server drives (`SshDeviceHostConfig`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SshDeviceHostConfig {
    /// Always `ssh`, which is also its default: hosts saved before the other kinds existed carry none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    pub id: String,
    pub label: String,
    pub target: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity_file: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u64>,
}

/// Cloud simulators from EAS Simulator (`EasDeviceHostConfig`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EasDeviceHostConfig {
    /// Always `eas`.
    pub kind: String,
    pub id: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_id: Option<String>,
    /// Defaults to `""`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_token: Option<SettingsSecretValue>,
    /// Defaults to `30`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_session_minutes: Option<u64>,
    /// Defaults to `10`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_minutes: Option<u64>,
    /// Defaults to `2`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrent_sessions: Option<u64>,
}

/// Devices leased from a Simlock gateway (`SimlockDeviceHostConfig`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SimlockDeviceHostConfig {
    /// Always `simlock`.
    pub kind: String,
    pub id: String,
    pub label: String,
    pub gateway_url: String,
    /// Defaults to `""`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_token: Option<SettingsSecretValue>,
    /// Defaults to `2`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_leases: Option<u64>,
    /// Defaults to `60`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_lease_minutes: Option<u64>,
}

string_enum! {
    /// When an idle thread's context is compacted (`IdleCompactionMode`).
    IdleCompactionMode {
        OnReturn = "onReturn",
        BeforeCacheExpires = "beforeCacheExpires",
        Off = "off",
    }
}

/// The older spelling of `idleCompaction` (`LegacyCompactIdleThreads`): a
/// switch, which is all the TypeScript server writes, or a mode from a server
/// older than `idleCompaction`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum CompactIdleThreads {
    Switch(bool),
    Mode(IdleCompactionMode),
}

string_enum! {
    /// The day a snooze preset wakes a thread on (`SnoozePresetDay`).
    SnoozePresetDay {
        Today = "today",
        Tomorrow = "tomorrow",
        Monday = "monday",
        Tuesday = "tuesday",
        Wednesday = "wednesday",
        Thursday = "thursday",
        Friday = "friday",
        Saturday = "saturday",
        Sunday = "sunday",
    }
}

/// A choice the snooze menu offers (`SnoozePresetSetting`).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum SnoozePresetSetting {
    /// Wakes this long from now.
    After { minutes: u64 },
    /// Wakes at a local time (`HH:MM`); a weekday is its next occurrence after today.
    At { day: SnoozePresetDay, time: String },
    /// A kind this build does not know, kept as it was sent.
    #[serde(untagged)]
    Other(Map<String, Value>),
}

impl<'de> Deserialize<'de> for SnoozePresetSetting {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        use serde::de::Error;

        #[derive(Deserialize)]
        struct After {
            minutes: u64,
        }
        #[derive(Deserialize)]
        struct At {
            day: SnoozePresetDay,
            time: String,
        }

        let fields = Map::<String, Value>::deserialize(deserializer)?;
        match tag_of::<D::Error>(&fields, "kind")? {
            "after" => After::deserialize(Value::Object(fields))
                .map(|after| Self::After { minutes: after.minutes })
                .map_err(|error| D::Error::custom(format_args!("SnoozePresetSetting `after`: {error}"))),
            "at" => At::deserialize(Value::Object(fields))
                .map(|at| Self::At { day: at.day, time: at.time })
                .map_err(|error| D::Error::custom(format_args!("SnoozePresetSetting `at`: {error}"))),
            _ => Ok(Self::Other(fields)),
        }
    }
}

string_enum! {
    /// How much the server does in the background (`BackgroundActivityProfile`).
    BackgroundActivityProfile {
        Balanced = "balanced",
        Performance = "performance",
        BatterySaver = "battery-saver",
    }
}

string_enum! {
    /// A background activity profile, or the user's own values (`BackgroundActivityProfileSelection`).
    BackgroundActivityProfileSelection {
        Balanced = "balanced",
        Performance = "performance",
        BatterySaver = "battery-saver",
        Custom = "custom",
    }
}

/// The background activity values the user set apart from a profile (`BackgroundActivityOverrides`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundActivityOverrides {
    /// Milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub automatic_git_fetch_interval: Option<Number>,
    /// Milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_health_refresh_interval: Option<Number>,
    /// Milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_power_monitor_active_interval: Option<Number>,
    /// Milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_power_monitor_idle_interval: Option<Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause_when_host_locked: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause_when_host_low_power: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause_when_client_low_power: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pause_when_on_battery: Option<bool>,
}

/// How much the server does in the background, and when it pauses (`BackgroundActivitySettings`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackgroundActivitySettings {
    /// Always `1`, which is also its default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub schema_version: Option<u64>,
    /// Defaults to `balanced`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<BackgroundActivityProfileSelection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_profile: Option<BackgroundActivityProfile>,
    /// Defaults to no overrides.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub overrides: Option<BackgroundActivityOverrides>,
}

string_enum! {
    /// How the branch of a new worktree is named (`BranchNamingMode`).
    BranchNamingMode {
        Static = "static",
        Semantic = "semantic",
        Custom = "custom",
    }
}

string_enum! {
    /// Whose conventions generated commit messages and pull request texts follow (`SourceControlWritingStyleMode`).
    SourceControlWritingStyleMode {
        RepoConventions = "repo_conventions",
        ConventionalCommits = "conventional_commits",
        Custom = "custom",
    }
}

/// How generated commit messages and pull request texts are written (`SourceControlWritingStyleSettings`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceControlWritingStyleSettings {
    /// Defaults to `repo_conventions`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<SourceControlWritingStyleMode>,
    /// Defaults to `""`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub custom_instructions: Option<String>,
    /// Defaults to `true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follow_change_request_templates: Option<bool>,
}

/// The `sourceControlWritingStyle` setting (`SourceControlWritingStyleSetting`):
/// the settings, which is all the TypeScript server writes, or the instructions
/// alone as servers before the settings existed stored them (empty means the
/// repository's conventions, anything else custom instructions).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SourceControlWritingStyle {
    Settings(SourceControlWritingStyleSettings),
    Legacy(String),
}

/// One environment variable a provider instance is started with, as it is
/// stored and sent (`ProviderInstanceEnvironmentVariable`): a secret reference
/// travels in `secret`, beside an empty `value`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderInstanceEnvironmentVariable {
    pub name: String,
    /// Defaults to `""`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    /// Defaults to `false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sensitive: Option<bool>,
    /// The server holds a value it did not send.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub value_redacted: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret: Option<SecretReference>,
}

/// A configured provider instance (`ProviderInstanceConfig`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderInstanceConfig {
    pub driver: ProviderDriverKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accent_color: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment: Option<Vec<ProviderInstanceEnvironmentVariable>>,
    /// Absent follows the driver's own default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// The driver's own settings, which only the driver reads (`Schema.Unknown`):
    /// the config of a driver this build does not know passes through whole.
    #[serde(default, deserialize_with = "present", skip_serializing_if = "Option::is_none")]
    pub config: Option<Value>,
}

/// Where the server sends its traces, metrics and logs (`ObservabilitySettings`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ObservabilitySettings {
    /// Defaults to `""`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub otlp_traces_url: Option<String>,
    /// Defaults to `""`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub otlp_metrics_url: Option<String>,
    /// Defaults to `""`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub otlp_logs_url: Option<String>,
}

/// A service the server asks how much of a provider's usage limit is left (`UsageLimitSourceConfig`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageLimitSourceConfig {
    /// Always `cliproxy` today.
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub url: String,
    /// Defaults to `""`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub management_key: Option<SettingsSecretValue>,
    /// Defaults to `true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

string_enum! {
    /// The API a text generator speaks (`TextGeneratorKind`).
    TextGeneratorKind {
        OpenaiCompatible = "openaiCompatible",
        Anthropic = "anthropic",
    }
}

/// A model API that only generates text, such as titles and commit messages (`TextGeneratorConfig`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextGeneratorConfig {
    pub kind: TextGeneratorKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub base_url: String,
    /// Defaults to `""`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<SettingsSecretValue>,
    /// Defaults to `true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

/// A configured secret provider, such as a password manager (`SecretProviderConfig`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SecretProviderConfig {
    /// The kind of secret provider (`SecretProviderKind`): an open set of names.
    pub kind: String,
    pub label: String,
    /// The kind's own settings, which only it reads (`Schema.Unknown`).
    #[serde(default, deserialize_with = "present", skip_serializing_if = "Option::is_none")]
    pub config: Option<Value>,
}

/// What a model's tokens cost, in place of the price the server knows (`UsageModelPriceOverride`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageModelPriceOverride {
    pub input_cost_per_million_tokens: Number,
    pub output_cost_per_million_tokens: Number,
    /// Absent uses the input rate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read_cost_per_million_tokens: Option<Number>,
    /// Absent uses the input rate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_cost_per_million_tokens: Option<Number>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn round_trip<T: DeserializeOwned + Serialize>(sent: Value) -> T {
        let typed: T = serde_json::from_value(sent.clone()).unwrap();
        assert_eq!(serde_json::to_value(&typed).unwrap(), sent);
        typed
    }

    #[test]
    fn settings_a_server_left_out_stay_absent() {
        let settings: ServerSettings = round_trip(json!({}));
        assert_eq!((settings.favorites, settings.default_runtime_mode, settings.worktrees_directory), (None, None, None));
    }

    #[test]
    fn a_null_setting_is_told_from_an_absent_one() {
        let settings: ServerSettings = round_trip(json!({ "favorites": null, "sidebarAutoSettleAfterDays": null, "defaultThreadEnvMode": null }));
        assert_eq!((settings.favorites, settings.sidebar_auto_settle_after_days, settings.default_thread_env_mode), (Some(None), Some(None), Some(None)));
    }

    #[test]
    fn values_this_build_does_not_know_survive_a_round_trip() {
        let settings: ServerSettings = round_trip(json!({
            "defaultRuntimeMode": "supervised",
            "branchNamingMode": "poetic",
            "idleCompaction": "nightly",
            "compactIdleThreads": "nightly",
            "environmentIcon": "mainframe",
            "worktreeCleanup": { "mode": "archive", "afterDays": 3 },
            "snoozePresets": [{ "kind": "after", "minutes": 5 }, { "kind": "until-reply", "from": "anyone" }],
            "hiddenThreadTags": ["wip", 7, "later"],
            "usageModelFilter": { "include": "everything" },
        }));
        assert_eq!(settings.default_runtime_mode, Some(RuntimeMode::Other("supervised".into())));
        assert_eq!(settings.branch_naming_mode, Some(BranchNamingMode::Other("poetic".into())));
        assert_eq!(settings.compact_idle_threads, Some(CompactIdleThreads::Mode(IdleCompactionMode::Other("nightly".into()))));
        assert!(matches!(settings.worktree_cleanup, Some(Some(WorktreeCleanup::Other(_)))));
        let presets = settings.snooze_presets.unwrap();
        assert_eq!(presets[0], SnoozePresetSetting::After { minutes: 5 });
        assert!(matches!(presets[1], SnoozePresetSetting::Other(_)));
        let tags = settings.hidden_thread_tags.unwrap();
        assert_eq!((&*tags, tags.unread()), (&["wip".to_owned(), "later".to_owned()][..], 1));
        assert_eq!(settings.usage_model_filter.unwrap().read(), None);
    }

    #[test]
    fn a_known_union_member_must_decode_in_full() {
        assert!(serde_json::from_value::<SnoozePresetSetting>(json!({ "kind": "at", "day": "today" })).is_err());
        assert!(serde_json::from_value::<SnoozePresetSetting>(json!({ "minutes": 5 })).is_err());
        assert!(serde_json::from_value::<WorktreeCleanup>(json!({ "mode": "custom" })).is_err());
        assert!(serde_json::from_value::<WorktreeCleanup>(json!({ "mode": 1 })).is_err());
    }

    #[test]
    fn a_record_missing_a_required_field_does_not_decode() {
        assert!(serde_json::from_value::<ModelFavorite>(json!({ "provider": "codex" })).is_err());
        assert!(serde_json::from_value::<ProviderInstanceConfig>(json!({ "displayName": "Codex" })).is_err());
        assert!(serde_json::from_value::<TextGeneratorConfig>(json!({ "kind": "anthropic" })).is_err());
        assert!(serde_json::from_value::<UsageModelPriceOverride>(json!({ "inputCostPerMillionTokens": 1 })).is_err());
        assert!(serde_json::from_value::<ServerSettings>(json!({ "favorites": [{ "model": "gpt" }] })).is_err());
        assert!(serde_json::from_value::<ServerSettings>(json!({ "defaultAutoPull": "yes" })).is_err());
    }

    #[test]
    fn a_project_override_entry_keeps_the_keys_this_build_does_not_know() {
        let entry: ProjectSettingsOverrideEntry = round_trip(json!({
            "defaultRuntimeMode": "auto",
            "defaultModelSelection": null,
            "futureSetting": { "level": 3 },
            "anotherOne": null,
        }));
        assert_eq!(entry.default_runtime_mode, Some(RuntimeMode::Auto));
        assert_eq!(entry.default_model_selection, Some(None));
        assert_eq!(entry.unknown.keys().collect::<Vec<_>>(), ["futureSetting", "anotherOne"]);
        assert!(serde_json::from_value::<ProjectSettingsOverrideEntry>(json!({ "defaultAutoPull": "yes" })).is_err());
    }

    /// The key list that tells known settings from unknown ones names exactly the fields of the record.
    #[test]
    fn every_project_override_field_is_a_known_key() {
        let selection = json!({ "instanceId": "codex", "model": "gpt-6" });
        let mut sent = Map::new();
        for key in PROJECT_SETTINGS_OVERRIDE_KEYS {
            let value = match key {
                "worktreeCleanup" => json!({ "mode": "off" }),
                "defaultModelSelection" | "textGenerationModelSelection" | "sourceControlWriterModelSelection" | "reviewGuideModelSelection" => {
                    selection.clone()
                }
                "defaultRuntimeMode" => json!("auto"),
                "defaultThreadEnvMode" => json!("worktree"),
                "worktreeSubmodules" => json!("none"),
                "defaultProjectScripts" | "speechProjectCustomWords" => json!([]),
                "threadTitleInstructions" | "reviewGuideInstructions" | "branchNamePrefix" | "branchNameInstructions" => json!("text"),
                "sourceControlWritingStyle" => json!({ "mode": "custom" }),
                "branchNamingMode" => json!("semantic"),
                "pullRequestMergeMethod" => json!("squash"),
                "sidebarAutoSettleAfterDays" => json!(3),
                "responseStreamingMode" => json!("turn"),
                "providerInstances" => json!({ "codex": { "enabled": false } }),
                "reviewCommentTrigger" => json!("/review"),
                _ => json!(true),
            };
            sent.insert(key.to_owned(), value);
        }
        let entry: ProjectSettingsOverrideEntry = round_trip(Value::Object(sent.clone()));
        assert!(entry.unknown.is_empty());
        // Read without the key list, the record itself writes back every key: none is missing from the record.
        let overrides: ProjectSettingsOverrides = serde_json::from_value(Value::Object(sent.clone())).unwrap();
        assert_eq!(serde_json::to_value(&overrides).unwrap(), Value::Object(sent));
        // And the record has no field the list lacks: a field outside it would be written here.
        let full = serde_json::to_value(&entry.overrides).unwrap();
        assert_eq!(full.as_object().unwrap().len(), PROJECT_SETTINGS_OVERRIDE_KEYS.len());
    }

    #[test]
    fn a_text_generation_selection_may_lack_what_a_patch_did_not_name() {
        let settings: ServerSettings = round_trip(json!({ "textGenerationModelSelection": { "model": "gpt-6-luna" } }));
        assert_eq!(settings.text_generation_model_selection.unwrap().selection(), None);
        let settings: ServerSettings = round_trip(json!({
            "textGenerationModelSelection": { "instanceId": "codex", "model": "gpt-6-luna", "options": [{ "id": "reasoningEffort", "value": "low" }] },
        }));
        let selection = settings.text_generation_model_selection.unwrap().selection().unwrap();
        assert_eq!((selection.instance_id.as_str(), selection.model.as_str()), ("codex", "gpt-6-luna"));
    }

    #[test]
    fn a_secret_is_a_literal_or_a_reference() {
        let credentials: CloudSpeechCredentials =
            round_trip(json!({ "openai": "", "xai": { "provider": "1password", "reference": "op://vault/xai/key" } }));
        assert_eq!(credentials.openai, Some(SettingsSecretValue::Literal(String::new())));
        assert!(matches!(credentials.xai, Some(SettingsSecretValue::Reference(_))));
        assert_eq!(credentials.openrouter, None);
    }

    #[test]
    fn a_writing_style_is_settings_or_the_older_instructions() {
        let settings: ServerSettings = round_trip(json!({ "sourceControlWritingStyle": "Keep it short." }));
        assert_eq!(settings.source_control_writing_style, Some(SourceControlWritingStyle::Legacy("Keep it short.".to_owned())));
        let settings: ServerSettings = round_trip(json!({ "sourceControlWritingStyle": { "mode": "custom" } }));
        assert!(matches!(settings.source_control_writing_style, Some(SourceControlWritingStyle::Settings(_))));
    }

    #[test]
    fn a_provider_instance_config_passes_through_whole() {
        let config = json!({ "driver": "futureDriver", "config": { "anything": [1, 2.0, null], "nested": { "deep": true } } });
        let instance: ProviderInstanceConfig = round_trip(config);
        assert_eq!(instance.driver.as_str(), "futureDriver");
        let null_config: ProviderInstanceConfig = round_trip(json!({ "driver": "codex", "config": null }));
        assert_eq!(null_config.config, Some(Value::Null));
    }
}
