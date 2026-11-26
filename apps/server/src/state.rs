use crate::auth::{Auth, Session};
use crate::providers::ProviderRegistry;
use crate::settings::Settings;
use crate::store::Store;
use std::path::PathBuf;
use std::sync::Arc;
use domovoi_contracts::environment::EnvironmentDescriptor;
use domovoi_model::typed::{ServerConfigOf, ServerObservability, ServerProvider};
use tokio::sync::{RwLock, broadcast};

/// The server's config as it sends it: `ServerConfig` with the settings as
/// the settings file holds them.
pub type ServerConfig = ServerConfigOf<Settings>;

#[derive(Debug, Clone)]
pub struct Paths {
    pub home: PathBuf,
    pub userdata: PathBuf,
    pub logs: PathBuf,
    pub projects_root: PathBuf,
}

impl Paths {
    pub fn new(home: PathBuf) -> std::io::Result<Self> {
        let userdata = home.join("userdata");
        let logs = userdata.join("logs");
        let projects_root = home.join("projects");
        for d in [&home, &userdata, &logs] {
            std::fs::create_dir_all(d)?;
        }
        Ok(Paths { home, userdata, logs, projects_root })
    }
    pub fn keybindings(&self) -> PathBuf {
        self.userdata.join("keybindings.json")
    }
}

pub struct AppState {
    pub paths: Paths,
    pub environment: EnvironmentDescriptor,
    pub auth: Auth,
    pub store: Store,
    pub providers: ProviderRegistry,
    pub settings: RwLock<Settings>,
    /// Working directory the server was started in (welcome payload, config cwd).
    pub cwd: String,
    /// Identifies this server process (preview sessions are scoped to it).
    pub server_epoch: String,
    /// Every provider's status, sent after each change to one.
    pub provider_updates: broadcast::Sender<Vec<ServerProvider>>,
    pub terminals: crate::terminal::Terminals,
    pub orchestrator: Arc<crate::orchestration::commands::Orchestrator>,
    /// Open RPC sockets per session.
    pub connections: crate::connections::Connections,
    /// Remote access over iroh.
    pub remote: Arc<crate::remote::Remote>,
    /// APNs credentials are configured (`push.rs`).
    pub push_configured: bool,
    /// Cancelled to stop the server (a signal, the status bar icon, `domovoi-server stop`).
    pub shutdown: tokio_util::sync::CancellationToken,
    /// Poked when devices or connections change, to refresh `status`.
    pub pulse: Arc<tokio::sync::Notify>,
    /// The current status, for the status bar icon and status readers.
    pub status: tokio::sync::watch::Sender<crate::control::ServerStatus>,
}

pub type SharedState = Arc<AppState>;

/// Per-WebSocket context handed to RPC handlers.
pub struct ConnCtx {
    pub state: SharedState,
    pub connection_id: u64,
    /// The authenticated session; its permissions gate every RPC.
    pub session: Session,
}

impl AppState {
    pub async fn server_config(&self) -> ServerConfig {
        let kb = crate::keybindings::resolved(&self.paths.keybindings());
        ServerConfig {
            environment: (&self.environment).into(),
            auth: (&self.auth.descriptor()).into(),
            cwd: self.cwd.clone(),
            keybindings_config_path: self.paths.keybindings().to_string_lossy().into_owned(),
            keybindings: kb.keybindings.into(),
            issues: kb.issues.into(),
            providers: self.providers.snapshot().await.into(),
            available_editors: crate::rpc::available_editors().into(),
            remote_open_targets: None,
            direct_endpoints: None,
            terminal_details: Some(true),
            observability: ServerObservability {
                logs_directory_path: self.paths.logs.to_string_lossy().into_owned(),
                local_tracing_enabled: crate::trace_writer::enabled(),
                otlp_traces_source: crate::trace_writer::otlp_traces_url().map(|_| domovoi_model::typed::OtlpEndpointSource::Settings),
                otlp_traces_enabled: crate::trace_writer::otlp_traces_url().is_some(),
                otlp_traces_url: crate::trace_writer::otlp_traces_url(),
                otlp_traces_variable: None,
                otlp_metrics_url: None,
                otlp_metrics_source: None,
                otlp_metrics_variable: None,
                otlp_metrics_enabled: false,
                otlp_logs_url: None,
                otlp_logs_source: None,
                otlp_logs_variable: None,
                otlp_logs_enabled: Some(false),
            },
            settings: self.settings.read().await.clone(),
            managed_policy: crate::policy::current().summary(),
            shell_resume_completion_marker: Some(true),
            shell_thread_families: None,
            thread_link_metadata: None,
            shell_older_settled: None,
            shell_reveal_in_file_manager: None,
            shell_reveal_in_file_manager_kind: None,
            thread_resume_completion_marker: None,
            thread_snapshot_pagination: None,
            thread_find: None,
            thread_find_progressive: None,
            secret_provider_settings: None,
            secret_references: None,
            secret_request_references: None,
            scratch_workspace_root: None,
            new_projects_root: Some(self.paths.projects_root.to_string_lossy().into_owned()),
            last_event_loop_stall: None,
            environment_themes: None,
            usage_limit_sources: None,
        }
    }
}

/// The running server's state, for workers started before it exists (the
/// effect worker closes a deleted thread's terminals through it).
pub static STATE: std::sync::OnceLock<std::sync::Arc<AppState>> = std::sync::OnceLock::new();
