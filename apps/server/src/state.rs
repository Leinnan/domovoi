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
    /// The home's folders, made when missing: readable by this user only
    /// (mode 0700; on Windows [`Paths::make_private`] gives them their ACL).
    pub fn new(home: PathBuf) -> std::io::Result<Self> {
        let userdata = home.join("userdata");
        let logs = userdata.join("logs");
        let projects_root = home.join("projects");
        for d in [&home, &userdata, &logs] {
            create_private_dir(d)?;
        }
        Ok(Paths { home, userdata, logs, projects_root })
    }

    /// What in a home must be readable by this user only: the folders with
    /// the database, logs and iroh keys, and the files that are written
    /// outside them or were made before they were private. The home itself
    /// is not listed ([`Paths::make_private`] has its own rule for it).
    pub fn private_paths(home: &std::path::Path) -> Vec<PathBuf> {
        let userdata = home.join("userdata");
        let mut paths = vec![userdata.clone(), userdata.join("logs"), home.join("remote")];
        paths.extend(crate::db::files(&userdata.join("domovoi.sqlite")));
        paths.extend([home.join("remote.json"), home.join("remote").join("ticket"), userdata.join("attachment-signing-key")]);
        paths
    }

    /// Makes the home private, at every server start: a home made by an older
    /// server (or copied, or restored) had the modes the umask gave it, so
    /// other accounts on this machine could read every thread.
    ///
    /// Unix: [`Paths::private_paths`] become 0700 and 0600, and the home
    /// itself 0700 when group or others can read or write it. A home they can
    /// only pass through (0711) is left alone: that is how worktrees under it
    /// stay reachable for a server or agent of another user. Windows: the same
    /// paths get an ACL of their own; never the home itself, whose ACL would
    /// spread to `worktrees/` and `projects/`.
    ///
    /// Returns what could not be made private, a line each. Never a reason
    /// not to start.
    pub fn make_private(&self) -> Vec<String> {
        let mut warnings = Vec::new();
        #[cfg(unix)]
        if let Err(why) = make_home_private(&self.home) {
            warnings.push(format!("{} is left readable by other users: {why}", self.home.display()));
        }
        for path in Self::private_paths(&self.home) {
            if let Err(e) = crate::util::tighten(&path) {
                warnings.push(format!("{} could not be made private: {e}", path.display()));
            }
        }
        warnings
    }
    pub fn keybindings(&self) -> PathBuf {
        self.userdata.join("keybindings.json")
    }
}

/// Makes `dir` and what is missing above it; `dir` itself for this user only
/// (the folders above it get the modes the umask gives).
pub(crate) fn create_private_dir(dir: &std::path::Path) -> std::io::Result<()> {
    if dir.is_dir() {
        return Ok(());
    }
    if let Some(parent) = dir.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)?;
    }
    let made = {
        #[cfg(unix)]
        {
            std::os::unix::fs::DirBuilderExt::mode(&mut std::fs::DirBuilder::new(), 0o700).create(dir)
        }
        #[cfg(not(unix))]
        {
            std::fs::create_dir(dir)
        }
    };
    match made {
        // Another process (`pair` beside a starting server) made it meanwhile.
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && dir.is_dir() => Ok(()),
        made => made,
    }
}

/// Whether group or others can read or write a folder of this mode (passing
/// through it, mode 0711, is neither).
#[cfg(unix)]
pub fn open_to_others(mode: u32) -> bool {
    mode & 0o066 != 0
}

/// The home becomes 0700 when [`open_to_others`]; `Err` says why it was left.
#[cfg(unix)]
fn make_home_private(home: &std::path::Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let meta = std::fs::metadata(home).map_err(|e| e.to_string())?;
    if !open_to_others(meta.permissions().mode()) {
        return Ok(());
    }
    let resolved = std::fs::canonicalize(home).map_err(|e| e.to_string())?;
    if resolved.parent().is_none() {
        return Err("it is the root folder".into());
    }
    if dirs::home_dir().and_then(|h| std::fs::canonicalize(h).ok()).is_some_and(|h| h == resolved) {
        return Err("it is your own home folder; give the server a folder of its own with --home-dir".into());
    }
    if !crate::util::owned_by_this_user(&meta) {
        return Err("another user owns it".into());
    }
    std::fs::set_permissions(home, std::fs::Permissions::from_mode(0o700)).map_err(|e| e.to_string())
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
    /// The names this server answers to over plain TCP (`--allowed-host`).
    pub hosts: crate::http::HostRules,
    /// Sign-in without a code for apps on this machine.
    pub local: crate::local::Local,
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
