//! Remote access over iroh against the real binary (`--remote --relay
//! disabled`, so nothing leaves this machine): the pairing link, the gate for
//! unknown peers, bearer token exchange, WebSocket tickets, sessions bound to
//! one device key, and revocation closing the device's socket.

use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;
use domovoi_remote::{Bridge, Network, SecretKey};
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};

fn http_client() -> reqwest::Client {
    let _ = rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::new()
}

struct Server {
    child: Child,
    port: u16,
    home: PathBuf,
    /// The pairing code printed at start.
    startup_code: String,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

fn start() -> Server {
    start_as("pairing")
}

/// Each test gets its own home directory (they run in parallel).
fn start_as(tag: &str) -> Server {
    let home = std::env::temp_dir().join(format!("domovoi-remote-access-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    // `DOMOVOI_SERVER_BIN` names another build of the server, to compare two.
    let bin = std::env::var("DOMOVOI_SERVER_BIN").unwrap_or_else(|_| env!("CARGO_BIN_EXE_domovoi-server").to_owned());
    let mut child = Command::new(bin).env("DOMOVOI_NO_SESSION_IMPORT", "1").env("DOMOVOI_NO_SHELL_ENV", "1")
        // An empty agent home, as in `rpc_calls`: with the developer's, the
        // server indexes their provider logs for the usage page as it starts
        // (gigabytes, in a debug build), and a reply over the tunnel misses
        // its ten seconds.
        .env("DOMOVOI_AGENT_SESSIONS_HOME", home.join("agent-home"))
        .args(["--home-dir", home.to_str().unwrap(), "--port", &port.to_string(), "--remote", "--relay", "disabled"])
        .env("RUST_LOG", "info")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let out = child.stdout.take().unwrap();
    let mut startup_code = String::new();
    for line in BufReader::new(out).lines() {
        if let Some(code) = line.unwrap().split("#token=").nth(1) {
            startup_code = code.trim().to_owned();
            break;
        }
    }
    let ticket = home.join("remote").join("ticket");
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while std::fs::read_to_string(&ticket).map(|t| t.trim().is_empty()).unwrap_or(true) {
        assert!(std::time::Instant::now() < deadline, "remote access never started");
        std::thread::sleep(Duration::from_millis(100));
    }
    Server { child, port, home, startup_code }
}

/// `domovoi-server pair --remote`: returns the `domovoi://connect` link.
fn pair_link(home: &Path, port: u16) -> String {
    pair_link_with(home, port, &[])
}

/// As [`pair_link`], granting only `permissions` (all when empty).
fn pair_link_with(home: &Path, port: u16, permissions: &[&str]) -> String {
    let mut args = vec!["--home-dir".to_owned(), home.to_str().unwrap().to_owned(), "--port".to_owned(), port.to_string(), "pair".to_owned(), "--remote".to_owned()];
    for p in permissions {
        args.extend(["--permission".to_owned(), (*p).to_owned()]);
    }
    let out = Command::new(env!("CARGO_BIN_EXE_domovoi-server")).env("DOMOVOI_NO_SESSION_IMPORT", "1").env("DOMOVOI_NO_SHELL_ENV", "1")
        .args(&args)
        .output()
        .unwrap();
    assert!(out.status.success(), "pair failed: {}", String::from_utf8_lossy(&out.stderr));
    let text = String::from_utf8_lossy(&out.stdout);
    let link = text.lines().find_map(|l| l.strip_prefix("link: ")).expect("link printed").to_owned();
    assert!(text.contains('█') || text.contains('▀'), "a QR code is printed");
    link
}

fn parse_link(link: &str) -> (String, String) {
    let query = link.strip_prefix("domovoi://connect?").expect("domovoi link");
    let get = |key: &str| {
        query.split('&').find_map(|kv| kv.strip_prefix(&format!("{key}="))).map(|v| v.replace('+', " ")).unwrap_or_default()
    };
    (get("ticket"), get("code"))
}

async fn bridge(ticket: &str, key: SecretKey) -> Bridge {
    Bridge::start(key, Network::Direct { port: 0 }, domovoi_remote::parse_ticket(ticket).unwrap(), 0).await.unwrap()
}

async fn exchange(base: &str, code: &str) -> reqwest::Response {
    let body = format!(
        "grant_type={}&subject_token={code}&subject_token_type={}&requested_token_type={}&client_label=Test%20Phone&client_device_type=mobile&client_os=iOS",
        "urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Atoken-exchange",
        "urn%3At3%3Aparams%3Aoauth%3Atoken-type%3Aenvironment-bootstrap",
        "urn%3Aietf%3Aparams%3Aoauth%3Atoken-type%3Aaccess_token",
    );
    http_client().post(format!("{base}/oauth/token")).header("content-type", "application/x-www-form-urlencoded").body(body).send().await.unwrap()
}

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn open_ws(base: &str, token: &str) -> Ws {
    let res = http_client().post(format!("{base}/api/auth/websocket-ticket/rpc")).bearer_auth(token).send().await.unwrap();
    assert_eq!(res.status(), 200);
    let ticket = res.json::<Value>().await.unwrap()["ticket"].as_str().unwrap().to_owned();
    let url = format!("{}/ws?orchestrationProtocol=2&wsTicket={ticket}", base.replacen("http", "ws", 1));
    tokio_tungstenite::connect_async(url.into_client_request().unwrap()).await.unwrap().0
}

async fn rpc(ws: &mut Ws, id: &str, tag: &str) -> Option<Value> {
    let req = json!({ "_tag": "Request", "id": id, "tag": tag, "payload": {}, "headers": [] });
    ws.send(Message::Text(req.to_string().into())).await.ok()?;
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(10), ws.next()).await.ok()??.ok()?;
        let Message::Text(t) = msg else { continue };
        let v: Value = serde_json::from_str(&t).unwrap();
        if v["requestId"] == id {
            return Some(v);
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn remote_devices_pair_and_stay_bound_to_their_key() {
    let server = start();
    let (ticket, code) = parse_link(&pair_link(&server.home, server.port));
    assert!(!code.is_empty());

    let phone_key = SecretKey::generate();
    let phone = bridge(&ticket, phone_key.clone()).await;
    let base = format!("http://127.0.0.1:{}", phone.port());

    // An unknown device may discover and pair, nothing else.
    assert_eq!(http_client().get(format!("{base}/.well-known/t3/environment")).send().await.unwrap().status(), 200);
    assert_eq!(http_client().get(format!("{base}/api/orchestration/shell")).send().await.unwrap().status(), 401);
    assert_eq!(exchange(&base, "WRONGCODE123").await.status(), 401);
    // The code printed at start lives a day: it works on this machine only.
    assert_eq!(exchange(&base, &server.startup_code).await.status(), 401);
    assert_eq!(exchange(&format!("http://127.0.0.1:{}", server.port), &server.startup_code).await.status(), 200, "refusing it over iroh did not use it up");

    // Token exchange.
    let res = exchange(&base, &code).await;
    assert_eq!(res.status(), 200);
    let token_res: Value = res.json().await.unwrap();
    assert_eq!(token_res["token_type"], "Bearer");
    assert!(token_res["expires_in"].as_i64().unwrap() > 0);
    let token = token_res["access_token"].as_str().unwrap().to_owned();
    assert_eq!(exchange(&base, &code).await.status(), 401, "codes work once");

    let session: Value = http_client().get(format!("{base}/api/auth/session")).bearer_auth(&token).send().await.unwrap().json().await.unwrap();
    assert_eq!(session["authenticated"], true);
    assert_eq!(session["sessionMethod"], "bearer-access-token");
    assert_eq!(http_client().get(format!("{base}/api/orchestration/shell")).bearer_auth(&token).send().await.unwrap().status(), 200);

    // Compressed responses pass through the bridge as they are.
    let runtime = format!("{base}/api/assets/artifact-runtime/v1/runtime.js");
    let plain = http_client().get(&runtime).bearer_auth(&token).send().await.unwrap().bytes().await.unwrap();
    let gzipped = http_client().get(&runtime).bearer_auth(&token).header("accept-encoding", "gzip").send().await.unwrap();
    assert_eq!(gzipped.headers().get("content-encoding").and_then(|v| v.to_str().ok()), Some("gzip"));
    let mut unzipped = Vec::new();
    std::io::Read::read_to_end(&mut flate2::read::GzDecoder::new(&gzipped.bytes().await.unwrap()[..]), &mut unzipped).unwrap();
    assert_eq!(unzipped, plain);

    // RPC over the WebSocket, opened with a ticket.
    let mut ws = open_ws(&base, &token).await;
    let config = rpc(&mut ws, "1", "server.getConfig").await.expect("reply");
    assert_eq!(config["exit"]["_tag"], "Success", "{config}");

    // The token is useless over plain TCP and from another device key.
    let local = format!("http://127.0.0.1:{}", server.port);
    assert_eq!(http_client().get(format!("{local}/api/orchestration/shell")).bearer_auth(&token).send().await.unwrap().status(), 401);
    let thief = bridge(&ticket, SecretKey::generate()).await;
    let thief_base = format!("http://127.0.0.1:{}", thief.port());
    assert_eq!(http_client().get(format!("{thief_base}/api/orchestration/shell")).bearer_auth(&token).send().await.unwrap().status(), 401);

    // Remote status (Domovoi only) for a device with access:read.
    let status: Value = http_client().get(format!("{base}/api/domovoi/remote")).bearer_auth(&token).send().await.unwrap().json().await.unwrap();
    assert_eq!(status["running"], true);
    assert_eq!(status["endpointId"].as_str().unwrap(), domovoi_remote::parse_ticket(&ticket).unwrap().endpoint_addr().id.to_string());

    // The local owner sees the device in the access list and revokes it.
    let local_code = {
        let out = Command::new(env!("CARGO_BIN_EXE_domovoi-server")).env("DOMOVOI_NO_SESSION_IMPORT", "1").env("DOMOVOI_NO_SHELL_ENV", "1")
            .args(["--home-dir", server.home.to_str().unwrap(), "--port", &server.port.to_string(), "pair"])
            .output()
            .unwrap();
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        text.lines().find_map(|l| l.split("#token=").nth(1)).unwrap().trim().to_owned()
    };
    let res = http_client().post(format!("{local}/api/auth/browser-session")).json(&json!({ "credential": local_code })).send().await.unwrap();
    let cookie = res.headers().get("set-cookie").unwrap().to_str().unwrap().split(';').next().unwrap().to_owned();
    let mut req = format!("ws://127.0.0.1:{}/ws?orchestrationProtocol=2", server.port).into_client_request().unwrap();
    req.headers_mut().insert("cookie", cookie.parse().unwrap());
    let mut owner = tokio_tungstenite::connect_async(req).await.unwrap().0;
    let snapshot = loop {
        let v = rpc(&mut owner, "a", "subscribeAuthAccess").await.expect("access snapshot");
        if v["_tag"] == "Chunk" {
            break v["values"][0].clone();
        }
    };
    let phone_entry = snapshot["payload"]["clientSessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["method"] == "bearer-access-token" && c["connected"] == true)
        .expect("the phone is listed as connected")
        .clone();
    assert_eq!(phone_entry["client"]["label"], "Test Phone");
    assert_eq!(phone_entry["client"]["deviceType"], "mobile");
    assert_eq!(phone_entry["connected"], true);

    let revoked: Value = http_client()
        .post(format!("{local}/api/auth/clients/revoke"))
        .header("cookie", &cookie)
        .json(&json!({ "sessionId": phone_entry["sessionId"] }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(revoked["revoked"], true);

    // The phone's socket gets a Close frame (not just a dropped connection,
    // which URLSession can take half a minute to notice) and its token stops working.
    let closed = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match ws.next().await {
                Some(Ok(Message::Close(_))) => return true,
                None | Some(Err(_)) => return false,
                _ => {}
            }
        }
    })
    .await;
    assert_eq!(closed.ok(), Some(true), "the revoked device's socket got no Close frame");
    assert_eq!(http_client().get(format!("{base}/api/orchestration/shell")).bearer_auth(&token).send().await.unwrap().status(), 401);

    phone.stop().await;
    thief.stop().await;
    drop(phone_key);
}

/// Runs `domovoi-server <args>` against a server's home directory; returns stdout.
fn cli(server: &Server, args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_domovoi-server")).env("DOMOVOI_NO_SESSION_IMPORT", "1").env("DOMOVOI_NO_SHELL_ENV", "1")
        .args(["--home-dir", server.home.to_str().unwrap(), "--port", &server.port.to_string()])
        .args(args)
        .output()
        .unwrap();
    assert!(out.status.success(), "domovoi-server {args:?} failed: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A local browser session (the machine's owner): its cookie.
async fn owner_cookie(server: &Server) -> String {
    let text = cli(server, &["pair"]);
    let code = text.lines().find_map(|l| l.split("#token=").nth(1)).unwrap().trim().to_owned();
    let res = http_client()
        .post(format!("http://127.0.0.1:{}/api/auth/browser-session", server.port))
        .json(&json!({ "credential": code }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    res.headers().get("set-cookie").unwrap().to_str().unwrap().split(';').next().unwrap().to_owned()
}

async fn paired(ticket: &str, code: &str) -> (Bridge, String, String) {
    let bridge = bridge(ticket, SecretKey::generate()).await;
    let base = format!("http://127.0.0.1:{}", bridge.port());
    let res = exchange(&base, code).await;
    assert_eq!(res.status(), 200);
    let token = res.json::<Value>().await.unwrap()["access_token"].as_str().unwrap().to_owned();
    (bridge, base, token)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_remote_device_cannot_cut_remote_access_or_gain_permissions() {
    let server = start_as("guard");
    let local = format!("http://127.0.0.1:{}", server.port);
    let cookie = owner_cookie(&server).await;
    // A phone that may manage access and read threads, nothing more.
    let granted = ["access:read", "access:write", "orchestration:read"];
    let (ticket, code) = parse_link(&pair_link_with(&server.home, server.port, &granted));
    let (phone, base, token) = paired(&ticket, &code).await;
    let post = |path: &str, body: Value| http_client().post(format!("{base}{path}")).bearer_auth(&token).json(&body).send();

    // Turning remote access off, changing relays or restarting it would cut
    // every remote device off: refused from a remote device.
    assert_eq!(post("/api/domovoi/remote/configure", json!({ "enabled": false })).await.unwrap().status(), 403);
    assert_eq!(post("/api/domovoi/remote/configure", json!({ "enabled": true, "relays": ["disabled"] })).await.unwrap().status(), 403);
    assert_eq!(post("/api/domovoi/remote/configure", json!({ "enabled": true, "relays": ["https://relay.invalid"] })).await.unwrap().status(), 403);
    assert_eq!(post("/api/domovoi/server/stop", json!({})).await.unwrap().status(), 403);
    // Asking for what is already so answers with the status and restarts nothing.
    let same = post("/api/domovoi/remote/configure", json!({ "enabled": true })).await.unwrap();
    assert_eq!(same.status(), 200);
    assert_eq!(same.json::<Value>().await.unwrap()["running"], true);
    assert_eq!(http_client().get(format!("{base}/api/orchestration/shell")).bearer_auth(&token).send().await.unwrap().status(), 200);

    // A code minted by the phone grants at most what the phone holds.
    let minted: Value = post("/api/domovoi/remote/pairing", json!({})).await.unwrap().json().await.unwrap();
    let (tablet, tablet_base, tablet_token) = paired(&ticket, minted["code"].as_str().unwrap()).await;
    let session: Value = http_client().get(format!("{tablet_base}/api/auth/session")).bearer_auth(&tablet_token).send().await.unwrap().json().await.unwrap();
    let mut got: Vec<&str> = session["permissions"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
    got.sort_unstable();
    assert_eq!(got, granted, "the new device holds exactly the minting device's permissions");
    // Asking for more than it holds is refused; a narrower grant is fine.
    assert_eq!(post("/api/domovoi/remote/pairing", json!({ "permissions": ["terminal:operate"] })).await.unwrap().status(), 400);
    assert_eq!(post("/api/domovoi/remote/pairing", json!({ "permissions": ["orchestration:read"] })).await.unwrap().status(), 200);

    // A remote device signs out other remote devices, never the machine's own sessions.
    let status: Value = http_client().get(format!("{local}/api/domovoi/remote")).header("cookie", &cookie).send().await.unwrap().json().await.unwrap();
    let devices = status["devices"].as_array().unwrap();
    let owner_id = devices.iter().find(|d| d["remote"] == false).expect("the owner's session")["id"].clone();
    assert_eq!(post("/api/auth/clients/revoke", json!({ "sessionId": owner_id })).await.unwrap().status(), 403);
    let others: Value = post("/api/auth/clients/revoke-others", json!({})).await.unwrap().json().await.unwrap();
    assert_eq!(others["revokedCount"], 1, "only the tablet");
    assert_eq!(http_client().get(format!("{tablet_base}/api/orchestration/shell")).bearer_auth(&tablet_token).send().await.unwrap().status(), 401);
    let owner: Value = http_client().get(format!("{local}/api/auth/session")).header("cookie", &cookie).send().await.unwrap().json().await.unwrap();
    assert_eq!(owner["authenticated"], true, "the owner stays signed in");

    // The owner changes it locally; a failed start does not leak why.
    let res = http_client().post(format!("{local}/api/domovoi/remote/configure")).header("cookie", &cookie).json(&json!({ "enabled": true, "relays": ["not a url"] })).send().await.unwrap();
    assert_eq!(res.status(), 400);
    assert!(res.json::<Value>().await.unwrap()["message"].as_str().unwrap().contains("relay not a url"));

    phone.stop().await;
    tablet.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn strangers_are_rate_limited_but_paired_devices_are_not() {
    let server = start_as("limit");
    let (ticket, code) = parse_link(&pair_link(&server.home, server.port));
    let (phone, base, token) = paired(&ticket, &code).await;

    // A stranger guessing codes in parallel: the limit holds for all of them.
    let stranger = bridge(&ticket, SecretKey::generate()).await;
    let stranger_base = format!("http://127.0.0.1:{}", stranger.port());
    let guesses = futures::future::join_all((0..12).map(|i| {
        let base = stranger_base.clone();
        async move { exchange(&base, &format!("WRONGCODE{i:03}")).await.status().as_u16() }
    }))
    .await;
    let refused = guesses.iter().filter(|s| **s == 401).count();
    let limited = guesses.iter().filter(|s| **s == 429).count();
    assert_eq!((refused, limited), (6, 6), "six guesses a minute per device key, got {guesses:?}");
    let res = exchange(&stranger_base, "WRONGCODE999").await;
    assert_eq!(res.status(), 429);
    assert_eq!(res.headers()["retry-after"], "60");
    assert_eq!(res.json::<Value>().await.unwrap()["code"], "rate_limited");

    // The paired phone is untouched, even on the pairing routes.
    assert_eq!(http_client().get(format!("{base}/api/orchestration/shell")).bearer_auth(&token).send().await.unwrap().status(), 200);
    for _ in 0..8 {
        assert_eq!(exchange(&base, "WRONGCODE000").await.status(), 401);
    }
    // And another new device can still pair.
    let (_, code) = parse_link(&pair_link(&server.home, server.port));
    let (tablet, _, _) = paired(&ticket, &code).await;

    phone.stop().await;
    stranger.stop().await;
    tablet.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_command_line_manages_a_running_server() {
    let mut server = start_as("cli");
    let ticket_file = server.home.join("remote").join("ticket");
    let status = cli(&server, &["remote", "status"]);
    assert!(status.contains("Server: running") && status.contains("Remote access: on (local network only)"), "{status}");
    assert!(status.contains("Devices: none paired"), "the command's own sign-in is not a device: {status}");
    // The IPv4 addresses: their port is the one kept across restarts (an
    // IPv6 socket, when the machine has addresses for it, takes a new one).
    let address = |text: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix("Addresses: "))
            .map(|list| list.split(", ").filter(|a| !a.starts_with('[')).collect::<Vec<_>>().join(", "))
    };
    let before = address(&status).expect("addresses listed");
    assert!(!before.is_empty(), "an IPv4 address is listed: {status}");

    let (ticket, code) = parse_link(&pair_link(&server.home, server.port));
    let (phone, base, token) = paired(&ticket, &code).await;
    let devices = cli(&server, &["remote", "devices"]);
    let line = devices.lines().find(|l| l.contains("Test Phone")).unwrap_or_else(|| panic!("the phone is listed: {devices}"));
    assert!(line.contains("mobile") && line.contains("remote"), "{line}");
    let id = line.split_whitespace().next().unwrap();
    assert!(cli(&server, &["remote", "revoke", id]).contains("Signed out Test Phone"));
    assert_eq!(http_client().get(format!("{base}/api/orchestration/shell")).bearer_auth(&token).send().await.unwrap().status(), 401);
    phone.stop().await;

    // Off removes the saved ticket; on again comes back on the same LAN port,
    // so links saved on other devices keep working.
    assert!(cli(&server, &["remote", "off"]).contains("Remote access: off"));
    assert!(!ticket_file.exists());
    let again = cli(&server, &["remote", "on"]);
    assert_eq!(address(&again).as_deref(), Some(before.as_str()), "{again}");
    assert!(ticket_file.exists());

    // `stop` shuts the server down cleanly: exit 0, the ticket gone.
    assert!(cli(&server, &["stop"]).contains("Stopping the server"));
    let exit = tokio::task::block_in_place(|| server.child.wait()).unwrap();
    assert!(exit.success(), "{exit:?}");
    assert!(!ticket_file.exists(), "the saved ticket outlived the server");

    // With no server the commands act on the home directory.
    assert!(cli(&server, &["remote", "status"]).contains("Server: not running"));
    assert!(cli(&server, &["remote", "off"]).contains("at the next start"));
    let saved: Value = serde_json::from_slice(&std::fs::read(server.home.join("remote.json")).unwrap()).unwrap();
    assert_eq!(saved["enabled"], false);
    assert!(saved["lanPort"].as_u64().unwrap() > 0);
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn sigterm_shuts_down_cleanly_with_a_socket_open() {
    let mut server = start_as("sigterm");
    let ticket_file = server.home.join("remote").join("ticket");
    let (ticket, code) = parse_link(&pair_link(&server.home, server.port));
    let (phone, base, token) = paired(&ticket, &code).await;
    let mut ws = open_ws(&base, &token).await;
    assert!(rpc(&mut ws, "1", "server.getConfig").await.is_some());

    let started = std::time::Instant::now();
    assert!(Command::new("kill").args(["-TERM", &server.child.id().to_string()]).status().unwrap().success());
    let exit = tokio::task::block_in_place(|| server.child.wait()).unwrap();
    assert!(exit.success(), "{exit:?}");
    assert!(started.elapsed() < Duration::from_secs(8), "an open socket must not hold shutdown up");
    assert!(!ticket_file.exists(), "the saved ticket outlived the server");
    let log = std::fs::read_dir(server.home.join("userdata").join("logs"))
        .unwrap()
        .flatten()
        .filter(|e| e.file_name().to_string_lossy().starts_with("server."))
        .map(|e| std::fs::read_to_string(e.path()).unwrap())
        .collect::<String>();
    assert!(log.contains("Stopped"), "the log file records the shutdown: {log}");
    assert!(!log.contains("token=") && !log.contains(&code), "pairing codes never reach the log file");
    phone.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_device_forgetting_the_server_signs_itself_out() {
    let server = start_as("signout");
    let (ticket, code) = parse_link(&pair_link(&server.home, server.port));
    let (phone, base, token) = paired(&ticket, &code).await;
    let device = "ab".repeat(32);
    let res = http_client().post(format!("{base}/api/domovoi/push/devices")).bearer_auth(&token).json(&json!({ "token": device, "environment": "sandbox" })).send().await.unwrap();
    assert_eq!(res.status(), 200);
    let mut ws = open_ws(&base, &token).await;
    assert!(rpc(&mut ws, "1", "server.getConfig").await.is_some());

    let res = http_client().post(format!("{base}/api/domovoi/session/end")).bearer_auth(&token).send().await.unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(res.json::<Value>().await.unwrap()["signedOut"], true);
    // Its token, its socket and its push registration are gone; it is no longer listed.
    assert_eq!(http_client().get(format!("{base}/api/orchestration/shell")).bearer_auth(&token).send().await.unwrap().status(), 401);
    let closed = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match ws.next().await {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => return,
                _ => {}
            }
        }
    })
    .await;
    assert!(closed.is_ok(), "the socket stays open");
    let status = cli(&server, &["remote", "status"]);
    assert!(status.contains("Devices: none paired") && status.contains("0 device(s) registered") || status.contains("Push: off"), "{status}");
    phone.stop().await;
}
