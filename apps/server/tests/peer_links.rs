//! Linked environments over iroh against the real binary: two servers with
//! `--remote --relay disabled` (nothing leaves this machine), one linked to
//! the other with a pairing link and code from it; the link listed, used, and
//! ended from either side.

use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};

/// The socket URL names the version of the protocol the test speaks.
const PROTOCOL: u32 = domovoi_model::PROTOCOL_VERSION;

fn http_client() -> reqwest::Client {
    let _ = rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::new()
}

struct Server {
    child: Child,
    port: u16,
    home: PathBuf,
    startup_code: String,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

fn start(tag: &str) -> Server {
    let home = std::env::temp_dir().join(format!("domovoi-peer-links-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let mut child = Command::new(env!("CARGO_BIN_EXE_domovoi-server"))
        .env("DOMOVOI_NO_SESSION_IMPORT", "1")
        .env("DOMOVOI_NO_SHELL_ENV", "1")
        .env("DOMOVOI_AGENT_SESSIONS_HOME", home.join("agent-home"))
        .args(["--home-dir", home.to_str().unwrap(), "--port", &port.to_string(), "--remote", "--relay", "disabled"])
        .env("RUST_LOG", "warn")
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

/// `domovoi-server pair --remote --json` on `server`: its link and code.
fn pairing(server: &Server) -> (String, String) {
    let out = Command::new(env!("CARGO_BIN_EXE_domovoi-server"))
        .env("DOMOVOI_NO_SESSION_IMPORT", "1")
        .env("DOMOVOI_NO_SHELL_ENV", "1")
        .args(["--home-dir", server.home.to_str().unwrap(), "--port", &server.port.to_string(), "pair", "--json"])
        .output()
        .unwrap();
    assert!(out.status.success(), "pair failed: {}", String::from_utf8_lossy(&out.stderr));
    let report: Value = serde_json::from_slice(&out.stdout).unwrap();
    (report["link"].as_str().unwrap().to_owned(), report["code"].as_str().unwrap().to_owned())
}

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// The server's socket over TCP, and the `Authorization` header of its session.
async fn connect(server: &Server) -> (Ws, String) {
    let base = format!("http://127.0.0.1:{}", server.port);
    let session = sign_in(&base, &server.startup_code).await;
    let mut req = format!("ws://127.0.0.1:{}/ws?protocol={PROTOCOL}", server.port).into_client_request().unwrap();
    req.headers_mut().insert("authorization", session.parse().unwrap());
    (tokio_tungstenite::connect_async(req).await.unwrap().0, session)
}

/// Calls `tag`; the first Chunk or the Exit.
struct Rpc {
    ws: Ws,
    id: u64,
}

impl Rpc {
    async fn call(&mut self, tag: &str, payload: Value) -> Value {
        self.id += 1;
        let id = self.id.to_string();
        let request = json!({ "_tag": "Request", "id": id, "tag": tag, "payload": payload, "headers": [] });
        self.ws.send(Message::Text(request.to_string().into())).await.unwrap();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(90);
        loop {
            let msg = tokio::time::timeout_at(deadline, self.ws.next()).await.expect("a reply in time").unwrap().unwrap();
            let Message::Text(t) = msg else { continue };
            let v: Value = serde_json::from_str(&t).unwrap();
            if v["_tag"] == "Ping" {
                self.ws.send(Message::Text(json!({ "_tag": "Pong" }).to_string().into())).await.unwrap();
                continue;
            }
            if v["requestId"] != json!(id) {
                continue;
            }
            if v["_tag"] == "Chunk" {
                let interrupt = json!({ "_tag": "Interrupt", "requestId": id, "interruptors": [] });
                self.ws.send(Message::Text(interrupt.to_string().into())).await.unwrap();
            }
            return v;
        }
    }

    /// The success value of `tag`, failing the test otherwise.
    async fn ok(&mut self, tag: &str, payload: Value) -> Value {
        let reply = self.call(tag, payload).await;
        assert_eq!(reply["exit"]["_tag"], "Success", "{tag}: {reply}");
        reply["exit"]["value"].clone()
    }

    /// The typed error `tag` fails with.
    async fn failure(&mut self, tag: &str, payload: Value) -> Value {
        let reply = self.call(tag, payload).await;
        assert_eq!(reply["exit"]["_tag"], "Failure", "{tag}: {reply}");
        reply["exit"]["cause"][0]["error"].clone()
    }
}

/// The sessions `server` lists for linked environments (`subscribeAuthAccess`).
async fn linked_sessions(peer: &mut Rpc) -> Vec<Value> {
    let chunk = peer.call("subscribeAuthAccess", json!({})).await;
    assert_eq!(chunk["_tag"], "Chunk", "{chunk}");
    let snapshot = &chunk["values"][0]["payload"];
    snapshot["clientSessions"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["client"]["label"].as_str().is_some_and(|l| l.starts_with("Domovoi · ")))
        .cloned()
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_server_links_another_over_iroh_and_either_side_ends_it() {
    let (a, b) = (start("a"), start("b"));
    let mut here = Rpc { ws: connect(&a).await.0, id: 0 };
    let (peer_ws, peer_session) = connect(&b).await;
    let mut there = Rpc { ws: peer_ws, id: 0 };
    let b_env = here_env(&b).await;
    let (link, code) = pairing(&b);

    // Off until Linked environments is turned on.
    let off = here.failure("peerLinks.link", json!({ "url": link, "pairingCode": code, "access": "read-only" })).await;
    assert_eq!((&off["_tag"], &off["reason"]), (&json!("PeerLinkError"), &json!("disabled")));
    here.ok("server.updateSettings", json!({ "patch": { "linkedEnvironmentsEnabled": true } })).await;
    assert_eq!(here.ok("peerLinks.list", json!({})).await, json!({ "links": [] }));

    // Refusals that spend no code: a web address, a mode ceiling, this server itself.
    let web = here.failure("peerLinks.link", json!({ "url": "https://box.example.com", "pairingCode": code, "access": "read-only" })).await;
    assert_eq!(web["reason"], "unreachable");
    let ceiling = here.failure("peerLinks.link", json!({ "url": link, "pairingCode": code, "access": "auto" })).await;
    assert_eq!(ceiling["reason"], "incompatible");
    let (own_link, own_code) = pairing(&a);
    let own = here.failure("peerLinks.link", json!({ "url": own_link, "pairingCode": own_code, "access": "read-only" })).await;
    assert_eq!(own["reason"], "self");
    let wrong = here.failure("peerLinks.link", json!({ "url": link, "pairingCode": "WRONGCODE234", "access": "read-only" })).await;
    assert_eq!(wrong["reason"], "pairing_rejected", "{wrong}");
    let elsewhere =
        here.failure("peerLinks.link", json!({ "url": link, "pairingCode": code, "access": "read-only", "expectedEnvironmentId": "env-elsewhere", "expectedLabel": "VPS" })).await;
    assert_eq!((&elsewhere["reason"], elsewhere["answeredAs"].is_string()), (&json!("unreachable"), true), "{elsewhere}");

    // Linked read-only, with the code the peer minted.
    let linked = here.ok("peerLinks.link", json!({ "url": link, "pairingCode": code, "access": "read-only" })).await;
    assert_eq!(linked["environmentId"], b_env);
    assert_eq!(linked["access"], "read-only");
    assert!(linked["urls"].as_array().unwrap().iter().all(|u| u.as_str().is_some_and(|u| u.starts_with("domovoi://connect?ticket=") && !u.contains(&code))), "{linked}");
    let listed = here.ok("peerLinks.list", json!({})).await;
    assert_eq!(listed["links"][0]["status"], "reachable", "{listed}");
    assert_eq!(listed["links"][0]["environmentId"], b_env);
    let again = here.failure("peerLinks.link", json!({ "url": link, "pairingCode": code, "access": "read-only" })).await;
    assert_eq!(again["reason"], "pairing_rejected", "codes work once");

    // The peer lists the link as a client with the access asked for, and nothing more.
    let sessions = linked_sessions(&mut there).await;
    assert_eq!(sessions.len(), 1, "{sessions:?}");
    assert_eq!(sessions[0]["scopes"], json!(["orchestration:read"]));
    assert_eq!(sessions[0]["permissions"], json!(["orchestration:read"]));

    // The peer revokes it: the link lists as expired, with why.
    let base_b = format!("http://127.0.0.1:{}", b.port);
    let revoked = http_client().post(format!("{base_b}/api/auth/clients/revoke")).header("authorization", &peer_session).json(&json!({ "sessionId": sessions[0]["sessionId"] })).send().await.unwrap();
    assert_eq!(revoked.status(), 200);
    let listed = here.ok("peerLinks.list", json!({})).await;
    assert_eq!(listed["links"][0]["status"], "expired", "{listed}");
    assert!(listed["links"][0]["lastError"].as_str().is_some_and(|e| e.contains("signed this environment out")), "{listed}");

    // Linked again with full access, from the environment the user picked.
    let (link, code) = pairing(&b);
    let relinked = here.ok("peerLinks.link", json!({ "url": link, "pairingCode": code, "access": "full-access", "expectedEnvironmentId": b_env })).await;
    assert_eq!(relinked["access"], "full-access");
    assert_eq!(here.ok("peerLinks.list", json!({})).await["links"][0]["status"], "reachable");
    let sessions = linked_sessions(&mut there).await;
    assert_eq!(sessions.len(), 1, "the revoked session is gone: {sessions:?}");
    let mut scopes: Vec<String> = serde_json::from_value(sessions[0]["permissions"].clone()).unwrap();
    scopes.sort();
    assert_eq!(scopes, ["orchestration:operate", "orchestration:read", "settings:write", "source-control:write"]);

    // Unlinking here signs the session out there too.
    assert_eq!(here.ok("peerLinks.unlink", json!({ "environmentId": b_env })).await, json!({ "removed": true }));
    assert_eq!(here.ok("peerLinks.list", json!({})).await, json!({ "links": [] }));
    assert!(linked_sessions(&mut there).await.is_empty());
    assert_eq!(here.ok("peerLinks.unlink", json!({ "environmentId": b_env })).await, json!({ "removed": false }));
    assert!(!a.home.join("userdata/server-secrets").read_dir().map(|d| d.flatten().any(|e| e.file_name().to_string_lossy().starts_with("peer-link-"))).unwrap_or(false));

}

/// The environment id `server` answers with.
async fn here_env(server: &Server) -> Value {
    let d: Value = http_client().get(format!("http://127.0.0.1:{}/.well-known/domovoi/environment", server.port)).send().await.unwrap().json().await.unwrap();
    d["environmentId"].clone()
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
