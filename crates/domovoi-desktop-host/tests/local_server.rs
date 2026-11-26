//! The host beside a real server on this machine: a fresh client finds the
//! server's descriptor, signs in by writing a request file, and connects with
//! no pairing code; signed out by the server, it signs itself in again. And
//! the host against a server it reaches over iroh: its RPC socket is an iroh
//! stream, not a WebSocket through the tunnel's loopback port.
//!
//! Needs a built server, named by `DOMOVOI_SERVER_BIN` (this crate does not
//! build one):
//!
//! ```sh
//! cargo build -p domovoi-server
//! DOMOVOI_SERVER_BIN=target/debug/domovoi-server \
//!   cargo test -p domovoi-desktop-host --test local_server -- --ignored --nocapture
//! ```

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use domovoi_core::Event;
use domovoi_core::view::ViewModel;
use domovoi_desktop_host::{HostConfig, HostEvent, Identity, SecureStore};

struct Server {
    child: Child,
    bin: PathBuf,
    home: PathBuf,
    port: u16,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

impl Server {
    fn command(bin: &Path, home: &Path, port: u16) -> Command {
        let mut command = Command::new(bin);
        command
            .env("DOMOVOI_NO_SESSION_IMPORT", "1")
            .env("DOMOVOI_NO_SHELL_ENV", "1")
            .env("DOMOVOI_AGENT_SESSIONS_HOME", home.join("agent-home"))
            .args(["--home-dir", home.to_str().expect("a UTF-8 path"), "--port", &port.to_string()]);
        command
    }

    fn start(bin: PathBuf) -> Server {
        let home = std::env::temp_dir().join(format!("domovoi-host-local-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let port = std::net::TcpListener::bind("127.0.0.1:0").expect("a free port").local_addr().expect("its address").port();
        let child = Self::command(&bin, &home, port).arg("--no-startup-code").stdout(Stdio::null()).stderr(Stdio::null()).spawn().expect("the server starts");
        let server = Server { child, bin, home, port };
        let deadline = Instant::now() + Duration::from_secs(60);
        while !server.descriptor().exists() {
            assert!(Instant::now() < deadline, "the server wrote no descriptor");
            std::thread::sleep(Duration::from_millis(100));
        }
        server
    }

    fn descriptor(&self) -> PathBuf {
        self.home.join("local").join("server.json")
    }

    /// `domovoi-server remote <args>` against this server; its stdout.
    fn remote(&self, args: &[&str]) -> String {
        let out = Self::command(&self.bin, &self.home, self.port).arg("remote").args(args).output().expect("the command runs");
        assert!(out.status.success(), "remote {args:?}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).into_owned()
    }
}

/// Applies host events to `view` until `done` holds for it.
async fn until(events: &mut tokio::sync::mpsc::Receiver<HostEvent>, view: &mut ViewModel, what: &str, done: impl Fn(&ViewModel) -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
    while !done(view) {
        let event = tokio::time::timeout_at(deadline, events.recv()).await.unwrap_or_else(|_| panic!("{what}: still {:?} ({:?})", view.connection.state, view.connection.error));
        if let Some(HostEvent::Delta(delta)) = event {
            delta.apply(view);
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a built server: DOMOVOI_SERVER_BIN"]
async fn a_fresh_client_beside_a_server_connects_with_no_code_and_signs_itself_in_again() {
    let bin = PathBuf::from(std::env::var_os("DOMOVOI_SERVER_BIN").expect("DOMOVOI_SERVER_BIN names the server"));
    let server = Server::start(std::fs::canonicalize(bin).expect("the server binary"));
    let url = format!("http://127.0.0.1:{}", server.port);

    let store = Arc::new(SecureStore::in_memory());
    let config = HostConfig { local_server: Some(server.descriptor()), ..HostConfig::for_tests(Identity::ICED) };
    let (host, mut events) = domovoi_desktop_host::start(Arc::clone(&store), config);
    let mut view = ViewModel::default();
    host.send(Event::Launch { surface: "desktop".into(), device_label: "Host Test".into(), device_os: "macOS".into() });

    // No code, no click: the descriptor is found, a request file written, and the socket opens.
    until(&mut events, &mut view, "the first connection", |view| view.connection.state == "connected" && !view.connection.environment_label.is_empty()).await;
    assert_eq!(view.connection.server_url, url);
    assert!(view.connection.local_server.is_none() && !view.connection.needs_pairing);
    let saved = store.load("environments").expect("the server is saved");
    assert!(saved.contains(&url), "{saved}");
    let id = store.load("activeEnvironment").expect("and is the active one");
    let first = store.load(&format!("token:{id}")).expect("with its token");
    assert_eq!(store.load("localAuto").as_deref(), Some("1"));
    assert_eq!(std::fs::read_dir(server.home.join("local").join("requests")).expect("the requests folder").count(), 0, "the request file was used up");

    // The server lists it as a local desktop client, and signs it out.
    let devices = server.remote(&["devices"]);
    let line = devices.lines().find(|line| line.contains("Host Test")).unwrap_or_else(|| panic!("listed: {devices}"));
    assert!(line.contains("desktop") && line.contains("local"), "{line}");
    let device = line.split_whitespace().next().expect("its id");
    assert!(server.remote(&["revoke", device]).contains("Signed out Host Test"));

    // Its socket is closed and its token refused: it asks for a new one by itself and is back.
    until(&mut events, &mut view, "the sign-out", |view| view.connection.state == "reconnecting" && view.connection.reconnect_in_seconds > 0).await;
    // The retry's timer is the UI's to run: here it fires at once.
    host.send(Event::RetryTimer { retry_gen: view.connection.retry_gen });
    until(&mut events, &mut view, "the second connection", |view| view.connection.state == "connected").await;
    let second = store.load(&format!("token:{id}")).expect("a token again");
    assert_ne!(first, second, "a new sign-in");
    assert!(!view.connection.needs_pairing);
    assert_eq!(store.load("environments").as_deref(), Some(saved.as_str()), "the same saved server");

    host.shutdown();
}

/// A server reached over iroh: the app pairs through the tunnel's loopback
/// port (HTTP), and its RPC socket goes onto an iroh stream of its own.
#[tokio::test(flavor = "multi_thread")]
#[ignore = "needs a built server: DOMOVOI_SERVER_BIN"]
async fn the_socket_of_a_server_reached_over_iroh_is_an_iroh_stream() {
    let bin = std::fs::canonicalize(PathBuf::from(std::env::var_os("DOMOVOI_SERVER_BIN").expect("DOMOVOI_SERVER_BIN names the server"))).expect("the server binary");
    let home = std::env::temp_dir().join(format!("domovoi-host-remote-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let port = std::net::TcpListener::bind("127.0.0.1:0").expect("a free port").local_addr().expect("its address").port();
    // Remote access on, reachable by its addresses only (no relay, no lookup).
    let child = Server::command(&bin, &home, port).args(["--no-startup-code", "--remote", "--relay", "disabled"]).stdout(Stdio::null()).stderr(Stdio::null()).spawn().expect("the server starts");
    let server = Server { child, bin, home, port };
    let deadline = Instant::now() + Duration::from_secs(60);
    while !server.descriptor().exists() {
        assert!(Instant::now() < deadline, "the server wrote no descriptor");
        std::thread::sleep(Duration::from_millis(100));
    }
    let pairing = Server::command(&server.bin, &server.home, server.port).args(["pair", "--remote"]).output().expect("pair runs");
    let printed = String::from_utf8_lossy(&pairing.stdout).into_owned();
    let link = printed.split_whitespace().find(|word| word.starts_with("domovoi://connect?")).unwrap_or_else(|| panic!("no pairing link in: {printed}")).to_owned();

    let store = Arc::new(SecureStore::in_memory());
    let (host, mut events) = domovoi_desktop_host::start(Arc::clone(&store), HostConfig::for_tests(Identity::ICED));
    let mut view = ViewModel::default();
    host.send(Event::Launch { surface: "desktop".into(), device_label: "Host Test".into(), device_os: "macOS".into() });
    host.send(Event::ConnectRemote { link });
    until(&mut events, &mut view, "the connection over iroh", |view| view.connection.state == "connected" && !view.connection.environment_label.is_empty()).await;

    // The server lists the app as connected, and served its socket as a stream.
    let devices = server.remote(&["devices"]);
    let line = devices.lines().find(|line| line.contains("Host Test")).unwrap_or_else(|| panic!("listed: {devices}"));
    assert!(line.contains("connected") || line.contains("online"), "{line}");
    let logs: String = std::fs::read_dir(server.home.join("userdata").join("logs"))
        .expect("the server's logs")
        .filter_map(|entry| std::fs::read_to_string(entry.ok()?.path()).ok())
        .collect();
    assert!(logs.contains("peer connected for RPC"), "the socket did not go onto an iroh stream:\n{}", logs.lines().filter(|line| line.contains("iroh")).collect::<Vec<_>>().join("\n"));

    host.shutdown();
}
