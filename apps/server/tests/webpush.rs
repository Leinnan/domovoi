//! Web Push to UnifiedPush endpoints (`push_web.rs`) against a mock push
//! service: a phone registers an endpoint with its keys; a thread that fails
//! pushes "Failed" to it as an RFC 8291 message the phone's key decrypts,
//! signed with VAPID for the endpoint's origin; a phone with the app open is
//! skipped; an endpoint the service calls gone (410) is forgotten; bad
//! registrations and unsigned requests are refused.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::post;
use axum::Router;
use base64::Engine;
use futures::{SinkExt, StreamExt};
use ring::agreement::{ECDH_P256, EphemeralPrivateKey, UnparsedPublicKey, agree_ephemeral};
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

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn unb64(text: &str) -> Vec<u8> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(text).unwrap()
}

/// What the mock push service received: the endpoint's name, headers, body.
type Received = Arc<Mutex<Vec<(String, HeaderMap, Vec<u8>)>>>;

async fn mock_push_service() -> (u16, Received) {
    let received: Received = Arc::default();
    let app = Router::new()
        .route(
            "/up/{name}",
            post(|State(r): State<Received>, Path(name): Path<String>, headers: HeaderMap, body: axum::body::Bytes| async move {
                r.lock().unwrap().push((name.clone(), headers, body.to_vec()));
                if name == "gone" { StatusCode::GONE } else { StatusCode::CREATED }
            }),
        )
        .with_state(received.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
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

fn start() -> Server {
    let dir = std::env::temp_dir().join(format!("domovoi-webpush-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("w")).unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let mut child = Command::new(env!("CARGO_BIN_EXE_domovoi-server"))
        .env("DOMOVOI_NO_SESSION_IMPORT", "1")
        .env("DOMOVOI_NO_SHELL_ENV", "1")
        .args(["--home-dir", dir.join("home").to_str().unwrap(), "--port", &port.to_string()])
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
    let out = Command::new(env!("CARGO_BIN_EXE_domovoi-server"))
        .env("DOMOVOI_NO_SESSION_IMPORT", "1")
        .env("DOMOVOI_NO_SHELL_ENV", "1")
        .args(["--home-dir", server.dir.join("home").to_str().unwrap(), "--port", &server.port.to_string(), "pair"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.lines().find_map(|l| l.split("#token=").nth(1)).unwrap().trim().to_owned()
}

async fn phone(server: &Server) -> String {
    let body = format!(
        "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Atoken-exchange&subject_token={}&subject_token_type=urn%3At3%3Aparams%3Aoauth%3Atoken-type%3Aenvironment-bootstrap&requested_token_type=urn%3Aietf%3Aparams%3Aoauth%3Atoken-type%3Aaccess_token&client_device_type=mobile",
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

/// A device's Web Push keys: its ECDH key (used once, to read one message) and auth secret.
struct Keys {
    private: EphemeralPrivateKey,
    public: Vec<u8>,
    auth: [u8; 16],
}

fn keys() -> Keys {
    let rng = ring::rand::SystemRandom::new();
    let private = EphemeralPrivateKey::generate(&ECDH_P256, &rng).unwrap();
    let public = private.compute_public_key().unwrap().as_ref().to_vec();
    let mut auth = [0u8; 16];
    ring::rand::SecureRandom::fill(&rng, &mut auth).unwrap();
    Keys { private, public, auth }
}

async fn register(server: &Server, token: &str, endpoint: &str, keys: &Keys) -> reqwest::Response {
    http()
        .post(format!("http://127.0.0.1:{}/api/domovoi/push/webpush", server.port))
        .bearer_auth(token)
        .json(&json!({ "endpoint": endpoint, "p256dh": b64(&keys.public), "auth": b64(&keys.auth) }))
        .send()
        .await
        .unwrap()
}

/// RFC 8291 decryption, as a device does it: written here apart from the server's code.
fn decrypt(keys: Keys, body: &[u8]) -> Vec<u8> {
    use ring::hkdf;
    struct Len(usize);
    impl hkdf::KeyType for Len {
        fn len(&self) -> usize {
            self.0
        }
    }
    let expand = |prk: &hkdf::Prk, info: &[u8], len: usize| {
        let mut out = vec![0u8; len];
        prk.expand(&[info], Len(len)).unwrap().fill(&mut out).unwrap();
        out
    };
    let salt = &body[..16];
    assert_eq!(u32::from_be_bytes(body[16..20].try_into().unwrap()), 4096, "record size");
    let id_len = body[20] as usize;
    let as_public = &body[21..21 + id_len];
    let record = &body[21 + id_len..];
    let ecdh = agree_ephemeral(keys.private, &UnparsedPublicKey::new(&ECDH_P256, as_public), |s| s.to_vec()).unwrap();
    let prk_key = hkdf::Salt::new(hkdf::HKDF_SHA256, &keys.auth).extract(&ecdh);
    let ikm = expand(&prk_key, &[b"WebPush: info\0".as_slice(), &keys.public, as_public].concat(), 32);
    let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, salt).extract(&ikm);
    let cek = expand(&prk, b"Content-Encoding: aes128gcm\0", 16);
    let nonce = expand(&prk, b"Content-Encoding: nonce\0", 12);
    let key = ring::aead::LessSafeKey::new(ring::aead::UnboundKey::new(&ring::aead::AES_128_GCM, &cek).unwrap());
    let mut data = record.to_vec();
    let plain = key.open_in_place(ring::aead::Nonce::try_assume_unique_for_key(&nonce).unwrap(), ring::aead::Aad::empty(), &mut data).unwrap();
    assert_eq!(plain.last(), Some(&2), "the last record's delimiter");
    plain[..plain.len() - 1].to_vec()
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
async fn a_failed_turn_pushes_an_encrypted_message_to_closed_android_apps() {
    let (push_port, received) = mock_push_service().await;
    let server = start();
    let base = format!("http://127.0.0.1:{push_port}/up");

    // Unsigned, and badly formed registrations, are refused.
    let unsigned = http().post(format!("http://127.0.0.1:{}/api/domovoi/push/webpush", server.port)).json(&json!({ "endpoint": "https://x/y" })).send().await.unwrap();
    assert_eq!(unsigned.status(), 401);
    let closed = phone(&server).await;
    let res = http()
        .post(format!("http://127.0.0.1:{}/api/domovoi/push/webpush", server.port))
        .bearer_auth(&closed)
        .json(&json!({ "endpoint": "file:///etc/passwd", "p256dh": "AAAA", "auth": "AAAA" }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 400);

    let good = keys();
    let res = register(&server, &closed, &format!("{base}/good"), &good).await;
    assert_eq!(res.status(), 200);
    let registered: Value = res.json().await.unwrap();
    let vapid_key = registered["vapidPublicKey"].as_str().unwrap().to_owned();
    assert_eq!(unb64(&vapid_key).len(), 65, "an uncompressed P-256 key");
    let gone = keys();
    assert_eq!(register(&server, &closed, &format!("{base}/gone"), &gone).await.status(), 200);
    // This phone has the app open (a socket), so it notifies itself.
    let open = phone(&server).await;
    let open_keys = keys();
    assert_eq!(register(&server, &open, &format!("{base}/open"), &open_keys).await.status(), 200);
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

    // The provider binary exits at once: the turn fails and pushes go out.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    while received.lock().unwrap().len() < 2 && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    let got = received.lock().unwrap().clone();
    let names: Vec<&str> = got.iter().map(|(n, _, _)| n.as_str()).collect();
    assert!(names.contains(&"good") && names.contains(&"gone"), "pushed to {names:?}");
    assert!(!names.contains(&"open"), "the open app is skipped");

    let (_, headers, body) = got.iter().find(|(n, _, _)| n == "good").unwrap();
    assert_eq!(headers["content-encoding"], "aes128gcm");
    assert_eq!(headers["ttl"], "86400");
    assert_eq!(headers["urgency"], "normal");
    assert_eq!(headers["topic"].to_str().unwrap().len(), 32);
    // VAPID: signed for the endpoint's origin by the key the registration named.
    let auth = headers["authorization"].to_str().unwrap();
    let (token, key) = auth.strip_prefix("vapid t=").and_then(|rest| rest.split_once(", k=")).expect("vapid t=…, k=…");
    assert_eq!(key, vapid_key);
    let claims: Value = serde_json::from_slice(&unb64(token.split('.').nth(1).unwrap())).unwrap();
    assert_eq!(claims["aud"], format!("http://127.0.0.1:{push_port}"));
    let (input, signature) = token.rsplit_once('.').unwrap();
    ring::signature::UnparsedPublicKey::new(&ring::signature::ECDSA_P256_SHA256_FIXED, unb64(key)).verify(input.as_bytes(), &unb64(signature)).expect("a valid ES256 signature");
    // Only the device's key reads it.
    let message: Value = serde_json::from_slice(&decrypt(good, body)).unwrap();
    assert_eq!((message["threadId"].as_str(), message["title"].as_str(), message["body"].as_str()), (Some("t1"), Some("Fix the tests"), Some("Failed")), "{message}");
    assert!(message["environmentId"].as_str().is_some_and(|id| !id.is_empty()));

    // The endpoint the service called gone was forgotten; the others stay.
    let db = rusqlite::Connection::open(server.dir.join("home/userdata/domovoi.sqlite")).unwrap();
    let mut endpoints: Vec<String> =
        db.prepare("SELECT endpoint FROM webpush_devices").unwrap().query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect();
    endpoints.sort();
    assert_eq!(endpoints, [format!("{base}/good"), format!("{base}/open")]);

    // A session removes its own registration.
    let res = http()
        .delete(format!("http://127.0.0.1:{}/api/domovoi/push/webpush", server.port))
        .bearer_auth(&open)
        .json(&json!({ "endpoint": format!("{base}/open") }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let left: i64 = db.query_row("SELECT count(*) FROM webpush_devices", [], |r| r.get(0)).unwrap();
    assert_eq!(left, 1);
}
