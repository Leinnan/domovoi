//! What the server's config, access and pairing surfaces put on the wire
//! beyond a first answer, which the cases of `conformance/rpc-groups` cannot
//! reach: the events a config or access subscription sends after its
//! snapshot, what a session with limited permissions is refused, and the
//! HTTP answers that carry a fresh secret (a pairing code, a ticket, a token).
//!
//! With `DOMOVOI_TURNS_DUMP_DIR` every answer is written to `config-wire.json`, to
//! compare two builds of the server (`DOMOVOI_SERVER_BIN` names another build).

use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::Duration;
use tokio_tungstenite::tungstenite::{Message, client::IntoClientRequest};

/// The socket URL names the version of the protocol the test speaks.
const PROTOCOL: u32 = domovoi_model::PROTOCOL_VERSION;

mod dump;

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

fn http_client() -> reqwest::Client {
    let _ = rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::new()
}

struct Server {
    child: Child,
    port: u16,
    pairing: String,
    dir: PathBuf,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn start() -> Server {
    let dir = std::env::temp_dir().join(format!("domovoi-config-wire-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let home = dir.join("home");
    // A server key of its own, so the endpoint id is the same every run.
    std::fs::create_dir_all(home.join("remote")).unwrap();
    std::fs::write(home.join("remote").join("secret.key"), format!("{:064x}", 7)).unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let bin = std::env::var("DOMOVOI_SERVER_BIN").unwrap_or_else(|_| env!("CARGO_BIN_EXE_domovoi-server").to_owned());
    let mut child = Command::new(bin)
        .args(["--home-dir", home.to_str().unwrap(), "--port", &port.to_string()])
        .env("DOMOVOI_NO_SESSION_IMPORT", "1")
        .env("DOMOVOI_NO_SHELL_ENV", "1")
        .env("RUST_LOG", "warn")
        .env("DOMOVOI_CLAUDE_BINARY", "/usr/bin/false")
        .env("DOMOVOI_CODEX_BINARY", "/usr/bin/false")
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .env("HOME", dir.join("userhome"))
        .env("DOMOVOI_AGENT_SESSIONS_HOME", dir.join("agent-home"))
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
    Server { child, port, pairing, dir }
}

/// One client: its cookie and its socket, with the frames read but not yet asked for.
struct Client {
    ws: Ws,
    cookie: String,
    seen: Vec<Value>,
    next_id: u64,
}

async fn session_cookie(base: &str, code: &str) -> (String, Value) {
    let res = http_client().post(format!("{base}/api/auth/browser-session")).json(&json!({ "credential": code })).send().await.unwrap();
    assert_eq!(res.status(), 200);
    let cookie = res.headers().get("set-cookie").unwrap().to_str().unwrap().split(';').next().unwrap().to_owned();
    (cookie, res.json().await.unwrap())
}

async fn open_socket(port: u16, cookie: &str) -> Ws {
    let mut req = format!("ws://127.0.0.1:{port}/ws?protocol={PROTOCOL}").into_client_request().unwrap();
    req.headers_mut().insert("cookie", cookie.parse().unwrap());
    tokio_tungstenite::connect_async(req).await.unwrap().0
}

impl Client {
    async fn send(&mut self, tag: &str, payload: Value) -> String {
        self.next_id += 1;
        let id = self.next_id.to_string();
        let req = json!({ "_tag": "Request", "id": id, "tag": tag, "payload": payload, "headers": [] });
        self.ws.send(Message::Text(req.to_string().into())).await.unwrap();
        id
    }

    /// The next frame of request `id` that `wanted` accepts; chunks are acknowledged, other frames kept.
    async fn frame(&mut self, id: &str, wanted: impl Fn(&Value) -> bool) -> Value {
        if let Some(at) = self.seen.iter().position(|f| f["requestId"] == id && wanted(f)) {
            return self.seen.remove(at);
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        loop {
            let msg = tokio::time::timeout_at(deadline, self.ws.next()).await.unwrap_or_else(|_| panic!("no frame of request {id} in time")).expect("the socket is open").unwrap();
            let Message::Text(text) = msg else { continue };
            let frame: Value = serde_json::from_str(&text).unwrap();
            if frame["_tag"] == "Ping" {
                self.ws.send(Message::Text(json!({ "_tag": "Pong" }).to_string().into())).await.unwrap();
                continue;
            }
            if frame["_tag"] == "Chunk" {
                let ack = json!({ "_tag": "Ack", "requestId": frame["requestId"] });
                self.ws.send(Message::Text(ack.to_string().into())).await.unwrap();
            }
            // A defect names no request.
            if (frame["requestId"] == id || frame["_tag"] == "Defect") && wanted(&frame) {
                return frame;
            }
            self.seen.push(frame);
        }
    }

    /// Calls a method and returns how it ended.
    async fn call(&mut self, tag: &str, payload: Value) -> Value {
        let id = self.send(tag, payload).await;
        self.frame(&id, |f| f["_tag"] == "Exit").await["exit"].clone()
    }

    /// The next value of stream `id` whose `type` is `kind`.
    async fn event(&mut self, id: &str, kind: &str) -> Value {
        let chunk = self.frame(id, |f| f["_tag"] == "Chunk" && f["values"].as_array().is_some_and(|values| values.iter().any(|v| v["type"] == kind))).await;
        let mut values = chunk["values"].as_array().unwrap().clone();
        let found = values.remove(values.iter().position(|v| v["type"] == kind).expect("the chunk was chosen for it"));
        // What came in the same chunk stays there to be asked for.
        if !values.is_empty() {
            self.seen.insert(0, json!({ "_tag": "Chunk", "requestId": id, "values": values }));
        }
        found
    }
}

/// Replaces what is made anew every run: secrets, and what is counted by the clock.
fn settled(mut value: Value) -> Value {
    fn walk(value: &mut Value, code: &regex::Regex) {
        match value {
            Value::Object(fields) => {
                for (key, value) in fields.iter_mut() {
                    match key.as_str() {
                        "code" | "access_token" => *value = json!(format!("<{key}>")),
                        // A socket ticket, or the endpoint's address while remote access is on; off, it is the endpoint id.
                        "ticket" if value.as_str().is_some_and(|t| t.len() != 64) => *value = json!("<ticket>"),
                        "expires_in" => *value = json!("<seconds>"),
                        // The machine's own addresses, however many it has.
                        "directAddresses" if value.as_array().is_some_and(|addresses| !addresses.is_empty()) => *value = json!(["<address>"]),
                        "link" => *value = json!(code.replace(value.as_str().unwrap_or_default(), "ticket=<ticket>&code=<code>").into_owned()),
                        _ => walk(value, code),
                    }
                }
            }
            Value::Array(items) => items.iter_mut().for_each(|item| walk(item, code)),
            _ => {}
        }
    }
    walk(&mut value, &regex::Regex::new("ticket=[^&]+&code=[A-Z0-9]+").unwrap());
    value
}

async fn answered(res: reqwest::Response) -> Value {
    let status = res.status().as_u16();
    let text = res.text().await.unwrap();
    json!({ "status": status, "body": serde_json::from_str::<Value>(&text).unwrap_or(Value::String(text)) })
}

async fn token_exchange(base: &str, fields: &[(&str, &str)]) -> Value {
    let body = url::form_urlencoded::Serializer::new(String::new()).extend_pairs(fields).finish();
    answered(http_client().post(format!("{base}/oauth/token")).header("content-type", "application/x-www-form-urlencoded").body(body).send().await.unwrap()).await
}

const GRANT: &str = "urn:ietf:params:oauth:grant-type:token-exchange";
const BOOTSTRAP: &str = "urn:t3:params:oauth:token-type:environment-bootstrap";
const ACCESS: &str = "urn:ietf:params:oauth:token-type:access_token";

#[tokio::test(flavor = "multi_thread")]
async fn config_and_access_streams_permissions_and_pairing() {
    let started = dump::started();
    let server = start();
    let base = format!("http://127.0.0.1:{}", server.port);
    let http = http_client();
    let mut steps: Vec<Value> = vec![];
    let mut step = |name: &str, value: Value| {
        if std::env::var_os("DOMOVOI_WIRE_TRACE").is_some() {
            eprintln!("{name}: {}", value.to_string().chars().take(300).collect::<String>());
        }
        steps.push(json!({ "step": name, "value": settled(value) }));
    };

    let (cookie, session) = session_cookie(&base, &server.pairing).await;
    step("browser-session", session);
    let owner_cookie = cookie.clone();
    let mut owner = Client { ws: open_socket(server.port, &cookie).await, cookie, seen: vec![], next_id: 0 };

    // The config stream after its snapshot.
    let config = owner.send("subscribeServerConfig", json!({})).await;
    assert_eq!(owner.event(&config, "snapshot").await["version"], 1);
    let updated = owner.call("server.updateSettings", json!({ "patch": { "defaultAutoPull": true, "futureSetting": { "level": 3 }, "storageCleanup": { "futureRule": 1 } } })).await;
    assert_eq!(updated["_tag"], "Success", "{updated}");
    let event = owner.event(&config, "settingsUpdated").await;
    assert_eq!(event["payload"]["settings"], updated["value"], "the event carries the settings the call answered with");
    step("settingsUpdated", event);
    let rule = json!({ "key": "mod+shift+j", "command": "chat.new", "when": "!terminalFocus" });
    let upserted = owner.call("server.upsertKeybinding", rule.clone()).await;
    let event = owner.event(&config, "keybindingsUpdated").await;
    assert_eq!(event["payload"], upserted["value"]);
    step("keybindingsUpdated after an upsert", event);
    assert_eq!(owner.call("server.upsertKeybinding", json!({ "key": "a+b", "command": "chat.new" })).await["_tag"], "Success");
    step("keybindingsUpdated with an issue", owner.event(&config, "keybindingsUpdated").await);
    assert_eq!(owner.call("server.removeKeybinding", rule).await["_tag"], "Success");
    step("keybindingsUpdated after a removal", owner.event(&config, "keybindingsUpdated").await);
    let refreshed = owner.call("server.refreshProviders", json!({})).await;
    let event = owner.event(&config, "providerStatuses").await;
    assert_eq!(event["payload"]["providers"], refreshed["value"]["providers"]);
    step("providerStatuses", event);
    assert_eq!(owner.call("server.refreshProviders", json!({ "instanceId": "codex" })).await["_tag"], "Success");
    step("providerStatuses after one instance was probed", owner.event(&config, "providerStatuses").await);

    // The access stream: a link is minted and used, a client connects and is signed out.
    let access = owner.send("subscribeAuthAccess", json!({})).await;
    step("access snapshot", owner.event(&access, "snapshot").await);
    let post = |path: &str, cookie: &str, body: Value| http.post(format!("{base}{path}")).header("cookie", cookie).json(&body).send();
    let minted = answered(post("/api/domovoi/remote/pairing", &owner_cookie, json!({ "ttlMinutes": 5, "permissions": ["orchestration:read"] })).await.unwrap()).await;
    let code = minted["body"]["code"].as_str().expect("a pairing code").to_owned();
    step("pairing link with permissions", minted);
    step("pairingLinkUpserted", owner.event(&access, "pairingLinkUpserted").await);
    let (limited_cookie, session) = session_cookie(&base, &code).await;
    step("browser-session of a limited code", session);
    step("pairingLinkRemoved", owner.event(&access, "pairingLinkRemoved").await);
    let joined = owner.event(&access, "clientUpserted").await;
    let limited_id = joined["payload"]["sessionId"].as_str().unwrap().to_owned();
    step("clientUpserted", joined);
    let mut limited = Client { ws: open_socket(server.port, &limited_cookie).await, cookie: limited_cookie, seen: vec![], next_id: 0 };
    step("clientUpserted on connecting", owner.event(&access, "clientUpserted").await);

    // What a session that may only read is refused.
    for (tag, payload) in [
        ("terminal.open", json!({ "threadId": "t", "terminalId": "term-1", "cwd": "/tmp" })),
        ("projects.readFile", json!({ "cwd": "/tmp", "relativePath": "a" })),
        ("server.updateSettings", json!({ "patch": { "defaultAutoPull": true } })),
        ("server.updateSettings", json!({ "patch": { "providerInstances": {} } })),
        ("server.updateSettings", json!({ "patch": {}, "providerInstanceMutation": { "operation": "remove", "instanceId": "x" } })),
        ("server.upsertKeybinding", json!({ "key": "mod+j", "command": "chat.new" })),
        ("server.refreshProviders", json!({})),
        ("subscribeAuthAccess", json!({})),
        ("provider.auth.start", json!({ "instanceId": "codex" })),
        ("no.such.method", json!({})),
    ] {
        let id = limited.send(tag, payload.clone()).await;
        let frame = limited.frame(&id, |f| f["_tag"] == "Exit" || f["_tag"] == "Defect").await;
        step(&format!("limited session: {tag} {payload}"), json!({ "_tag": frame["_tag"], "exit": frame["exit"], "defect": frame["defect"] }));
    }
    assert_eq!(limited.call("server.getConfig", json!({})).await["_tag"], "Success", "reading the config is allowed");
    step("limited session: mint a pairing link", answered(post("/api/domovoi/remote/pairing", &limited.cookie, json!({})).await.unwrap()).await);
    step("limited session: remote status", answered(http.get(format!("{base}/api/domovoi/remote")).header("cookie", &limited.cookie).send().await.unwrap()).await);
    step("limited session: auth session", answered(http.get(format!("{base}/api/auth/session")).header("cookie", &limited.cookie).send().await.unwrap()).await);

    // Answers that carry a fresh secret.
    step("websocket ticket", answered(http.post(format!("{base}/api/auth/websocket-ticket")).header("cookie", &owner_cookie).send().await.unwrap()).await);
    step("pairing link", answered(http.post(format!("{base}/api/domovoi/remote/pairing")).header("cookie", &owner_cookie).send().await.unwrap()).await);
    let mint = |body: Value| async { answered(post("/api/domovoi/remote/pairing", &owner_cookie, body).await.unwrap()).await["body"]["code"].as_str().expect("a pairing code").to_owned() };
    let code = mint(json!({})).await;
    let exchange = [("grant_type", GRANT), ("subject_token_type", BOOTSTRAP), ("requested_token_type", ACCESS)];
    let with = |more: &[(&'static str, &str)]| -> Vec<(&'static str, String)> { exchange.iter().chain(more).map(|(k, v)| (*k, (*v).to_owned())).collect() };
    let run = |fields: Vec<(&'static str, String)>| {
        let base = base.clone();
        async move { token_exchange(&base, &fields.iter().map(|(k, v)| (*k, v.as_str())).collect::<Vec<_>>()).await }
    };
    step("token exchange", run(with(&[("subject_token", &code), ("client_label", " Phone "), ("client_device_type", "mobile"), ("client_os", "iOS")])).await);
    step("clientUpserted for the phone", owner.event(&access, "clientUpserted").await);
    step("token exchange: the code again", run(with(&[("subject_token", &code)])).await);
    let code = mint(json!({ "permissions": ["orchestration:read", "terminal:read"] })).await;
    step("token exchange: an empty scope", run(with(&[("subject_token", &code), ("scope", " ")])).await);
    step("token exchange: a scope the code does not grant", run(with(&[("subject_token", &code), ("scope", "terminal:operate")])).await);
    let code = mint(json!({ "permissions": ["orchestration:read", "terminal:read"] })).await;
    step("token exchange: a narrower scope", run(with(&[("subject_token", &code), ("scope", "orchestration:read")])).await);
    step("clientUpserted for the second token", owner.event(&access, "clientUpserted").await);
    step("token exchange: another grant type", token_exchange(&base, &[("grant_type", "password"), ("subject_token", "x"), ("subject_token_type", BOOTSTRAP), ("requested_token_type", ACCESS)]).await);
    step("remote status with devices", answered(http.get(format!("{base}/api/domovoi/remote")).header("cookie", &owner_cookie).send().await.unwrap()).await);

    // Remote access turned on (the local network only: no relay is asked) and off again.
    step("remote access on", answered(post("/api/domovoi/remote/configure", &owner_cookie, json!({ "enabled": true, "relays": ["disabled"] })).await.unwrap()).await);
    step("remote status while on", answered(http.get(format!("{base}/api/domovoi/remote")).header("cookie", &owner_cookie).send().await.unwrap()).await);
    step("pairing link while on", answered(post("/api/domovoi/remote/pairing", &owner_cookie, json!({})).await.unwrap()).await);
    step("remote access off", answered(post("/api/domovoi/remote/configure", &owner_cookie, json!({ "enabled": false })).await.unwrap()).await);

    // Signing clients out.
    step("revoke", answered(post("/api/auth/clients/revoke", &owner_cookie, json!({ "sessionId": limited_id })).await.unwrap()).await);
    step("clientRemoved", owner.event(&access, "clientRemoved").await);
    step("revoke of a session that is not there", answered(post("/api/auth/clients/revoke", &owner_cookie, json!({ "sessionId": limited_id })).await.unwrap()).await);
    step("revoke-others", answered(http.post(format!("{base}/api/auth/clients/revoke-others")).header("cookie", &owner_cookie).send().await.unwrap()).await);
    step("clientRemoved, first token", owner.event(&access, "clientRemoved").await);
    step("clientRemoved, second token", owner.event(&access, "clientRemoved").await);
    step("access snapshot at the end", {
        let again = owner.send("subscribeAuthAccess", json!({})).await;
        owner.event(&again, "snapshot").await
    });

    dump::write_value(Path::new(&server.dir), "config-wire", &json!(steps), started);
}
