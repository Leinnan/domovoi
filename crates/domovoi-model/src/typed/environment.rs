//! The environment a server runs in (`environment.ts`): the machine, what the
//! server on it can do, and the repository a project's checkout belongs to.

use super::ids::EnvironmentId;
use super::macros::string_enum;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

string_enum! {
    /// Where a new thread works (`ThreadEnvMode`): the project's checkout or a worktree of its own.
    ThreadEnvMode {
        Local = "local",
        Worktree = "worktree",
    }
}

string_enum! {
    /// Which submodules a new worktree checks out (`WorktreeSubmodules`).
    WorktreeSubmodules {
        Recursive = "recursive",
        TopLevel = "top-level",
        None = "none",
    }
}

string_enum! {
    /// The kind of machine an environment is shown as (`EnvironmentMachineKind`).
    EnvironmentMachineKind {
        Server = "server",
        Cloud = "cloud",
        Linux = "linux",
        Desktop = "desktop",
        Laptop = "laptop",
        MacMini = "mac-mini",
        MacStudio = "mac-studio",
    }
}

string_enum! {
    /// The operating system a server runs on (`ExecutionEnvironmentPlatformOs`).
    /// The Domovoi server sends Rust's own name for a system it has no t3code name
    /// for (`win32` on Windows, `freebsd`, …), which is kept as `Other`.
    ExecutionEnvironmentPlatformOs {
        Darwin = "darwin",
        Linux = "linux",
        Windows = "windows",
        Unknown = "unknown",
    }
}

string_enum! {
    /// The processor architecture a server runs on (`ExecutionEnvironmentPlatformArch`).
    /// The Domovoi server sends Rust's own name for an architecture that is neither
    /// of the first two (`x86`, `riscv64`, …), which is kept as `Other`.
    ExecutionEnvironmentPlatformArch {
        Arm64 = "arm64",
        X64 = "x64",
        /// The literal `other`: an architecture t3code does not name.
        Unnamed = "other",
    }
}

/// The machine a server runs on (`ExecutionEnvironmentPlatform`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionEnvironmentPlatform {
    pub os: ExecutionEnvironmentPlatformOs,
    pub arch: ExecutionEnvironmentPlatformArch,
    /// The kind of machine to show the environment as, when the server could tell.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine: Option<EnvironmentMachineKind>,
}

string_enum! {
    /// How a server can update itself (`ServerSelfUpdateCapability`).
    ServerSelfUpdateCapability {
        BootService = "boot-service",
        Respawn = "respawn",
        DesktopManaged = "desktop-managed",
    }
}

string_enum! {
    /// A package runner that starts a server without installing it (the
    /// `kind` of the first `ServerInstallation` member).
    ServerPackageRunner {
        Npx = "npx",
        PnpmDlx = "pnpm-dlx",
        Bunx = "bunx",
    }
}

/// How a server was installed (`ServerInstallation`), which decides the
/// command that updates it by hand.
///
/// The schema reads this leniently (`ForwardCompatibleOptional`): a value
/// that is not one of its members, a known `kind` with a field missing
/// included, counts as absent. Such a value is `Other` here, kept as sent.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all_fields = "camelCase")]
pub enum ServerInstallation {
    /// Installed globally with npm under `prefix`.
    #[serde(rename = "npm-global")]
    NpmGlobal { prefix: String },
    /// A launcher linking into a runtime downloaded under `base_dir`.
    #[serde(rename = "t3-launcher")]
    T3Launcher { launcher: String, base_dir: String },
    /// Run through a package runner, with nothing installed.
    #[serde(untagged)]
    PackageRunner { kind: ServerPackageRunner },
    /// An installation this build cannot read, kept as it was sent.
    #[serde(untagged)]
    Other(Value),
}

impl ServerInstallation {
    /// The installation `sent` describes, if it is a member this build knows in full.
    fn known(sent: &Value) -> Option<Self> {
        let fields = sent.as_object()?;
        let text = |key: &str| fields.get(key)?.as_str().filter(|text| !text.trim().is_empty()).map(str::to_owned);
        match fields.get("kind")?.as_str()? {
            "npx" => Some(Self::PackageRunner { kind: ServerPackageRunner::Npx }),
            "pnpm-dlx" => Some(Self::PackageRunner { kind: ServerPackageRunner::PnpmDlx }),
            "bunx" => Some(Self::PackageRunner { kind: ServerPackageRunner::Bunx }),
            "npm-global" => Some(Self::NpmGlobal { prefix: text("prefix")? }),
            "t3-launcher" => Some(Self::T3Launcher { launcher: text("launcher")?, base_dir: text("baseDir")? }),
            _ => None,
        }
    }
}

impl<'de> Deserialize<'de> for ServerInstallation {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let sent = Value::deserialize(deserializer)?;
        Ok(Self::known(&sent).unwrap_or(Self::Other(sent)))
    }
}

/// The limit on an attached file (`ExecutionEnvironmentCapabilities.fileAttachments`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileAttachmentCapability {
    pub max_upload_bytes: u64,
}

/// What a server can do (`ExecutionEnvironmentCapabilities`): a client offers
/// a feature only where its flag is set. A flag a server does not send means
/// the server does not have the feature.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionEnvironmentCapabilities {
    /// The schema reads an absent flag as `false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repository_identity: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub connection_probe: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attachment_uploads: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub question_attachments: Option<bool>,
    /// Present when files of any type can be attached, with the size the server accepts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_attachments: Option<FileAttachmentCapability>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_requests: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_request_checks: Option<bool>,
    /// Threads can fork hidden side chats.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub side_chats: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vcs_branch_status: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inline_message_context: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_worktree_bootstrap: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_settlement: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_auto_settlement: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage_cleanup: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage_cleanup_run: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub database_storage: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub otlp_endpoint_check: Option<bool>,
    /// The server answers `server.checkSecretSource`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_source_check: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_worktree_cleanup: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settled_worktree_cleanup: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktrees_directory: Option<bool>,
    /// The server honors the `fastWorktreeCheckout` setting (macOS servers).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fast_worktree_checkout: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_auto_settlement_scope: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_restart_continuation: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_settings_overrides: Option<bool>,
    /// The server keeps per-project provider instance patches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_provider_instance_overrides: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_snooze: Option<bool>,
    /// The server answers `orchestration.forceStopThread`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_force_stop: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_themes: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_limit_sources: Option<bool>,
    /// The server honors `workspaceSnapshotsOnDemand` on its config stream and provider refreshes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_workspace_snapshots_on_demand: Option<bool>,
    /// The server honors `modelCatalogOnly` on a provider refresh.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_catalog_refresh: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_price_overrides: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_model_aliases: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_pinning: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_pin_reorder: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_active_reorder: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_auto_settle_opt_out: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_project_from_scratch: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_title_regeneration: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_visited_tracking: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_pull_request_linking: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_resolved_command_context: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_stop: Option<bool>,
    /// The server can stop another server it discovered on its machine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_server_stop: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_pull_requests: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_pull_request_watch: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pull_request_stack_actions: Option<bool>,
    /// How the server can update itself, when it can.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_self_update: Option<ServerSelfUpdateCapability>,
    /// The server answers `server.getNpmRelease` from its configured npm registry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub npm_release_lookup: Option<bool>,
    /// How the server was installed, for the update command a user runs by hand.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_installation: Option<ServerInstallation>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_self_update_progress: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_update_thread_continuation: Option<bool>,
    /// Only an explicit `false` says the server publishes no agent activity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_activity_publishing: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_clone_tracking: Option<bool>,
    /// The server keeps `pinned` and `archived` from a project update.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_organization: Option<bool>,
    /// The server answers `projects.listIgnoredFiles`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ignored_file_listing: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_icon: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub desktop_app_update: Option<bool>,
    /// The desktop app changes update track when a server update names one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub desktop_app_channel_switch: Option<bool>,
    /// The server streams command output to subscribers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_output_streaming: Option<bool>,
    /// The server hosts browser tabs it streams to clients without a local runtime.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_browser: Option<bool>,
    /// The server links to other environments, and its agents can work in them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer_links: Option<bool>,
    /// The server's `/mcp` narrows a linked environment's modes from the `T3-Mode-Limit` header.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_mode_limit_header: Option<bool>,
    /// Work a linked environment starts on the server stays within its link.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link_fence: Option<bool>,
    /// The server mints pairing codes that approve one agent sign-in at a set level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_approval_codes: Option<bool>,
    /// Agent sign-ins on the server's T3 Connect address are approved on the hosted app.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_hosted_approval: Option<bool>,
    /// The server answers `server.prepareTriage`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub triage: Option<bool>,
    /// The server accepts scheduled tasks that run once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub one_time_schedules: Option<bool>,
    /// The server honors `freshContext` on a send.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fresh_context: Option<bool>,
    /// The server saves and runs a scheduled task's `threadMode`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scheduled_task_thread_modes: Option<bool>,
    /// The server can transcribe audio on its own machine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice_transcription: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cloud_voice_transcription: Option<bool>,
    /// The server config's flag of the same name, readable before the socket opens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell_thread_families: Option<bool>,
    /// The server config's flag of the same name, readable before the socket opens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell_older_settled: Option<bool>,
    /// The server publishes a pull request refresh when linked-thread answers change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub linked_thread_refreshes: Option<bool>,
}

/// A server and the machine it runs on, as it describes itself
/// (`ExecutionEnvironmentDescriptor`): served at `/.well-known/domovoi/environment`
/// and repeated in the server's config and lifecycle events.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionEnvironmentDescriptor {
    pub environment_id: EnvironmentId,
    pub label: String,
    pub platform: ExecutionEnvironmentPlatform,
    pub server_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub orchestration_protocol_version: Option<u64>,
    /// The highest migration the server's database holds; older servers leave it out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub database_migration: Option<u64>,
    pub capabilities: ExecutionEnvironmentCapabilities,
}

/// The git remote a repository identity was read from (`RepositoryIdentityLocator`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepositoryIdentityLocator {
    /// Always `git-remote` today.
    pub source: String,
    pub remote_name: String,
    pub remote_url: String,
}

/// The repository a fork was made from (`RepositoryOrigin`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepositoryOrigin {
    pub canonical_key: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remote_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
}

/// Which repository a checkout is of (`RepositoryIdentity`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RepositoryIdentity {
    pub canonical_key: String,
    pub locator: RepositoryIdentityLocator,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_url: Option<String>,
    /// Work tree root, or the git dir for a bare repository.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_name: Option<String>,
    /// A source control provider kind (`github`, `gitlab`, …): an open set of names.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<RepositoryOrigin>,
}
