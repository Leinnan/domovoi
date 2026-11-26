//! Calls every RPC listed in `apps/server/tests/cases/calls/*.json` (one file per
//! area, run in file-name order) against the real
//! `domovoi-server` binary and checks each answer's kind (first chunk / success /
//! typed failure). Defects and unregistered methods always fail.
//!
//! Case fields: `tag`, `payload`, `expect` (`chunk` | `success` | `failure`),
//! optional `error` (the failure's `_tag`). Strings in payloads may use
//! `$WORKSPACE`, `$PROJECT`, `$THREAD`, `$HOME` and `$ITEM` (the thread's first turn item).
//!
//! `apps/server/tests/cases/groups/<name>.json` holds, group by group, the calls that
//! reach every answer and every failure of a group's methods; see
//! [`every_rpc_group_answers_as_recorded`].

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

/// System tools only, as a `PATH` (after `first`): no editor, `gh` or
/// provider CLI is found, so no case can launch one. On Windows: its own
/// folders and git's.
fn system_path(first: Option<&Path>) -> std::ffi::OsString {
    let mut dirs: Vec<PathBuf> = first.into_iter().map(Path::to_path_buf).collect();
    if cfg!(windows) {
        let root = PathBuf::from(std::env::var_os("SystemRoot").unwrap_or_else(|| r"C:\Windows".into()));
        dirs.extend([root.join("System32"), root]);
        // Git for Windows' launcher folder (`Git\cmd`), as a user's PATH has it:
        // its `mingw64\bin\git.exe` crashes without the rest of Git on PATH.
        let git = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).find(|d| d.join("git.exe").is_file());
        dirs.extend(git.map(|d| d.ancestors().map(|a| a.join("cmd")).find(|c| c.join("git.exe").is_file()).unwrap_or(d)));
    } else {
        dirs.extend(["/usr/bin", "/bin", "/usr/sbin", "/sbin"].map(PathBuf::from));
    }
    std::env::join_paths(dirs).expect("folders without the separator")
}

/// iroh (through domovoi-remote) turns on reqwest's rustls backend without a crypto
/// provider, so every client needs one installed first.
fn http_client() -> reqwest::Client {
    let _ = rustls::crypto::ring::default_provider().install_default();
    reqwest::Client::new()
}


type Ws = tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

/// The checkout whose cases are read: this one, or with
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
    // Git as on a machine without the developer's settings (hooks, LFS
    // filters): a home of the test's own, whose `.gitconfig` (unless a case
    // wrote one) only names who commits.
    let userhome = dir.join("userhome");
    std::fs::create_dir_all(&userhome).unwrap();
    let gitconfig = userhome.join(".gitconfig");
    if !gitconfig.exists() {
        std::fs::write(&gitconfig, "[user]\n\tname = t\n\temail = t@t\n").unwrap();
    }
    let mut child = Command::new(server_bin()).env("DOMOVOI_NO_SESSION_IMPORT", "1").env("DOMOVOI_NO_SHELL_ENV", "1")
        .args(&args)
        .env("HOME", &userhome)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("RUST_LOG", "warn")
        // Turns fail at once instead of reaching a real provider.
        .env("DOMOVOI_CLAUDE_BINARY", "/usr/bin/false")
        .env("DOMOVOI_CODEX_BINARY", "/usr/bin/false")
        // System tools only: no editor, `gh` or provider CLI is found, so no case can launch one.
        .env("PATH", system_path(None))
        // Agent-session scans read this empty home, never the real ~/.claude or ~/.codex.
        .env("DOMOVOI_AGENT_SESSIONS_HOME", dir.join("agent-home"))
        // The ACP registry the cases search: a file, not the network.
        .env("DOMOVOI_ACP_REGISTRY_URL", format!("file://{}/tests/cases/acp-registry.json", env!("CARGO_MANIFEST_DIR")))
        // The usage rate table: a file in LiteLLM's format, not the network.
        .env("DOMOVOI_USAGE_RATES_URL", format!("file://{}/tests/cases/usage-rates.json", env!("CARGO_MANIFEST_DIR")))
        // No headless browser: a server preview tab a case opens loads nothing.
        .env("DOMOVOI_PREVIEW_BROWSER_BINARY", "/nonexistent/chrome")
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
    let mut req = format!("ws://127.0.0.1:{}/ws?protocol={PROTOCOL}", server.port).into_client_request().unwrap();
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
    let ok = Command::new("git").args(args).current_dir(dir).env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_CONFIG_NOSYSTEM", "1")
        .output().unwrap().status.success();
    assert!(ok, "git {args:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn session_file_image_previews() {
    file_image_previews(Transport::Tcp).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn session_file_image_previews_over_iroh() {
    file_image_previews(Transport::Iroh).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn mermaid_diagrams_are_drawn_for_clients() {
    mermaid_diagrams(Transport::Tcp).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn mermaid_diagrams_are_drawn_for_clients_over_iroh() {
    mermaid_diagrams(Transport::Iroh).await;
}

/// `assets.createUrl` with Domovoi's `mermaid-diagram` resource: a signed
/// URL of the PNG, or a failure the client shows the source for.
async fn mermaid_diagrams(transport: Transport) {
    let dir = std::env::temp_dir().join(format!("domovoi-diagrams-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let server = start_with(&dir, transport);
    let mut ws = connect(&server).await;
    let base = format!("http://127.0.0.1:{}", server.port);
    let http = http_client();
    let diagram = |source: &str, theme: &str| json!({ "resource": { "_tag": "mermaid-diagram", "source": source, "theme": theme } });

    for (id, theme) in [(1, "light"), (2, "dark")] {
        let asset = icon_call(&mut ws, id, "assets.createUrl", diagram("flowchart LR\nA[Request] --> B[Response]", theme)).await;
        let url = asset["relativeUrl"].as_str().expect("relative URL");
        assert!(url.starts_with("/api/assets/diagram/"), "{asset}");
        let response = http.get(format!("{base}{url}")).send().await.unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()["content-type"], "image/png");
        assert!(imagesize::blob_size(&response.bytes().await.unwrap()).unwrap().width > 0);
    }
    for (id, source) in [(3, "not a Mermaid diagram".to_owned()), (4, "a".repeat(50_001))] {
        let exit = call(&mut ws, id, "assets.createUrl", &diagram(&source, "light")).await.unwrap()["exit"].clone();
        assert_eq!(exit["_tag"], "Failure", "{exit}");
    }
    assert_eq!(http.get(format!("{base}/api/assets/diagram/forged.token")).send().await.unwrap().status(), 404);
}

async fn file_image_previews(transport: Transport) {
    use base64::{Engine, engine::general_purpose::STANDARD};
    use sha2::{Digest, Sha256};

    let dir = std::env::temp_dir().join(format!("domovoi-file-images-{}", uuid::Uuid::new_v4()));
    let workspace = dir.join("workspace");
    let linked = dir.join("linked");
    std::fs::create_dir_all(&workspace).unwrap();
    let server = start_with(&dir, transport);
    let mut ws = connect(&server).await;
    let base = format!("http://127.0.0.1:{}", server.port);
    let http = http_client();
    let png = STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAIAAACQd1PeAAAADElEQVR4nGP4z8AAAAMBAQDJ/pLvAAAAAElFTkSuQmCC").unwrap();
    let oid = hex::encode(Sha256::digest(&png));
    let pointer = format!("version https://git-lfs.github.com/spec/v1\noid sha256:{oid}\nsize {}\n", png.len());
    let object = format!("objects/{}/{}/{oid}", &oid[..2], &oid[2..4]);
    let mut id = 0;

    let preview_url = |exit: &Value| {
        assert_eq!(exit["_tag"], "Success", "{exit}");
        assert_eq!(exit["value"]["contents"], "");
        assert_eq!(exit["value"]["truncated"], false);
        format!("{base}{}", exit["value"]["imageUrl"].as_str().expect("image URL"))
    };
    let message = |exit: Value| {
        assert_eq!(exit["_tag"], "Failure", "{exit}");
        exit["cause"][0]["error"]["message"].as_str().unwrap().to_owned()
    };
    // A Git LFS file read with `previewImages`: its object's oid, size and state.
    let lfs = |exit: &Value| {
        assert_eq!(exit["_tag"], "Success", "{exit}");
        let lfs = &exit["value"]["lfs"];
        (lfs["oid"].as_str().expect("an oid").to_owned(), lfs["size"].as_u64().expect("a size"), lfs["state"].as_str().expect("a state").to_owned())
    };

    std::fs::write(workspace.join("normal.PNG"), &png).unwrap();
    std::fs::write(workspace.join("page.html"), "<h1>Hello</h1>").unwrap();
    let url = preview_url(&read_file_exit(&mut ws, &mut id, &workspace, "normal.PNG", true).await);
    let asset = http.get(&url).send().await.unwrap();
    assert_eq!(asset.status(), 200);
    assert_eq!(asset.headers()["content-type"], "image/png");
    assert_eq!(asset.bytes().await.unwrap().as_ref(), png);
    let tampered = format!("{url}.invalid");
    assert_eq!(http.get(tampered).send().await.unwrap().status(), 404);
    assert_eq!(message(read_file_exit(&mut ws, &mut id, &workspace, "normal.PNG", false).await), "Binary file.");
    let html = read_file_exit(&mut ws, &mut id, &workspace, "page.html", true).await;
    assert_eq!(html["value"]["contents"], "<h1>Hello</h1>");
    assert!(html["value"].get("imageUrl").is_none());
    assert!(html["value"].get("lfs").is_none(), "not kept in LFS");

    // Formats not every client draws come as PNG.
    let raster = image::RgbaImage::from_pixel(3, 2, image::Rgba([255, 0, 0, 255]));
    for (name, format) in [("bitmap.bmp", image::ImageFormat::Bmp), ("scan.TIFF", image::ImageFormat::Tiff), ("favicon.ico", image::ImageFormat::Ico)] {
        raster.save_with_format(workspace.join(name), format).unwrap();
        let drawn = http.get(preview_url(&read_file_exit(&mut ws, &mut id, &workspace, name, true).await)).send().await.unwrap();
        assert_eq!(drawn.headers()["content-type"], "image/png", "{name}");
        let size = imagesize::blob_size(&drawn.bytes().await.unwrap()).unwrap();
        assert_eq!((size.width, size.height), (3, 2), "{name}");
    }

    let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="4000" height="1000"><rect width="4000" height="1000" fill="red"/></svg>"#;
    std::fs::write(workspace.join("vector.svg"), svg).unwrap();
    let vector = http.get(preview_url(&read_file_exit(&mut ws, &mut id, &workspace, "vector.svg", true).await)).send().await.unwrap();
    assert_eq!(vector.headers()["content-type"], "image/png");
    let size = imagesize::blob_size(&vector.bytes().await.unwrap()).unwrap();
    assert_eq!((size.width, size.height), (2048, 512));

    git(&workspace, &["init", "-q", "-b", "main"]);
    git(&workspace, &["-c", "user.name=Test", "-c", "user.email=test@example.com", "commit", "-qm", "Start", "--allow-empty"]);
    git(&workspace, &["worktree", "add", "--detach", linked.to_str().unwrap()]);
    std::fs::write(linked.join(".gitattributes"), "*.png filter=lfs diff=lfs merge=lfs -text\n").unwrap();
    std::fs::write(linked.join("checked-out.png"), &png).unwrap();
    let read = read_file_exit(&mut ws, &mut id, &linked, "checked-out.png", true).await;
    assert_eq!(lfs(&read), (oid.clone(), png.len() as u64, "cached".into()), "a rule puts it in LFS and it is checked out");
    let checked_out = preview_url(&read);
    assert_eq!(http.get(checked_out).send().await.unwrap().bytes().await.unwrap().as_ref(), png);
    let cache = workspace.join(".git/lfs").join(&object);
    std::fs::create_dir_all(cache.parent().unwrap()).unwrap();
    std::fs::write(&cache, &png).unwrap();
    std::fs::write(workspace.join("cached.png"), &pointer).unwrap();
    let plain = read_file_exit(&mut ws, &mut id, &workspace, "cached.png", false).await;
    assert_eq!(plain["value"]["contents"], pointer);
    assert!(plain["value"].get("lfs").is_none(), "as t3code reads it");
    let read = read_file_exit(&mut ws, &mut id, &workspace, "cached.png", true).await;
    assert_eq!(lfs(&read), (oid.clone(), png.len() as u64, "cached".into()));
    assert_eq!(read["value"]["byteLength"], png.len());
    let url = preview_url(&read);
    assert_eq!(http.get(url).send().await.unwrap().bytes().await.unwrap().as_ref(), png);
    assert_eq!(std::fs::read_to_string(workspace.join("cached.png")).unwrap(), pointer);

    // Any file can be a pointer: text shows the object's, a binary file
    // nothing, and a pointer with extensions only that it is one.
    let lfs_pointer = |contents: &[u8], extensions: &str| {
        let oid = hex::encode(Sha256::digest(contents));
        let cache = workspace.join(".git/lfs/objects").join(&oid[..2]).join(&oid[2..4]).join(&oid);
        std::fs::create_dir_all(cache.parent().unwrap()).unwrap();
        std::fs::write(cache, contents).unwrap();
        (oid.clone(), format!("version https://git-lfs.github.com/spec/v1\n{extensions}oid sha256:{oid}\nsize {}\n", contents.len()))
    };
    let (text_oid, text_pointer) = lfs_pointer(b"stored in LFS\n", "");
    std::fs::write(workspace.join("notes.txt"), &text_pointer).unwrap();
    let read = read_file_exit(&mut ws, &mut id, &workspace, "notes.txt", true).await;
    assert_eq!(read["value"]["contents"], "stored in LFS\n");
    assert_eq!(lfs(&read), (text_oid, 14, "cached".into()));
    let (blob_oid, blob_pointer) = lfs_pointer(&[0, 159, 146, 150, 255], "");
    std::fs::write(workspace.join("blob"), &blob_pointer).unwrap();
    let read = read_file_exit(&mut ws, &mut id, &workspace, "blob", true).await;
    assert_eq!(read["value"]["contents"], "", "binary");
    assert_eq!(lfs(&read), (blob_oid, 5, "cached".into()));
    let ext = format!("ext-0-crypt sha256:{oid}\n");
    let (_, transformed) = lfs_pointer(b"transformed", &ext);
    std::fs::write(workspace.join("secret.png"), &transformed).unwrap();
    let read = read_file_exit(&mut ws, &mut id, &workspace, "secret.png", true).await;
    assert_eq!(lfs(&read).2, "unsupported");
    assert_eq!(read["value"]["contents"], transformed, "the pointer's text");

    git(&workspace, &["config", "lfs.storage", "custom-lfs"]);
    std::fs::rename(workspace.join(".git/lfs"), workspace.join(".git/custom-lfs")).unwrap();
    std::fs::write(linked.join("cached.png"), &pointer).unwrap();
    let linked_url = preview_url(&read_file_exit(&mut ws, &mut id, &linked, "cached.png", true).await);
    assert_eq!(http.get(&linked_url).send().await.unwrap().bytes().await.unwrap().as_ref(), png);
    let moved_cache = workspace.join(".git/custom-lfs").join(&object);
    std::fs::write(&moved_cache, vec![0; png.len()]).unwrap();
    let read = read_file_exit(&mut ws, &mut id, &linked, "cached.png", true).await;
    assert_eq!(lfs(&read).2, "mismatch");
    assert_eq!(read["value"]["contents"], pointer);
    assert!(read["value"].get("imageUrl").is_none());
    assert_eq!(http.get(linked_url).send().await.unwrap().status(), 404);
    std::fs::remove_file(moved_cache).unwrap();
    let read = read_file_exit(&mut ws, &mut id, &linked, "cached.png", true).await;
    assert_eq!(lfs(&read), (oid.clone(), png.len() as u64, "missing".into()), "not downloaded");
    assert_eq!(read["value"]["contents"], pointer);
    assert!(message(read_file_exit(&mut ws, &mut id, &linked, "../workspace/normal.PNG", true).await).contains("outside"));
    #[cfg(unix)]
    {
        let checked_out = preview_url(&read_file_exit(&mut ws, &mut id, &linked, "checked-out.png", true).await);
        std::fs::remove_file(linked.join("checked-out.png")).unwrap();
        std::os::unix::fs::symlink(workspace.join("normal.PNG"), linked.join("checked-out.png")).unwrap();
        assert_eq!(http.get(checked_out).send().await.unwrap().status(), 404);
    }

    let oversized = workspace.join("large.png");
    std::fs::write(&oversized, &png).unwrap();
    std::fs::OpenOptions::new().write(true).open(&oversized).unwrap().set_len(20 * 1024 * 1024 + 1).unwrap();
    assert!(message(read_file_exit(&mut ws, &mut id, &workspace, "large.png", true).await).contains("20 MiB"));
}

#[tokio::test(flavor = "multi_thread")]
async fn project_icon_files_are_ranked_served_and_shared() {
    let dir = std::env::temp_dir().join(format!("domovoi-project-icons-{}", uuid::Uuid::new_v4()));
    let workspace = dir.join("workspace");
    std::fs::create_dir_all(workspace.join("public")).unwrap();
    std::fs::create_dir_all(workspace.join("fixtures")).unwrap();
    let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" width="400" height="100"><rect width="400" height="100" fill="red"/></svg>"#;
    std::fs::write(workspace.join("favicon.svg"), "corrupt").unwrap();
    std::fs::write(workspace.join("public/favicon.svg"), svg).unwrap();
    std::fs::write(workspace.join("fixtures/favicon.svg"), svg).unwrap();
    std::fs::write(workspace.join("workspace-logo.svg"), svg).unwrap();
    std::fs::write(dir.join("outside.svg"), svg).unwrap();
    let ico = include_bytes!("../icon/domovoi-server.ico");
    assert!(ico.starts_with(&[0, 0, 1, 0]), "icon/domovoi-server.ico should be an icon; run `git lfs pull`");
    std::fs::write(workspace.join("custom.ico"), ico).unwrap();
    let mut server = start(&dir);
    let mut ws = connect(&server).await;
    let http = http_client();
    let base = format!("http://127.0.0.1:{}", server.port);
    let made = json!({ "type": "project.create", "commandId": "make", "projectId": "icons", "title": "Icons", "workspaceRoot": workspace });
    assert_eq!(icon_call(&mut ws, 1, "projects.mutate", made).await["id"], "icons");

    let files = icon_call(&mut ws, 2, "projects.searchEntries", json!({ "cwd": workspace, "kind": "file", "imageOnly": true, "query": "", "limit": 20 })).await;
    let paths: Vec<_> = files["entries"].as_array().unwrap().iter().map(|entry| entry["path"].as_str().unwrap()).collect();
    assert_eq!(paths, ["favicon.svg", "public/favicon.svg", "fixtures/favicon.svg", "workspace-logo.svg", "custom.ico"]);

    let favicon = |hint: &str| json!({ "resource": { "_tag": "project-favicon", "cwd": workspace, "path": hint } });
    let thumbnail = |path: &str| json!({ "resource": { "_tag": "draft-workspace-file", "cwd": workspace, "path": path } });
    let automatic = icon_call(&mut ws, 3, "assets.createUrl", favicon("custom.ico")).await;
    assert_eq!(automatic["sourcePath"], "public/favicon.svg");
    let asset_url = |value: &Value| format!("{base}{}", value["relativeUrl"].as_str().unwrap());
    let response = http.get(asset_url(&automatic)).send().await.unwrap();
    assert_eq!(response.headers()["content-type"], "image/png");
    assert_eq!(imagesize::blob_size(&response.bytes().await.unwrap()).unwrap().width, 128);

    let preview = icon_call(&mut ws, 4, "assets.createUrl", thumbnail("custom.ico")).await;
    assert_eq!(preview["sourcePath"], "custom.ico");
    assert_eq!(http.get(asset_url(&preview)).send().await.unwrap().headers()["content-type"], "image/png");
    let outside = icon_call(&mut ws, 5, "assets.createUrl", thumbnail("../outside.svg")).await;
    assert!(outside.get("sourcePath").is_none());
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(dir.join("outside.svg"), workspace.join("escape.svg")).unwrap();
        let escaped = icon_call(&mut ws, 6, "assets.createUrl", thumbnail("escape.svg")).await;
        assert!(escaped.get("sourcePath").is_none());
    }

    let change = |command: &str, fields: Value| {
        let mut payload = json!({ "type": "project.update", "commandId": command, "projectId": "icons" });
        payload.as_object_mut().unwrap().extend(fields.as_object().unwrap().clone());
        payload
    };
    let picked = change("file", json!({ "faviconPath": "custom.ico", "projectIcon": null }));
    assert_eq!(icon_call(&mut ws, 7, "projects.mutate", picked).await["faviconPath"], "custom.ico");
    // A new RPC client observes the same saved file.
    server.pairing = minted_code(&dir.join("home"), server.port);
    let mut other = connect(&server).await;
    let saved = icon_call(&mut other, 1, "assets.createUrl", favicon("public/favicon.svg")).await;
    assert_eq!(saved["sourcePath"], "custom.ico");
    let rename = change("rename", json!({ "title": "Renamed" }));
    assert_eq!(icon_call(&mut other, 2, "projects.mutate", rename).await["faviconPath"], "custom.ico");
    std::fs::remove_file(workspace.join("custom.ico")).unwrap();
    assert_eq!(http.get(asset_url(&saved)).send().await.unwrap().status(), 404);
    let missing = icon_call(&mut ws, 8, "assets.createUrl", favicon("")).await;
    assert!(missing.get("sourcePath").is_none());

    let builtin = change("builtin", json!({ "faviconPath": null, "projectIcon": { "kind": "emoji", "emoji": "🌱" } }));
    assert_eq!(icon_call(&mut ws, 9, "projects.mutate", builtin).await["projectIcon"]["emoji"], "🌱");
    let reset = change("reset", json!({ "faviconPath": null, "projectIcon": null }));
    assert_eq!(icon_call(&mut ws, 10, "projects.mutate", reset).await["projectIcon"], Value::Null);
    assert_eq!(icon_call(&mut other, 3, "assets.createUrl", favicon("")).await["sourcePath"], "public/favicon.svg");

    std::fs::remove_file(workspace.join("public/favicon.svg")).unwrap();
    let fallback = icon_call(&mut other, 4, "assets.createUrl", favicon("")).await;
    assert!(fallback.get("sourcePath").is_none());
}

async fn icon_call(ws: &mut Ws, id: u64, method: &str, payload: Value) -> Value {
    let result = call(ws, id, method, &payload).await.unwrap();
    assert_eq!(result["exit"]["_tag"], "Success", "{result}");
    result["exit"]["value"].clone()
}

/// `projects.fetchLfsObject` downloads a Git LFS object (with git-lfs, over
/// a `file://` remote here) into the cache, and the next read shows it.
#[tokio::test(flavor = "multi_thread")]
async fn lfs_objects_are_fetched_on_request() {
    use sha2::{Digest, Sha256};
    let Some(lfs_dir) = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()).find(|d| d.join("git-lfs").is_file()) else {
        eprintln!("skipped: git-lfs is not on PATH");
        return;
    };
    let dir = std::env::temp_dir().join(format!("domovoi-lfs-fetch-{}", uuid::Uuid::new_v4()));
    let (remote, source, clone) = (dir.join("remote.git"), dir.join("source"), dir.join("clone"));
    for d in [&remote, &source] {
        std::fs::create_dir_all(d).unwrap();
    }
    let path = format!("{}:/usr/bin:/bin:/usr/sbin:/sbin", lfs_dir.display());
    let lfs_git = |cwd: &Path, args: &[&str]| {
        let out = Command::new("git").args(args).current_dir(cwd).env("GIT_CONFIG_GLOBAL", "/dev/null").env("GIT_CONFIG_NOSYSTEM", "1").env("PATH", &path).output().unwrap();
        assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    };
    lfs_git(&remote, &["init", "-q", "--bare", "-b", "main"]);
    lfs_git(&source, &["init", "-q", "-b", "main"]);
    // The pointer and its object written by hand: no LFS filter is configured.
    let object = b"fetched from the LFS remote\n";
    let oid = hex::encode(Sha256::digest(object));
    let cache = source.join(".git/lfs/objects").join(&oid[..2]).join(&oid[2..4]);
    std::fs::create_dir_all(&cache).unwrap();
    std::fs::write(cache.join(&oid), object).unwrap();
    let pointer = format!("version https://git-lfs.github.com/spec/v1\noid sha256:{oid}\nsize {}\n", object.len());
    std::fs::write(source.join("notes.txt"), &pointer).unwrap();
    std::fs::write(source.join(".gitattributes"), "*.txt filter=lfs diff=lfs merge=lfs -text\n").unwrap();
    std::fs::write(source.join("plain.md"), "plain\n").unwrap();
    lfs_git(&source, &["add", "."]);
    lfs_git(&source, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-qm", "Start"]);
    let url = format!("file://{}", remote.display());
    lfs_git(&source, &["remote", "add", "origin", &url]);
    lfs_git(&source, &["lfs", "push", "--object-id", "origin", &oid]);
    lfs_git(&source, &["push", "-q", "origin", "main"]);
    lfs_git(&dir, &["clone", "-q", "-b", "main", &url, clone.to_str().unwrap()]);

    let server = start_env(&dir, Transport::Tcp, &[("PATH".into(), path.clone())]);
    let mut ws = connect(&server).await;
    let mut id = 0;
    let read = read_file_exit(&mut ws, &mut id, &clone, "notes.txt", true).await;
    assert_eq!(read["value"]["lfs"]["state"], "missing", "{read}");
    assert_eq!(read["value"]["contents"], pointer);

    let fetch = |path: &str| json!({ "cwd": clone, "relativePath": path });
    let fetched = call(&mut ws, 100, "projects.fetchLfsObject", &fetch("notes.txt")).await.unwrap()["exit"].clone();
    assert_eq!(fetched["_tag"], "Success", "{fetched}");
    assert_eq!(fetched["value"]["state"], "cached");
    let read = read_file_exit(&mut ws, &mut id, &clone, "notes.txt", true).await;
    assert_eq!(read["value"]["lfs"]["state"], "cached");
    assert_eq!(read["value"]["contents"], "fetched from the LFS remote\n");
    assert_eq!(std::fs::read_to_string(clone.join("notes.txt")).unwrap(), pointer, "the working tree is left as it is");

    let refused = call(&mut ws, 101, "projects.fetchLfsObject", &fetch("plain.md")).await.unwrap()["exit"].clone();
    assert_eq!(refused["_tag"], "Failure", "{refused}");
    assert!(refused["cause"][0]["error"]["message"].as_str().unwrap().contains("isn't kept in Git LFS"), "{refused}");
    let outside = call(&mut ws, 102, "projects.fetchLfsObject", &fetch("../source/notes.txt")).await.unwrap()["exit"].clone();
    assert!(outside["cause"][0]["error"]["message"].as_str().unwrap().contains("outside"), "{outside}");
}

async fn read_file_exit(ws: &mut Ws, id: &mut u64, cwd: &Path, path: &str, preview: bool) -> Value {
    let payload = json!({ "cwd": cwd, "relativePath": path, "previewImages": preview });
    *id += 1;
    call(ws, *id, "projects.readFile", &payload).await.unwrap()["exit"].clone()
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
    let mut files: Vec<PathBuf> = std::fs::read_dir(workspace_root().join("apps/server/tests/cases/calls"))
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
        let Some(reply) = call(&mut ws, id, tag, &payload).await else {
            problems.push(format!("{tag}: no reply"));
            continue;
        };
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
    eprintln!("{} calls answered as expected", cases.len());
}

fn truncate(s: &str) -> String {
    if s.len() > 400 { format!("{}…", &s[..s.char_indices().nth(400).map(|(i, _)| i).unwrap_or(s.len())]) } else { s.to_owned() }
}

fn dirs_home() -> String {
    std::env::var("HOME").unwrap_or_else(|_| "/".into())
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
    assert!(folder.replace('\\', "/").contains("/scratch/") && folder.contains("plan-a-weekend-trip-to"), "thread folder: {folder}");
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
    // A shell that works out 42 itself, so its echo of the typed text can't pass.
    let (shell, line) = if cfg!(windows) {
        (vec![("DOMOVOI_SHELL".to_owned(), "cmd.exe".to_owned())], r#"cmd /v:on /c "set /a x=40+2 >nul & echo ordered-!x!""#)
    } else {
        (vec![], "echo ordered-$((40+2))")
    };
    let server = start_env(&dir, Transport::Tcp, &shell);
    let mut ws = connect(&server).await;
    let cwd = dir.join("w").canonicalize().unwrap();
    let attach = json!({ "_tag": "Request", "id": "1", "tag": "terminal.attach", "headers": [],
        "payload": { "threadId": "t", "terminalId": "term-1", "cwd": cwd, "cols": 80, "rows": 24 } });
    ws.send(Message::Text(attach.to_string().into())).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    for (id, data) in [("2", line), ("3", "\r")] {
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

/// What a file of `apps/server/tests/cases/groups` asks for besides its calls.
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
    /// Why the case is there.
    note: Option<String>,
}

/// Sends a request and reads its stream to the end: every value, then the exit frame.
async fn call_all(ws: &mut Ws, id: u64, tag: &str, payload: &Value) -> Option<(Vec<Value>, Value)> {
    let req = json!({ "_tag": "Request", "id": id.to_string(), "tag": tag, "payload": payload, "headers": [] });
    ws.send(Message::Text(req.to_string().into())).await.unwrap();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut values = vec![];
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
        if v["_tag"] == "Chunk" {
            values.extend(v["values"].as_array().cloned().unwrap_or_default());
            let ack = json!({ "_tag": "Ack", "requestId": id.to_string() });
            ws.send(Message::Text(ack.to_string().into())).await.unwrap();
            continue;
        }
        return Some((values, v));
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

/// Group by group (`apps/server/tests/cases/groups/<name>.json`, with `DOMOVOI_GROUPS=a,b`
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
    let folder = workspace_root().join("apps/server/tests/cases/groups");
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
        ("PATH".to_owned(), system_path(Some(&dir.join("bin"))).to_string_lossy().into_owned()),
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
        let (reply, mut answer, value) = if case.all {
            let Some((values, exit)) = call_all(&mut ws, id, tag, &payload).await else {
                problems.push(format!("{tag}: no end of the stream"));
                continue;
            };
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
    eprintln!("group {name}: {} answers ({failures} failures) of {} methods", answers.len(), methods.len());
    (answers.len(), failures, methods.len())
}
