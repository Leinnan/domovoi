//! Operations: `/health` and `/ready`, compressed HTTP responses, what the RPC
//! socket tells a client of another protocol version, and `domovoi-server doctor`.

#![cfg_attr(windows, allow(dead_code, reason = "the tests using these helpers script fake CLIs as shell scripts, so they run on Unix only"))]

use serde_json::Value;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

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

fn temp_home(tag: &str) -> PathBuf {
    let home = std::env::temp_dir().join(format!("domovoi-ops-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    home
}

fn start(tag: &str) -> Server {
    let home = temp_home(tag);
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let mut child = Command::new(env!("CARGO_BIN_EXE_domovoi-server"))
        .env("DOMOVOI_NO_SESSION_IMPORT", "1")
        .env("DOMOVOI_NO_SHELL_ENV", "1")
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

fn gunzip(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    flate2::read::GzDecoder::new(bytes).read_to_end(&mut out).unwrap();
    out
}

#[tokio::test(flavor = "multi_thread")]
async fn health_ready_compression_and_protocol_check() {
    let server = start("http");
    let base = format!("http://127.0.0.1:{}", server.port);
    let client = http_client();

    // `/health` and `/ready` need no credential.
    let health = client.get(format!("{base}/health")).send().await.unwrap();
    assert_eq!(health.status(), 200);
    assert_eq!(health.json::<Value>().await.unwrap(), serde_json::json!({ "status": "ok" }));
    let mut ready = None;
    for _ in 0..50 {
        let res = client.get(format!("{base}/ready")).send().await.unwrap();
        let status = res.status();
        let body: Value = res.json().await.unwrap();
        if status == 200 {
            ready = Some(body);
            break;
        }
        assert_eq!(status, 503, "{body}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    assert_eq!(ready, Some(serde_json::json!({ "status": "ready" })));

    // A large script: gzip or brotli when asked, the plain bytes otherwise.
    let runtime = format!("{base}/api/assets/artifact-runtime/v1/runtime.js");
    let plain = client.get(&runtime).send().await.unwrap();
    assert_eq!(plain.status(), 200);
    assert!(plain.headers().get("content-encoding").is_none());
    let plain = plain.bytes().await.unwrap();
    assert!(plain.len() > 10_000, "the runtime is large enough to compress");
    let gzipped = client.get(&runtime).header("accept-encoding", "gzip").send().await.unwrap();
    assert_eq!(gzipped.headers().get("content-encoding").map(|v| v.to_str().unwrap().to_owned()).as_deref(), Some("gzip"));
    assert_eq!(gzipped.headers().get("vary").and_then(|v| v.to_str().ok()), Some("accept-encoding"));
    let gzipped = gzipped.bytes().await.unwrap();
    assert!(gzipped.len() < plain.len() / 2, "{} -> {}", plain.len(), gzipped.len());
    assert_eq!(gunzip(&gzipped), plain);
    let brotli = client.get(&runtime).header("accept-encoding", "br").send().await.unwrap();
    assert_eq!(brotli.headers().get("content-encoding").and_then(|v| v.to_str().ok()), Some("br"));

    // JSON too.
    let environment = client.get(format!("{base}/.well-known/domovoi/environment")).header("accept-encoding", "gzip").send().await.unwrap();
    assert_eq!(environment.headers().get("content-encoding").and_then(|v| v.to_str().ok()), Some("gzip"));
    let environment: Value = serde_json::from_slice(&gunzip(&environment.bytes().await.unwrap())).unwrap();
    assert_eq!(environment["orchestrationProtocolVersion"], PROTOCOL);
    // An app from before the server was named after Domovoi asks where it always did, and is answered the same.
    let as_before: Value = client.get(format!("{base}/.well-known/t3/environment")).send().await.unwrap().json().await.unwrap();
    assert_eq!(as_before, environment);

    // The socket asks for a credential first, whatever protocol the URL names.
    for query in [String::new(), "?orchestrationProtocol=2".to_owned(), format!("?protocol={PROTOCOL}")] {
        let req = format!("ws://127.0.0.1:{}/ws{query}", server.port).into_client_request().unwrap();
        let Err(tokio_tungstenite::tungstenite::Error::Http(res)) = tokio_tungstenite::connect_async(req).await else {
            panic!("the socket opened without a credential for {query:?}");
        };
        assert_eq!(res.status(), 401, "{query:?}");
    }
}

/// A signed-in client of another protocol version is told which side to
/// update, as a defect (the one thing every version of the app shows), at
/// once and again for whatever it sends; nothing it asks for is answered.
#[tokio::test(flavor = "multi_thread")]
async fn a_client_of_another_protocol_version_is_told_to_update() {
    use futures::StreamExt;
    let (server, mut current) = start_signed_in("protocol", &[]).await;
    send(&mut current, "1", "subscribeServerLifecycle", serde_json::json!({})).await;
    let first = next_frame(&mut current).await;
    assert_eq!((first["_tag"].as_str(), first["requestId"].as_str()), (Some("Chunk"), Some("1")), "this version is served: {first}");

    let token = bearer_token(&server).await;
    // An app from before the versions were compared, an older one that names
    // its version, and one newer than the server.
    for (query, advice) in [
        ("orchestrationProtocol=2".to_owned(), "Update Domovoi on this device."),
        (format!("protocol={}", PROTOCOL - 1), "Update Domovoi on this device."),
        (format!("protocol={}", PROTOCOL + 1), "Update domovoi-server on the machine it runs on."),
    ] {
        let mut req = format!("ws://127.0.0.1:{}/ws?{query}", server.port).into_client_request().unwrap();
        req.headers_mut().insert("authorization", format!("Bearer {token}").parse().unwrap());
        let mut ws = tokio_tungstenite::connect_async(req).await.unwrap_or_else(|e| panic!("the socket did not open for {query}: {e}")).0;
        let told = next_frame(&mut ws).await;
        assert_eq!(told["_tag"], "Defect", "{query}: {told}");
        assert!(told["defect"].as_str().unwrap().ends_with(advice), "{query}: {told}");
        send(&mut ws, "1", "subscribeServerLifecycle", serde_json::json!({})).await;
        assert_eq!(next_frame(&mut ws).await, told, "{query}: every frame is answered with the same advice");
        let nothing_more = tokio::time::timeout(Duration::from_millis(300), ws.next()).await;
        assert!(nothing_more.is_err(), "{query}: the request itself is not served");
    }
}

/// A client that asks for packed frames (`pack` on the socket URL) is sent
/// its large frames deflated, and its small ones as they are; one that does
/// not ask is sent every frame as it is.
#[tokio::test(flavor = "multi_thread")]
async fn a_client_that_asks_is_sent_large_frames_packed() {
    use futures::StreamExt;
    let (server, mut plain) = start_signed_in("packed", &[]).await;
    let token = bearer_token(&server).await;
    let mut req = format!("ws://127.0.0.1:{}/ws?protocol={PROTOCOL}&pack=1", server.port).into_client_request().unwrap();
    req.headers_mut().insert("authorization", format!("Bearer {token}").parse().unwrap());
    let mut packing = tokio_tungstenite::connect_async(req).await.unwrap().0;

    // The configuration is tens of kilobytes.
    for ws in [&mut plain, &mut packing] {
        send(ws, "1", "subscribeServerConfig", serde_json::json!({})).await;
    }
    let text = |message: tokio_tungstenite::tungstenite::Message| message.into_text().unwrap().to_string();
    let sent_plain = text(plain.next().await.unwrap().unwrap());
    let sent_packed = text(packing.next().await.unwrap().unwrap());
    assert!(sent_plain.starts_with('{'), "not asked for: {}", &sent_plain[..40]);
    assert!(sent_packed.starts_with(domovoi_rpc::packed::PACKED), "{}", &sent_packed[..40]);
    assert!(sent_packed.len() * 2 < sent_plain.len(), "{} -> {}", sent_plain.len(), sent_packed.len());
    let unpacked: Value = serde_json::from_str(&domovoi_rpc::packed::unpack(&sent_packed).unwrap()).unwrap();
    let plain: Value = serde_json::from_str(&sent_plain).unwrap();
    assert_eq!((unpacked["_tag"].as_str(), unpacked["requestId"].as_str()), (Some("Chunk"), Some("1")));
    assert_eq!(unpacked["values"][0]["type"], plain["values"][0]["type"]);

    // A small frame is sent as it is: here the refusal of a method there is none of.
    send(&mut packing, "2", "no.such.method", serde_json::json!({})).await;
    loop {
        let sent = text(tokio::time::timeout(Duration::from_secs(10), packing.next()).await.expect("a frame").unwrap().unwrap());
        let frame = domovoi_rpc::packed::unpack(&sent).unwrap();
        assert_eq!(sent.starts_with(domovoi_rpc::packed::PACKED), frame.len() >= domovoi_rpc::packed::PACK_FROM, "{} bytes", frame.len());
        if serde_json::from_str::<Value>(&frame).unwrap()["requestId"] == "2" {
            assert!(frame.len() < domovoi_rpc::packed::PACK_FROM, "{} bytes", frame.len());
            break;
        }
    }
}

/// The next text frame, parsed.
async fn next_frame(ws: &mut Ws) -> Value {
    use futures::StreamExt;
    loop {
        let message = tokio::time::timeout(Duration::from_secs(10), ws.next()).await.expect("a frame").expect("the socket is open").unwrap();
        if let tokio_tungstenite::tungstenite::Message::Text(text) = message {
            return serde_json::from_str(&text).unwrap();
        }
    }
}

/// A bearer token for a new device, from a pairing code minted beside the running server.
/// The code is offered as an app from before does: under the name it had (`urn:t3:…`).
async fn bearer_token(server: &Server) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_domovoi-server"))
        .env("DOMOVOI_NO_SESSION_IMPORT", "1")
        .env("DOMOVOI_NO_SHELL_ENV", "1")
        .args(["--home-dir", server.home.to_str().unwrap(), "--port", &server.port.to_string(), "pair"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let code = text.lines().find_map(|l| l.split("#token=").nth(1)).expect("a pairing code").trim().to_owned();
    let body = format!(
        "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Atoken-exchange&subject_token={code}&subject_token_type=urn%3At3%3Aparams%3Aoauth%3Atoken-type%3Aenvironment-bootstrap&requested_token_type=urn%3Aietf%3Aparams%3Aoauth%3Atoken-type%3Aaccess_token&client_device_type=mobile"
    );
    let res: Value = http_client()
        .post(format!("http://127.0.0.1:{}/oauth/token", server.port))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    res["access_token"].as_str().expect("an access token").to_owned()
}

/// Runs `domovoi-server doctor --json` with `home` and `path` as PATH.
fn doctor(home: &Path, port: u16, path: &Path) -> (i32, Value) {
    let out = Command::new(env!("CARGO_BIN_EXE_domovoi-server"))
        .env("DOMOVOI_NO_SESSION_IMPORT", "1")
        .env("DOMOVOI_NO_SHELL_ENV", "1")
        .env("PATH", path)
        // The CLIs the test names, not the developer's (found in the usual folders too).
        .env("DOMOVOI_CLAUDE_BINARY", path.join("claude"))
        .env("DOMOVOI_CODEX_BINARY", path.join("codex"))
        .env("DOMOVOI_OPENCODE_BINARY", path.join("missing").join("opencode"))
        .env("DOMOVOI_PI_BINARY", path.join("missing").join("pi"))
        .env("DOMOVOI_ANTIGRAVITY_BINARY", path.join("missing").join("agy"))
        .env("DOMOVOI_AUTOSTART_DIR", path.join("autostart"))
        .env("DOMOVOI_SYSTEMD_USER_DIR", path.join("systemd"))
        .env("DOMOVOI_AGENT_SESSIONS_HOME", home.join("agent-home"))
        .args(["--home-dir", home.to_str().unwrap(), "--port", &port.to_string(), "doctor", "--json"])
        .output()
        .unwrap();
    let report: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&out.stdout)));
    (out.status.code().unwrap_or(-1), report)
}

fn check<'a>(report: &'a Value, name: &str) -> &'a Value {
    report["checks"].as_array().unwrap().iter().find(|c| c["name"] == name).unwrap_or_else(|| panic!("no {name} check in {report}"))
}

#[cfg(unix)]
fn script(dir: &Path, name: &str, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    let file = dir.join(name);
    std::fs::write(&file, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[cfg(unix)]
#[test]
fn doctor_reports_a_temp_home() {
    let home = temp_home("doctor");
    let bin = home.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    // As the server makes a home: readable by nobody else (one that is gets a warning).
    std::fs::set_permissions(&home, std::os::unix::fs::PermissionsExt::from_mode(0o700)).unwrap();
    // Stand-ins for the tools doctor asks; nothing else is on PATH.
    script(&bin, "git", r#"case "$1" in --version) echo "git version 2.50.0";; lfs) echo "git-lfs/3.7.0 (GitHub; darwin arm64; go 1.24)";; esac"#);
    script(&bin, "gh", r#"case "$1" in --version) echo "gh version 2.80.0 (2025-09-01)";; auth) echo "not logged in" >&2; exit 1;; esac"#);
    script(&bin, "claude", r#"echo "2.1.0 (Claude Code)""#);
    script(&bin, "codex", "exec /bin/sleep 30");
    let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();

    let started = std::time::Instant::now();
    let (code, report) = doctor(&home, free, &bin);
    assert!(started.elapsed() < Duration::from_secs(25), "a hung probe is cut off");
    assert_eq!(code, 0, "warnings alone pass: {report}");
    assert_eq!(check(&report, "home")["status"], "ok", "{report}");
    assert_eq!(check(&report, "database")["status"], "warn", "not made yet: {report}");
    assert_eq!(check(&report, "git")["status"], "ok");
    assert_eq!(check(&report, "git")["detail"], "git version 2.50.0");
    assert_eq!(check(&report, "git-lfs")["status"], "ok");
    assert_eq!(check(&report, "gh")["status"], "warn", "{report}");
    assert_eq!(check(&report, "provider:claudeAgent")["status"], "ok");
    assert!(check(&report, "provider:claudeAgent")["detail"].as_str().unwrap().starts_with("2.1.0 (Claude Code) at "), "{report}");
    assert_eq!(check(&report, "remote")["status"], "ok");
    assert_eq!(check(&report, "private")["status"], "ok", "{report}");
    assert_eq!(check(&report, "local-sign-in")["status"], "ok", "no server, no descriptor: {report}");
    assert!(!home.join("userdata").exists() && !home.join("remote").exists() && !home.join("local").exists(), "doctor changes nothing");
    assert_eq!(check(&report, "provider:codex")["status"], "warn", "a probe that hangs is reported: {report}");
    assert_eq!(check(&report, "provider:opencode")["status"], "warn", "a missing CLI is a warning: {report}");
    assert_eq!(check(&report, "port")["status"], "ok", "{report}");
    assert!(report["checks"].as_array().unwrap().iter().all(|c| c["detail"].as_str().is_some_and(|d| !d.is_empty() && !d.contains('\n'))));
    assert_eq!(report["ok"], true);

    // A home that is not a directory fails, and the exit code says so.
    let not_dir = temp_home("doctor-file");
    std::fs::write(&not_dir, "x").unwrap();
    let (code, report) = doctor(&not_dir, free, &bin);
    assert_eq!(check(&report, "database")["status"], "warn", "{report}");
    assert_ne!(code, 0, "{report}");
    assert_eq!(check(&report, "home")["status"], "fail", "{report}");
    assert_eq!(report["ok"], false);
    let _ = std::fs::remove_file(&not_dir);
    let _ = std::fs::remove_dir_all(&home);
}

#[cfg(unix)]
#[test]
fn doctor_sees_a_running_server() {
    let server = start("doctor-live");
    let bin = server.home.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let (_, report) = doctor(&server.home, server.port, &bin);
    let port = check(&report, "port");
    assert_eq!(port["status"], "ok", "{report}");
    assert!(port["detail"].as_str().unwrap().contains("running"), "{report}");
    assert_eq!(check(&report, "database")["status"], "ok", "{report}");
}

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// The server started with `env`, and a socket signed in with its startup code.
async fn start_signed_in(tag: &str, env: &[(&str, String)]) -> (Server, Ws) {
    let home = temp_home(tag);
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let mut child = Command::new(env!("CARGO_BIN_EXE_domovoi-server"))
        .env("DOMOVOI_NO_SESSION_IMPORT", "1")
        .env("DOMOVOI_NO_SHELL_ENV", "1")
        .env("DOMOVOI_AGENT_SESSIONS_HOME", home.join("agent-home"))
        .env("RUST_LOG", "warn")
        // System tools only, and no provider CLI but the ones `env` names.
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("DOMOVOI_CLAUDE_BINARY", "/usr/bin/false")
        .env("DOMOVOI_CODEX_BINARY", "/usr/bin/false")
        .env("DOMOVOI_OPENCODE_BINARY", "/nonexistent/opencode")
        .env("DOMOVOI_PI_BINARY", "/nonexistent/pi")
        .env("DOMOVOI_ANTIGRAVITY_BINARY", "/nonexistent/agy")
        .envs(env.iter().map(|(k, v)| (*k, v.as_str())))
        .args(["--home-dir", home.to_str().unwrap(), "--port", &port.to_string()])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let out = child.stdout.take().unwrap();
    let mut code = String::new();
    for line in BufReader::new(out).lines() {
        let line = line.unwrap();
        if let Some(at) = line.find("#token=") {
            code = line[at + 7..].trim().to_owned();
            break;
        }
    }
    let server = Server { child, port, home };
    let session = sign_in(&format!("http://127.0.0.1:{port}"), &code).await;
    let mut req = format!("ws://127.0.0.1:{port}/ws?protocol={PROTOCOL}").into_client_request().unwrap();
    req.headers_mut().insert("authorization", session.parse().unwrap());
    (server, tokio_tungstenite::connect_async(req).await.unwrap().0)
}

async fn send(ws: &mut Ws, id: &str, tag: &str, payload: Value) {
    use futures::SinkExt;
    let req = serde_json::json!({ "_tag": "Request", "id": id, "tag": tag, "payload": payload, "headers": [] });
    ws.send(tokio_tungstenite::tungstenite::Message::Text(req.to_string().into())).await.unwrap();
}

/// Reads frames until each of `ids` has exited; acknowledges chunks. Every frame read.
async fn until_exits(ws: &mut Ws, ids: &[&str]) -> Vec<Value> {
    use futures::{SinkExt, StreamExt};
    let mut frames = vec![];
    let mut open: Vec<&str> = ids.to_vec();
    while !open.is_empty() {
        let msg = tokio::time::timeout(Duration::from_secs(60), ws.next()).await.expect("a frame").unwrap().unwrap();
        let tokio_tungstenite::tungstenite::Message::Text(t) = msg else { continue };
        let v: Value = serde_json::from_str(&t).unwrap();
        match v["_tag"].as_str() {
            Some("Ping") => ws.send(tokio_tungstenite::tungstenite::Message::Text(serde_json::json!({ "_tag": "Pong" }).to_string().into())).await.unwrap(),
            Some("Chunk") => {
                let ack = serde_json::json!({ "_tag": "Ack", "requestId": v["requestId"] });
                ws.send(tokio_tungstenite::tungstenite::Message::Text(ack.to_string().into())).await.unwrap();
            }
            Some("Exit") => open.retain(|id| v["requestId"] != *id),
            _ => {}
        }
        frames.push(v);
    }
    frames
}

fn exit_of<'a>(frames: &'a [Value], id: &str) -> &'a Value {
    &frames.iter().find(|f| f["_tag"] == "Exit" && f["requestId"] == id).unwrap()["exit"]
}

fn claude_of(providers: &Value) -> &Value {
    providers.as_array().unwrap().iter().find(|p| p["instanceId"] == "claudeAgent").expect("claudeAgent listed")
}

/// A Claude Code installed with npm under `root/npm`, and an `npm` there that
/// "updates" it (or fails, once `root/state/npm-fail` exists).
#[cfg(unix)]
fn npm_installed_claude(root: &Path) -> PathBuf {
    let package = root.join("npm/lib/node_modules/@anthropic-ai/claude-code");
    for dir in [package.as_path(), &root.join("npm/bin"), &root.join("bin"), &root.join("state")] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let root = root.canonicalize().unwrap();
    let state = root.join("state");
    std::fs::write(state.join("claude-version"), "2.1.0 (Claude Code)\n").unwrap();
    script(
        &package,
        "cli.js",
        &format!(
            r#"case "$*" in *--version*) cat '{state}/claude-version';; *"auth status"*) echo '{{"loggedIn": true, "authMethod": "claude.ai", "subscriptionType": "max"}}';; *) exit 1;; esac"#,
            state = state.display()
        ),
    );
    script(
        &root.join("npm/bin"),
        "npm",
        &format!(
            r#"echo "$@" > '{state}/npm-args'
if [ -f '{state}/npm-fail' ]; then echo "npm error code E404" >&2; echo "npm error 404 Not Found - @anthropic-ai/claude-code" >&2; exit 1; fi
echo "downloading 50%" >&2
/bin/sleep 2.5
echo "2.2.0 (Claude Code)" > '{state}/claude-version'
echo "changed 1 package in 3s""#,
            state = state.display()
        ),
    );
    let launcher = root.join("bin/claude");
    std::os::unix::fs::symlink(package.join("cli.js"), &launcher).unwrap();
    launcher
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn update_provider_runs_npm_and_streams_its_progress() {
    let root = temp_home("update-cli");
    let launcher = npm_installed_claude(&root);
    let root = root.canonicalize().unwrap();
    let (server, mut ws) = start_signed_in("update", &[("DOMOVOI_CLAUDE_BINARY", launcher.to_string_lossy().into_owned())]).await;

    // Every client's provider list follows the update.
    send(&mut ws, "config", "subscribeServerConfig", serde_json::json!({})).await;
    send(&mut ws, "update", "server.updateProvider", serde_json::json!({ "provider": "claudeAgent" })).await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    send(&mut ws, "again", "server.updateProvider", serde_json::json!({ "provider": "claudeAgent", "instanceId": "claudeAgent" })).await;
    let frames = until_exits(&mut ws, &["update", "again"]).await;

    let again = exit_of(&frames, "again");
    assert_eq!(again["_tag"], "Failure", "{again}");
    assert_eq!(
        again["cause"][0]["error"],
        serde_json::json!({ "_tag": "ServerProviderUpdateError", "provider": "claudeAgent", "reason": "An update is already running for this provider." })
    );
    let args = std::fs::read_to_string(root.join("state/npm-args")).unwrap();
    let prefix = root.join("npm");
    assert_eq!(args.trim(), format!("install -g --prefix {} --allow-scripts=@anthropic-ai/claude-code @anthropic-ai/claude-code@latest", prefix.display()));

    let shown: Vec<&Value> = frames
        .iter()
        .filter(|f| f["_tag"] == "Chunk" && f["requestId"] == "config")
        .flat_map(|f| f["values"].as_array().unwrap())
        .filter_map(|event| event["payload"]["providers"].as_array())
        .flatten()
        .filter(|p| p["instanceId"] == "claudeAgent")
        .filter_map(|p| p.get("updateState"))
        .collect();
    assert!(shown.iter().any(|s| s["status"] == "running" && s["message"] == "downloading 50%"), "the installer's progress is shown: {shown:?}");

    let done = exit_of(&frames, "update");
    assert_eq!(done["_tag"], "Success", "{done}");
    let claude = claude_of(&done["value"]["providers"]);
    assert_eq!(claude["version"], "2.2.0", "probed again: {claude}");
    let update = &claude["updateState"];
    assert_eq!((update["status"].as_str(), update["message"].as_str()), (Some("succeeded"), Some("Provider updated.")), "{update}");
    assert!(update["startedAt"].is_string() && update["finishedAt"].is_string());
    assert!(update["output"].as_str().unwrap().contains("changed 1 package"), "{update}");

    // A failing installer is a failed update state, with what it printed.
    std::fs::write(root.join("state/npm-fail"), "").unwrap();
    send(&mut ws, "fail", "server.updateProvider", serde_json::json!({ "provider": "claudeAgent" })).await;
    let frames = until_exits(&mut ws, &["fail"]).await;
    let failed = exit_of(&frames, "fail");
    let update = &claude_of(&failed["value"]["providers"])["updateState"];
    assert_eq!(update["status"], "failed", "{failed}");
    assert_eq!(update["message"], "Update command exited with code 1: npm error 404 Not Found - @anthropic-ai/claude-code");
    assert!(update["output"].as_str().unwrap().contains("npm error code E404"));

    // A specific version, another instance, a CLI not installed by a package manager: refused.
    send(&mut ws, "pinned", "server.updateProvider", serde_json::json!({ "provider": "claudeAgent", "targetVersion": "2.2.1" })).await;
    send(&mut ws, "codex", "server.updateProvider", serde_json::json!({ "provider": "codex" })).await;
    send(&mut ws, "other", "server.updateProvider", serde_json::json!({ "provider": "claudeAgent", "instanceId": "claude_work" })).await;
    let frames = until_exits(&mut ws, &["pinned", "codex", "other"]).await;
    let pinned = &claude_of(&exit_of(&frames, "pinned")["value"]["providers"])["updateState"];
    assert_eq!(pinned["status"], "failed");
    assert_eq!(pinned["message"], "This version is no longer recommended or this installer cannot install a specific version. Refresh provider settings.");
    for id in ["codex", "other"] {
        let refused = exit_of(&frames, id);
        assert_eq!(refused["cause"][0]["error"]["reason"], "This provider does not support one-click updates.", "{refused}");
    }
    drop(server);
    let _ = std::fs::remove_dir_all(&root);
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
