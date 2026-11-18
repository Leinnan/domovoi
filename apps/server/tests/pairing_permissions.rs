//! `domovoi-server pair` mints codes for a running server, and the permissions a
//! code grants are enforced per RPC with t3code's authorization error.

use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};

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
    let res = http_client().post(format!("{base}/api/auth/browser-session")).json(&json!({ "credential": code })).send().await.unwrap();
    assert_eq!(res.status(), 200);
    let cookie = res.headers().get("set-cookie").unwrap().to_str().unwrap().split(';').next().unwrap().to_owned();
    let mut req = format!("ws://127.0.0.1:{}/ws", server.port).into_client_request().unwrap();
    req.headers_mut().insert("cookie", cookie.parse().unwrap());
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

    // The default code grants full access, and each code works only once.
    let full = pair_code(&server, &[]);
    let mut ws = connect(&server, &full).await;
    let browse = call(&mut ws, "1", "filesystem.browse", json!({ "partialPath": "/" })).await;
    assert_eq!(browse["exit"]["_tag"], "Success", "{browse}");
    let again = http_client()
        .post(format!("http://127.0.0.1:{}/api/auth/browser-session", server.port))
        .json(&json!({ "credential": full }))
        .send()
        .await
        .unwrap();
    assert_eq!(again.status(), 401);
}
