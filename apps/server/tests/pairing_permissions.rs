//! `domovoi-server pair` mints codes for a running server, and the permissions a
//! code grants are enforced per RPC with t3code's authorization error.

use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};

/// The socket URL names the version of the protocol the test speaks.
const PROTOCOL: u32 = domovoi_model::PROTOCOL_VERSION;

/// iroh (through domovoi-remote) turns on reqwest's rustls backend without a crypto
/// provider, so every client needs one installed first.
fn http_client() -> reqwest::Client {
    let _ = rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::new()
}


struct Server {
    child: Child,
    port: u16,
    home: PathBuf,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

fn start() -> Server {
    let home = std::env::temp_dir().join(format!("domovoi-perm-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    start_in(home)
}

/// Starts a server on the home directory `home`, as it is.
fn start_in(home: PathBuf) -> Server {
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let mut child = Command::new(env!("CARGO_BIN_EXE_domovoi-server")).env("DOMOVOI_NO_SESSION_IMPORT", "1").env("DOMOVOI_NO_SHELL_ENV", "1")
        .args(["--home-dir", home.to_str().unwrap(), "--port", &port.to_string()])
        // An empty agent home: the developer's provider logs are not indexed as the server starts.
        .env("DOMOVOI_AGENT_SESSIONS_HOME", home.join("agent-home"))
        .env("RUST_LOG", "warn")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let out = child.stdout.take().unwrap();
    for line in BufReader::new(out).lines() {
        if line.unwrap().contains("#token=") {
            break;
        }
    }
    Server { child, port, home }
}

/// Runs `domovoi-server pair` against the server's home directory; returns the code.
fn pair_code(server: &Server, permissions: &[&str]) -> String {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_domovoi-server"));
    cmd.env("DOMOVOI_NO_SESSION_IMPORT", "1").env("DOMOVOI_NO_SHELL_ENV", "1");
    cmd.args(["--home-dir", server.home.to_str().unwrap(), "--port", &server.port.to_string(), "pair"]);
    for p in permissions {
        cmd.args(["--permission", p]);
    }
    let out = cmd.output().unwrap();
    assert!(out.status.success(), "pair failed: {}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8_lossy(&out.stdout);
    let line = text.lines().find(|l| l.contains("#token=")).expect("pairing URL printed");
    line.split("#token=").nth(1).unwrap().trim().to_owned()
}

async fn connect(server: &Server, code: &str) -> tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>> {
    let base = format!("http://127.0.0.1:{}", server.port);
    let session = sign_in(&base, code).await;
    let mut req = format!("ws://127.0.0.1:{}/ws?protocol={PROTOCOL}", server.port).into_client_request().unwrap();
    req.headers_mut().insert("authorization", session.parse().unwrap());
    tokio_tungstenite::connect_async(req).await.unwrap().0
}

/// Sends one request and returns its first Chunk or Exit.
async fn call(ws: &mut tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>, id: &str, tag: &str, payload: Value) -> Value {
    let req = json!({ "_tag": "Request", "id": id, "tag": tag, "payload": payload, "headers": [] });
    ws.send(Message::Text(req.to_string().into())).await.unwrap();
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(10), ws.next()).await.expect("reply").unwrap().unwrap();
        if let Message::Text(t) = msg {
            let v: Value = serde_json::from_str(&t).unwrap();
            if v["requestId"] == json!(id) {
                return v;
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn pair_command_and_permission_enforcement() {
    let server = start();

    // A read-only code minted while the server runs.
    let read_only = pair_code(&server, &["orchestration:read"]);
    let mut ws = connect(&server, &read_only).await;
    let shell = call(&mut ws, "1", "orchestration.subscribeShell", json!({})).await;
    assert_eq!(shell["_tag"], "Chunk", "reading is allowed: {shell}");
    let denied = call(&mut ws, "2", "terminal.open", json!({ "threadId": "t", "terminalId": "term-1", "cwd": "/tmp" })).await;
    assert_eq!(denied["exit"]["_tag"], "Failure", "{denied}");
    let error = &denied["exit"]["cause"][0]["error"];
    assert_eq!(error["_tag"], "EnvironmentAuthorizationError");
    assert_eq!(error["requiredPermission"], "terminal:operate");
    let denied = call(&mut ws, "3", "projects.mutate", json!({ "type": "project.delete", "commandId": "c", "projectId": "p" })).await;
    assert_eq!(denied["exit"]["cause"][0]["error"]["requiredPermission"], "orchestration:operate");

    let no_details = call(&mut ws, "4", "domovoi.terminals.subscribe", json!({})).await;
    assert_eq!(no_details["exit"]["cause"][0]["error"]["requiredPermission"], "terminal:read");

    // The default code grants full access, and each code works only once.
    let full = pair_code(&server, &[]);
    let mut ws = connect(&server, &full).await;
    // The root of the drive the server runs on: `/` has no drive on Windows.
    let root = if cfg!(windows) { std::env::temp_dir().ancestors().last().map(|r| r.to_string_lossy().into_owned()).unwrap_or_default() } else { "/".to_owned() };
    let browse = call(&mut ws, "1", "filesystem.browse", json!({ "partialPath": root })).await;
    assert_eq!(browse["exit"]["_tag"], "Success", "{browse}");
    // A separately paired observer can read the live PTY without changing
    // its grid or sending input. The desktop remains the sole size owner.
    let terminal = json!({ "threadId": "t", "terminalId": "watch", "cwd": std::env::temp_dir(), "cols": 101, "rows": 41 });
    let desktop = call(&mut ws, "2", "terminal.attach", terminal).await;
    let read_code = pair_code(&server, &["terminal:read"]);
    let mut observer = connect(&server, &read_code).await;
    let reference = json!({ "threadId": "t", "terminalId": "watch" });
    let watched = call(&mut observer, "1", "terminal.observe", reference.clone()).await;
    assert_eq!(desktop["values"][0]["snapshot"]["pid"], watched["values"][0]["snapshot"]["pid"]);
    assert_eq!(watched["values"][0]["snapshot"]["cols"], 101);
    let written = call(&mut observer, "2", "terminal.write", json!({ "threadId": "t", "terminalId": "watch", "data": "pwd" })).await;
    assert_eq!(written["exit"]["cause"][0]["error"]["requiredPermission"], "terminal:operate");
    let details = call(&mut observer, "3", "domovoi.terminals.subscribe", json!({})).await;
    assert_eq!(details["values"][0][0]["terminalId"], "watch");
    assert!(details["values"][0][0].get("history").is_none());
    let closed = call(&mut ws, "3", "terminal.close", reference).await;
    assert_eq!(closed["exit"]["_tag"], "Success");

    let again = try_sign_in(&format!("http://127.0.0.1:{}", server.port), &full).await;
    assert_eq!(again.status(), 401, "a code works once");
}

/// What other accounts on the machine can do with `path` (its group and other mode bits).
#[cfg(unix)]
fn open_bits(path: &std::path::Path) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap_or_else(|e| panic!("{}: {e}", path.display())).permissions().mode() & 0o077
}

/// A home an older server made, with the modes the umask gave: starting the
/// server closes it to other accounts, and `pair` beside the running server
/// (which opens the database too) leaves it closed.
#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn the_home_is_closed_to_other_accounts_at_every_start() {
    use std::os::unix::fs::PermissionsExt;
    let home = std::env::temp_dir().join(format!("domovoi-perm-modes-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let userdata = home.join("userdata");
    std::fs::create_dir_all(userdata.join("logs")).unwrap();
    std::fs::create_dir_all(home.join("remote")).unwrap();
    std::fs::create_dir_all(home.join("worktrees")).unwrap();
    let old = [userdata.join("attachment-signing-key"), home.join("remote.json"), home.join("remote").join("ticket"), userdata.join("logs").join("server.old.log")];
    for file in &old {
        std::fs::write(file, if file.ends_with("remote.json") { "{}" } else { "x" }).unwrap();
        std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o644)).unwrap();
    }
    for dir in [&home, &userdata, &userdata.join("logs"), &home.join("remote"), &home.join("worktrees")] {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    let server = start_in(home.clone());
    let database = userdata.join("domovoi.sqlite");
    let private = |when: &str| {
        for path in [&home, &userdata, &userdata.join("logs"), &home.join("remote"), &database, &old[0], &old[1], &old[2]] {
            assert_eq!(open_bits(path), 0, "{} {when}", path.display());
        }
        for suffix in ["-wal", "-shm"] {
            let beside = std::path::PathBuf::from(format!("{}{suffix}", database.display()));
            if beside.exists() {
                assert_eq!(open_bits(&beside), 0, "{} {when}", beside.display());
            }
        }
    };
    private("after start");
    // Not everything under the home is the server's to close: worktrees keep their modes.
    assert_eq!(open_bits(&home.join("worktrees")), 0o055);
    // A file inside a closed folder is out of reach whatever its own mode says.
    assert_eq!(open_bits(&old[3]), 0o044);

    pair_code(&server, &[]);
    private("after pair");
    let remote = cli_pair_remote(&server);
    assert!(remote.contains("link: domovoi://connect"), "{remote}");
    private("after pair --remote");
    drop(server);

    // A home others may pass through but not list (for worktrees another
    // account's agent works in) is left as its owner set it.
    let home = std::env::temp_dir().join(format!("domovoi-perm-modes-through-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    std::fs::set_permissions(&home, std::fs::Permissions::from_mode(0o711)).unwrap();
    let server = start_in(home.clone());
    assert_eq!(open_bits(&home), 0o011);
    assert_eq!(open_bits(&home.join("userdata")), 0, "what holds the threads is closed all the same");
    assert_eq!(open_bits(&home.join("userdata").join("domovoi.sqlite")), 0);
    drop(server);
}

#[cfg(unix)]
fn cli_pair_remote(server: &Server) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_domovoi-server"))
        .env("DOMOVOI_NO_SESSION_IMPORT", "1")
        .env("DOMOVOI_NO_SHELL_ENV", "1")
        .args(["--home-dir", server.home.to_str().unwrap(), "--port", &server.port.to_string(), "pair", "--remote"])
        .output()
        .unwrap();
    assert!(out.status.success(), "pair --remote failed: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// Exchanges a pairing code for a session; the `Authorization` header that presents it.
async fn sign_in(base: &str, code: &str) -> String {
    let res = try_sign_in(base, code).await;
    assert_eq!(res.status(), 200);
    let token: Value = res.json().await.unwrap();
    format!("Bearer {}", token["access_token"].as_str().expect("an access token"))
}

/// The answer to a pairing code offered for a session.
async fn try_sign_in(base: &str, code: &str) -> reqwest::Response {
    let body = format!(
        "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Atoken-exchange&subject_token={code}&subject_token_type=urn%3Adomovoi%3Aparams%3Aoauth%3Atoken-type%3Aenvironment-bootstrap&requested_token_type=urn%3Aietf%3Aparams%3Aoauth%3Atoken-type%3Aaccess_token"
    );
    http_client().post(format!("{base}/oauth/token")).header("content-type", "application/x-www-form-urlencoded").body(body).send().await.unwrap()
}
