//! Golden replay: boots the real `domovoi-server` binary, pairs like the web client,
//! replays every client request from a recorded t3code session
//! (fixtures/golden/ts-session2-boot), and asserts each one gets the same kind
//! of answer the TypeScript server gave (stream chunk / success / failure / none).
//!
//! When the t3code checkout (`.reference/t3code`) and Node 26+ are available,
//! the Rust responses are additionally decoded with t3code's own Effect
//! schemas via `conformance/wire-check.ts`.
//!
//! The replay runs twice: over plain TCP, and through an iroh bridge to a
//! server started with `--remote --relay disabled`.

use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};

/// iroh (through domovoi-remote) turns on reqwest's rustls backend without a crypto
/// provider, so every client needs one installed first.
fn http_client() -> reqwest::Client {
    let _ = rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::new()
}


fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().unwrap()
}

struct Server {
    child: Child,
    /// Where clients connect: the server's TCP port, or the iroh bridge's.
    port: u16,
    pairing: String,
    _bridge: Option<domovoi_remote::Bridge>,
    _home: TempDir,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Transport {
    Tcp,
    Iroh,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct TempDir(PathBuf);
impl TempDir {
    fn new(tag: &str) -> Self {
        let p = std::env::temp_dir().join(format!("domovoi-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        TempDir(p)
    }
}
impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn start_server(transport: Transport) -> Server {
    let home = TempDir::new(&format!("replay-home-{transport:?}"));
    // Bind to a free port chosen up front (the server takes a fixed port).
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let mut args = vec!["--home-dir".to_owned(), home.0.to_str().unwrap().to_owned(), "--port".to_owned(), port.to_string()];
    if transport == Transport::Iroh {
        args.extend(["--remote", "--relay", "disabled"].map(String::from));
    }
    let mut child = Command::new(env!("CARGO_BIN_EXE_domovoi-server")).env("DOMOVOI_NO_SESSION_IMPORT", "1").env("DOMOVOI_NO_SHELL_ENV", "1")
        .args(&args)
        // An empty agent home: the developer's provider logs are not indexed as the server starts.
        .env("DOMOVOI_AGENT_SESSIONS_HOME", home.0.join("agent-home"))
        .env("NO_COLOR", "1")
        .env("RUST_LOG", "warn")
        .stdout(Stdio::piped())
        .stderr(std::fs::File::create(std::env::temp_dir().join(format!("domovoi-replay-{transport:?}-{}.log", std::process::id()))).expect("server log"))
        .spawn()
        .expect("spawn domovoi-server");
    let stdout = child.stdout.take().unwrap();
    let mut pairing = None;
    for line in BufReader::new(stdout).lines() {
        let line = line.unwrap();
        if let Some(idx) = line.find("#token=") {
            pairing = Some(line[idx + 7..].trim().to_owned());
            break;
        }
    }
    let pairing = pairing.expect("server printed a pairing URL");
    let mut server = Server { child, port, pairing, _bridge: None, _home: home };
    if transport == Transport::Iroh {
        server.pairing = minted_code(&server._home.0, port);
        let bridge = bridge_to(&server._home.0);
        server.port = bridge.port();
        server._bridge = Some(bridge);
    }
    server
}

/// A fresh pairing code from `domovoi-server pair`: the one printed at start only
/// works on plain TCP, so the iroh runs mint their own.
fn minted_code(home: &Path, port: u16) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_domovoi-server")).env("DOMOVOI_NO_SESSION_IMPORT", "1").env("DOMOVOI_NO_SHELL_ENV", "1")
        .args(["--home-dir", home.to_str().unwrap(), "--port", &port.to_string(), "pair"])
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    text.lines().find_map(|l| l.split("#token=").nth(1)).expect("pair prints a code").trim().to_owned()
}

/// Waits for the server's saved ticket and opens a bridge to it.
fn bridge_to(home: &Path) -> domovoi_remote::Bridge {
    let path = home.join("remote").join("ticket");
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let ticket = loop {
        if let Ok(t) = std::fs::read_to_string(&path) && !t.trim().is_empty() {
            break t;
        }
        assert!(std::time::Instant::now() < deadline, "the server never saved a remote ticket");
        std::thread::sleep(Duration::from_millis(100));
    };
    let ticket = domovoi_remote::parse_ticket(&ticket).unwrap();
    // The bridge's tasks need a runtime that outlives this call: the test's.
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(domovoi_remote::Bridge::start(
            domovoi_remote::SecretKey::generate(),
            domovoi_remote::Network::Direct { port: 0 },
            ticket,
            0,
        ))
    })
    .unwrap()
}

fn init_repo(dir: &Path) {
    let git = |args: &[&str]| {
        let ok = Command::new("git").args(args).current_dir(dir).output().unwrap().status.success();
        assert!(ok, "git {args:?}");
    };
    git(&["init", "-q", "-b", "main"]);
    std::fs::write(dir.join("calc.py"), "def add(a, b):\n    return a + b\n").unwrap();
    git(&["add", "-A"]);
    git(&["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "init"]);
}

/// Recorded client requests (in order) and the TS outcome category per request.
fn recorded() -> (Vec<Value>, Vec<String>) {
    let path = workspace_root().join("fixtures/golden/ts-session2-boot/wire.ndjson");
    let mut requests = Vec::new();
    let mut keys = Vec::new();
    let mut outcome: HashMap<(i64, String), String> = HashMap::new();
    for line in std::fs::read_to_string(path).unwrap().lines() {
        let v: Value = serde_json::from_str(line).unwrap();
        if v["kind"] != "ws" || !v["frame"].is_object() {
            continue;
        }
        let f = &v["frame"];
        let conn = v["conn"].as_i64().unwrap_or(0);
        match f["_tag"].as_str() {
            Some("Request") => {
                requests.push(f.clone());
                keys.push((conn, f["id"].to_string()));
            }
            Some("Chunk") | Some("Exit") => {
                let key = (conn, f["requestId"].to_string());
                outcome.entry(key).or_insert_with(|| category(f));
            }
            _ => {}
        }
    }
    let cats = keys.iter().map(|k| outcome.get(k).cloned().unwrap_or_else(|| "none".into())).collect();
    (requests, cats)
}

fn category(frame: &Value) -> String {
    match frame["_tag"].as_str() {
        Some("Chunk") => "chunk".into(),
        Some("Exit") => format!("exit-{}", frame["exit"]["_tag"].as_str().unwrap_or("?").to_lowercase()),
        _ => "none".into(),
    }
}

/// Points recorded paths and project ids at this test's fixtures.
fn rewrite(v: &mut Value, repo: &str, project_id: &str) {
    match v {
        Value::Object(m) => {
            for (k, x) in m.iter_mut() {
                match (k.as_str(), &x) {
                    ("cwd" | "workspaceRoot", Value::String(_)) => *x = json!(repo),
                    ("projectId", Value::String(_)) => *x = json!(project_id),
                    _ => rewrite(x, repo, project_id),
                }
            }
        }
        Value::Array(a) => a.iter_mut().for_each(|x| rewrite(x, repo, project_id)),
        _ => {}
    }
}

fn thread_ids(requests: &[Value]) -> Vec<String> {
    let mut ids: Vec<String> = requests
        .iter()
        .filter_map(|r| r["payload"]["threadId"].as_str().map(str::to_owned))
        .collect();
    ids.sort();
    ids.dedup();
    ids
}

#[tokio::test(flavor = "multi_thread")]
async fn replays_recorded_t3code_session_with_matching_outcomes() {
    replay(Transport::Tcp).await;
}

/// The same session through iroh: the bridge, HTTP/1.1 on bi-streams, the
/// cookie session bound to the bridge's key.
#[tokio::test(flavor = "multi_thread")]
async fn replays_recorded_t3code_session_over_iroh() {
    replay(Transport::Iroh).await;
}

async fn replay(transport: Transport) {
    let server = start_server(transport);
    let base = format!("http://127.0.0.1:{}", server.port);
    let repo = TempDir::new(&format!("replay-repo-{transport:?}"));
    init_repo(&repo.0);
    let repo_path = repo.0.canonicalize().unwrap().to_string_lossy().into_owned();

    // Pair exactly like the web client.
    let http = http_client();
    let res = http.post(format!("{base}/api/auth/browser-session")).json(&json!({ "credential": server.pairing })).send().await.unwrap();
    assert_eq!(res.status(), 200, "pairing accepted");
    let cookie = res.headers().get("set-cookie").unwrap().to_str().unwrap().split(';').next().unwrap().to_owned();
    let state: Value = http.get(format!("{base}/api/auth/session")).header("cookie", &cookie).send().await.unwrap().json().await.unwrap();
    assert_eq!(state["authenticated"], true);
    // A second exchange of the same code must fail (one-time token).
    let again = http.post(format!("{base}/api/auth/browser-session")).json(&json!({ "credential": server.pairing })).send().await.unwrap();
    assert_eq!(again.status(), 401);

    let mut req = format!("ws://127.0.0.1:{}/ws?orchestrationProtocol=2&frameParts=1", server.port).into_client_request().unwrap();
    req.headers_mut().insert("cookie", cookie.parse().unwrap());
    let (mut ws, _) = tokio_tungstenite::connect_async(req).await.expect("authenticated websocket");

    // Fixtures the recorded requests refer to: a project and the recorded threads.
    let send = |v: Value| Message::Text(v.to_string().into());
    let project_id = "replay-project";
    let mut next_id = 100_000i64;
    let mut setup = vec![json!({ "_tag": "Request", "id": next_id.to_string(), "tag": "projects.mutate", "headers": [],
        "payload": { "type": "project.create", "commandId": "replay-setup-project", "projectId": project_id, "title": "replay",
                     "workspaceRoot": repo_path, "createWorkspaceRootIfMissing": false, "defaultModelSelection": null } })];
    let (requests, expected) = recorded();
    for tid in thread_ids(&requests) {
        next_id += 1;
        setup.push(json!({ "_tag": "Request", "id": next_id.to_string(), "tag": "orchestration.launchThread", "headers": [],
            "payload": { "commandId": format!("replay-setup-{tid}"), "threadId": tid, "projectId": project_id, "title": "replay",
                         "modelSelection": { "instanceId": "codex", "model": "gpt-6-luna" }, "runtimeMode": "full-access",
                         "interactionMode": "default", "workspaceStrategy": { "type": "root" } } }));
    }
    // Setup runs one request at a time: the server handles requests concurrently.
    for r in &setup {
        let id = r["id"].as_str().unwrap().to_owned();
        ws.send(send(r.clone())).await.unwrap();
        loop {
            let msg = tokio::time::timeout(Duration::from_secs(10), ws.next()).await.expect("setup reply").unwrap().unwrap();
            let Message::Text(t) = msg else { continue };
            let v: Value = serde_json::from_str(&t).unwrap();
            if v["_tag"] == "Exit" && v["requestId"] == json!(id) {
                assert_eq!(v["exit"]["_tag"], "Success", "setup failed: {v}");
                break;
            }
        }
    }

    // Replay. Request ids are renumbered so both recorded connections fit one socket.
    let mut tags = Vec::new();
    for (i, r) in requests.iter().enumerate() {
        let mut r = r.clone();
        rewrite(&mut r["payload"], &repo_path, project_id);
        r["id"] = json!(i.to_string());
        tags.push(r["tag"].as_str().unwrap_or_default().to_owned());
        ws.send(send(r)).await.unwrap();
    }

    let mut got: BTreeMap<usize, String> = BTreeMap::new();
    let mut transcript = Vec::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(12);
    // The socket was opened with `frameParts=1`: a frame over 48 KB arrives
    // as parts prefixed with U+001E, the last one without the prefix.
    let mut partial = String::new();
    while got.len() < requests.len() {
        let Ok(Some(Ok(msg))) = tokio::time::timeout_at(deadline, ws.next()).await else { break };
        let Message::Text(t) = msg else { continue };
        if let Some(part) = t.strip_prefix('\u{1e}') {
            partial.push_str(part);
            continue;
        }
        let t = std::mem::take(&mut partial) + t.as_str();
        let v: Value = serde_json::from_str(&t).unwrap();
        if v["_tag"] == "Chunk" {
            // Acknowledge so streams keep flowing (Effect flow control).
            if let Err(e) = ws.send(send(json!({ "_tag": "Ack", "requestId": v["requestId"] }))).await {
                let log = std::fs::read_to_string(std::env::temp_dir().join(format!("domovoi-replay-{transport:?}-{}.log", std::process::id()))).unwrap_or_default();
                panic!("the server closed the socket ({e}); its log:\n{}", &log[log.len().saturating_sub(4000)..]);
            }
        }
        transcript.push(v.clone());
        if let Some(i) = v["requestId"].as_str().and_then(|s| s.parse::<usize>().ok()) {
            if i < requests.len() {
                got.entry(i).or_insert_with(|| category(&v));
            }
        }
    }

    let mut mismatches = Vec::new();
    for (i, want) in expected.iter().enumerate() {
        let have = got.get(&i).cloned().unwrap_or_else(|| "none".into());
        if &have != want {
            mismatches.push(format!("#{i} {}: t3code={want} domovoi={have}", tags[i]));
        }
    }
    assert!(mismatches.is_empty(), "{} of {} replayed requests differ:\n{}", mismatches.len(), requests.len(), mismatches.join("\n"));

    schema_check(&requests, &tags, &transcript, &repo_path, project_id, transport);
}

/// Decodes the Rust responses with t3code's own schemas when the reference
/// checkout and Node 26+ are present; otherwise reports that it was skipped.
fn schema_check(requests: &[Value], tags: &[String], transcript: &[Value], repo: &str, project_id: &str, transport: Transport) {
    let root = workspace_root();
    let node = std::env::var("DOMOVOI_NODE").unwrap_or_else(|_| "node".into());
    let version = Command::new(&node).arg("--version").output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned());
    let major: u32 = version.as_deref().and_then(|v| v.trim_start_matches('v').split('.').next()?.parse().ok()).unwrap_or(0);
    if !root.join(".reference/t3code/packages/contracts").exists() || major < 26 {
        eprintln!("schema check skipped (needs .reference/t3code and Node 26+, found {version:?})");
        return;
    }
    let file = std::env::temp_dir().join(format!("domovoi-replay-{transport:?}-{}.ndjson", std::process::id()));
    let mut out = String::new();
    for (i, r) in requests.iter().enumerate() {
        let mut r = r.clone();
        rewrite(&mut r["payload"], repo, project_id);
        r["id"] = json!(i.to_string());
        r["tag"] = json!(tags[i]);
        out.push_str(&json!({ "kind": "ws", "conn": 0, "dir": "c2s", "frame": r }).to_string());
        out.push('\n');
    }
    for f in transcript {
        out.push_str(&json!({ "kind": "ws", "conn": 0, "dir": "s2c", "frame": f }).to_string());
        out.push('\n');
    }
    std::fs::write(&file, out).unwrap();
    let res = Command::new(&node).arg(root.join("conformance/wire-check.ts")).arg(&file).arg("--quiet").output().unwrap();
    let _ = std::fs::remove_file(&file);
    let text = String::from_utf8_lossy(&res.stdout);
    assert!(res.status.success(), "t3code schema check failed:\n{text}");
    eprintln!("{}", text.trim());
}
