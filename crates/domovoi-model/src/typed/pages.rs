//! What the settings pages show of a server besides its threads: scheduled
//! tasks (`scheduledTask.ts`), who may reach it (`auth.ts`), its storage
//! (`settings.ts`), its processes and host (`server.ts`,
//! `resourceTelemetry.ts`), token usage (`usage.ts`) and the sessions of
//! other agents found on disk (`agentSessions.ts`).

use super::common::{Actor, CreationSource, InteractionMode, ModelSelection, RuntimeMode, Timestamp, double_option};
use super::ids::{ProjectId, ScheduledTaskId, ThreadId};
use super::list::ForwardList;
use super::macros::{string_enum, tagged_union_deserialize};
use super::run::WorkspaceStrategy;
use serde::{Deserialize, Serialize};
use serde_json::Number;

/// Defines the fallback variant's payload of a union: a record whose tag this
/// build does not know, kept as it was sent.
macro_rules! unknown_record {
    ($(#[$meta:meta])* $name:ident tagged $field:literal) => {
        $(#[$meta])*
        #[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
        #[serde(transparent)]
        pub struct $name(serde_json::Map<String, serde_json::Value>);

        impl $name {
            /// The tag the record was sent with.
            pub fn tag(&self) -> &str {
                self.0.get($field).and_then(serde_json::Value::as_str).unwrap_or_default()
            }

            /// The whole record, its tag included.
            pub fn record(&self) -> &serde_json::Map<String, serde_json::Value> {
                &self.0
            }
        }
    };
}

pub(crate) use unknown_record;

/// A value in the encoding of Effect's `Schema.Option`: `{ "_tag": "None" }`
/// or `{ "_tag": "Some", "value": … }`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "_tag")]
pub enum TaggedOption<T> {
    None,
    Some { value: T },
}

impl<T> TaggedOption<T> {
    /// The value, when there is one.
    pub fn as_option(&self) -> Option<&T> {
        match self {
            Self::None => None,
            Self::Some { value } => Some(value),
        }
    }
}

// ---- scheduled tasks ----

/// What `scheduledTasks.subscribe` streams (`ScheduledTaskListResult`): every
/// task, sent again after each change.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduledTaskList {
    pub tasks: ForwardList<ScheduledTask>,
    /// The server's IANA zone, which fixed times are read in. The schema leaves
    /// it out on older servers; Domovoi writes `null` when it cannot name its zone.
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub time_zone: Option<Option<String>>,
}

/// A task that sends a prompt on its own, on a schedule or on a webhook request (`ScheduledTask`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduledTask {
    pub id: ScheduledTaskId,
    pub title: String,
    pub prompt: String,
    pub enabled: bool,
    pub schedule: ScheduledTaskSchedule,
    pub project_id: ProjectId,
    /// The thread runs go to, once the task has one.
    pub thread_id: Option<ThreadId>,
    /// Left out by servers older than thread modes: a task bound to a thread then continues it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_mode: Option<ScheduledTaskThreadMode>,
    pub workspace_strategy: WorkspaceStrategy,
    pub model_selection: ModelSelection,
    pub runtime_mode: RuntimeMode,
    pub interaction_mode: InteractionMode,
    pub created_by: Actor,
    pub creation_source: CreationSource,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub next_run_at: Option<Timestamp>,
    pub last_run_at: Option<Timestamp>,
    pub last_run_status: ScheduledTaskRunStatus,
    pub last_run_error: Option<String>,
    pub run_count: u64,
    /// Where webhook requests for the task go; only on webhook tasks, and only for sessions that may operate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhook: Option<ScheduledTaskWebhookEndpoint>,
    /// Why the task cannot run whatever `enabled` says, such as a managed policy refusing webhooks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_by_policy: Option<String>,
}

string_enum! {
    /// How a scheduled task's last run ended (`ScheduledTaskRunStatus`).
    ScheduledTaskRunStatus {
        Never = "never",
        Running = "running",
        Succeeded = "succeeded",
        Failed = "failed",
    }
}

string_enum! {
    /// Where a scheduled task's runs go (`ScheduledTaskThreadMode`): a new
    /// thread each time, or one thread, continued or with a fresh provider
    /// session per run.
    ScheduledTaskThreadMode {
        NewThread = "new_thread",
        Continue = "continue",
        FreshSession = "fresh_session",
    }
}

/// When a scheduled task runs (`ScheduledTaskSchedule`).
///
/// `Other` is a schedule type this build does not know.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ScheduledTaskSchedule {
    Interval(IntervalSchedule),
    FixedTime(FixedTimeSchedule),
    Once(OnceSchedule),
    Webhook(WebhookSchedule),
    #[serde(untagged)]
    Other(UnknownSchedule),
}

tagged_union_deserialize! {
    ScheduledTaskSchedule by "type" {
        "interval" => Interval,
        "fixed_time" => FixedTime,
        "once" => Once,
        "webhook" => Webhook,
        _ => Other,
    }
}

impl ScheduledTaskSchedule {
    /// The wire name of the schedule's type.
    pub fn schedule_type(&self) -> &str {
        match self {
            Self::Interval(_) => "interval",
            Self::FixedTime(_) => "fixed_time",
            Self::Once(_) => "once",
            Self::Webhook(_) => "webhook",
            Self::Other(unknown) => unknown.tag(),
        }
    }
}

/// A schedule that runs again every so many milliseconds.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IntervalSchedule {
    pub every_ms: u64,
}

/// A schedule that runs at a time of day, in the server's zone.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FixedTimeSchedule {
    /// Local time as `HH:MM`, such as `09:30`.
    pub time_of_day: String,
    /// The weekdays it runs on, 0 for Sunday to 6 for Saturday; every day when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weekdays: Option<Vec<u64>>,
}

/// A schedule that runs once.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OnceSchedule {
    /// An ISO date and time with an offset; servers store it in UTC.
    pub run_at: Timestamp,
}

/// A schedule that runs on each webhook request.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebhookSchedule {
    /// The signature requests must carry; `null` when the URL token alone admits them.
    pub signature: Option<WebhookSignature>,
    /// Requests held offline for more than this many minutes are skipped; all run when unset.
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub max_delivery_age_minutes: Option<Option<u64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dedupe: Option<WebhookDedupe>,
    /// Every filter must match for a request to run the task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filters: Option<Vec<WebhookFilter>>,
}

unknown_record! {
    /// A schedule of a type this build does not know, as it was sent.
    UnknownSchedule tagged "type"
}

/// The HMAC-SHA256 check a webhook request's raw body must pass (`ScheduledTaskWebhookSignature`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebhookSignature {
    /// The header with the signature, such as `x-hub-signature-256`.
    pub header: String,
    pub encoding: WebhookSignatureEncoding,
    /// Text before the digest, such as `sha256=`; empty for none.
    pub prefix: String,
}

string_enum! {
    /// How a webhook signature's digest is written.
    WebhookSignatureEncoding {
        Hex = "hex",
        Base64 = "base64",
    }
}

/// The templates that tell repeated webhook requests apart (`ScheduledTaskWebhookDedupe`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebhookDedupe {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replay_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coalesce_key: Option<String>,
    /// 120 seconds when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coalesce_window_seconds: Option<u64>,
}

/// A condition a webhook request must meet (`ScheduledTaskWebhookFilter`): a
/// comparison, named by `op`, of the value at a path such as `headers.x-event`
/// or `body.action`.
///
/// `Other` is a comparison this build does not know.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "op", rename_all = "camelCase")]
pub enum WebhookFilter {
    Equals(WebhookTextFilter),
    NotEquals(WebhookTextFilter),
    StartsWith(WebhookTextFilter),
    In(WebhookListFilter),
    Exists(WebhookPresenceFilter),
    #[serde(untagged)]
    Other(UnknownWebhookFilter),
}

tagged_union_deserialize! {
    WebhookFilter by "op" {
        "equals" => Equals,
        "notEquals" => NotEquals,
        "startsWith" => StartsWith,
        "in" => In,
        "exists" => Exists,
        _ => Other,
    }
}

/// A webhook filter that compares the value at a path with one text.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebhookTextFilter {
    pub path: String,
    pub value: String,
}

/// A webhook filter that passes when the value at a path is one of a list.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebhookListFilter {
    pub path: String,
    pub value: Vec<String>,
}

/// A webhook filter that passes when a request has anything at a path.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebhookPresenceFilter {
    pub path: String,
}

unknown_record! {
    /// A webhook filter with a comparison this build does not know, as it was sent.
    UnknownWebhookFilter tagged "op"
}

/// Where a webhook task receives its requests (`ScheduledTaskWebhookEndpoint`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScheduledTaskWebhookEndpoint {
    pub path: String,
    /// The public address of `path`, when the server has one.
    pub url: Option<String>,
    pub has_secret: bool,
}

// ---- who may reach the server ----

string_enum! {
    /// How a session proves itself to the server (`ServerAuthSessionMethod`).
    ServerAuthSessionMethod {
        BrowserSessionCookie = "browser-session-cookie",
        BearerAccessToken = "bearer-access-token",
        DpopAccessToken = "dpop-access-token",
    }
}

string_enum! {
    /// One thing a session may do on a server (`AuthEnvironmentScope`).
    AuthEnvironmentScope {
        OrchestrationRead = "orchestration:read",
        OrchestrationOperate = "orchestration:operate",
        SettingsWrite = "settings:write",
        ProvidersManage = "providers:manage",
        EnvironmentMaintain = "environment:maintain",
        PreviewOperate = "preview:operate",
        DiagnosticsRead = "diagnostics:read",
        TerminalRead = "terminal:read",
        TerminalOperate = "terminal:operate",
        FilesystemRead = "filesystem:read",
        FilesystemWrite = "filesystem:write",
        /// Kept so existing credentials decode; it grants nothing.
        ReviewWrite = "review:write",
        SourceControlWrite = "source-control:write",
        AccessRead = "access:read",
        AccessWrite = "access:write",
        RelayRead = "relay:read",
        RelayWrite = "relay:write",
    }
}

string_enum! {
    /// The kind of device a client says it is (`AuthClientMetadataDeviceType`).
    AuthClientDeviceType {
        Desktop = "desktop",
        Mobile = "mobile",
        Tablet = "tablet",
        Bot = "bot",
        Unknown = "unknown",
    }
}

/// A one-time pairing code that has not been used yet (`AuthPairingLink`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthPairingLink {
    pub id: String,
    /// The scopes clients older than exact permissions understand.
    pub scopes: Vec<AuthEnvironmentScope>,
    /// Exactly what the code grants; left out by servers older than permissions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permissions: Option<ForwardList<AuthEnvironmentScope>>,
    pub subject: String,
    /// Domovoi does not record a label for its links.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub created_at: Timestamp,
    pub expires_at: Timestamp,
}

/// What a signed-in client told the server about itself (`AuthClientMetadata`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthClientMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ip_address: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_agent: Option<String>,
    pub device_type: AuthClientDeviceType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser: Option<String>,
}

/// A client signed in to the server (`AuthClientSession`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthClientSession {
    /// An `AuthSessionId`.
    pub session_id: String,
    pub subject: String,
    /// The scopes clients older than exact permissions understand.
    pub scopes: Vec<AuthEnvironmentScope>,
    /// Exactly what the session may do; left out by servers older than permissions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permissions: Option<ForwardList<AuthEnvironmentScope>>,
    pub method: ServerAuthSessionMethod,
    pub client: AuthClientMetadata,
    pub issued_at: Timestamp,
    pub expires_at: Timestamp,
    pub last_connected_at: Option<Timestamp>,
    /// Whether the session has a socket open now.
    pub connected: bool,
    /// Whether it is the session of the client asking.
    pub current: bool,
}

/// The unused pairing codes and the signed-in clients of a server (`AuthAccessSnapshot`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthAccessSnapshot {
    pub pairing_links: Vec<AuthPairingLink>,
    pub client_sessions: Vec<AuthClientSession>,
}

/// What `subscribeAuthAccess` streams (`AuthAccessStreamEvent`): a snapshot
/// first, then each change to it.
///
/// `Other` is an event type this build does not know.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum AuthAccessStreamEvent {
    Snapshot(AuthAccessEvent<AuthAccessSnapshot>),
    PairingLinkUpserted(AuthAccessEvent<AuthPairingLink>),
    PairingLinkRemoved(AuthAccessEvent<AuthPairingLinkRemoved>),
    ClientUpserted(AuthAccessEvent<AuthClientSession>),
    ClientRemoved(AuthAccessEvent<AuthClientRemoved>),
    #[serde(untagged)]
    Other(UnknownAuthAccessEvent),
}

tagged_union_deserialize! {
    AuthAccessStreamEvent by "type" {
        "snapshot" => Snapshot,
        "pairingLinkUpserted" => PairingLinkUpserted,
        "pairingLinkRemoved" => PairingLinkRemoved,
        "clientUpserted" => ClientUpserted,
        "clientRemoved" => ClientRemoved,
        _ => Other,
    }
}

/// What every event of the access stream carries around its payload.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthAccessEvent<P> {
    /// Always 1.
    pub version: u64,
    /// Counts the events of one subscription, the snapshot included.
    pub revision: Number,
    pub payload: P,
}

/// The pairing code an access event says is gone: used, revoked or expired.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthPairingLinkRemoved {
    pub id: String,
}

/// The client session an access event says is gone: signed out or expired.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthClientRemoved {
    /// An `AuthSessionId`.
    pub session_id: String,
}

unknown_record! {
    /// An access event of a type this build does not know, as it was sent.
    UnknownAuthAccessEvent tagged "type"
}

// ---- storage ----

/// What `server.getDatabaseStorage` streams: the database's size, or `null` when the server cannot tell.
pub type DatabaseStorageItem = Option<DatabaseStorageStatus>;

/// How much disk the server's database takes (`DatabaseStorageStatus`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DatabaseStorageStatus {
    pub database_bytes: Number,
    /// Free pages inside the file, which compacting returns to the disk.
    pub reclaimable_bytes: Number,
    /// False on databases created before incremental vacuum.
    pub incremental_vacuum_enabled: bool,
    /// The command that compacts the database, to run on the host while the server is off.
    pub compact_command: String,
    /// Whether the server can compact the database itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub can_reclaim: Option<bool>,
    /// Why the last compaction started from Settings failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reclaim_failure: Option<String>,
}

/// What `server.getStorageCleanupReport` streams: the last cleanup's report, or `null` before any ran.
pub type StorageCleanupReportItem = Option<StorageCleanupReport>;

/// What one storage cleanup removed and kept (`StorageCleanupReport`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageCleanupReport {
    pub trigger: StorageCleanupTrigger,
    pub started_at: Timestamp,
    pub finished_at: Timestamp,
    pub entries: Vec<StorageCleanupReportEntry>,
    pub counts: StorageCleanupCounts,
    /// How many entries the report leaves out for length.
    pub omitted_count: Number,
    /// 0 when left out, as servers older than the field do.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes_freed: Option<Number>,
}

string_enum! {
    /// What started a storage cleanup.
    StorageCleanupTrigger {
        Automatic = "automatic",
        Manual = "manual",
    }
}

/// How many entries of a cleanup report had each outcome.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageCleanupCounts {
    pub removed: Number,
    pub kept: Number,
    pub failed: Number,
}

/// One thing a storage cleanup looked at (`StorageCleanupReportEntry`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageCleanupReportEntry {
    pub kind: StorageCleanupKind,
    pub outcome: StorageCleanupOutcome,
    pub reason: String,
    pub path: Option<String>,
    /// `null` when left out; Domovoi always leaves it out.
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub project_path: Option<Option<String>>,
    pub thread_id: Option<ThreadId>,
    pub thread_title: Option<String>,
    /// `null` when left out; Domovoi leaves it out for a worktree no thread uses.
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub bytes: Option<Option<Number>>,
    /// `null` when left out; Domovoi counts files only for logs.
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub files: Option<Option<Number>>,
}

string_enum! {
    /// What a storage cleanup entry is about.
    StorageCleanupKind {
        Worktree = "worktree",
        BrowserArtifacts = "browser-artifacts",
        Logs = "logs",
    }
}

string_enum! {
    /// What a storage cleanup did with an entry.
    StorageCleanupOutcome {
        Removed = "removed",
        Kept = "kept",
        Failed = "failed",
    }
}

// ---- processes and the host ----

/// What `server.getProcessDiagnostics` answers (`ServerProcessDiagnosticsResult`):
/// the server's process and its descendants, parents before children.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerProcessDiagnosticsResult {
    pub server_pid: u64,
    pub read_at: Timestamp,
    pub process_count: u64,
    pub total_rss_bytes: u64,
    pub total_cpu_percent: Number,
    pub processes: Vec<ServerProcessDiagnosticsEntry>,
    /// Why the processes could not be read.
    pub error: TaggedOption<ServerProcessDiagnosticsError>,
}

/// One process under the server (`ServerProcessDiagnosticsEntry`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerProcessDiagnosticsEntry {
    pub pid: u64,
    pub start_time_ms: u64,
    pub ppid: u64,
    /// The process group; none on Windows.
    pub pgid: TaggedOption<i64>,
    pub status: String,
    pub cpu_percent: Number,
    pub rss_bytes: u64,
    /// How long it has run, as `HH:MM:SS`.
    pub elapsed: String,
    pub command: String,
    /// 0 for the server itself, one more for each generation below it.
    pub depth: u64,
    pub child_pids: Vec<u64>,
}

/// Why the server could not read its processes.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerProcessDiagnosticsError {
    pub message: String,
}

/// What `server.getHostResources` answers (`HostResourcesSnapshot`): the load of the machine the server runs on.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostResourcesSnapshot {
    /// When it was read, in milliseconds since the epoch.
    pub sampled_at: u64,
    /// From 0 to 1 over all cores; `null` when it could not be read.
    pub cpu_utilization: Option<Number>,
    pub cpu_count: u64,
    pub available_memory_bytes: u64,
    pub total_memory_bytes: u64,
}

// ---- token usage ----

/// What `server.getUsageSummary` answers (`UsageSummary`): the tokens the
/// providers' own logs record for a range of days.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageSummary {
    pub contract_version: Number,
    pub read_at: Timestamp,
    /// The IANA zone the days are cut in.
    pub time_zone: String,
    /// A `UsageDay`: `YYYY-MM-DD`.
    pub since_day: String,
    /// A `UsageDay`: `YYYY-MM-DD`.
    pub until_day: String,
    pub buckets: ForwardList<UsageBucket>,
    pub sources: ForwardList<UsageSource>,
    pub pricing: UsagePricing,
    pub scan_duration_ms: u64,
    /// The filter the server applied; unfiltered when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_filter: Option<UsageModelFilter>,
}

/// The usage of one model of one provider on one day, or in one hour of it (`UsageBucket`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageBucket {
    /// A `UsageDay`: `YYYY-MM-DD`.
    pub day: String,
    /// When the bucket's hour starts, for a summary by the hour. The schema
    /// leaves it out of a summary by the day; Domovoi writes `null` there.
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub hour_start: Option<Option<String>>,
    pub provider: UsageProviderKind,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    pub totals: UsageTokenTotals,
    pub cost_usd: Number,
    pub cache_savings_usd: Number,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category_cost_usd: Option<UsageCategoryCost>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fast_cost_usd: Option<Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ultrafast_cost_usd: Option<Number>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub speed_premium_usd: Option<Number>,
    pub cost_source: UsageCostSource,
    pub records: u64,
    pub unpriced_records: u64,
    /// Left out by older servers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unpriced_tokens: Option<u64>,
    pub sessions: u64,
}

string_enum! {
    /// The provider whose logs a usage record was read from (`UsageProviderKind`).
    UsageProviderKind {
        Claude = "claude",
        Codex = "codex",
        Grok = "grok",
        Cursor = "cursor",
        Opencode = "opencode",
        Antigravity = "antigravity",
        Pi = "pi",
        Muse = "muse",
    }
}

string_enum! {
    /// Where a usage bucket's cost comes from (`UsageCostSource`).
    UsageCostSource {
        ProviderReported = "providerReported",
        ModelPriced = "modelPriced",
        Unpriced = "unpriced",
    }
}

/// Token counts (`UsageTokenTotals`). Input is the sum of uncached input,
/// cache reads and cache writes; reasoning is already part of output.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageTokenTotals {
    pub uncached_input_tokens: u64,
    pub cached_input_tokens: u64,
    pub cache_creation_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_tokens: u64,
}

/// A usage bucket's cost in dollars, by kind of token (`UsageCategoryCost`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageCategoryCost {
    pub input: Number,
    pub cache_read: Number,
    pub cache_write: Number,
    pub output: Number,
}

/// One provider's logs as a usage scan found them (`UsageSource`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageSource {
    pub fingerprint: UsageSourceFingerprint,
    pub status: UsageSourceStatus,
    pub scanned_files: u64,
    pub skipped_files: u64,
    pub malformed_records: u64,
    /// Sessions counted once each; a bucket counts a session again for each day and model it spans.
    pub distinct_sessions: u64,
    pub message: Option<String>,
    /// Present, and `true`, on an answer from the last scan while a new one runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refreshing: Option<bool>,
}

/// Which logs on which machine a usage source is (`UsageSourceFingerprint`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageSourceFingerprint {
    pub host_id: String,
    pub provider: UsageProviderKind,
    pub resolved_home_path: String,
    /// Tells apart machines that share a host name and a home path.
    pub volume_id: String,
}

string_enum! {
    /// How the scan of a usage source went (`UsageSourceStatus`).
    UsageSourceStatus {
        Ok = "ok",
        Missing = "missing",
        Partial = "partial",
        Failed = "failed",
    }
}

/// The price table a usage summary's costs were worked out with (`UsagePricing`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsagePricing {
    pub status: UsagePricingStatus,
    pub source: String,
    pub fetched_at: Option<Timestamp>,
    pub known_models: u64,
}

string_enum! {
    /// Whether the server has a price table (`UsagePricingStatus`); with none, costs are not known.
    UsagePricingStatus {
        Fresh = "fresh",
        Cached = "cached",
        Unavailable = "unavailable",
    }
}

/// Which models a usage summary counts (`UsageModelFilter`): those `include`
/// names, all when left out, less those `exclude` names.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageModelFilter {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub include: Option<Vec<UsageModelSelector>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exclude: Option<Vec<UsageModelSelector>>,
}

/// A model as a provider names it, of one provider or of any (`UsageModelSelector`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UsageModelSelector {
    /// Any provider when left out. A name, not a `UsageProviderKind`, so a
    /// selector for a provider this build lacks still passes through.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    pub model: String,
}

// ---- other agents' sessions ----

/// What `agentSessions.scan` answers (`AgentSessionScanResult`): the
/// directories other agents' sessions on this machine worked in.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSessionScanResult {
    pub candidates: Vec<AgentSessionProjectCandidate>,
    pub scanned_at: Timestamp,
    /// Present when the scan stopped early and may have missed sessions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub truncated: Option<bool>,
}

/// A directory with agent sessions that could become a project (`AgentSessionProjectCandidate`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSessionProjectCandidate {
    pub path: String,
    pub title: String,
    /// The project already rooted at `path`, when there is one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<ProjectId>,
    pub sources: Vec<AgentSessionSource>,
    /// How many sessions an import would bring in.
    pub thread_count: u64,
    pub last_active_at: Option<Timestamp>,
    pub already_imported: bool,
    /// The repository the directory is a checkout of; `null` outside git.
    #[serde(default, deserialize_with = "double_option", skip_serializing_if = "Option::is_none")]
    pub git: Option<Option<AgentSessionProjectGit>>,
}

string_enum! {
    /// The agent whose sessions were found (`AgentSessionSource`).
    AgentSessionSource {
        ClaudeAgent = "claudeAgent",
        Codex = "codex",
        Pi = "pi",
    }
}

/// The repository a session directory belongs to (`AgentSessionProjectGit`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSessionProjectGit {
    /// The `origin` remote as `host/path`, the same however its URL is written.
    pub remote_key: Option<String>,
    /// The repository's path on its host, such as `owner/name`.
    pub repository: Option<String>,
}

/// What `agentSessions.import` answers (`AgentSessionImportResult`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSessionImportResult {
    pub imported_count: u64,
    pub skipped_count: u64,
}
