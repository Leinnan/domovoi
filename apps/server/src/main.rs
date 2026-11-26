//! domovoi-server: Rust execution server, wire-compatible with t3code's `apps/server`.

mod acp_cli;
mod acp_registry;
mod adapters;
mod agent_instructions;
mod antigravity;
mod artifacts;
mod attachments;
mod agent_sessions;
mod agent_sync;
mod auth;
mod autostart;
mod checkpoint;
mod claude_cli;
mod claude_limits;
mod cli;
#[cfg(test)]
mod command_differential;
mod config_failure;
mod connections;
mod control;
mod db;
mod diagrams;
mod doctor;
mod document_text;
mod folder_access;
mod gh;
mod glab;
mod http;
mod file_images;
mod jsonrpc;
mod keep_awake;
mod keybindings;
mod lfs;
mod lifecycle;
mod logging;
mod managed;
mod mcp;
mod orchestration;
#[cfg(test)]
mod perf_tests;
mod peer_links;
mod permissions;
mod pi_rpc;
mod policy;
mod preview;
mod project_icons;
mod project_scripts;
mod process;
mod provider_update;
mod providers;
mod push;
mod push_web;
#[cfg(test)]
mod record_dumps;
mod remote;
mod rpc;
mod service;
mod settings;
mod shell_env;
mod skills;
mod scheduled_tasks;
mod secrets;
#[cfg(test)]
mod sent_tests;
mod state;
mod store;
mod terminal;
#[cfg(test)]
mod test_records;
mod text_gen;
mod thread_permissions;
mod tool_images;
mod trace_diagnostics;
mod trace_writer;
#[cfg(feature = "tray")]
mod tray;
mod usage;
mod usage_pricing;
mod usage_limits;
mod usage_recovery;
mod util;
mod vcs;
mod vcs_ops;
mod webhooks;
#[cfg(windows)]
mod win;
mod worktree_setup;
mod workspace;

use clap::Parser;
use std::path::PathBuf;
use std::sync::Arc;
use domovoi_contracts::environment::{
    DATABASE_MIGRATION, EnvironmentDescriptor, ORCHESTRATION_PROTOCOL_VERSION, Platform, SERVER_VERSION,
};

#[derive(Parser)]
#[command(version, about)]
struct Args {
    /// Data directory (database, logs). Never point this at a live t3code home.
    #[arg(long, global = true, default_value_os_t = default_home())]
    home_dir: PathBuf,
    #[arg(long, global = true, default_value = "127.0.0.1")]
    host: String,
    #[arg(long, global = true, default_value_t = 13773)]
    port: u16,
    /// Origin of the web client, used to print the pairing URL.
    #[arg(long, global = true)]
    web_url: Option<String>,
    /// Serve remote clients over iroh (also on when enabled in `remote.json`).
    #[arg(long)]
    remote: bool,
    /// Show a status bar icon with remote access, pairing and Quit.
    #[cfg(feature = "tray")]
    #[arg(long)]
    tray: bool,
    /// Don't print a pairing URL at start (for a server started at login;
    /// mint codes with `domovoi-server pair` or the status bar icon instead).
    #[arg(long)]
    no_startup_code: bool,
    /// Don't add projects and sessions from Claude Code and Codex on their
    /// own, and don't follow new terminal work (the app's import still works).
    /// `DOMOVOI_NO_SESSION_IMPORT=1` does the same.
    #[arg(long)]
    no_session_import: bool,
    /// APNs auth key (`AuthKey_XXXX.p8`) for push to the iPhone app; also
    /// `keyPath` in `<home>/push.json`.
    #[arg(long, global = true)]
    apns_key: Option<std::path::PathBuf>,
    /// The APNs key's id (10 characters).
    #[arg(long, global = true)]
    apns_key_id: Option<String>,
    /// Your Apple Developer team id.
    #[arg(long, global = true)]
    apns_team_id: Option<String>,
    /// The iPhone app's bundle id (default `com.crayen.domovoi`).
    #[arg(long, global = true)]
    apns_topic: Option<String>,
    /// Keep thread titles out of pushes: alerts carry this machine's name
    /// and what happened, nothing else (`privateAlerts` in `push.json`).
    #[arg(long, global = true)]
    apns_private_alerts: bool,
    /// A self-hosted iroh relay (repeatable); without it n0's public relays are
    /// used. `disabled` keeps remote access on the local network; `relay-only`
    /// (for `connect`) sends everything through a relay.
    #[arg(long = "relay", global = true)]
    relays: Vec<String>,
    /// Also serve webhook task URLs (`/api/hooks/...`), and nothing else, on
    /// this address, such as `0.0.0.0:13780` behind a reverse proxy or tunnel.
    #[arg(long)]
    hooks_listen: Option<String>,
    /// The public origin webhook URLs are shown with, such as
    /// `https://hooks.example.com` (what the proxy in front of `--hooks-listen` answers on).
    #[arg(long)]
    hooks_url: Option<String>,
    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(clap::Subcommand)]
enum Cmd {
    /// Mint a one-time pairing code for a server using this home directory
    /// (works while the server is running).
    Pair {
        /// Minutes the code stays valid.
        #[arg(long, default_value_t = 5)]
        ttl_minutes: u64,
        /// Grant only these permissions (repeatable), e.g. `orchestration:read`.
        /// Without it the code grants t3code's default full access.
        #[arg(long = "permission")]
        permissions: Vec<String>,
        /// Print a `domovoi://connect` link and QR code for remote clients (iroh).
        #[arg(long)]
        remote: bool,
        /// Draw the QR code for a light terminal background (the default
        /// reads on a dark one).
        #[arg(long)]
        light: bool,
        /// Print the link, code and expiry as JSON, without a QR code.
        #[arg(long)]
        json: bool,
    },
    /// Remote access of the server using this home directory: status, on,
    /// off, devices, revoke.
    Remote {
        #[command(subcommand)]
        action: cli::RemoteCmd,
    },
    /// Stop the running server using this home directory.
    Stop,
    /// Check what a server using this home directory needs: the home
    /// folder, database, provider CLIs, git, gh, remote access, launch at
    /// login and the port. Exits nonzero when a check fails.
    Doctor {
        /// Print the checks as JSON.
        #[arg(long)]
        json: bool,
    },
    /// Domovoi's agent tools over stdio, for an ACP agent that starts MCP
    /// servers only as commands (started by the server, not by hand).
    #[command(hide = true)]
    AcpMcpBridge,
    /// Writes a document's text to stdout, for an attachment (started by
    /// the server, in a child of its own: `document_text.rs`).
    #[command(hide = true, name = "__document-text")]
    DocumentText { path: PathBuf, extension: String },
    /// `BROWSER` for Antigravity's agent: hands the Google sign-in link to
    /// the server instead of opening a browser (started by the agent).
    #[command(hide = true, name = "antigravity-browser-url")]
    AntigravityBrowserUrl {
        url: String,
        /// Append the link to this file instead of writing it to stderr
        /// (the CLI discards the stderr of what it opens links with).
        #[arg(long)]
        to: Option<PathBuf>,
    },
    /// Start the server with its status bar icon when you sign in: on, off
    /// or status. Uses this executable, home directory and port.
    Autostart {
        #[arg(value_parser = ["on", "off", "status"])]
        action: String,
    },
    /// The headless server as a systemd user service (Linux): install,
    /// uninstall or status. Uses this executable, home directory and port.
    Service {
        #[command(subcommand)]
        action: service::ServiceCmd,
    },
    /// Reach a remote Domovoi server over iroh: serve it on a loopback port, so the
    /// t3code web app (or any HTTP client) can use it.
    Connect {
        /// A `domovoi://connect?…` link, an iroh ticket or an endpoint id.
        link: String,
        /// Loopback port for the bridge (0 picks a free one).
        #[arg(long, default_value_t = 0)]
        bridge_port: u16,
    },
}

fn default_home() -> PathBuf {
    dirs::home_dir().unwrap_or_else(|| ".".into()).join(".domovoi")
}

fn machine_label() -> String {
    let run = |cmd: &str, args: &[&str]| {
        crate::process::output_blocking(crate::process::blocking_command(cmd).args(args), crate::process::QUICK).ok().filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned()).filter(|s| !s.is_empty())
    };
    let computer_name = if cfg!(target_os = "macos") { run("scutil", &["--get", "ComputerName"]) } else { None };
    computer_name.or_else(|| sysinfo::System::host_name().ok().as_deref().and_then(short_host_name)).unwrap_or_else(|| "This machine".into())
}

/// A host name without its domain (`fedora-4.home` → `fedora-4`).
fn short_host_name(name: &str) -> Option<String> {
    let short = name.trim().split('.').next().unwrap_or_default();
    (!short.is_empty()).then(|| short.to_owned())
}

type Fallible<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

fn runtime() -> std::io::Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_multi_thread().enable_all().build()
}

/// How long tasks still running after the server stopped get before the
/// process exits without them.
pub const RUNTIME_GRACE: std::time::Duration = std::time::Duration::from_secs(2);

/// Runs `serve` to its end, then gives what is still running (a reader
/// blocked on an agent's pipe, a turn mid-way) a moment and drops it. A
/// runtime simply dropped would wait for its blocking tasks forever, and the
/// process would never exit. Dropping the tasks ends their child processes.
pub fn finish(runtime: tokio::runtime::Runtime, serve: impl std::future::Future<Output = Fallible>) -> Fallible {
    let result = runtime.block_on(serve);
    runtime.shutdown_timeout(RUNTIME_GRACE);
    result
}

/// What `pair` and the server share: the home directory's database, keys and
/// identity.
struct Base {
    remote: Arc<remote::Remote>,
    db: db::Db,
    environment: EnvironmentDescriptor,
    auth: auth::Auth,
    /// Origin the pairing URL points at.
    web: String,
}

fn base(args: &Args, paths: &state::Paths) -> Fallible<Base> {
    let remote = Arc::new(remote::Remote::open(&paths.home, args.relays.clone())?);
    let _ = managed::HOME.set(paths.home.clone());
    // Asked once and kept: answered off the startup path, before the first launch needs it.
    std::thread::spawn(|| drop(managed::scratch_root()));
    let _ = vcs_ops::WORKTREES_DIR.set(paths.home.join("worktrees"));
    let db = db::Db::open(&paths.userdata.join("domovoi.sqlite"))?;
    let environment_id = match db.meta_get("environment_id")? {
        Some(id) => id,
        None => {
            let id = util::new_uuid();
            db.meta_set("environment_id", &id)?;
            id
        }
    };
    let environment = EnvironmentDescriptor {
        environment_id,
        label: machine_label(),
        platform: Platform::current(),
        server_version: SERVER_VERSION.into(),
        orchestration_protocol_version: ORCHESTRATION_PROTOCOL_VERSION,
        database_migration: DATABASE_MIGRATION,
        capabilities: domovoi_contracts::defaults::capabilities(),
    };
    let auth = auth::Auth::new(db.clone(), args.port, &paths.home.to_string_lossy());
    let web = args.web_url.clone().unwrap_or_else(|| format!("http://localhost:{}", args.port));
    Ok(Base { remote, db, environment, auth, web })
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        // `doctor` has printed what failed.
        Err(e) if e.is::<doctor::Failed>() => std::process::ExitCode::FAILURE,
        // A policy this build cannot enforce: t3code's exit code, so a supervisor can tell.
        Err(e) if e.is::<policy::PolicyFileError>() => {
            eprintln!("error: {e}");
            std::process::ExitCode::from(policy::EXIT_CODE)
        }
        Err(e) => {
            eprintln!("error: {e}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run() -> Fallible {
    #[allow(unused_mut)]
    let mut args = Args::parse();
    #[cfg(all(feature = "tray", target_os = "macos"))]
    if args.command.is_none() && std::env::current_exe().is_ok_and(|exe| autostart::is_app_executable(&exe)) {
        args.tray = true;
        args.no_startup_code = true;
    }
    // iroh brings reqwest's rustls backend without a crypto provider.
    let _ = rustls::crypto::ring::default_provider().install_default();
    if matches!(args.command, Some(Cmd::AcpMcpBridge)) {
        return runtime()?.block_on(mcp::bridge::run()).map_err(Into::into);
    }
    if let Some(Cmd::DocumentText { path, extension }) = &args.command {
        std::process::exit(document_text::worker_main(path, extension));
    }
    if let Some(Cmd::AntigravityBrowserUrl { url, to }) = &args.command {
        use std::io::Write;
        let line = antigravity::browser_url_line(url);
        match to {
            Some(file) => std::fs::OpenOptions::new().create(true).append(true).open(file)?.write_all(line.as_bytes())?,
            // A closed pipe (the sign-in was cancelled) is no reason to fall back to a browser.
            None => drop(std::io::stderr().write_all(line.as_bytes())),
        }
        return Ok(());
    }
    // Before the home directory is touched: doctor changes nothing.
    if let Some(Cmd::Doctor { json }) = &args.command {
        let _log = logging::init(None);
        shell_env::hydrate();
        return runtime()?.block_on(doctor::run(&args.home_dir, &args.host, args.port, &args.relays, *json));
    }
    let paths = state::Paths::new(args.home_dir.clone())?;
    match &args.command {
        Some(Cmd::Connect { link, bridge_port }) => {
            let _log = logging::init(None);
            runtime()?.block_on(connect(&paths, link, *bridge_port, &args.relays, args.web_url.as_deref()))
        }
        Some(Cmd::Pair { ttl_minutes, permissions, remote, light, json }) => {
            let _log = logging::init(None);
            let policy = policy::install_from_system()?;
            if *remote && !policy.allows_remote_clients() {
                return Err(policy.refuse_remote_clients(Some("so no link for another device is made".into())).into());
            }
            let base = base(&args, &paths)?;
            let output = if *json { PairOutput::Json } else if *remote { PairOutput::Remote { light: *light } } else { PairOutput::Local };
            runtime()?.block_on(pair(&base, *ttl_minutes, permissions, output))
        }
        Some(Cmd::Remote { action }) => {
            let _log = logging::init(None);
            policy::install_from_system()?;
            let base = base(&args, &paths)?;
            runtime()?.block_on(cli::remote(&base, &args.host, args.port, action))
        }
        // Run before the home directory is touched (above).
        Some(Cmd::AcpMcpBridge | Cmd::AntigravityBrowserUrl { .. } | Cmd::Doctor { .. } | Cmd::DocumentText { .. }) => Ok(()),
        Some(Cmd::Stop) => {
            let _log = logging::init(None);
            let base = base(&args, &paths)?;
            runtime()?.block_on(cli::stop(&base, &args.host, args.port))
        }
        Some(Cmd::Service { action }) => {
            let _log = logging::init(None);
            let base = base(&args, &paths)?;
            let exec = service::Exec::current(&paths.home, &args.host, args.port, &args.relays)?;
            service::service(&base, &exec, action)
        }
        Some(Cmd::Autostart { action }) => {
            let _log = logging::init(None);
            let entry = autostart::Entry::current(&paths.home, &args.host, args.port, &args.relays)?;
            let note = match action.as_str() {
                "on" => autostart::enable(&entry)?,
                "off" => {
                    autostart::disable()?;
                    None
                }
                _ => None,
            };
            println!("Launch at login: {}", if autostart::is_enabled() { "on" } else { "off" });
            if let Some(note) = note {
                println!("Next: {note}");
            }
            Ok(())
        }
        None => {
            // Before logging and the runtime start threads (it sets PATH).
            let shell = shell_env::hydrate();
            let _log = logging::init(Some(&paths.logs));
            tracing::info!(version = env!("CARGO_PKG_VERSION"), commit = env!("DOMOVOI_BUILD_COMMIT"), "domovoi-server starting");
            shell.log();
            // Before anything listens: a refused configuration never serves, not even briefly.
            let policy = policy::install_from_system()?;
            policy.check_reachability(&args.host, args.remote)?;
            let base = base(&args, &paths)?;
            if !policy.allows_remote_clients() {
                // Devices paired before the policy came are signed out (`revokePairedClientsThePolicyDisallows`).
                let revoked = base.auth.revoke_remote()?;
                if revoked > 0 {
                    tracing::info!(revoked, "signed out remote devices the managed policy does not allow");
                }
            }
            let shutdown = tokio_util::sync::CancellationToken::new();
            #[cfg(feature = "tray")]
            if args.tray {
                // The icon needs the main thread; the server gets its own.
                let stop = shutdown.clone();
                return tray::run(runtime()?, shutdown, move |on_ready| async move { serve(&args, paths, base, stop, on_ready).await });
            }
            finish(runtime()?, serve(&args, paths, base, shutdown, None))
        }
    }
}

/// Called once the server is serving, with the handle that drives it.
type OnReady = Box<dyn FnOnce(control::Control) + Send>;

/// What `pair` prints.
#[derive(Clone, Copy)]
enum PairOutput {
    /// The pairing URL for a client on this machine (the web app).
    Local,
    /// A `domovoi://connect` link and its QR code, for a phone.
    Remote { light: bool },
    /// Everything as JSON (scripts).
    Json,
}

/// `pair --json`.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
struct PairReport {
    #[serde(flatten)]
    link: domovoi_model::typed::RemotePairingLink,
    pairing_url: String,
    permissions: Vec<String>,
    remote_enabled: bool,
}

/// `domovoi-server pair`: a one-time code for a server using this home
/// directory (running or not: the code is in its database).
async fn pair(base: &Base, ttl_minutes: u64, permissions: &[String], output: PairOutput) -> Fallible {
    let ttl_minutes = ttl_minutes.clamp(1, auth::MAX_PAIRING_TTL.as_secs() / 60);
    let grant = if permissions.is_empty() { auth::Grant::full() } else { auth::Grant::limited(permissions.to_vec()) };
    let code = base.auth.issue_pairing_code_with(std::time::Duration::from_secs(ttl_minutes * 60), &grant)?;
    let expires_at = domovoi_contracts::time_fmt::iso_from_millis(domovoi_contracts::time_fmt::now_millis() + (ttl_minutes * 60_000) as i64);
    let pairing_url = format!("{}/pair#token={code}", base.web.trim_end_matches('/'));
    let remote_enabled = base.remote.settings().enabled;
    match output {
        PairOutput::Json => {
            let ticket = base.remote.ticket().await;
            let report = PairReport {
                link: domovoi_model::typed::RemotePairingLink {
                    link: remote::pairing_link(&ticket, &code, &base.environment.label),
                    code,
                    ticket,
                    endpoint_id: base.remote.endpoint_id().to_string(),
                    expires_at: expires_at.into(),
                },
                pairing_url,
                permissions: grant.permissions,
                remote_enabled,
            };
            println!("{}", serde_json::to_string_pretty(&report)?);
            return Ok(());
        }
        PairOutput::Remote { light } => {
            let link = remote::pairing_link(&base.remote.ticket().await, &code, &base.environment.label);
            if let Some(qr) = remote::terminal_qr(&link, !light) {
                println!("{qr}");
            }
            println!("link: {link}");
            if !remote_enabled {
                println!("note: remote access is off; start the server with --remote or turn it on in the app");
            }
        }
        PairOutput::Local => {
            println!("pairingUrl: {pairing_url}");
            if remote_enabled {
                println!("note: add --remote for a phone's link and QR code");
            }
        }
    }
    println!("code: {code} (valid {ttl_minutes} min, grants: {})", grant.permissions.join(", "));
    println!("expires: {expires_at}");
    Ok(())
}

/// Runs the server until `shutdown` is cancelled (a signal, the status bar
/// icon's Quit or `domovoi-server stop`), then cleans up.
async fn serve(args: &Args, paths: state::Paths, base: Base, shutdown: tokio_util::sync::CancellationToken, on_ready: Option<OnReady>) -> Fallible {
    let Base { remote, db, environment, auth, web } = base;
    let initial_settings = settings::load(&paths.userdata);
    // The code printed at start lives a day, so it only works from this machine.
    let pairing = if args.no_startup_code { None } else { Some(auth.issue_local_pairing_code(auth::STARTUP_PAIRING_TTL)?) };
    let push_config = push::PushConfig::load(
        &paths.home,
        push::PushConfig {
            key_path: args.apns_key.clone(),
            key_id: args.apns_key_id.clone(),
            team_id: args.apns_team_id.clone(),
            topic: args.apns_topic.clone(),
            private_alerts: args.apns_private_alerts.then_some(true),
        },
    );
    let apns = push::Apns::new(&push_config)?;
    let (provider_updates, _) = tokio::sync::broadcast::channel(16);
    let store = store::Store::new(db);
    let (orchestrator, effects) = orchestration::commands::Orchestrator::new(store.clone());
    let orchestrator = Arc::new(orchestrator);
    providers::init_claude_commands(&paths.userdata);
    usage_limits::init(&paths.userdata);
    skills::init(&paths.userdata);
    let host = adapters::Host::new(orchestrator.clone(), paths.userdata.clone(), paths.logs.join("provider"));
    let _ = adapters::HOST.set(host.clone());
    orchestration::effects::spawn(orchestrator.clone(), host, effects);
    // Before anything listens: runs a stopped server left open are settled,
    // and effects it left undone are queued again.
    let continuation = orchestration::recovery::take_restart_continuation(&store);
    orchestration::recovery::recover(orchestrator.clone());
    let replayed = orchestrator.replay_outbox();
    if replayed > 0 {
        tracing::info!(replayed, "queued effects a stopped server left undone");
    }
    let terminals = terminal::Terminals::load(store.db().clone(), initial_settings.save_terminal_screens.unwrap_or(false))?;
    #[cfg(unix)]
    {
        let activity = terminals.clone();
        let stopped = shutdown.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(2));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                tokio::select! {
                    () = stopped.cancelled() => break,
                    _ = interval.tick() => activity.sample_activity().await,
                }
            }
        });
    }
    let terminal_screens = terminals.clone();
    let shutdown_terminals = shutdown.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
        interval.tick().await;
        loop {
            tokio::select! {
                () = shutdown_terminals.cancelled() => break,
                _ = interval.tick() => {
                    let terminals = terminal_screens.clone();
                    let _ = tokio::task::spawn_blocking(move || terminals.capture_screens()).await;
                }
            }
        }
    });
    let pulse = Arc::new(tokio::sync::Notify::new());
    let state = Arc::new(state::AppState {
        paths,
        environment,
        auth,
        store,
        providers: providers::ProviderRegistry::default(),
        settings: tokio::sync::RwLock::new(initial_settings),
        cwd: std::env::current_dir()?.to_string_lossy().into_owned(),
        server_epoch: util::new_uuid(),
        provider_updates,
        terminals,
        orchestrator,
        connections: connections::Connections::new(pulse.clone()),
        remote: remote.clone(),
        push_configured: apns.is_some(),
        shutdown: shutdown.clone(),
        pulse,
        status: tokio::sync::watch::Sender::new(control::ServerStatus::default()),
    });
    let _ = state::STATE.set(state.clone());
    if state.settings.read().await.continue_threads_after_server_update != Some(false) {
        orchestration::recovery::continue_after_restart(state.orchestrator.clone(), continuation);
    }
    {
        let state = state.clone();
        tokio::spawn(async move {
            let projects = state.store.list_projects().unwrap_or_default();
            let settings = state.settings.read().await.clone();
            vcs_ops::auto_pull_projects(&projects, &settings).await;
        });
    }
    {
        // Background agents an older server recorded with their launch receipt as the result,
        // and Antigravity plan artifacts an older server recorded only as file changes.
        let store = state.store.clone();
        let logs = state.paths.logs.join("provider");
        let userdata = state.paths.userdata.clone();
        tokio::task::spawn_blocking(move || {
            let fixed = adapters::repair::background_receipts(&store, &logs);
            if fixed > 0 {
                tracing::info!("repaired {fixed} background agent result(s)");
            }
            let plans = adapters::repair::antigravity_plans(&store, &userdata);
            if plans > 0 {
                tracing::info!("repaired {plans} Antigravity plan(s)");
            }
            let subagents = adapters::repair::antigravity_subagents(&store, &userdata);
            if subagents > 0 {
                tracing::info!("repaired {subagents} Antigravity subagent(s)");
            }
        });
    }
    folder_access::spawn(state.clone(), shutdown.clone());
    control::spawn_status(state.clone());
    control::spawn_maintenance(state.clone());
    keep_awake::spawn(state.clone());
    if !args.no_session_import && std::env::var_os("DOMOVOI_NO_SESSION_IMPORT").is_none_or(|v| v.is_empty()) {
        agent_sync::spawn(state.clone());
    }
    if let Some(apns) = apns {
        tracing::info!("Push notifications on (APNs)");
        push::spawn(state.clone(), apns);
    }
    // Web Push to UnifiedPush endpoints needs no setup: its VAPID key is made on first use.
    push_web::spawn(state.clone());
    if args.remote || remote.settings().enabled {
        let state = state.clone();
        tokio::spawn(async move {
            // A failure is logged by `start` and shown in the status.
            let _ = state.remote.start(http::router(state.clone())).await;
        });
    }
    {
        let state = state.clone();
        tokio::spawn(async move { state.providers.refresh(None).await });
    }
    {
        // Snoozed threads wake when their time comes.
        let orchestrator = state.orchestrator.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(20));
            loop {
                tick.tick().await;
                let o = orchestrator.clone();
                let _ = tokio::task::spawn_blocking(move || o.wake_snoozed()).await;
            }
        });
    }
    {
        // A Claude session listed other commands: clients get them now.
        let state = state.clone();
        tokio::spawn(async move {
            loop {
                providers::CLAUDE_COMMANDS_CHANGED.notified().await;
                let all = state.providers.snapshot().await;
                let _ = state.provider_updates.send(all);
            }
        });
    }
    {
        // Accounts' usage limits stay fresh while a client watches them.
        let state = state.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(60));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            // The first tick is now: the boot-time probe reads them then.
            tick.tick().await;
            loop {
                tick.tick().await;
                if state.provider_updates.receiver_count() == 0 {
                    continue;
                }
                let all = state.providers.snapshot().await;
                providers::refresh_stale_limits(&all).await;
            }
        });
    }
    {
        // Spans go to `observability.otlpTracesUrl` too, from the moment it is saved.
        let otlp = |s: &settings::Settings| s.observability.as_ref().and_then(|o| o.otlp_traces_url.clone());
        trace_writer::set_otlp_traces_url(otlp(&*state.settings.read().await));
        let mut saved = settings::subscribe();
        tokio::spawn(async move {
            loop {
                match saved.recv().await {
                    Ok(s) => trace_writer::set_otlp_traces_url(otlp(&s)),
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                }
            }
        });
    }
    {
        // Usage reads gigabytes of provider logs; index the last month in the background.
        let state = state.clone();
        tokio::spawn(async move {
            let settings = state.settings.read().await.clone();
            let base = state.paths.home.clone();
            let _ = tokio::task::spawn_blocking(move || usage::warm(&agent_sessions::ScanEnv::new(&settings, &base))).await;
        });
    }
    // Recovers interrupted scheduled runs and starts their timer.
    if let Some(origin) = &args.hooks_url {
        webhooks::set_public_origin(origin);
    }
    scheduled_tasks::service(&state);
    // Threads a usage limit stopped resume (or wake) when it resets.
    usage_recovery::start(state.clone());
    if let Some(addr) = &args.hooks_listen {
        let hooks = tokio::net::TcpListener::bind(addr.as_str()).await?;
        tracing::info!("Serving webhook URLs on http://{}", hooks.local_addr()?);
        let (router, stop) = (http::hooks_router(state.clone()), shutdown.clone());
        tokio::spawn(async move {
            if let Err(e) = axum::serve(hooks, router).with_graceful_shutdown(stop.cancelled_owned()).await {
                tracing::warn!(error = %e, "the webhook listener stopped");
            }
        });
    }

    let listener = tokio::net::TcpListener::bind((args.host.as_str(), args.port)).await?;
    let local = listener.local_addr()?;
    tracing::info!("Listening on http://{local}");
    // Agents reach the MCP tools on this machine's loopback.
    let mcp_host = if local.ip().is_unspecified() || local.ip().is_loopback() { "127.0.0.1".to_owned() } else { local.ip().to_string() };
    let _ = mcp::ENDPOINT.set(format!("http://{mcp_host}:{}/mcp", local.port()));
    if !local.ip().is_loopback() {
        // Plain TCP sessions count as the local owner: no rate limit, no device binding.
        tracing::warn!("--host {} exposes the server beyond this machine without TLS; prefer --remote (iroh) for other devices", args.host);
    }
    if let Some(pairing) = &pairing {
        tracing::info!("Authentication required. Open T3 Code using the pairing URL.");
        // stdout, not the log: the code grants full access for a day.
        println!("pairingUrl: {}/pair#token={pairing}", web.trim_end_matches('/'));
    }
    lifecycle::watch_signals(shutdown.clone());
    // An entry left by an older copy of this executable is brought up to date.
    let login_entry = autostart::Entry::current(&state.paths.home, &args.host, args.port, &args.relays).ok();
    if let Some(entry) = &login_entry {
        autostart::ensure(entry);
    }
    if let Some(on_ready) = on_ready {
        on_ready(control::Control::new(state.clone(), login_entry));
    }
    let served = {
        let serving = axum::serve(listener, http::router(state.clone())).with_graceful_shutdown(shutdown.clone().cancelled_owned());
        let state = state.clone();
        let stop = shutdown.clone();
        tokio::select! {
            r = serving => r,
            // Graceful shutdown waits for open connections; sockets are told
            // to close, and anything still open after the grace period is cut.
            () = async move {
                stop.cancelled().await;
                state.connections.close_everything();
                tokio::time::sleep(SHUTDOWN_GRACE).await;
            } => Ok(()),
        }
    };
    // Turns this stop cuts off continue when the server is back.
    if let Some(host) = adapters::HOST.get() {
        orchestration::recovery::record_restart_continuation(&state.store, &host.live_runs());
        host.shutdown().await;
    }
    // Don't leave login shells running after the server exits.
    state.terminals.shutdown();
    remote.stop().await;
    tracing::info!("Stopped");
    Ok(served?)
}

/// How long open connections get to finish once shutdown starts.
const SHUTDOWN_GRACE: std::time::Duration = std::time::Duration::from_secs(3);

/// `domovoi-server connect`: a loopback bridge to a remote server.
async fn connect(paths: &state::Paths, link: &str, port: u16, relays: &[String], web_url: Option<&str>) -> Fallible {
    let (ticket, code) = remote::parse_link(link);
    let ticket = domovoi_remote::parse_ticket(&ticket)?;
    let key = remote::load_or_create_key(&paths.home.join("remote").join("client.key"))?;
    println!("device: {}", key.public());
    println!("dialing {}…", ticket.endpoint_addr().id);
    let bridge = domovoi_remote::Bridge::start(key, remote::network(relays)?, ticket, port).await?;
    let base = format!("http://127.0.0.1:{}", bridge.port());
    println!("bridge: {base}");
    if let Some(code) = code {
        println!("pairingUrl: {}/pair#token={code}", web_url.unwrap_or(&base).trim_end_matches('/'));
    }
    let mut last = String::new();
    loop {
        tokio::select! {
            _ = tokio::signal::ctrl_c() => break,
            () = tokio::time::sleep(std::time::Duration::from_secs(2)) => {
                let s = bridge.status();
                let line = format!("connected: {}, path: {:?}, rtt: {:?} ms, error: {:?}", s.connected, s.path, s.rtt_ms, s.error);
                if line != last {
                    println!("{line}");
                    last = line;
                }
            }
        }
    }
    bridge.stop().await;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_machine_label_drops_the_domain() {
        assert_eq!(super::short_host_name("fedora-4.home").as_deref(), Some("fedora-4"));
        assert_eq!(super::short_host_name("box").as_deref(), Some("box"));
        assert_eq!(super::short_host_name(" "), None);
    }
}
