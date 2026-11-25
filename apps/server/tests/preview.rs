//! The preview tunnel (`/api/preview/tunnel`) against a local TCP echo
//! server, and the server's headless browser through RPC and its stream
//! (`/api/preview-stream/ws`) against a local web page. The browser tests
//! skip, saying why, on a machine without Chrome or Chromium.

use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

fn http() -> reqwest::Client {
    let _ = rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::new()
}

struct Server {
    child: Child,
    port: u16,
    pairing: String,
    home: PathBuf,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

/// Where requests go: a server, or a bridge to one.
trait Base {
    fn port(&self) -> u16;
}

impl Base for Server {
    fn port(&self) -> u16 {
        self.port
    }
}

impl Base for u16 {
    fn port(&self) -> u16 {
        *self
    }
}

/// The browser the tests drive, if this machine has one.
fn chrome() -> Result<String, String> {
    if let Ok(path) = std::env::var("DOMOVOI_PREVIEW_BROWSER_BINARY") {
        return if std::path::Path::new(&path).exists() { Ok(path) } else { Err(format!("DOMOVOI_PREVIEW_BROWSER_BINARY={path} does not exist")) };
    }
    let mac = "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
    if std::path::Path::new(mac).exists() {
        return Ok(mac.to_owned());
    }
    for name in ["chromium", "chromium-browser", "google-chrome", "google-chrome-stable"] {
        if let Ok(out) = Command::new("which").arg(name).output()
            && out.status.success()
        {
            return Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned());
        }
    }
    Err("no /Applications/Google Chrome.app and no chromium or google-chrome on PATH".into())
}

fn start(tag: &str, browser: Option<&str>) -> Server {
    start_with(tag, browser, &[])
}

fn start_with(tag: &str, browser: Option<&str>, args: &[&str]) -> Server {
    let home = std::env::temp_dir().join(format!("domovoi-preview-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let mut child = Command::new(env!("CARGO_BIN_EXE_domovoi-server"))
        .env("DOMOVOI_NO_SESSION_IMPORT", "1")
        .env("DOMOVOI_NO_SHELL_ENV", "1")
        .env("DOMOVOI_AGENT_SESSIONS_HOME", home.join("agent-home"))
        .env("DOMOVOI_CLAUDE_BINARY", "/usr/bin/false")
        .env("DOMOVOI_CODEX_BINARY", "/usr/bin/false")
        .env("DOMOVOI_PREVIEW_BROWSER_BINARY", browser.unwrap_or("/nonexistent/chrome"))
        .env("RUST_LOG", "warn")
        .args(["--home-dir", home.to_str().unwrap(), "--port", &port.to_string()])
        .args(args)
        .stdout(Stdio::piped())
        .stderr(if std::env::var_os("DOMOVOI_TEST_STDERR").is_some() { Stdio::inherit() } else { Stdio::null() })
        .spawn()
        .unwrap();
    let mut pairing = String::new();
    for line in BufReader::new(child.stdout.take().unwrap()).lines() {
        let line = line.unwrap();
        if let Some(i) = line.find("#token=") {
            pairing = line[i + 7..].trim().to_owned();
            break;
        }
    }
    Server { child, port, pairing, home }
}

/// A session cookie for `code`.
async fn cookie_for(server: &impl Base, code: &str) -> String {
    let res = http().post(format!("http://127.0.0.1:{}/api/auth/browser-session", server.port())).json(&json!({ "credential": code })).send().await.unwrap();
    assert_eq!(res.status(), 200);
    res.headers().get("set-cookie").unwrap().to_str().unwrap().split(';').next().unwrap().to_owned()
}

/// A cookie of a device granted only `permissions`.
async fn limited_cookie(server: &Server, owner: &str, permissions: &[&str]) -> String {
    let res: Value = http()
        .post(format!("http://127.0.0.1:{}/api/domovoi/remote/pairing", server.port))
        .header("cookie", owner)
        .json(&json!({ "permissions": permissions }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    cookie_for(server, res["code"].as_str().expect("a pairing code")).await
}

async fn ws(server: &impl Base, path: &str, cookie: Option<&str>) -> Result<Ws, tokio_tungstenite::tungstenite::Error> {
    let mut req = format!("ws://127.0.0.1:{}{path}", server.port()).into_client_request().unwrap();
    if let Some(c) = cookie {
        req.headers_mut().insert("cookie", c.parse().unwrap());
    }
    tokio_tungstenite::connect_async(req).await.map(|(s, _)| s)
}

/// The refusal a WebSocket upgrade got: status, the tunnel's marker headers, body.
async fn refused(server: &impl Base, path: &str, cookie: Option<&str>) -> (u16, Vec<String>, String) {
    match ws(server, path, cookie).await {
        Err(tokio_tungstenite::tungstenite::Error::Http(res)) => {
            let markers = ["x-t3-tunnel-port-refused", "x-t3-tunnel-disabled"].iter().filter(|h| res.headers().get(**h).is_some_and(|v| v == "1")).map(|h| (*h).to_owned()).collect();
            (res.status().as_u16(), markers, String::from_utf8_lossy(res.body().as_deref().unwrap_or_default()).into_owned())
        }
        Err(e) => panic!("{path}: {e}"),
        Ok(_) => panic!("{path}: the socket opened"),
    }
}

async fn rpc(server: &Server, cookie: &str) -> Ws {
    ws(server, "/ws?orchestrationProtocol=2", Some(cookie)).await.unwrap()
}

/// Every RPC frame the tests sent and got, for `wire-check.ts`.
static TRANSCRIPT: std::sync::Mutex<Vec<Value>> = std::sync::Mutex::new(Vec::new());

fn record(conn: u64, frame: &Value) {
    TRANSCRIPT.lock().unwrap().push(json!({ "kind": "ws", "conn": conn, "frame": frame }));
}

/// Decodes what was recorded with t3code's schemas (skipped without Node 26 and the reference checkout).
fn schema_check(name: &str) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let node = std::env::var("DOMOVOI_NODE").unwrap_or_else(|_| "node".into());
    let version = Command::new(&node).arg("--version").output().ok().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned());
    let major: u32 = version.as_deref().and_then(|v| v.trim_start_matches('v').split('.').next()?.parse().ok()).unwrap_or(0);
    if !root.join(".reference/t3code/packages/contracts").exists() || major < 26 {
        eprintln!("schema check skipped (needs .reference/t3code and Node 26+, found {version:?})");
        return;
    }
    let file = std::env::temp_dir().join(format!("domovoi-preview-{name}-{}.ndjson", std::process::id()));
    let lines: String = TRANSCRIPT.lock().unwrap().iter().map(|r| format!("{r}\n")).collect();
    std::fs::write(&file, lines).unwrap();
    let res = Command::new(&node).arg(root.join("conformance/wire-check.ts")).arg(&file).output().unwrap();
    let _ = std::fs::remove_file(&file);
    let text = String::from_utf8_lossy(&res.stdout);
    assert!(res.status.success(), "t3code schema check failed:\n{text}");
    eprintln!("{}", text.trim());
}

/// One request's Exit frame.
async fn call(ws: &mut Ws, id: &str, tag: &str, payload: Value) -> Value {
    let req = json!({ "_tag": "Request", "id": id, "tag": tag, "payload": payload, "headers": [] });
    record(1, &req);
    ws.send(Message::Text(req.to_string().into())).await.unwrap();
    loop {
        let msg = tokio::time::timeout(Duration::from_secs(30), ws.next()).await.expect("an answer").expect("open").unwrap();
        let Message::Text(t) = msg else { continue };
        let v: Value = serde_json::from_str(&t).unwrap();
        if v["_tag"] == "Ping" {
            ws.send(Message::Text(json!({ "_tag": "Pong" }).to_string().into())).await.unwrap();
            continue;
        }
        if v["requestId"] == id && v["_tag"] == "Exit" {
            record(1, &v);
            return v;
        }
    }
}

async fn echo_server() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = vec![0u8; 4096];
                while let Ok(n) = socket.read(&mut buf).await {
                    if n == 0 || socket.write_all(&buf[..n]).await.is_err() {
                        break;
                    }
                }
            });
        }
    });
    port
}

async fn next_binary(ws: &mut Ws) -> Option<Vec<u8>> {
    loop {
        match tokio::time::timeout(Duration::from_secs(10), ws.next()).await {
            Ok(Some(Ok(Message::Binary(b)))) => return Some(b.to_vec()),
            Ok(Some(Ok(Message::Close(_)) | Err(_))) | Ok(None) | Err(_) => return None,
            Ok(Some(Ok(_))) => continue,
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_tunnel_reaches_loopback_ports_only_while_it_is_on() {
    let server = start("tunnel", None);
    let owner = cookie_for(&server, &server.pairing).await;
    let echo = echo_server().await;

    // Bytes go to the loopback port and back, both ways.
    let mut tunnel = ws(&server, &format!("/api/preview/tunnel?port={echo}"), Some(&owner)).await.unwrap();
    tunnel.send(Message::Binary(b"hello through the tunnel".to_vec().into())).await.unwrap();
    assert_eq!(next_binary(&mut tunnel).await.as_deref(), Some(&b"hello through the tunnel"[..]));
    // Only a port is read: a host in the query does not take the tunnel off this machine.
    let mut elsewhere = ws(&server, &format!("/api/preview/tunnel?port={echo}&host=192.0.2.1"), Some(&owner)).await.unwrap();
    elsewhere.send(Message::Binary(b"still loopback".to_vec().into())).await.unwrap();
    assert_eq!(next_binary(&mut elsewhere).await.as_deref(), Some(&b"still loopback"[..]));
    drop(elsewhere);

    // A WebSocket ticket stands in for the cookie.
    let ticket: Value = http().post(format!("http://127.0.0.1:{}/api/auth/websocket-ticket/media", server.port)).header("cookie", &owner).send().await.unwrap().json().await.unwrap();
    let mut by_ticket = ws(&server, &format!("/api/preview/tunnel?port={echo}&wsTicket={}", ticket["ticket"].as_str().unwrap()), None).await.unwrap();
    by_ticket.send(Message::Binary(b"ticket".to_vec().into())).await.unwrap();
    assert_eq!(next_binary(&mut by_ticket).await.as_deref(), Some(&b"ticket"[..]));
    drop(by_ticket);

    // Refusals, as t3code's.
    for bad in ["", "?port=0", "?port=65536", "?port=http", "?port=1.5"] {
        assert_eq!(refused(&server, &format!("/api/preview/tunnel{bad}"), Some(&owner)).await.0, 400, "{bad:?}");
    }
    let plain = http().get(format!("http://127.0.0.1:{}/api/preview/tunnel?port={echo}", server.port)).header("cookie", &owner).send().await.unwrap();
    assert_eq!(plain.status(), 400, "not an upgrade");
    assert_eq!(refused(&server, &format!("/api/preview/tunnel?port={echo}"), None).await.0, 401);
    let reader = limited_cookie(&server, &owner, &["orchestration:read"]).await;
    let (status, _, body) = refused(&server, &format!("/api/preview/tunnel?port={echo}"), Some(&reader)).await;
    assert_eq!(status, 403);
    assert_eq!(serde_json::from_str::<Value>(&body).unwrap()["requiredPermission"], "terminal:operate", "{body}");
    let own = refused(&server, &format!("/api/preview/tunnel?port={}", server.port), Some(&owner)).await;
    assert_eq!(own, (403, vec!["x-t3-tunnel-port-refused".to_owned()], format!("Port {} is this T3 Code server's own port.", server.port)));
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let nothing = refused(&server, &format!("/api/preview/tunnel?port={closed}"), Some(&owner)).await;
    assert_eq!(nothing, (502, vec!["x-t3-tunnel-port-refused".to_owned()], format!("Nothing accepted a connection on loopback port {closed}.")));

    // Switching the tunnel off closes the one open and refuses new ones.
    let mut socket = rpc(&server, &owner).await;
    let off = call(&mut socket, "1", "server.updateSettings", json!({ "patch": { "enablePreviewTunnel": false } })).await;
    assert_eq!(off["exit"]["_tag"], "Success", "{off}");
    let ended = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match tunnel.next().await {
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return true,
                Some(Ok(_)) => continue,
            }
        }
    })
    .await;
    assert_eq!(ended, Ok(true), "the open tunnel closed");
    let disabled = refused(&server, &format!("/api/preview/tunnel?port={echo}"), Some(&owner)).await;
    assert_eq!(disabled, (403, vec!["x-t3-tunnel-disabled".to_owned()], "The preview tunnel is turned off on this T3 Code server.".to_owned()));
    let on = call(&mut socket, "2", "server.updateSettings", json!({ "patch": { "enablePreviewTunnel": true } })).await;
    assert_eq!(on["exit"]["_tag"], "Success", "{on}");
    let mut again = ws(&server, &format!("/api/preview/tunnel?port={echo}"), Some(&owner)).await.unwrap();
    again.send(Message::Binary(b"on again".to_vec().into())).await.unwrap();
    assert_eq!(next_binary(&mut again).await.as_deref(), Some(&b"on again"[..]));

    // Signing the session out closes its tunnels.
    let out = http().post(format!("http://127.0.0.1:{}/api/domovoi/session/end", server.port)).header("cookie", &owner).send().await.unwrap();
    assert_eq!(out.status(), 200);
    let ended = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match again.next().await {
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return true,
                Some(Ok(_)) => continue,
            }
        }
    })
    .await;
    assert_eq!(ended, Ok(true), "the tunnel closed with its session");
}

/// A small site: `/` with a title, a field and a button that logs; `/two`.
async fn web_page() -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let Ok(n) = socket.read(&mut buf).await else { return };
                let head = String::from_utf8_lossy(&buf[..n]).into_owned();
                let path = head.split_whitespace().nth(1).unwrap_or("/").to_owned();
                let body = match path.as_str() {
                    "/" => r#"<!doctype html><html><head><title>Preview Home</title></head><body style="margin:0">
<h1>Hello preview</h1>
<input id="name" placeholder="Your name">
<button id="go" style="position:absolute;left:20px;top:200px;width:200px;height:60px" onclick="console.log('clicked ' + document.getElementById('name').value); document.getElementById('out').textContent = 'Hi ' + document.getElementById('name').value">Greet</button>
<p id="out"></p>
<script>console.log('hello from the page')</script>
</body></html>"#,
                    "/two" => "<!doctype html><html><head><title>Second</title></head><body><p>two</p></body></html>",
                    _ => "<!doctype html><title>Missing</title>",
                };
                let status = if matches!(path.as_str(), "/" | "/two") { "200 OK" } else { "404 Not Found" };
                let res = format!("HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                let _ = socket.write_all(res.as_bytes()).await;
            });
        }
    });
    port
}

/// Lists the thread's tabs until `done` holds of the one tab.
async fn until_tab(ws: &mut Ws, thread: &str, what: &str, done: impl Fn(&Value) -> bool) -> Value {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut n = 0;
    loop {
        n += 1;
        let r = call(ws, &format!("list-{what}-{n}"), "preview.list", json!({ "threadId": thread })).await;
        let tab = r["exit"]["value"]["sessions"][0].clone();
        if done(&tab) {
            return tab;
        }
        assert!(tokio::time::Instant::now() < deadline, "{what}: {tab}");
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

/// The next JSON message of `type` a viewer is sent (acknowledging frames on the way).
async fn viewer_message(viewer: &mut Ws, kind: &str) -> Value {
    loop {
        match tokio::time::timeout(Duration::from_secs(15), viewer.next()).await.unwrap_or_else(|_| panic!("no {kind} message")) {
            Some(Ok(Message::Text(t))) => {
                let v: Value = serde_json::from_str(&t).unwrap();
                if v["type"] == kind {
                    return v;
                }
            }
            Some(Ok(Message::Binary(_))) => viewer.send(Message::Text(json!({ "type": "ack" }).to_string().into())).await.unwrap(),
            Some(Ok(_)) => {}
            other => panic!("the viewer closed before a {kind} message: {other:?}"),
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_server_browser_runs_streams_and_closes_a_server_tab() {
    let browser = match chrome() {
        Ok(b) => b,
        Err(why) => {
            eprintln!("SKIPPED the_server_browser_runs_streams_and_closes_a_server_tab: {why}");
            return;
        }
    };
    let server = start("browser", Some(&browser));
    let owner = cookie_for(&server, &server.pairing).await;
    let site = web_page().await;
    let mut socket = rpc(&server, &owner).await;
    // What subscribers are told, decoded with t3code's schemas at the end.
    let mut events = rpc(&server, &owner).await;
    let subscribe = json!({ "_tag": "Request", "id": "events", "tag": "subscribePreviewEvents", "payload": {}, "headers": [] });
    record(2, &subscribe);
    events.send(Message::Text(subscribe.to_string().into())).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let opened = call(&mut socket, "open", "preview.open", json!({ "threadId": "t-browser", "url": format!("127.0.0.1:{site}/"), "runtime": "server", "colorScheme": "light", "zoomFactor": 1 })).await;
    let tab_id = opened["exit"]["value"]["tabId"].as_str().expect("a tab").to_owned();
    // The server's browser loads it and reports the page: headless, with its title.
    let home = format!("http://127.0.0.1:{site}/");
    let tab = until_tab(&mut socket, "t-browser", "loaded", |t| t["headless"] == true && t["navStatus"]["_tag"] == "Success" && t["navStatus"]["title"] == "Preview Home").await;
    assert_eq!(tab["navStatus"]["url"], json!(home));
    // A client cannot report a tab the server drives.
    let report = call(&mut socket, "report", "preview.reportStatus", json!({ "threadId": "t-browser", "tabId": tab_id, "navStatus": { "_tag": "Idle" }, "canGoBack": false, "canGoForward": false })).await;
    assert_eq!(report["exit"]["cause"][0]["error"]["_tag"], "PreviewControlRequiredError", "{report}");

    // A viewer: control (an unclaimed tab is the first operator's), its size, and JPEG frames.
    let stream = format!("/api/preview-stream/ws?threadId=t-browser&tabId={tab_id}&maxWidth=800&maxHeight=600&quality=50");
    let mut viewer = ws(&server, &stream, Some(&owner)).await.unwrap();
    let control = viewer_message(&mut viewer, "control").await;
    assert_eq!((control["canOperate"].clone(), control["controller"].clone()), (json!(true), json!("you")), "{control}");
    let size = viewer_message(&mut viewer, "viewport").await;
    assert_eq!((size["width"].clone(), size["height"].clone()), (json!(1280), json!(800)), "{size}");
    let frame = next_binary(&mut viewer).await.expect("a frame");
    assert_eq!(&frame[..2], &[0xFF, 0xD8], "a JPEG");
    viewer.send(Message::Text(json!({ "type": "ack" }).to_string().into())).await.unwrap();

    // Its size becomes a fill tab's; input reaches the page.
    viewer.send(Message::Text(json!({ "type": "resize", "width": 900, "height": 700 }).to_string().into())).await.unwrap();
    let size = viewer_message(&mut viewer, "viewport").await;
    assert_eq!((size["width"].clone(), size["height"].clone()), (json!(900), json!(700)), "{size}");
    viewer.send(Message::Text(json!({ "type": "navigate", "url": format!("127.0.0.1:{site}/two") }).to_string().into())).await.unwrap();
    let tab = until_tab(&mut socket, "t-browser", "navigated", |t| t["navStatus"]["title"] == "Second").await;
    assert_eq!(tab["canGoBack"], true, "{tab}");
    viewer.send(Message::Text(json!({ "type": "history", "delta": -1 }).to_string().into())).await.unwrap();
    until_tab(&mut socket, "t-browser", "back", |t| t["navStatus"]["url"] == json!(home) && t["canGoForward"] == true).await;
    viewer.send(Message::Text(json!({ "type": "probe", "x": 30, "y": 10 }).to_string().into())).await.unwrap();
    let probe = viewer_message(&mut viewer, "probe").await;
    assert_eq!(probe["editable"], false, "{probe}");

    // A viewer without `preview:operate` watches only.
    let reader = limited_cookie(&server, &owner, &["orchestration:read"]).await;
    let mut watcher = ws(&server, &stream, Some(&reader)).await.unwrap();
    let control = viewer_message(&mut watcher, "control").await;
    assert_eq!((control["canOperate"].clone(), control["controller"].clone()), (json!(false), json!("another-viewer")), "{control}");

    // A tab that does not exist, and one that closes, end their streams with 4404.
    let mut missing = ws(&server, "/api/preview-stream/ws?threadId=t-browser&tabId=nope", Some(&owner)).await.unwrap();
    match tokio::time::timeout(Duration::from_secs(10), missing.next()).await {
        Ok(Some(Ok(Message::Close(Some(frame))))) => assert_eq!(u16::from(frame.code), 4404),
        other => panic!("{other:?}"),
    }
    let closed = call(&mut socket, "close", "preview.close", json!({ "threadId": "t-browser", "tabId": tab_id })).await;
    assert_eq!(closed["exit"]["_tag"], "Success");
    let code = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match viewer.next().await {
                Some(Ok(Message::Close(Some(frame)))) => return Some(u16::from(frame.code)),
                Some(Ok(_)) => continue,
                _ => return None,
            }
        }
    })
    .await
    .unwrap();
    assert_eq!(code, Some(4404));

    // Every event so far: opened, headless, loading, loaded, navigated, closed.
    let mut kinds = vec![];
    while let Ok(Some(Ok(msg))) = tokio::time::timeout(Duration::from_millis(500), events.next()).await {
        let Message::Text(t) = msg else { continue };
        let v: Value = serde_json::from_str(&t).unwrap();
        if v["_tag"] == "Chunk" {
            kinds.extend(v["values"].as_array().unwrap().iter().map(|e| e["type"].as_str().unwrap_or_default().to_owned()));
            record(2, &v);
            events.send(Message::Text(json!({ "_tag": "Ack", "requestId": "events" }).to_string().into())).await.unwrap();
        }
    }
    assert!(kinds.first().is_some_and(|k| k == "opened") && kinds.last().is_some_and(|k| k == "closed") && kinds.iter().any(|k| k == "navigated"), "{kinds:?}");
    schema_check("browser");
}

#[tokio::test(flavor = "multi_thread")]
async fn without_a_browser_server_tabs_stay_announced_and_streams_are_unavailable() {
    let server = start("nobrowser", None);
    let owner = cookie_for(&server, &server.pairing).await;
    let mut socket = rpc(&server, &owner).await;
    let opened = call(&mut socket, "open", "preview.open", json!({ "threadId": "t-none", "url": "localhost:5173", "runtime": "server" })).await;
    let tab_id = opened["exit"]["value"]["tabId"].as_str().expect("a tab").to_owned();
    let (status, _, _) = refused(&server, &format!("/api/preview-stream/ws?threadId=t-none&tabId={tab_id}"), Some(&owner)).await;
    assert_eq!(status, 503, "the browser cannot start");
    let list = call(&mut socket, "list", "preview.list", json!({ "threadId": "t-none" })).await;
    let tab = &list["exit"]["value"]["sessions"][0];
    assert_eq!((tab["navStatus"]["_tag"].clone(), tab.get("headless").cloned()), (json!("Loading"), None), "{tab}");
    let upload = http().post(format!("http://127.0.0.1:{}/api/preview-stream/upload?threadId=t-none&tabId={tab_id}&chooser=x", server.port)).header("cookie", &owner).send().await.unwrap();
    assert_eq!(upload.status(), 409);
}

/// The tunnel through an iroh connection, as a remote client reaches it.
#[tokio::test(flavor = "multi_thread")]
async fn the_tunnel_works_over_iroh() {
    let server = start_with("iroh", None, &["--remote", "--relay", "disabled"]);
    let home = server.home.clone();
    // The printed code works on plain TCP only; an iroh peer pairs with one of its own.
    let out = Command::new(env!("CARGO_BIN_EXE_domovoi-server"))
        .env("DOMOVOI_NO_SESSION_IMPORT", "1")
        .env("DOMOVOI_NO_SHELL_ENV", "1")
        .args(["--home-dir", home.to_str().unwrap(), "--port", &server.port.to_string(), "pair"])
        .output()
        .unwrap();
    let code = String::from_utf8_lossy(&out.stdout).lines().find_map(|l| l.split("#token=").nth(1).map(|c| c.trim().to_owned())).expect("a code");
    let path = home.join("remote").join("ticket");
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let ticket = loop {
        if let Ok(t) = std::fs::read_to_string(&path)
            && !t.trim().is_empty()
        {
            break t;
        }
        assert!(std::time::Instant::now() < deadline, "the server never saved a remote ticket");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    let bridge = domovoi_remote::Bridge::start(domovoi_remote::SecretKey::generate(), domovoi_remote::Network::Direct { port: 0 }, domovoi_remote::parse_ticket(&ticket).unwrap(), 0)
        .await
        .unwrap();
    let remote = bridge.port();
    let cookie = cookie_for(&remote, &code).await;
    let echo = echo_server().await;
    let mut tunnel = ws(&remote, &format!("/api/preview/tunnel?port={echo}"), Some(&cookie)).await.unwrap();
    tunnel.send(Message::Binary(b"over iroh".to_vec().into())).await.unwrap();
    assert_eq!(next_binary(&mut tunnel).await.as_deref(), Some(&b"over iroh"[..]));
    // A peer without a session reaches no tunnel.
    assert_eq!(refused(&remote, &format!("/api/preview/tunnel?port={echo}"), None).await.0, 401);
}
