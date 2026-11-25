//! The managed policy against the real binary: a policy file named by
//! `DOMOVOI_DEV_MANAGED_POLICY_PATH` (debug builds only, as t3code's dev
//! builds read `T3CODE_DEV_MANAGED_POLICY_PATH`). The summary reaches clients
//! in `server.getConfig`, locked settings and disallowed providers are
//! refused with t3code's typed error, webhook tasks are blocked, and a policy
//! this build cannot enforce, or a refused way of listening, stops the
//! server. Every frame is decoded with t3code's schemas.

use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};

fn http_client() -> reqwest::Client {
    let _ = rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::new()
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).ancestors().nth(2).unwrap().to_owned()
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/policy").join(name)
}

fn home(tag: &str) -> PathBuf {
    let home = std::env::temp_dir().join(format!("domovoi-policy-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    home
}

fn command(home: &Path, policy: &Path, args: &[&str]) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_domovoi-server"));
    c.env("DOMOVOI_NO_SESSION_IMPORT", "1")
        .env("DOMOVOI_NO_SHELL_ENV", "1")
        .env("DOMOVOI_AGENT_SESSIONS_HOME", home.join("agent-home"))
        .env("DOMOVOI_DEV_MANAGED_POLICY_PATH", policy)
        .env("RUST_LOG", "warn")
        .args(["--home-dir", home.to_str().unwrap()])
        .args(args);
    c
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

fn start(tag: &str, policy: &Path) -> Server {
    let home = home(tag);
    std::fs::create_dir_all(home.join("userdata")).unwrap();
    // What the user chose before the policy came: kept in the file, not shown while locked.
    std::fs::write(home.join("userdata/settings.json"), json!({ "enableAgentBrowserAccess": true, "defaultAutoPull": true }).to_string()).unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let mut child = command(&home, policy, &["--port", &port.to_string()]).stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let out = child.stdout.take().unwrap();
    let mut startup_code = String::new();
    for line in BufReader::new(out).lines() {
        if let Some(code) = line.unwrap().split("#token=").nth(1) {
            startup_code = code.trim().to_owned();
            break;
        }
    }
    assert!(!startup_code.is_empty(), "the server did not start");
    Server { child, port, home, startup_code }
}

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

struct Rpc {
    ws: Ws,
    id: u64,
    transcript: Vec<Value>,
}

impl Rpc {
    async fn open(server: &Server) -> Rpc {
        let base = format!("http://127.0.0.1:{}", server.port);
        let res = http_client().post(format!("{base}/api/auth/browser-session")).json(&json!({ "credential": server.startup_code })).send().await.unwrap();
        assert_eq!(res.status(), 200);
        let cookie = res.headers().get("set-cookie").unwrap().to_str().unwrap().split(';').next().unwrap().to_owned();
        let mut req = format!("ws://127.0.0.1:{}/ws?orchestrationProtocol=2", server.port).into_client_request().unwrap();
        req.headers_mut().insert("cookie", cookie.parse().unwrap());
        Rpc { ws: tokio_tungstenite::connect_async(req).await.unwrap().0, id: 0, transcript: vec![] }
    }

    async fn call(&mut self, tag: &str, payload: Value) -> Value {
        self.id += 1;
        let id = self.id.to_string();
        let request = json!({ "_tag": "Request", "id": id, "tag": tag, "payload": payload, "headers": [] });
        self.transcript.push(json!({ "kind": "ws", "conn": 0, "dir": "c2s", "frame": request }));
        self.ws.send(Message::Text(request.to_string().into())).await.unwrap();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
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
            self.transcript.push(json!({ "kind": "ws", "conn": 0, "dir": "s2c", "frame": v.clone() }));
            return v;
        }
    }

    async fn ok(&mut self, tag: &str, payload: Value) -> Value {
        let reply = self.call(tag, payload).await;
        assert_eq!(reply["exit"]["_tag"], "Success", "{tag}: {reply}");
        reply["exit"]["value"].clone()
    }

    async fn failure(&mut self, tag: &str, payload: Value) -> Value {
        let reply = self.call(tag, payload).await;
        assert_eq!(reply["exit"]["_tag"], "Failure", "{tag}: {reply}");
        reply["exit"]["cause"][0]["error"].clone()
    }
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
    let file = std::env::temp_dir().join(format!("domovoi-policy-{name}-{}.ndjson", std::process::id()));
    std::fs::write(&file, transcript.iter().map(|r| format!("{r}\n")).collect::<String>()).unwrap();
    let res = Command::new(&node).arg(root.join("conformance/wire-check.ts")).arg(&file).arg("--quiet").output().unwrap();
    let _ = std::fs::remove_file(&file);
    assert!(res.status.success(), "t3code schema check failed:\n{}", String::from_utf8_lossy(&res.stdout));
    eprintln!("{}", String::from_utf8_lossy(&res.stdout).trim());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_managed_server_tells_clients_and_refuses_what_its_policy_locks() {
    let server = start("acme", &fixture("acme.json"));
    let mut rpc = Rpc::open(&server).await;

    let config = rpc.ok("server.getConfig", json!({})).await;
    assert_eq!(
        config["managedPolicy"],
        json!({
            "organization": "Acme Corp", "supportUrl": "https://help.acme.test", "lockedSettings": ["enableAgentBrowserAccess", "storageCleanup.logsAfterDays"],
            "limits": { "clients": ["local-browser"], "connect": false, "webhookTasks": false, "previewTunnel": false,
                "providers": { "allowedDrivers": ["codex", "claudeAgent"] }, "updates": { "mode": "off" } }
        })
    );
    assert!(!config["managedPolicy"].to_string().contains("mirror.acme.test"), "the feed URL stays on the server");
    let settings = &config["settings"];
    assert_eq!((&settings["enableAgentBrowserAccess"], &settings["storageCleanup"]["logsAfterDays"]), (&json!(false), &json!(14)));
    assert_eq!((&settings["branchNamePrefix"], &settings["defaultAutoPull"]), (&json!("acme"), &json!(true)), "the policy's default, the user's own choice");
    let providers = config["providers"].as_array().unwrap();
    let opencode = providers.iter().find(|p| p["driver"] == "opencode").expect("OpenCode is listed");
    assert_eq!((&opencode["availability"], &opencode["enabled"]), (&json!("unavailable"), &json!(false)));
    assert_eq!(opencode["unavailableReason"], "Acme Corp's policy does not allow the OpenCode provider.");

    // Locked settings are refused, however they are sent; the rest is written.
    let settings_path = server.home.join("userdata").join("settings.json");
    let locked = rpc.failure("server.updateSettings", json!({ "patch": { "storageCleanup": { "logsAfterDays": 30 } } })).await;
    assert_eq!(
        locked,
        json!({ "_tag": "ServerSettingsError", "settingsPath": settings_path.to_string_lossy(), "operation": "write-managed-setting",
            "managedSetting": "storageCleanup.logsAfterDays", "managedBy": "Acme Corp" })
    );
    let project = rpc.failure("server.updateSettings", json!({ "patch": { "projectSettingsOverrides": { "p1": { "enableAgentBrowserAccess": true } } } })).await;
    assert_eq!(project["managedSetting"], "enableAgentBrowserAccess");
    let disallowed = rpc
        .failure("server.updateSettings", json!({ "patch": {}, "providerInstanceMutation": { "operation": "create", "instanceId": "opencode-2", "instance": { "driver": "opencode" } } }))
        .await;
    assert_eq!(
        disallowed,
        json!({ "_tag": "ServerSettingsError", "settingsPath": settings_path.to_string_lossy(), "operation": "use-disallowed-provider",
            "managedBy": "Acme Corp", "providerInstanceId": "opencode-2", "providerLabel": "OpenCode" })
    );
    let saved = rpc.ok("server.updateSettings", json!({ "patch": { "enableAgentBrowserAccess": false, "defaultAutoPull": false } })).await;
    assert_eq!((&saved["enableAgentBrowserAccess"], &saved["defaultAutoPull"]), (&json!(false), &json!(false)));
    let file: Value = serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
    assert_eq!(file["enableAgentBrowserAccess"], true, "the user's own value stays in the file");
    assert_eq!(file["defaultAutoPull"], false);
    assert_eq!(file["storageCleanup"]["logsAfterDays"], Value::Null);
    assert_ne!(file["branchNamePrefix"], "acme", "the policy's default is not written as the user's");

    // Webhook tasks are blocked; their URLs answer 403.
    let workspace = server.home.join("workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    rpc.ok("projects.mutate", json!({ "type": "project.create", "commandId": "make", "projectId": "p-policy", "title": "Policy", "workspaceRoot": workspace })).await;
    let webhook = json!({ "id": "task-hook", "title": "Hook", "prompt": "Ran", "enabled": true, "projectId": "p-policy",
        "schedule": { "type": "webhook" }, "workspaceStrategy": { "type": "root" },
        "modelSelection": { "instanceId": "claudeAgent", "model": "claude-haiku-5-5" }, "runtimeMode": "approval-required", "interactionMode": "default" });
    let refused = rpc.failure("scheduledTasks.upsert", webhook.clone()).await;
    assert_eq!((&refused["_tag"], &refused["message"]), (&json!("ScheduledTaskError"), &json!("Acme Corp's policy does not allow webhook tasks.")), "{refused}");
    let mut off = webhook;
    off["enabled"] = json!(false);
    let task = rpc.ok("scheduledTasks.upsert", off).await["task"].clone();
    assert_eq!(task["blockedByPolicy"], "Acme Corp's policy does not allow webhook tasks.", "{task}");
    let enable = rpc.failure("scheduledTasks.setEnabled", json!({ "id": task["id"], "enabled": true })).await;
    assert_eq!(enable["message"], "Acme Corp's policy does not allow webhook tasks.");
    let path = task["webhook"]["path"].as_str().expect("a webhook path");
    let hook = http_client().post(format!("http://127.0.0.1:{}{path}", server.port)).body("{}").send().await.unwrap();
    assert_eq!(hook.status(), 403);
    assert_eq!(hook.json::<Value>().await.unwrap(), json!({ "error": "blocked_by_policy" }));

    // No T3 Connect: the relay client is not installed.
    let relay = rpc.call("cloud.installRelayClient", json!({})).await;
    assert_eq!(relay["exit"]["cause"][0]["error"]["reason"], "managed_by_policy", "{relay}");
    assert_eq!(relay["exit"]["cause"][0]["error"]["message"], "Acme Corp's policy does not allow T3 Connect.");

    schema_check(&rpc.transcript, "acme");
}

/// Runs the server until it exits; its exit code and what it printed.
fn run_to_exit(tag: &str, policy: &Path, args: &[&str]) -> (Option<i32>, String) {
    let home = home(tag);
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let mut all = vec!["--port".to_owned(), port.to_string()];
    all.extend(args.iter().map(|a| (*a).to_owned()));
    let all: Vec<&str> = all.iter().map(String::as_str).collect();
    let mut child = command(&home, policy, &all).stdout(Stdio::null()).stderr(Stdio::piped()).spawn().unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break Some(status);
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            break None;
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    let mut err = String::new();
    std::io::Read::read_to_string(&mut child.stderr.take().unwrap(), &mut err).unwrap();
    let _ = std::fs::remove_dir_all(&home);
    (status.and_then(|s| s.code()), err)
}

#[test]
fn a_policy_this_build_cannot_enforce_stops_the_server() {
    let (code, err) = run_to_exit("unknown", &fixture("unknown-limit.json"), &[]);
    assert_eq!(code, Some(77), "{err}");
    assert!(err.contains("cannot use the managed policy at") && err.contains("screenRecording") && err.contains("It will not start until an administrator fixes this."), "{err}");
    let (code, err) = run_to_exit("ceiling", &fixture("max-runtime-mode.json"), &[]);
    assert_eq!(code, Some(77), "{err}");
    assert!(err.contains("\"limits.maxRuntimeMode\" is a restriction this Domovoi build cannot enforce"), "{err}");
}

#[test]
fn a_policy_without_remote_clients_refuses_to_listen_beyond_this_machine() {
    let (code, err) = run_to_exit("host", &fixture("acme.json"), &["--host", "0.0.0.0"]);
    assert_eq!(code, Some(1), "{err}");
    assert!(err.contains("Acme Corp's policy does not allow remote clients, so the server cannot listen on every interface."), "{err}");
    let (code, err) = run_to_exit("remote", &fixture("acme.json"), &["--remote", "--relay", "disabled"]);
    assert_eq!(code, Some(1), "{err}");
    assert!(err.contains("so remote access over iroh cannot be turned on"), "{err}");
}
