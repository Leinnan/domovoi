//! APNs push against a mock APNs (HTTP/2 without TLS, through `DOMOVOI_APNS_URL`):
//! a thread that fails pushes "Failed" to registered devices whose app is
//! closed, skips a device with a socket open, signs requests with an ES256
//! provider token, and forgets tokens APNs reports gone.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use axum::{Json, Router};
use base64::Engine;
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};

/// The socket URL names the version of the protocol the test speaks.
const PROTOCOL: u32 = domovoi_model::PROTOCOL_VERSION;

fn http() -> reqwest::Client {
    let _ = rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::new()
}

const GOOD: &str = "aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11aa11";
const GONE: &str = "bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22bb22";
const OPEN: &str = "cc33cc33cc33cc33cc33cc33cc33cc33cc33cc33cc33cc33cc33cc33cc33cc33";

type Received = Arc<Mutex<Vec<(String, HeaderMap, Value)>>>;

/// How a mock APNs host answers the `nth` (from 0) push to `token`.
type Behave = fn(token: &str, nth: usize) -> (u16, &'static str);

fn unregistered_is_gone(token: &str, _: usize) -> (u16, &'static str) {
    if token == GONE { (410, "Unregistered") } else { (200, "") }
}

async fn mock_apns() -> (u16, Received) {
    mock_apns_that(unregistered_is_gone).await
}

async fn mock_apns_that(behave: Behave) -> (u16, Received) {
    let received: Received = Arc::default();
    let app = Router::new()
        .route(
            "/3/device/{token}",
            post(move |State(r): State<Received>, Path(token): Path<String>, headers: HeaderMap, Json(body): Json<Value>| async move {
                let nth = {
                    let mut r = r.lock().unwrap();
                    let nth = r.iter().filter(|(t, _, _)| *t == token).count();
                    r.push((token.clone(), headers, body));
                    nth
                };
                let (status, reason) = behave(&token, nth);
                (StatusCode::from_u16(status).unwrap(), Json(if reason.is_empty() { json!({}) } else { json!({ "reason": reason }) }))
            }),
        )
        .with_state(received.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    // axum::serve speaks HTTP/2 with prior knowledge (h2c), as the client sends.
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (port, received)
}

struct Server {
    child: Child,
    port: u16,
    dir: std::path::PathBuf,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn start(apns_port: u16) -> Server {
    start_with("basic", &[("DOMOVOI_APNS_URL", format!("http://127.0.0.1:{apns_port}"))])
}

/// A server pushing to the mock hosts named in `env`; each test has its own directory.
fn start_with(tag: &str, env: &[(&str, String)]) -> Server {
    let dir = std::env::temp_dir().join(format!("domovoi-push-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("w")).unwrap();
    // A P-256 key made for this test (Apple's are the same kind).
    let rng = ring::rand::SystemRandom::new();
    let pkcs8 = ring::signature::EcdsaKeyPair::generate_pkcs8(&ring::signature::ECDSA_P256_SHA256_FIXED_SIGNING, &rng).unwrap();
    let pem = format!("-----BEGIN PRIVATE KEY-----\n{}\n-----END PRIVATE KEY-----\n", base64::engine::general_purpose::STANDARD.encode(pkcs8.as_ref()));
    std::fs::write(dir.join("AuthKey_TEST.p8"), pem).unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let mut child = Command::new(env!("CARGO_BIN_EXE_domovoi-server")).env("DOMOVOI_NO_SESSION_IMPORT", "1").env("DOMOVOI_NO_SHELL_ENV", "1")
        .args(["--home-dir", dir.join("home").to_str().unwrap(), "--port", &port.to_string()])
        .args(["--apns-key", dir.join("AuthKey_TEST.p8").to_str().unwrap(), "--apns-key-id", "KEYID12345", "--apns-team-id", "TEAMID6789"])
        .envs(env.iter().map(|(k, v)| (*k, v.as_str())))
        .env("DOMOVOI_CLAUDE_BINARY", "/usr/bin/false")
        .env("DOMOVOI_CODEX_BINARY", "/usr/bin/false")
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("DOMOVOI_AGENT_SESSIONS_HOME", dir.join("agent-home"))
        .env("RUST_LOG", "warn")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    for line in BufReader::new(child.stdout.take().unwrap()).lines() {
        if line.unwrap().contains("#token=") {
            break;
        }
    }
    Server { child, port, dir }
}

fn pair_code(server: &Server) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_domovoi-server")).env("DOMOVOI_NO_SESSION_IMPORT", "1").env("DOMOVOI_NO_SHELL_ENV", "1")
        .args(["--home-dir", server.dir.join("home").to_str().unwrap(), "--port", &server.port.to_string(), "pair"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.lines().find_map(|l| l.split("#token=").nth(1)).unwrap().trim().to_owned()
}

/// A paired "phone": a bearer token.
async fn phone(server: &Server) -> String {
    let body = format!(
        "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Atoken-exchange&subject_token={}&subject_token_type=urn%3Adomovoi%3Aparams%3Aoauth%3Atoken-type%3Aenvironment-bootstrap&requested_token_type=urn%3Aietf%3Aparams%3Aoauth%3Atoken-type%3Aaccess_token&client_device_type=mobile",
        pair_code(server)
    );
    let res: Value = http()
        .post(format!("http://127.0.0.1:{}/oauth/token", server.port))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    res["access_token"].as_str().unwrap().to_owned()
}

async fn register(server: &Server, token: &str, device: &str) {
    register_for(server, token, device, "sandbox").await;
}

async fn register_for(server: &Server, token: &str, device: &str, environment: &str) {
    let res = http()
        .post(format!("http://127.0.0.1:{}/api/domovoi/push/devices", server.port))
        .bearer_auth(token)
        .json(&json!({ "token": device, "environment": environment }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200, "{}", res.text().await.unwrap());
}

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn socket(server: &Server, token: &str) -> Ws {
    let res: Value = http()
        .post(format!("http://127.0.0.1:{}/api/auth/websocket-ticket/rpc", server.port))
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let url = format!("ws://127.0.0.1:{}/ws?protocol={PROTOCOL}&wsTicket={}", server.port, res["ticket"].as_str().unwrap());
    tokio_tungstenite::connect_async(url.into_client_request().unwrap()).await.unwrap().0
}

async fn call(ws: &mut Ws, id: &str, tag: &str, payload: Value) -> Value {
    ws.send(Message::Text(json!({ "_tag": "Request", "id": id, "tag": tag, "payload": payload, "headers": [] }).to_string().into())).await.unwrap();
    loop {
        let Some(Ok(Message::Text(t))) = ws.next().await else { panic!("socket closed") };
        let v: Value = serde_json::from_str(&t).unwrap();
        if v["requestId"] == id {
            return v;
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_turn_pushes_to_closed_apps() {
    let (apns_port, received) = mock_apns().await;
    let server = start(apns_port);

    let closed = phone(&server).await;
    register(&server, &closed, GOOD).await;
    register(&server, &closed, GONE).await;
    // This phone has the app open (a socket), so it notifies itself.
    let open = phone(&server).await;
    register(&server, &open, OPEN).await;
    let mut ws = socket(&server, &open).await;

    let workspace = server.dir.join("w").canonicalize().unwrap();
    let r = call(&mut ws, "1", "projects.mutate", json!({ "type": "project.create", "commandId": "c1", "projectId": "p1", "title": "w",
        "workspaceRoot": workspace, "createWorkspaceRootIfMissing": false, "defaultModelSelection": null })).await;
    assert_eq!(r["exit"]["_tag"], "Success", "{r}");
    let r = call(&mut ws, "2", "orchestration.launchThread", json!({ "commandId": "c2", "creationSource": "web", "threadId": "t1", "projectId": "p1",
        "title": "Fix the tests", "modelSelection": { "instanceId": "claudeAgent", "model": "claude-haiku-5-5" },
        "runtimeMode": "approval-required", "interactionMode": "default", "workspaceStrategy": { "type": "root" },
        "initialMessage": { "messageId": "m1", "text": "hi", "attachments": [] } })).await;
    assert_eq!(r["exit"]["_tag"], "Success", "{r}");

    // The provider binary exits at once: the turn fails and a push goes out.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while received.lock().unwrap().len() < 2 && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    let got = received.lock().unwrap().clone();
    let tokens: Vec<&str> = got.iter().map(|(t, _, _)| t.as_str()).collect();
    assert!(tokens.contains(&GOOD) && tokens.contains(&GONE), "pushed to {tokens:?}");
    assert!(!tokens.contains(&OPEN), "the open app is skipped");

    let (_, headers, body) = got.iter().find(|(t, _, _)| t == GOOD).unwrap();
    assert_eq!(body["aps"]["alert"]["title"], "Fix the tests");
    assert_eq!(body["aps"]["alert"]["body"], "Failed");
    assert_eq!(body["threadId"], "t1");
    assert_eq!(body["aps"]["badge"], 0, "no thread waits for an approval");
    assert_eq!(headers["apns-collapse-id"], "t1");
    assert_eq!(headers["apns-topic"], "com.crayen.domovoi");
    assert_eq!(headers["apns-push-type"], "alert");
    let auth = headers["authorization"].to_str().unwrap();
    let jwt = auth.strip_prefix("bearer ").expect("provider token");
    let header: Value = serde_json::from_slice(&base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(jwt.split('.').next().unwrap()).unwrap()).unwrap();
    assert_eq!(header, json!({ "alg": "ES256", "kid": "KEYID12345" }));

    // The token APNs called gone was forgotten; status shows what is left.
    let status: Value = http()
        .get(format!("http://127.0.0.1:{}/api/domovoi/remote", server.port))
        .bearer_auth(&open)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(status["push"], json!({ "configured": true, "devices": 2 }));
}

const ACTIVITY: &str = "ff66ff66ff66ff66ff66ff66ff66ff66ff66ff66ff66ff66ff66ff66ff66ff66";

/// A thread's Live Activity follows its turn and ends with it, saying how:
/// `liveactivity` pushes to the activity's own token, the end at once.
#[tokio::test(flavor = "multi_thread")]
async fn a_live_activity_ends_with_its_turn() {
    let (apns_port, received) = mock_apns().await;
    let server = start_with("activity", &[("DOMOVOI_APNS_URL", format!("http://127.0.0.1:{apns_port}"))]);
    let open = phone(&server).await;
    let mut ws = socket(&server, &open).await;
    let res = http()
        .post(format!("http://127.0.0.1:{}/api/domovoi/push/activities", server.port))
        .bearer_auth(&open)
        .json(&json!({ "token": ACTIVITY, "threadId": "t1", "environment": "sandbox" }))
        .send()
        .await
        .unwrap();
    assert!(res.status().is_success(), "{}", res.text().await.unwrap());

    let workspace = server.dir.join("w").canonicalize().unwrap();
    let r = call(&mut ws, "1", "projects.mutate", json!({ "type": "project.create", "commandId": "c1", "projectId": "p1", "title": "w",
        "workspaceRoot": workspace, "createWorkspaceRootIfMissing": false, "defaultModelSelection": null })).await;
    assert_eq!(r["exit"]["_tag"], "Success", "{r}");
    let r = call(&mut ws, "2", "orchestration.launchThread", json!({ "commandId": "c2", "creationSource": "web", "threadId": "t1", "projectId": "p1",
        "title": "Fix the tests", "modelSelection": { "instanceId": "claudeAgent", "model": "claude-haiku-5-5" },
        "runtimeMode": "approval-required", "interactionMode": "default", "workspaceStrategy": { "type": "root" },
        "initialMessage": { "messageId": "m1", "text": "hi", "attachments": [] } })).await;
    assert_eq!(r["exit"]["_tag"], "Success", "{r}");

    let ended = || received.lock().unwrap().iter().any(|(t, _, b)| t == ACTIVITY && b["aps"]["event"] == "end");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while !ended() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let got = received.lock().unwrap().clone();
    let (_, headers, body) = got.iter().rev().find(|(t, _, _)| t == ACTIVITY).expect("an activity push");
    assert_eq!(headers["apns-push-type"], "liveactivity");
    assert_eq!(headers["apns-topic"], "com.crayen.domovoi.push-type.liveactivity");
    assert_eq!(headers["apns-priority"], "10", "the end is shown at once");
    let state = &body["aps"]["content-state"];
    assert_eq!((body["aps"]["event"].as_str(), state["status"].as_str(), state["detail"].as_str()), (Some("end"), Some("failed"), Some("Failed")), "{body}");
    assert!(state["startedAtMs"].as_f64().is_some() && state["endedAtMs"].as_f64().is_some(), "{state}");
    assert!(body["aps"]["dismissal-date"].as_i64().is_some());
}

const EXPIRED: &str = "dd44dd44dd44dd44dd44dd44dd44dd44dd44dd44dd44dd44dd44dd44dd44dd44";
const MISFILED: &str = "ee55ee55ee55ee55ee55ee55ee55ee55ee55ee55ee55ee55ee55ee55ee55ee55";

/// Production: the first push to EXPIRED is refused for its provider token;
/// MISFILED is a sandbox token it has never heard of.
fn production(token: &str, nth: usize) -> (u16, &'static str) {
    match (token, nth) {
        (EXPIRED, 0) => (403, "ExpiredProviderToken"),
        (MISFILED, _) => (400, "BadDeviceToken"),
        _ => (200, ""),
    }
}

fn accepts_all(_: &str, _: usize) -> (u16, &'static str) {
    (200, "")
}

#[tokio::test(flavor = "multi_thread")]
async fn apns_refusals_are_recovered_from() {
    let (production_port, production) = mock_apns_that(production).await;
    let (sandbox_port, sandbox) = mock_apns_that(accepts_all).await;
    let server = start_with(
        "recover",
        &[
            ("DOMOVOI_APNS_PRODUCTION_URL", format!("http://127.0.0.1:{production_port}")),
            ("DOMOVOI_APNS_SANDBOX_URL", format!("http://127.0.0.1:{sandbox_port}")),
        ],
    );
    let closed = phone(&server).await;
    register_for(&server, &closed, EXPIRED, "production").await;
    register_for(&server, &closed, MISFILED, "production").await;
    let open = phone(&server).await;
    // Another session cannot remove a registration that is not its own.
    let res = http()
        .delete(format!("http://127.0.0.1:{}/api/domovoi/push/devices", server.port))
        .bearer_auth(&open)
        .json(&json!({ "token": EXPIRED }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let mut ws = socket(&server, &open).await;

    // A thread id and a title that fit neither a header nor a small payload.
    let thread = format!("wątek-{}", "ż".repeat(40));
    let title = "Ś".repeat(400);
    let workspace = server.dir.join("w").canonicalize().unwrap();
    let r = call(&mut ws, "1", "projects.mutate", json!({ "type": "project.create", "commandId": "c1", "projectId": "p1", "title": "w",
        "workspaceRoot": workspace, "createWorkspaceRootIfMissing": false, "defaultModelSelection": null })).await;
    assert_eq!(r["exit"]["_tag"], "Success", "{r}");
    let r = call(&mut ws, "2", "orchestration.launchThread", json!({ "commandId": "c2", "creationSource": "web", "threadId": thread, "projectId": "p1",
        "title": title, "modelSelection": { "instanceId": "claudeAgent", "model": "claude-haiku-5-5" },
        "runtimeMode": "approval-required", "interactionMode": "default", "workspaceStrategy": { "type": "root" },
        "initialMessage": { "messageId": "m1", "text": "hi", "attachments": [] } })).await;
    assert_eq!(r["exit"]["_tag"], "Success", "{r}");

    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while (production.lock().unwrap().len() < 3 || sandbox.lock().unwrap().is_empty()) && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    let production = production.lock().unwrap().clone();
    let sandbox = sandbox.lock().unwrap().clone();

    // The refused provider token was signed again and the push sent once more.
    let expired: Vec<_> = production.iter().filter(|(t, _, _)| t == EXPIRED).collect();
    assert_eq!(expired.len(), 2, "one refusal, one retry");
    assert_ne!(expired[0].1["authorization"], expired[1].1["authorization"], "a fresh provider token");
    // The token production did not know was delivered through the sandbox.
    assert_eq!(production.iter().filter(|(t, _, _)| t == MISFILED).count(), 1);
    assert_eq!(sandbox.iter().map(|(t, _, _)| t.as_str()).collect::<Vec<_>>(), vec![MISFILED]);

    let (_, headers, body) = expired[1];
    assert_eq!(body["threadId"], thread, "the app gets the whole id");
    let collapse = headers["apns-collapse-id"].to_str().unwrap();
    assert!(collapse.len() == 32 && collapse.is_ascii(), "{collapse}");
    assert_eq!(body["aps"]["alert"]["title"].as_str().unwrap().chars().count(), 120);
    assert_eq!(body["aps"]["alert"]["body"], "Failed");

    // Both devices are still registered: neither was forgotten.
    let status: Value = http()
        .get(format!("http://127.0.0.1:{}/api/domovoi/remote", server.port))
        .bearer_auth(&open)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(status["push"], json!({ "configured": true, "devices": 2 }));
}

/// A client that stops answering pings (gone without a Close frame) is
/// closed, so it no longer counts as an open app; one that answers stays.
#[tokio::test(flavor = "multi_thread")]
async fn a_vanished_client_is_disconnected_and_a_live_one_kept() {
    let (apns_port, _) = mock_apns().await;
    let server = start_with("heartbeat", &[("DOMOVOI_APNS_URL", format!("http://127.0.0.1:{apns_port}")), ("DOMOVOI_WS_HEARTBEAT_MS", "150".into())]);
    let token = phone(&server).await;
    let mut gone = socket(&server, &token).await;
    let mut live = socket(&server, &token).await;
    // `live` keeps reading (tungstenite answers pings as it reads); `gone`
    // isn't read at all, so its pings go unanswered.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let reader = tokio::spawn(async move {
        while let Some(Ok(m)) = live.next().await {
            if let Message::Text(t) = m {
                let _ = tx.send(t.to_string());
            }
        }
    });
    tokio::time::sleep(Duration::from_millis(1200)).await;
    let closed = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            match gone.next().await {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => return true,
                Some(Ok(_)) => {}
            }
        }
    })
    .await
    .unwrap_or(false);
    assert!(closed, "the silent client was closed");
    assert!(!reader.is_finished(), "the answering client is still connected");
    drop(rx.try_recv());
    reader.abort();
}
