//! Calls every RPC listed in `conformance/rpc-calls/*.json` (one file per
//! area, run in file-name order) against the real
//! `domovoi-server` binary and checks each answer's kind (first chunk / success /
//! typed failure). Defects and unregistered methods always fail.
//!
//! When the t3code checkout and Node 26+ are available, every request and
//! answer is also decoded with t3code's own schemas (`conformance/wire-check.ts`).
//!
//! Case fields: `tag`, `payload`, `expect` (`chunk` | `success` | `failure`),
//! optional `error` (the failure's `_tag`). Strings in payloads may use
//! `$WORKSPACE`, `$PROJECT`, `$THREAD`, `$HOME` and `$ITEM` (the thread's first turn item).
//!
//! `conformance/rpc-groups/<name>.json` holds, group by group, the calls that
//! reach every answer and every failure of a group's methods; see
//! [`every_rpc_group_answers_as_recorded`].

use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};

mod dump;

/// iroh (through domovoi-remote) turns on reqwest's rustls backend without a crypto
/// provider, so every client needs one installed first.
fn http_client() -> reqwest::Client {
    let _ = rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::new()
}


type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// The checkout whose `conformance` folder is read: this one, or with
/// `DOMOVOI_WORKSPACE_ROOT` another (a test built here, run for a copy).
fn workspace_root() -> PathBuf {
    let root = std::env::var("DOMOVOI_WORKSPACE_ROOT").map(PathBuf::from).unwrap_or_else(|_| Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
    root.canonicalize().unwrap()
}

struct Server {
    child: Child,
    /// Where clients connect: the server's TCP port, or the iroh bridge's.
    port: u16,
    pairing: String,
    dir: PathBuf,
    _bridge: Option<domovoi_remote::Bridge>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Transport {
    Tcp,
    /// Through an iroh bridge to a server started with `--remote --relay disabled`.
    Iroh,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn start(dir: &Path) -> Server {
    start_with(dir, Transport::Tcp)
}

fn start_with(dir: &Path, transport: Transport) -> Server {
    start_env(dir, transport, &[])
}

/// The server under test: the one built with this test, or with
/// `DOMOVOI_SERVER_BIN` another build of it (to record what that one answers).
fn server_bin() -> String {
    std::env::var("DOMOVOI_SERVER_BIN").unwrap_or_else(|_| env!("CARGO_BIN_EXE_domovoi-server").to_owned())
}

/// Starts the server with `env` laid over the test's own environment.
fn start_env(dir: &Path, transport: Transport, env: &[(String, String)]) -> Server {
    let home = dir.join("home");
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let mut args = vec!["--home-dir".to_owned(), home.to_str().unwrap().to_owned(), "--port".to_owned(), port.to_string()];
    if transport == Transport::Iroh {
        args.extend(["--remote", "--relay", "disabled"].map(String::from));
    }
    let mut child = Command::new(server_bin()).env("DOMOVOI_NO_SESSION_IMPORT", "1").env("DOMOVOI_NO_SHELL_ENV", "1")
        .args(&args)
        .env("RUST_LOG", "warn")
        // Turns fail at once instead of reaching a real provider.
        .env("DOMOVOI_CLAUDE_BINARY", "/usr/bin/false")
        .env("DOMOVOI_CODEX_BINARY", "/usr/bin/false")
        // System tools only: no editor, `gh` or provider CLI is found, so no case can launch one.
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        // Agent-session scans read this empty home, never the real ~/.claude or ~/.codex.
        .env("DOMOVOI_AGENT_SESSIONS_HOME", dir.join("agent-home"))
        .envs(env.iter().map(|(key, value)| (key.as_str(), value.as_str())))
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let out = child.stdout.take().unwrap();
    let mut pairing = String::new();
    for line in BufReader::new(out).lines() {
        let line = line.unwrap();
        if let Some(i) = line.find("#token=") {
            pairing = line[i + 7..].trim().to_owned();
            break;
        }
    }
    let mut server = Server { child, port, pairing, dir: dir.to_owned(), _bridge: None };
    if transport == Transport::Iroh {
        server.pairing = minted_code(&home, port);
        let bridge = bridge_to(&home);
        server.port = bridge.port();
        server._bridge = Some(bridge);
    }
    server
}

/// A fresh pairing code from `domovoi-server pair`: the one printed at start only
/// works on plain TCP, so the iroh runs mint their own.
fn minted_code(home: &Path, port: u16) -> String {
    let out = Command::new(server_bin()).env("DOMOVOI_NO_SESSION_IMPORT", "1").env("DOMOVOI_NO_SHELL_ENV", "1")
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

async fn connect(server: &Server) -> Ws {
    connect_with_cookie(server).await.0
}

/// The socket, and the session cookie it was opened with.
async fn connect_with_cookie(server: &Server) -> (Ws, String) {
    let base = format!("http://127.0.0.1:{}", server.port);
    let res = http_client()
        .post(format!("{base}/api/auth/browser-session"))
        .json(&json!({ "credential": server.pairing }))
        .send()
        .await
        .unwrap();
    assert_eq!(res.status(), 200);
    let cookie = res.headers().get("set-cookie").unwrap().to_str().unwrap().split(';').next().unwrap().to_owned();
    let mut req = format!("ws://127.0.0.1:{}/ws", server.port).into_client_request().unwrap();
    req.headers_mut().insert("cookie", cookie.parse().unwrap());
    (tokio_tungstenite::connect_async(req).await.unwrap().0, cookie)
}

/// Sends a request; returns its first Chunk / Exit / Defect frame (streams are interrupted after one chunk).
async fn call(ws: &mut Ws, id: u64, tag: &str, payload: &Value) -> Option<Value> {
    let req = json!({ "_tag": "Request", "id": id.to_string(), "tag": tag, "payload": payload, "headers": [] });
    ws.send(Message::Text(req.to_string().into())).await.unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let msg = match tokio::time::timeout_at(deadline, ws.next()).await {
            Ok(Some(Ok(m))) => m,
            _ => return None,
        };
        let Message::Text(t) = msg else { continue };
        let v: Value = serde_json::from_str(&t).unwrap();
        if v["_tag"] == "Ping" {
            ws.send(Message::Text(json!({ "_tag": "Pong" }).to_string().into())).await.unwrap();
            continue;
        }
        if v["requestId"] != json!(id.to_string()) && v["_tag"] != "Defect" {
            continue;
        }
        if v["_tag"] == "Chunk" {
            let interrupt = json!({ "_tag": "Interrupt", "requestId": id.to_string(), "interruptors": [] });
            ws.send(Message::Text(interrupt.to_string().into())).await.unwrap();
        }
        return Some(v);
    }
}

fn substitute(v: &mut Value, vars: &[(&str, String)]) {
    match v {
        Value::String(s) => {
            for (k, val) in vars {
                if s.contains(k) {
                    *s = s.replace(k, val);
                }
            }
        }
        Value::Array(a) => a.iter_mut().for_each(|x| substitute(x, vars)),
        Value::Object(m) => m.values_mut().for_each(|x| substitute(x, vars)),
        _ => {}
    }
}

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git").args(args).current_dir(dir).output().unwrap().status.success();
    assert!(ok, "git {args:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn every_listed_rpc_answers_with_its_expected_kind() {
    all_cases(Transport::Tcp).await;
}

/// The same cases through iroh: the bridge, HTTP/1.1 on bi-streams, the
/// WebSocket upgrade and the peer-bound session.
#[tokio::test(flavor = "multi_thread")]
async fn every_listed_rpc_answers_over_iroh() {
    all_cases(Transport::Iroh).await;
}

async fn all_cases(transport: Transport) {
    let started = dump::started();
    let dir = std::env::temp_dir().join(format!("domovoi-calls-{transport:?}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let workspace = dir.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    git(&workspace, &["init", "-q", "-b", "main"]);
    std::fs::write(workspace.join("calc.py"), "def add(a, b):\n    return a + b\n").unwrap();
    std::fs::write(workspace.join("README.md"), "# Calc\n\nAdds numbers.\n").unwrap();
    git(&workspace, &["add", "-A"]);
    git(&workspace, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "init"]);
    let workspace = workspace.canonicalize().unwrap();
    let server = start_with(&dir, transport);
    let mut ws = connect(&server).await;
    let mut transcript: Vec<Value> = vec![];
    let mut id = 0u64;

    // Fixture data: a project and a thread whose turn fails at once.
    let project = "project-1";
    let thread = "thread-1";
    let setup = [
        ("projects.mutate", json!({ "type": "project.create", "commandId": "c1", "projectId": project, "title": "calc",
            "workspaceRoot": workspace, "createWorkspaceRootIfMissing": false, "defaultModelSelection": null })),
        ("orchestration.launchThread", json!({ "commandId": "c2", "creationSource": "web", "threadId": thread, "projectId": project,
            "title": "Find the add function", "modelSelection": { "instanceId": "claudeAgent", "model": "claude-haiku-5-5" },
            "runtimeMode": "approval-required", "interactionMode": "default", "workspaceStrategy": { "type": "root" },
            "initialMessage": { "messageId": "m1", "text": "Where is the add function defined?", "attachments": [] } })),
    ];
    for (tag, payload) in setup {
        id += 1;
        let r = call(&mut ws, id, tag, &payload).await.expect("setup reply");
        assert_eq!(r["exit"]["_tag"], "Success", "{tag}: {r}");
    }
    // Wait for the failed turn to settle so items exist. A run is marked failed
    // before finalization captures its checkpoint (turn count 1), so wait for
    // the checkpoint to be linked as well: the diff cases depend on it.
    let mut item = String::new();
    for _ in 0..100 {
        id += 1;
        let r = call(&mut ws, id, "orchestration.getThreadProjection", &json!({ "threadId": thread })).await.unwrap();
        let p = &r["exit"]["value"];
        let settled = p["runs"].as_array().is_some_and(|runs| {
            !runs.is_empty() && runs.iter().all(|r| !matches!(r["status"].as_str(), Some("queued" | "starting" | "running")) && r["checkpointId"].is_string())
        });
        if let Some(first) = p["turnItems"].as_array().and_then(|a| a.first()) {
            item = first["id"].as_str().unwrap_or_default().to_owned();
        }
        if settled && !item.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let vars = [
        ("$WORKSPACE", workspace.to_string_lossy().into_owned()),
        ("$PROJECT", project.to_owned()),
        ("$THREAD", thread.to_owned()),
        ("$ITEM", item),
        ("$HOME", dirs_home()),
    ];
    let mut files: Vec<PathBuf> = std::fs::read_dir(workspace_root().join("conformance/rpc-calls"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    files.sort();
    let only = std::env::var("DOMOVOI_CALLS").ok(); // e.g. DOMOVOI_CALLS=vcs runs only vcs.json
    let cases: Vec<Value> = files
        .iter()
        .filter(|p| only.as_deref().is_none_or(|o| p.file_stem().is_some_and(|s| s == o)))
        .flat_map(|p| serde_json::from_str::<Vec<Value>>(&std::fs::read_to_string(p).unwrap()).unwrap_or_else(|e| panic!("{}: {e}", p.display())))
        .collect();
    let mut problems = vec![];
    let mut answers: Vec<Value> = vec![];
    for case in &cases {
        let tag = case["tag"].as_str().unwrap();
        let mut payload = case["payload"].clone();
        substitute(&mut payload, &vars);
        id += 1;
        let request = json!({ "_tag": "Request", "id": id.to_string(), "tag": tag, "payload": payload, "headers": [] });
        transcript.push(json!({ "kind": "ws", "conn": 0, "dir": "c2s", "frame": request }));
        let Some(reply) = call(&mut ws, id, tag, &payload).await else {
            problems.push(format!("{tag}: no reply"));
            continue;
        };
        transcript.push(json!({ "kind": "ws", "conn": 0, "dir": "s2c", "frame": reply }));
        let mut answer = reply.clone();
        // Request ids count the calls before this one, the waiting ones included.
        if let Some(fields) = answer.as_object_mut() {
            fields.remove("requestId");
            // How many refreshes there have been so far: by the clock.
            if tag == "pullRequests.subscribeLinkedThreadRefreshes" {
                fields.insert("values".into(), json!("<a count>"));
            }
        }
        answers.push(json!({ "tag": tag, "payload": payload, "answer": answer }));
        let kind = match (reply["_tag"].as_str(), reply["exit"]["_tag"].as_str()) {
            (Some("Chunk"), _) => "chunk",
            (Some("Exit"), Some("Success")) => "success",
            (Some("Exit"), Some("Failure")) if reply["exit"]["cause"][0]["_tag"] == "Fail" => "failure",
            _ => "defect",
        };
        let expect = case["expect"].as_str().unwrap_or("success");
        if kind != expect {
            problems.push(format!("{tag}: expected {expect}, got {kind}: {}", truncate(&reply.to_string())));
            continue;
        }
        if let Some(want) = case["error"].as_str() {
            let got = &reply["exit"]["cause"][0]["error"]["_tag"];
            if got != want {
                problems.push(format!("{tag}: expected error {want}, got {got}"));
            }
        }
    }
    assert!(problems.is_empty(), "{} of {} calls wrong:\n{}", problems.len(), cases.len(), problems.join("\n"));
    if transport == Transport::Tcp {
        // With `DOMOVOI_TURNS_DUMP_DIR`: every call with its answer, and what the server recorded.
        dump::write_value(&dir, "rpc-calls-answers", &json!(answers), started);
        dump::write(&dir, "rpc-calls", started);
    }
    schema_check(&transcript, &format!("{transport:?}"));
    eprintln!("{} calls answered as expected", cases.len());
}

fn truncate(s: &str) -> String {
    if s.len() > 400 { format!("{}…", &s[..s.char_indices().nth(400).map(|(i, _)| i).unwrap_or(s.len())]) } else { s.to_owned() }
}

fn dirs_home() -> String {
    std::env::var("HOME").unwrap_or_else(|_| "/".into())
}

fn schema_check(transcript: &[Value], name: &str) {
    let root = workspace_root();
    let node = std::env::var("DOMOVOI_NODE").unwrap_or_else(|_| "node".into());
    let version = Command::new(&node).arg("--version").output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned());
    let major: u32 = version.as_deref().and_then(|v| v.trim_start_matches('v').split('.').next()?.parse().ok()).unwrap_or(0);
    if !root.join(".reference/t3code/packages/contracts").exists() || major < 26 {
        eprintln!("schema check skipped (needs .reference/t3code and Node 26+, found {version:?})");
        return;
    }
    let file = std::env::temp_dir().join(format!("domovoi-calls-{name}-{}.ndjson", std::process::id()));
    let out: String = transcript.iter().map(|r| format!("{r}\n")).collect();
    std::fs::write(&file, out).unwrap();
    let res = Command::new(&node).arg(root.join("conformance/wire-check.ts")).arg(&file).arg("--quiet").output().unwrap();
    if std::env::var("DOMOVOI_KEEP_TRANSCRIPT").is_ok() {
        eprintln!("transcript kept at {}", file.display());
    } else {
        let _ = std::fs::remove_file(&file);
    }
    let text = String::from_utf8_lossy(&res.stdout);
    assert!(res.status.success(), "t3code schema check failed:\n{text}");
    eprintln!("{}", text.trim());
}

/// Scratch flow end to end: a "No project" thread gets its own folder, which
/// becomes a project the thread moves into; plus a signed attachment upload.
#[tokio::test(flavor = "multi_thread")]
async fn scratch_thread_promotion_and_attachment_upload() {
    let dir = std::env::temp_dir().join(format!("domovoi-scratch-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let server = start(&dir);
    let mut ws = connect(&server).await;
    let ok = |r: Option<Value>| -> Value {
        let r = r.expect("reply");
        assert_eq!(r["exit"]["_tag"], "Success", "{r}");
        r["exit"]["value"].clone()
    };
    let scratch = ok(call(&mut ws, 1, "projects.ensureScratch", &json!({})).await)["projectId"].as_str().unwrap().to_owned();
    let again = ok(call(&mut ws, 2, "projects.ensureScratch", &json!({})).await);
    assert_eq!(again["projectId"], json!(scratch), "one scratch project");
    ok(call(&mut ws, 3, "orchestration.launchThread", &json!({ "commandId": "c1", "creationSource": "web", "threadId": "s1", "projectId": scratch,
        "title": "Plan a trip", "modelSelection": { "instanceId": "claudeAgent", "model": "claude-haiku-5-5" },
        "runtimeMode": "approval-required", "interactionMode": "default", "workspaceStrategy": { "type": "root" },
        "initialMessage": { "messageId": "m1", "text": "Plan a weekend trip to the coast", "attachments": [] } })).await);
    let mut folder = String::new();
    for i in 0..100 {
        let p = ok(call(&mut ws, 10 + i, "orchestration.getThreadProjection", &json!({ "threadId": "s1" })).await);
        folder = p["thread"]["worktreePath"].as_str().unwrap_or_default().to_owned();
        let settled = p["runs"].as_array().is_some_and(|runs| runs.iter().all(|r| !matches!(r["status"].as_str(), Some("queued" | "starting" | "running"))));
        if settled {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(folder.contains("/scratch/") && folder.contains("plan-a-weekend-trip-to"), "thread folder: {folder}");
    assert!(Path::new(&folder).is_dir());
    let promoted = ok(call(&mut ws, 200, "projects.createFromThread", &json!({ "threadId": "s1" })).await)["projectId"].as_str().unwrap().to_owned();
    assert_ne!(promoted, scratch);
    let p = ok(call(&mut ws, 201, "orchestration.getThreadProjection", &json!({ "threadId": "s1" })).await);
    assert_eq!((p["thread"]["projectId"].as_str(), p["thread"]["worktreePath"].as_str()), (Some(promoted.as_str()), None));

    // Signed upload: the URL works once per id and checks the size.
    let issued = ok(call(&mut ws, 300, "attachments.createUploadUrl", &json!({ "name": "a.png", "mimeType": "image/png", "sizeBytes": 3 })).await);
    let url = format!("http://127.0.0.1:{}{}", server.port, issued["relativeUrl"].as_str().unwrap());
    let http = http_client();
    assert_eq!(http.post(&url).body(vec![1u8, 2]).send().await.unwrap().status(), 400);
    assert_eq!(http.post(&url).body(vec![1u8, 2, 3]).send().await.unwrap().status(), 204);
    assert_eq!(http.post(format!("{url}x")).body(vec![1u8, 2, 3]).send().await.unwrap().status(), 404, "tampered token");
    let id = issued["attachmentId"].as_str().unwrap();
    assert!(dir.join(format!("home/userdata/attachments/{id}.png")).is_file());
}

/// Two writes sent back to back (a paste, then Enter) reach the PTY in order.
#[tokio::test(flavor = "multi_thread")]
async fn terminal_writes_keep_their_order() {
    let dir = std::env::temp_dir().join(format!("domovoi-term-order-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("w")).unwrap();
    let server = start(&dir);
    let mut ws = connect(&server).await;
    let cwd = dir.join("w").canonicalize().unwrap();
    let attach = json!({ "_tag": "Request", "id": "1", "tag": "terminal.attach", "headers": [],
        "payload": { "threadId": "t", "terminalId": "term-1", "cwd": cwd, "cols": 80, "rows": 24 } });
    ws.send(Message::Text(attach.to_string().into())).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    for (id, data) in [("2", "echo ordered-$((40+2))"), ("3", "\r")] {
        let w = json!({ "_tag": "Request", "id": id, "tag": "terminal.write", "headers": [], "payload": { "threadId": "t", "terminalId": "term-1", "data": data } });
        ws.send(Message::Text(w.to_string().into())).await.unwrap();
    }
    let mut output = String::new();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !output.contains("ordered-42\r\n") {
        let Ok(Some(Ok(Message::Text(t)))) = tokio::time::timeout_at(deadline, ws.next()).await else { break };
        let v: Value = serde_json::from_str(&t).unwrap();
        if v["_tag"] == "Chunk" && v["requestId"] == "1" {
            for e in v["values"].as_array().unwrap() {
                output.push_str(e["data"].as_str().unwrap_or_default());
            }
            let ack = json!({ "_tag": "Ack", "requestId": "1" });
            ws.send(Message::Text(ack.to_string().into())).await.unwrap();
        }
    }
    assert!(output.contains("ordered-42\r\n"), "the command ran (Enter arrived after the text): {output:?}");
}

/// What a file of `conformance/rpc-groups` asks for besides its calls.
#[derive(serde::Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct Group {
    /// Start with the project `project-1` on `$WORKSPACE` and its thread `thread-1`, whose one turn has failed.
    fixture: bool,
    /// The server's environment, laid over the test's; `$DIR` is the test's folder.
    env: std::collections::BTreeMap<String, String>,
    /// Texts of this group that differ from run to run: each pattern with what stands for it in the dump.
    normalize: Vec<(String, String)>,
    cases: Vec<GroupCase>,
}

#[derive(serde::Deserialize, Default)]
#[serde(default, deny_unknown_fields)]
struct GroupCase {
    /// The RPC to call. Without one the case is an HTTP request or only its `sh`.
    tag: Option<String>,
    payload: Value,
    /// `chunk` (the first one, then the stream is interrupted), `success` or `failure`.
    expect: Option<String>,
    /// The failure's `_tag`.
    error: Option<String>,
    /// Read the stream to its end: the answer is every value it sent and how it ended.
    all: bool,
    /// A shell command run in the test's folder before the call.
    sh: Option<String>,
    sleep_ms: u64,
    /// Variables for later payloads: each a JSON pointer into the answer's value (the first value of a chunk).
    save: std::collections::BTreeMap<String, String>,
    /// An HTTP request instead of a call: `GET /path` or `POST /path`, with `payload` as its JSON body.
    http: Option<String>,
    /// For `http`: send no session cookie.
    anonymous: bool,
    /// The expected HTTP status.
    status: Option<u16>,
    /// Leave the call out of the t3code schema check (say why in `note`).
    skip_schema: bool,
    /// Why the case is there.
    note: Option<String>,
}

/// Sends a request and reads its stream to the end: every value, then the exit frame.
async fn call_all(ws: &mut Ws, id: u64, tag: &str, payload: &Value) -> Option<(Vec<Value>, Value, Vec<Value>)> {
    let req = json!({ "_tag": "Request", "id": id.to_string(), "tag": tag, "payload": payload, "headers": [] });
    ws.send(Message::Text(req.to_string().into())).await.unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut values = vec![];
    let mut frames = vec![];
    loop {
        let msg = match tokio::time::timeout_at(deadline, ws.next()).await {
            Ok(Some(Ok(m))) => m,
            _ => {
                // A stream that does not end is not one to read with `all`.
                let interrupt = json!({ "_tag": "Interrupt", "requestId": id.to_string(), "interruptors": [] });
                let _ = ws.send(Message::Text(interrupt.to_string().into())).await;
                return None;
            }
        };
        let Message::Text(t) = msg else { continue };
        let v: Value = serde_json::from_str(&t).unwrap();
        if v["_tag"] == "Ping" {
            ws.send(Message::Text(json!({ "_tag": "Pong" }).to_string().into())).await.unwrap();
            continue;
        }
        if v["requestId"] != json!(id.to_string()) && v["_tag"] != "Defect" {
            continue;
        }
        frames.push(v.clone());
        if v["_tag"] == "Chunk" {
            values.extend(v["values"].as_array().cloned().unwrap_or_default());
            let ack = json!({ "_tag": "Ack", "requestId": id.to_string() });
            ws.send(Message::Text(ack.to_string().into())).await.unwrap();
            continue;
        }
        return Some((values, v, frames));
    }
}

fn run_sh(dir: &Path, script: &str, what: &str) {
    let out = Command::new("/bin/bash")
        .args(["-c", script])
        .current_dir(dir)
        .env("DIR", dir)
        .env("HOME", dir.join("userhome"))
        .env("PATH", format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", dir.join("bin").display()))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .unwrap();
    assert!(out.status.success(), "{what}: {}\n{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
}

fn replace_in(v: &mut Value, patterns: &[(regex::Regex, String)]) {
    match v {
        Value::String(s) => {
            for (pattern, stands_for) in patterns {
                if pattern.is_match(s) {
                    *s = pattern.replace_all(s, stands_for.as_str()).into_owned();
                }
            }
        }
        Value::Array(a) => a.iter_mut().for_each(|x| replace_in(x, patterns)),
        Value::Object(m) => m.values_mut().for_each(|x| replace_in(x, patterns)),
        _ => {}
    }
}

/// Group by group (`conformance/rpc-groups/<name>.json`, with `DOMOVOI_GROUPS=a,b`
/// only those), every method of the group is called for each answer and each
/// failure it can give. Each group gets a folder (`$DIR`) and a server of its
/// own, whose `PATH` begins with `$DIR/bin` and whose `HOME` is
/// `$DIR/userhome`; `<name>.sh`, when there is one, prepares the folder before
/// the server starts (repositories, stand-ins for `gh` and other tools, a
/// settings file).
///
/// With `DOMOVOI_TURNS_DUMP_DIR`, what each group was answered is written to
/// `rpc-group-<name>-answers.json`, to compare two builds of the server.
///
/// Not part of a plain `cargo test`: the 46 groups start a server each and
/// take six to ten minutes together (`just rpc-groups` runs them).
#[tokio::test(flavor = "multi_thread")]
#[ignore = "six to ten minutes: run with `just rpc-groups`"]
async fn every_rpc_group_answers_as_recorded() {
    let folder = workspace_root().join("conformance/rpc-groups");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&folder).unwrap().map(|e| e.unwrap().path()).filter(|p| p.extension().is_some_and(|e| e == "json")).collect();
    files.sort();
    let only: Option<Vec<String>> = std::env::var("DOMOVOI_GROUPS").ok().map(|names| names.split(',').map(str::to_owned).collect());
    let mut totals = (0, 0, 0);
    for file in files {
        let name = file.file_stem().unwrap().to_string_lossy().into_owned();
        if only.as_ref().is_some_and(|only| !only.contains(&name)) {
            continue;
        }
        let group: Group = serde_json::from_str(&std::fs::read_to_string(&file).unwrap()).unwrap_or_else(|e| panic!("{}: {e}", file.display()));
        let (calls, failures, methods) = group_cases(&name, group, &folder).await;
        totals = (totals.0 + calls, totals.1 + failures, totals.2 + methods);
    }
    eprintln!("groups: {} answers, {} of them failures", totals.0, totals.1);
}

async fn group_cases(name: &str, group: Group, folder: &Path) -> (usize, usize, usize) {
    let started = dump::started();
    let dir = std::env::temp_dir().join(format!("domovoi-group-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let workspace = dir.join("workspace");
    for sub in ["workspace", "bin", "userhome"] {
        std::fs::create_dir_all(dir.join(sub)).unwrap();
    }
    let dir = dir.canonicalize().unwrap();
    let workspace = workspace.canonicalize().unwrap();
    let setup = folder.join(format!("{name}.sh"));
    if setup.exists() {
        run_sh(&dir, &format!("set -e\nsource '{}'", setup.display()), &format!("{name}.sh"));
    }
    if group.fixture {
        git(&workspace, &["init", "-q", "-b", "main"]);
        std::fs::write(workspace.join("calc.py"), "def add(a, b):\n    return a + b\n").unwrap();
        std::fs::write(workspace.join("README.md"), "# Calc\n\nAdds numbers.\n").unwrap();
        git(&workspace, &["add", "-A"]);
        git(&workspace, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "init"]);
    }
    let dir_text = dir.to_string_lossy().into_owned();
    let mut env = vec![
        ("PATH".to_owned(), format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", dir.join("bin").display())),
        ("HOME".to_owned(), dir.join("userhome").to_string_lossy().into_owned()),
        ("GIT_CONFIG_NOSYSTEM".to_owned(), "1".to_owned()),
    ];
    env.extend(group.env.iter().map(|(key, value)| (key.clone(), value.replace("$DIR", &dir_text))));
    let server = start_env(&dir, Transport::Tcp, &env);
    let (mut ws, cookie) = connect_with_cookie(&server).await;
    let mut id = 0u64;
    let mut vars: Vec<(String, String)> = vec![
        ("$WORKSPACE".to_owned(), workspace.to_string_lossy().into_owned()),
        ("$DIR".to_owned(), dir_text.clone()),
        ("$HOME".to_owned(), dir.join("userhome").to_string_lossy().into_owned()),
    ];
    if group.fixture {
        let (project, thread) = ("project-1", "thread-1");
        let setup = [
            ("projects.mutate", json!({ "type": "project.create", "commandId": "c1", "projectId": project, "title": "calc",
                "workspaceRoot": workspace, "createWorkspaceRootIfMissing": false, "defaultModelSelection": null })),
            ("orchestration.launchThread", json!({ "commandId": "c2", "creationSource": "web", "threadId": thread, "projectId": project,
                "title": "Find the add function", "modelSelection": { "instanceId": "claudeAgent", "model": "claude-haiku-5-5" },
                "runtimeMode": "approval-required", "interactionMode": "default", "workspaceStrategy": { "type": "root" },
                "initialMessage": { "messageId": "m1", "text": "Where is the add function defined?", "attachments": [] } })),
        ];
        for (tag, payload) in setup {
            id += 1;
            let r = call(&mut ws, id, tag, &payload).await.expect("setup reply");
            assert_eq!(r["exit"]["_tag"], "Success", "{tag}: {r}");
        }
        let mut item = String::new();
        for _ in 0..100 {
            id += 1;
            let r = call(&mut ws, id, "orchestration.getThreadProjection", &json!({ "threadId": thread })).await.unwrap();
            let p = &r["exit"]["value"];
            let settled = p["runs"].as_array().is_some_and(|runs| {
                !runs.is_empty() && runs.iter().all(|r| !matches!(r["status"].as_str(), Some("queued" | "starting" | "running")) && r["checkpointId"].is_string())
            });
            if let Some(first) = p["turnItems"].as_array().and_then(|a| a.first()) {
                item = first["id"].as_str().unwrap_or_default().to_owned();
            }
            if settled && !item.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        vars.extend([("$PROJECT".to_owned(), project.to_owned()), ("$THREAD".to_owned(), thread.to_owned()), ("$ITEM".to_owned(), item)]);
    }

    let patterns: Vec<(regex::Regex, String)> = group.normalize.iter().map(|(pattern, stands_for)| (regex::Regex::new(pattern).expect("pattern"), stands_for.clone())).collect();
    let mut transcript: Vec<Value> = vec![];
    let mut problems = vec![];
    let mut answers: Vec<Value> = vec![];
    let mut failures = 0;
    let mut methods = std::collections::BTreeSet::new();
    let http = http_client();
    for case in &group.cases {
        // Longest names first, so `$TASK2` is not read as `$TASK`.
        let mut by_length: Vec<(&str, String)> = vars.iter().map(|(key, value)| (key.as_str(), value.clone())).collect();
        by_length.sort_by_key(|(key, _)| std::cmp::Reverse(key.len()));
        if let Some(script) = &case.sh {
            let mut script = Value::String(script.clone());
            substitute(&mut script, &by_length);
            run_sh(&dir, script.as_str().unwrap(), "sh of a case");
        }
        if case.sleep_ms > 0 {
            tokio::time::sleep(Duration::from_millis(case.sleep_ms)).await;
        }
        let mut payload = case.payload.clone();
        substitute(&mut payload, &by_length);
        if let Some(request) = &case.http {
            let (method, path) = request.split_once(' ').expect("`http` is a method and a path");
            let mut path = Value::String(path.to_owned());
            substitute(&mut path, &by_length);
            let url = format!("http://127.0.0.1:{}{}", server.port, path.as_str().unwrap());
            let mut req = http.request(method.parse().expect("an HTTP method"), &url);
            if !case.anonymous {
                req = req.header("cookie", &cookie);
            }
            if !payload.is_null() {
                req = req.json(&payload);
            }
            let res = req.send().await.unwrap();
            let status = res.status().as_u16();
            let text = res.text().await.unwrap();
            let body = serde_json::from_str::<Value>(&text).unwrap_or(Value::String(text));
            if case.status.is_some_and(|want| want != status) {
                problems.push(format!("{request}: expected status {:?}, got {status}: {}", case.status, truncate(&body.to_string())));
            }
            answers.push(json!({ "http": request, "payload": payload, "status": status, "body": body }));
            if status >= 400 {
                failures += 1;
            }
            continue;
        }
        let Some(tag) = case.tag.as_deref() else { continue };
        methods.insert(tag.to_owned());
        id += 1;
        let request = json!({ "_tag": "Request", "id": id.to_string(), "tag": tag, "payload": payload, "headers": [] });
        let (reply, mut answer, value) = if case.all {
            let Some((values, exit, frames)) = call_all(&mut ws, id, tag, &payload).await else {
                problems.push(format!("{tag}: no end of the stream"));
                continue;
            };
            if !case.skip_schema {
                transcript.push(json!({ "kind": "ws", "conn": 0, "dir": "c2s", "frame": request }));
                transcript.extend(frames.into_iter().map(|frame| json!({ "kind": "ws", "conn": 0, "dir": "s2c", "frame": frame })));
            }
            let first = values.first().cloned().unwrap_or(Value::Null);
            let mut end = exit.clone();
            if let Some(fields) = end.as_object_mut() {
                fields.remove("requestId");
            }
            (exit, json!({ "values": values, "end": end }), first)
        } else {
            let Some(reply) = call(&mut ws, id, tag, &payload).await else {
                problems.push(format!("{tag}: no reply to {}", truncate(&payload.to_string())));
                continue;
            };
            if !case.skip_schema {
                transcript.push(json!({ "kind": "ws", "conn": 0, "dir": "c2s", "frame": request }));
                transcript.push(json!({ "kind": "ws", "conn": 0, "dir": "s2c", "frame": reply }));
            }
            let value = if reply["_tag"] == "Chunk" { reply["values"][0].clone() } else { reply["exit"]["value"].clone() };
            (reply.clone(), reply, value)
        };
        if let Some(fields) = answer.as_object_mut() {
            fields.remove("requestId");
        }
        for (var, pointer) in &case.save {
            match value.pointer(pointer) {
                Some(found) => vars.push((format!("${var}"), found.as_str().map(str::to_owned).unwrap_or_else(|| found.to_string()))),
                None => problems.push(format!("{tag}: nothing at {pointer} to save as {var}: {}", truncate(&value.to_string()))),
            }
        }
        replace_in(&mut answer, &patterns);
        // The payload as the file gives it: what it names is in the answer.
        answers.push(json!({ "tag": tag, "payload": case.payload, "answer": answer }));
        let kind = match (reply["_tag"].as_str(), reply["exit"]["_tag"].as_str()) {
            (Some("Chunk"), _) => "chunk",
            (Some("Exit"), Some("Success")) => "success",
            (Some("Exit"), Some("Failure")) if reply["exit"]["cause"][0]["_tag"] == "Fail" => "failure",
            _ => "defect",
        };
        if kind == "failure" {
            failures += 1;
        }
        let expect = case.expect.as_deref().unwrap_or("success");
        if kind != expect {
            problems.push(format!("{tag}: expected {expect}, got {kind}: {}", truncate(&reply.to_string())));
            continue;
        }
        if let Some(want) = &case.error {
            let got = &reply["exit"]["cause"][0]["error"]["_tag"];
            if got != want.as_str() {
                problems.push(format!("{tag}: expected error {want}, got {got}"));
            }
        }
    }
    // Written before anything is asserted: a dump of what was answered is how a wrong expectation is put right.
    dump::write_value(&dir, &format!("rpc-group-{name}-answers"), &json!(answers), started);
    assert!(problems.is_empty(), "{name}: {} of {} calls wrong:\n{}", problems.len(), group.cases.len(), problems.join("\n"));
    schema_check(&transcript, &format!("group-{name}"));
    eprintln!("group {name}: {} answers ({failures} failures) of {} methods", answers.len(), methods.len());
    (answers.len(), failures, methods.len())
}
