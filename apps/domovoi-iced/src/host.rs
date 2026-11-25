//! Direct Rust Crux host driving `domovoi_core::DomovoiApp` on Tokio.
//!
//! Executes all six `domovoi_core::Effect` capabilities (`Render`, `Http`,
//! `Socket`, `Tunnel`, `Store`, `Notify`), coalesces burst renders into
//! `ViewDelta` patches, and provides an authenticated async image loader.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use crux_core::{Core, Request};
use crux_http::HttpError;
use crux_http::protocol::{HttpHeader, HttpRequest, HttpResponse, HttpResult};
use domovoi_core::delta::ViewDelta;
use domovoi_core::notify::{NotifyOperation, NotifyOutput};
use domovoi_core::socket::{SocketEvent, SocketOperation};
use domovoi_core::store::{StoreOperation, StoreOutput};
use domovoi_core::tunnel::{TunnelOperation, TunnelOutput};
use domovoi_core::view::ViewModel;
use domovoi_core::{DomovoiApp, Effect, Event};
use domovoi_remote::{Bridge, Network, RelayUrl};
use futures::{SinkExt, StreamExt};
use iced::Subscription;
use tokio::sync::{Mutex, Notify, mpsc};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::{Bytes, Message as WsMessage};

use crate::store::SecureStore;

const RENDER_COALESCE_MS: u64 = 16;
const HTTP_TIMEOUT: Duration = Duration::from_secs(30);
const WS_CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const WS_PING_INTERVAL: Duration = Duration::from_secs(20);
const WS_PONG_TIMEOUT: Duration = Duration::from_secs(10);
const WS_MAX_MESSAGE_BYTES: usize = 64 * 1024 * 1024;
const TUNNEL_POLL_INTERVAL: Duration = Duration::from_secs(3);

#[derive(Clone)]
pub enum HostMsg {
    Ready(HostHandle),
    Delta(Box<ViewDelta>),
    ImageLoaded { url: String, handle: iced::widget::image::Handle },
    /// The image couldn't be fetched; asking for it again tries again.
    ImageFailed { url: String },
    #[cfg_attr(target_os = "macos", allow(dead_code, reason = "clicks are only reported by freedesktop and Windows notifications"))]
    OpenThreadFromNotification { environment_id: String, thread_id: String },
    /// Allow or Decline on an approval's notification.
    #[cfg_attr(target_os = "macos", allow(dead_code, reason = "buttons are only reported by freedesktop and Windows notifications"))]
    RespondFromNotification { environment_id: String, thread_id: String, request_id: String, decision: String },
}

impl std::fmt::Debug for HostMsg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HostMsg::Ready(_) => f.write_str("Ready"),
            HostMsg::Delta(_) => f.write_str("Delta(..)"),
            HostMsg::ImageLoaded { url, .. } => write!(f, "ImageLoaded({url})"),
            HostMsg::ImageFailed { url } => write!(f, "ImageFailed({url})"),
            HostMsg::OpenThreadFromNotification { thread_id, .. } => write!(f, "OpenThreadFromNotification({thread_id})"),
            HostMsg::RespondFromNotification { request_id, decision, .. } => write!(f, "RespondFromNotification({request_id}, {decision})"),
        }
    }
}

#[derive(Clone)]
pub struct HostHandle {
    tx: mpsc::Sender<HostCmd>,
}

impl HostHandle {
    pub fn send(&self, event: Event) {
        let _ = self.tx.try_send(HostCmd::Event(Box::new(event)));
    }

    pub fn fetch_image(&self, url: String) {
        let _ = self.tx.try_send(HostCmd::FetchImage { url });
    }

    pub fn set_focus_context(&self, focused: bool, thread_id: Option<String>) {
        let _ = self.tx.try_send(HostCmd::FocusContext { focused, thread_id });
    }

    pub fn shutdown(&self) {
        let _ = self.tx.try_send(HostCmd::Shutdown);
    }
}

enum HostCmd {
    Event(Box<Event>),
    FetchImage { url: String },
    FocusContext { focused: bool, thread_id: Option<String> },
    Shutdown,
}

struct ActiveSocket {
    outbound: mpsc::Sender<String>,
    task: JoinHandle<()>,
}

struct ActiveTunnel {
    bridge: Arc<Bridge>,
    poll_task: JoinHandle<()>,
}

struct FocusContext {
    focused: bool,
    thread_id: Option<String>,
}

struct Engine {
    core: Core<DomovoiApp>,
    store: Arc<SecureStore>,
    http: reqwest::Client,
    ui_tx: mpsc::Sender<HostMsg>,
    render_dirty: AtomicBool,
    render_notify: Notify,
    socket: Mutex<Option<ActiveSocket>>,
    socket_gen: AtomicU64,
    tunnel: Mutex<Option<ActiveTunnel>>,
    tunnel_gen: AtomicU64,
    focus: Mutex<FocusContext>,
    /// Images being fetched now. Only in-flight ones: the UI keeps what has
    /// loaded (and what failed), so a URL it drops can be fetched again.
    fetching_images: Arc<Mutex<HashSet<String>>>,
    trace_ws: Option<String>,
}

impl Engine {
    fn new(store: Arc<SecureStore>, ui_tx: mpsc::Sender<HostMsg>) -> Arc<Self> {
        let http = reqwest::Client::builder()
            .timeout(HTTP_TIMEOUT)
            .build()
            .expect("build reqwest client");
        Arc::new(Self {
            core: Core::new(),
            store,
            http,
            ui_tx,
            render_dirty: AtomicBool::new(false),
            render_notify: Notify::new(),
            socket: Mutex::new(None),
            socket_gen: AtomicU64::new(0),
            tunnel: Mutex::new(None),
            tunnel_gen: AtomicU64::new(0),
            focus: Mutex::new(FocusContext { focused: true, thread_id: None }),
            fetching_images: Arc::new(Mutex::new(HashSet::new())),
            trace_ws: std::env::var("DOMOVOI_TRACE_WS").ok().filter(|s| !s.is_empty()),
        })
    }

    fn process_event(self: &Arc<Self>, event: Event) {
        let effects = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.core.process_event(event))) {
            Ok(effects) => effects,
            Err(_) => {
                tracing::error!("domovoi-core panicked while processing event");
                return;
            }
        };
        self.dispatch(effects);
    }

    fn resolve<Op: crux_core::capability::Operation>(self: &Arc<Self>, request: &mut Request<Op>, output: Op::Output) {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| self.core.resolve(request, output))) {
            Ok(Ok(effects)) => self.dispatch(effects),
            Ok(Err(err)) => tracing::debug!(?err, "stale effect resolution"),
            Err(_) => tracing::error!("domovoi-core panicked while resolving effect"),
        }
    }

    fn mark_render(&self) {
        if !self.render_dirty.swap(true, Ordering::AcqRel) {
            self.render_notify.notify_one();
        }
    }

    fn dispatch(self: &Arc<Self>, effects: Vec<Effect>) {
        for effect in effects {
            match effect {
                Effect::Render(_) => self.mark_render(),
                Effect::Store(mut req) => {
                    let op = req.operation.clone();
                    let is_cache = match &op {
                        StoreOperation::Load { key } | StoreOperation::Save { key, .. } | StoreOperation::Delete { key } => {
                            key.starts_with("cache:")
                        }
                    };
                    if is_cache {
                        let engine = Arc::clone(self);
                        tokio::spawn(async move {
                            let store = Arc::clone(&engine.store);
                            let value = tokio::task::spawn_blocking(move || run_store_op(&store, &op)).await.unwrap_or(None);
                            engine.resolve(&mut req, StoreOutput::Value { value });
                        });
                    } else {
                        let value = run_store_op(&self.store, &op);
                        self.resolve(&mut req, StoreOutput::Value { value });
                    }
                }
                Effect::Http(mut req) => {
                    let engine = Arc::clone(self);
                    tokio::spawn(async move {
                        let result = perform_http(&engine.http, &req.operation).await;
                        engine.resolve(&mut req, result);
                    });
                }
                Effect::Socket(req) => {
                    let engine = Arc::clone(self);
                    tokio::spawn(async move {
                        engine.handle_socket(req).await;
                    });
                }
                Effect::Tunnel(req) => {
                    let engine = Arc::clone(self);
                    tokio::spawn(async move {
                        engine.handle_tunnel(req).await;
                    });
                }
                Effect::Notify(mut req) => {
                    let engine = Arc::clone(self);
                    tokio::spawn(async move {
                        engine.handle_notify(&req.operation).await;
                        engine.resolve(&mut req, NotifyOutput::Posted);
                    });
                }
            }
        }
    }

    async fn handle_socket(self: &Arc<Self>, mut req: Request<SocketOperation>) {
        match req.operation.clone() {
            SocketOperation::Open { url } => {
                let gen_id = self.socket_gen.fetch_add(1, Ordering::AcqRel) + 1;
                self.close_socket().await;
                let (outbound_tx, mut outbound_rx) = mpsc::channel::<String>(256);
                let engine = Arc::clone(self);
                let trace = self.trace_ws.clone();
                let task = tokio::spawn(async move {
                    let mut cfg = WebSocketConfig::default();
                    cfg.max_message_size = Some(WS_MAX_MESSAGE_BYTES);
                    cfg.max_frame_size = Some(WS_MAX_MESSAGE_BYTES);
                    let connect = tokio::time::timeout(
                        WS_CONNECT_TIMEOUT,
                        tokio_tungstenite::connect_async_with_config(&url, Some(cfg), false),
                    )
                    .await;
                    let ws_stream = match connect {
                        Ok(Ok((stream, _))) => stream,
                        Ok(Err(err)) => {
                            if engine.socket_gen.load(Ordering::Acquire) == gen_id {
                                engine.resolve(&mut req, SocketEvent::Closed { reason: format!("Connection failed: {err}") });
                            }
                            return;
                        }
                        Err(_) => {
                            if engine.socket_gen.load(Ordering::Acquire) == gen_id {
                                engine.resolve(&mut req, SocketEvent::Closed { reason: "The server did not answer.".into() });
                            }
                            return;
                        }
                    };
                    if engine.socket_gen.load(Ordering::Acquire) != gen_id {
                        return;
                    }
                    engine.resolve(&mut req, SocketEvent::Opened);
                    let (mut writer, mut reader) = ws_stream.split();
                    let mut ping_interval = tokio::time::interval(WS_PING_INTERVAL);
                    ping_interval.tick().await;
                    let mut waiting_pong_since: Option<tokio::time::Instant> = None;

                    let close_reason = loop {
                        tokio::select! {
                            biased;
                            maybe_out = outbound_rx.recv() => {
                                match maybe_out {
                                    Some(text) => {
                                        // An answer to a secret request carries the value: never traced.
                                        if let Some(filter) = trace.as_deref()
                                            && text.contains(filter) && !text.contains("\"secrets.answerRequest\"") {
                                                tracing::info!(frame = %text.chars().take(300).collect::<String>(), "ws >>");
                                            }
                                        if let Err(err) = writer.send(WsMessage::Text(text.into())).await {
                                            break format!("Send failed: {err}");
                                        }
                                    }
                                    None => {
                                        let _ = writer.send(WsMessage::Close(None)).await;
                                        break "Closed".into();
                                    }
                                }
                            }
                            _ = ping_interval.tick() => {
                                if let Some(since) = waiting_pong_since
                                    && since.elapsed() >= WS_PONG_TIMEOUT {
                                        break "The connection stopped answering.".into();
                                    }
                                waiting_pong_since = Some(tokio::time::Instant::now());
                                if let Err(err) = writer.send(WsMessage::Ping(Bytes::new())).await {
                                    break format!("The connection dropped: {err}");
                                }
                            }
                            maybe_in = reader.next() => {
                                match maybe_in {
                                    Some(Ok(WsMessage::Text(text))) => {
                                        waiting_pong_since = None;
                                        let text_str = text.to_string();
                                        if let Some(filter) = trace.as_deref()
                                            && text_str.contains(filter) {
                                                tracing::info!(frame = %text_str.chars().take(600).collect::<String>(), "ws <<");
                                            }
                                        engine.resolve(&mut req, SocketEvent::Message { text: text_str });
                                    }
                                    Some(Ok(WsMessage::Binary(bin))) => {
                                        waiting_pong_since = None;
                                        let text_str = String::from_utf8_lossy(&bin).into_owned();
                                        engine.resolve(&mut req, SocketEvent::Message { text: text_str });
                                    }
                                    Some(Ok(WsMessage::Ping(payload))) => {
                                        waiting_pong_since = None;
                                        let _ = writer.send(WsMessage::Pong(payload)).await;
                                    }
                                    Some(Ok(WsMessage::Pong(_))) => {
                                        waiting_pong_since = None;
                                    }
                                    Some(Ok(WsMessage::Close(frame))) => {
                                        let reason = frame
                                            .map(|f| f.reason.to_string())
                                            .filter(|s| !s.is_empty())
                                            .unwrap_or_else(|| "Connection closed".into());
                                        break reason;
                                    }
                                    Some(Ok(WsMessage::Frame(_))) => {}
                                    Some(Err(err)) => {
                                        break format!("Connection error: {err}");
                                    }
                                    None => {
                                        break "Connection closed".into();
                                    }
                                }
                            }
                        }
                    };
                    if engine.socket_gen.load(Ordering::Acquire) == gen_id {
                        engine.resolve(&mut req, SocketEvent::Closed { reason: close_reason });
                    }
                });
                *self.socket.lock().await = Some(ActiveSocket { outbound: outbound_tx, task });
            }
            SocketOperation::Send { text } => {
                let outbound = self.socket.lock().await.as_ref().map(|s| s.outbound.clone());
                if let Some(tx) = outbound {
                    let _ = tx.send(text).await;
                }
            }
            SocketOperation::Close => {
                self.socket_gen.fetch_add(1, Ordering::AcqRel);
                self.close_socket().await;
            }
        }
    }

    async fn close_socket(&self) {
        let prev = self.socket.lock().await.take();
        if let Some(sock) = prev {
            drop(sock.outbound);
            sock.task.abort();
        }
    }

    async fn handle_tunnel(self: &Arc<Self>, mut req: Request<TunnelOperation>) {
        match req.operation.clone() {
            TunnelOperation::Start { ticket } => {
                let gen_id = self.tunnel_gen.fetch_add(1, Ordering::AcqRel) + 1;
                self.stop_tunnel().await;
                let key = match self.store.device_key() {
                    Ok(k) => k,
                    Err(reason) => {
                        self.resolve(&mut req, TunnelOutput::Failed { reason });
                        return;
                    }
                };
                let parsed_ticket = match domovoi_remote::parse_ticket(&ticket) {
                    Ok(t) => t,
                    Err(err) => {
                        self.resolve(&mut req, TunnelOutput::Failed { reason: err.to_string() });
                        return;
                    }
                };
                let relays = parse_relays_env();
                let bridge = match Bridge::start(key, Network::Internet { relays }, parsed_ticket, 0).await {
                    Ok(b) => Arc::new(b),
                    Err(err) => {
                        if self.tunnel_gen.load(Ordering::Acquire) == gen_id {
                            self.resolve(&mut req, TunnelOutput::Failed { reason: err.to_string() });
                        }
                        return;
                    }
                };
                if self.tunnel_gen.load(Ordering::Acquire) != gen_id {
                    if let Ok(b) = Arc::try_unwrap(bridge) {
                        b.stop().await;
                    }
                    return;
                }
                let port = bridge.port();
                self.resolve(&mut req, TunnelOutput::Started { port });
                let poll_bridge = Arc::clone(&bridge);
                let engine = Arc::clone(self);
                let poll_task = tokio::spawn(async move {
                    let mut interval = tokio::time::interval(TUNNEL_POLL_INTERVAL);
                    loop {
                        interval.tick().await;
                        if engine.tunnel_gen.load(Ordering::Acquire) != gen_id {
                            break;
                        }
                        let st = poll_bridge.status();
                        let path = st.path.map(|p| match p { domovoi_remote::PathKind::Direct => "direct".to_owned(), domovoi_remote::PathKind::Relay => "relay".to_owned() }).unwrap_or_default();
                        let rtt_ms = st.rtt_ms.unwrap_or(0).min(60_000) as u32;
                        engine.resolve(
                            &mut req,
                            TunnelOutput::Status {
                                connected: st.connected,
                                path,
                                rtt_ms,
                                error: st.error.unwrap_or_default(),
                                listening: st.listening,
                            },
                        );
                    }
                });
                *self.tunnel.lock().await = Some(ActiveTunnel { bridge, poll_task });
            }
            TunnelOperation::NetworkChanged => {
                let bridge = self.tunnel.lock().await.as_ref().map(|t| Arc::clone(&t.bridge));
                if let Some(b) = bridge {
                    b.network_change().await;
                }
            }
            TunnelOperation::Stop => {
                self.tunnel_gen.fetch_add(1, Ordering::AcqRel);
                self.stop_tunnel().await;
            }
        }
    }

    async fn stop_tunnel(&self) {
        let prev = self.tunnel.lock().await.take();
        if let Some(active) = prev {
            active.poll_task.abort();
            let _ = active.poll_task.await;
            if let Ok(bridge) = Arc::try_unwrap(active.bridge) {
                bridge.stop().await;
            }
        }
    }

    async fn handle_notify(&self, note: &NotifyOperation) {
        if cfg!(test) {
            return;
        }
        let suppress = {
            let ctx = self.focus.lock().await;
            ctx.focused && ctx.thread_id.as_deref() == Some(note.thread_id.as_str())
        };
        if suppress {
            return;
        }
        let title = note.title.clone();
        let body = note.body.clone();
        // freedesktop and Windows notifications report clicks and buttons;
        // wait for one off-thread (detached: the handle blocks until the
        // notification closes). An approval's buttons answer it from there.
        #[cfg(any(target_os = "windows", all(unix, not(target_os = "macos"))))]
        {
            use notify_rust::NotificationResponse;
            let ui_tx = self.ui_tx.clone();
            let (environment_id, thread_id, request_id) = (note.environment_id.clone(), note.thread_id.clone(), note.request_id.clone());
            drop(tokio::task::spawn_blocking(move || {
                let mut notification = notify_rust::Notification::new();
                notification.appname("Domovoi").summary(&title).body(&body);
                // Windows reports a click on the body by itself; a "default" action there would be a button.
                #[cfg(not(target_os = "windows"))]
                notification.action("default", "Open");
                if request_id.is_some() {
                    notification.action("accept", "Allow").action("decline", "Decline");
                }
                let Ok(handle) = notification.show() else {
                    return;
                };
                let _ = handle.wait_for_response(move |response: &NotificationResponse| {
                    let msg = match response {
                        NotificationResponse::Default => Some(HostMsg::OpenThreadFromNotification { environment_id, thread_id }),
                        NotificationResponse::Action(action) if action == "default" => Some(HostMsg::OpenThreadFromNotification { environment_id, thread_id }),
                        NotificationResponse::Action(action) if action == "accept" || action == "decline" => {
                            request_id.map(|request_id| HostMsg::RespondFromNotification { environment_id, thread_id, request_id, decision: action.clone() })
                        }
                        _ => None,
                    };
                    if let Some(msg) = msg {
                        let _ = ui_tx.blocking_send(msg);
                    }
                });
            }));
        }
        // macOS: `NSUserNotification` waits on the main run loop, which Iced owns.
        #[cfg(target_os = "macos")]
        {
            let _ = tokio::task::spawn_blocking(move || {
                let _ = notify_rust::Notification::new().appname("Domovoi").summary(&title).body(&body).show();
            })
            .await;
        }
    }

    async fn fetch_image(self: &Arc<Self>, url: String) {
        if url.is_empty() || !self.fetching_images.lock().await.insert(url.clone()) {
            return;
        }
        let client = self.http.clone();
        let ui_tx = self.ui_tx.clone();
        let fetching = Arc::clone(&self.fetching_images);
        tokio::spawn(async move {
            let loaded = async {
                let resp = client.get(&url).send().await.ok()?;
                if !resp.status().is_success() {
                    return None;
                }
                let bytes = resp.bytes().await.ok()?;
                (!bytes.is_empty()).then_some(bytes)
            }
            .await;
            fetching.lock().await.remove(&url);
            let msg = match loaded {
                Some(bytes) => HostMsg::ImageLoaded { url, handle: iced::widget::image::Handle::from_bytes(bytes.to_vec()) },
                None => HostMsg::ImageFailed { url },
            };
            let _ = ui_tx.send(msg).await;
        });
    }
}

fn run_store_op(store: &SecureStore, op: &StoreOperation) -> Option<String> {
    match op {
        StoreOperation::Load { key } => store.load(key),
        StoreOperation::Save { key, value } => {
            let _ = store.save(key, value);
            None
        }
        StoreOperation::Delete { key } => {
            let _ = store.delete(key);
            None
        }
    }
}

async fn perform_http(client: &reqwest::Client, req: &HttpRequest) -> HttpResult {
    let method = match reqwest::Method::from_bytes(req.method.as_bytes()) {
        Ok(m) => m,
        Err(e) => return HttpResult::Err(HttpError::Url(e.to_string())),
    };
    let mut builder = client.request(method, &req.url);
    let mut has_content_type = false;
    for h in &req.headers {
        if h.name.eq_ignore_ascii_case("content-type") {
            has_content_type = true;
        }
        builder = builder.header(&h.name, &h.value);
    }
    if !req.body.is_empty() {
        if !has_content_type {
            builder = builder.header("Content-Type", "application/json");
        }
        builder = builder.body(req.body.clone());
    }
    match builder.send().await {
        Ok(resp) => {
            let status = resp.status().as_u16();
            let headers = resp
                .headers()
                .iter()
                .filter_map(|(k, v)| v.to_str().ok().map(|s| HttpHeader { name: k.as_str().to_owned(), value: s.to_owned() }))
                .collect();
            match resp.bytes().await {
                Ok(body) => HttpResult::Ok(HttpResponse { status, headers, body: body.to_vec() }),
                Err(e) => HttpResult::Err(HttpError::Io(e.to_string())),
            }
        }
        Err(e) => HttpResult::Err(HttpError::Io(e.to_string())),
    }
}

fn parse_relays_env() -> Vec<RelayUrl> {
    std::env::var("DOMOVOI_RELAYS")
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .filter_map(|s| s.parse::<RelayUrl>().ok())
        .collect()
}

pub fn device_label() -> String {
    hostname::get()
        .ok()
        .and_then(|s| s.into_string().ok())
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "Desktop".into())
}

pub fn device_os() -> &'static str {
    if cfg!(target_os = "macos") {
        "macOS"
    } else if cfg!(target_os = "windows") {
        "Windows"
    } else {
        "Linux"
    }
}

pub fn local_timezone() -> (String, i32) {
    let iana = iana_time_zone::get_timezone().unwrap_or_else(|_| "UTC".into());
    // Compute approximate UTC offset only if available via libc/env; default 0 when unknown.
    (iana, 0)
}

fn worker() -> impl iced::futures::Stream<Item = HostMsg> {
    iced::stream::channel(128, async |mut out| {
        let store = Arc::new(SecureStore::from_env());
        let (ui_tx, mut ui_rx) = mpsc::channel::<HostMsg>(128);
        let (cmd_tx, mut cmd_rx) = mpsc::channel::<HostCmd>(512);
        let engine = Engine::new(store, ui_tx.clone());

        // Forward engine -> Iced subscription channel.
        let handle = HostHandle { tx: cmd_tx };
        if out.send(HostMsg::Ready(handle)).await.is_err() {
            return;
        }

        // Send initial full snapshot before any event.
        let initial_vm = engine.core.view();
        let initial_delta = ViewDelta::between(None, &initial_vm);
        let _ = out.send(HostMsg::Delta(Box::new(initial_delta))).await;

        // Spawn render coalescing loop.
        let render_engine = Arc::clone(&engine);
        let render_tx = ui_tx.clone();
        let render_task = tokio::spawn(async move {
            let mut last_view: Option<ViewModel> = Some(initial_vm);
            loop {
                render_engine.render_notify.notified().await;
                tokio::time::sleep(Duration::from_millis(RENDER_COALESCE_MS)).await;
                if render_engine.render_dirty.swap(false, Ordering::AcqRel) {
                    let next = render_engine.core.view();
                    let delta = ViewDelta::between(last_view.as_ref(), &next);
                    last_view = Some(next);
                    if render_tx.send(HostMsg::Delta(Box::new(delta))).await.is_err() {
                        break;
                    }
                }
            }
        });

        // Command loop + UI message pump.
        let cmd_engine = Arc::clone(&engine);
        let cmd_task = tokio::spawn(async move {
            while let Some(cmd) = cmd_rx.recv().await {
                match cmd {
                    HostCmd::Event(ev) => cmd_engine.process_event(*ev),
                    HostCmd::FetchImage { url } => cmd_engine.fetch_image(url).await,
                    HostCmd::FocusContext { focused, thread_id } => {
                        let mut ctx = cmd_engine.focus.lock().await;
                        ctx.focused = focused;
                        ctx.thread_id = thread_id;
                    }
                    HostCmd::Shutdown => {
                        cmd_engine.close_socket().await;
                        cmd_engine.stop_tunnel().await;
                        break;
                    }
                }
            }
        });

        while let Some(msg) = ui_rx.recv().await {
            if out.send(msg).await.is_err() {
                break;
            }
        }
        render_task.abort();
        cmd_task.abort();
    })
}

pub fn subscription() -> Subscription<HostMsg> {
    Subscription::run(worker)
}

#[cfg(test)]
pub fn start_test_host(store: Arc<SecureStore>) -> (HostHandle, mpsc::Receiver<HostMsg>) {
    let (ui_tx, ui_rx) = mpsc::channel::<HostMsg>(128);
    let (cmd_tx, mut cmd_rx) = mpsc::channel::<HostCmd>(256);
    let engine = Engine::new(store, ui_tx.clone());
    let initial_vm = engine.core.view();
    let initial_delta = ViewDelta::between(None, &initial_vm);
    let _ = ui_tx.try_send(HostMsg::Delta(Box::new(initial_delta)));

    let render_engine = Arc::clone(&engine);
    let render_tx = ui_tx.clone();
    tokio::spawn(async move {
        let mut last_view: Option<ViewModel> = Some(initial_vm);
        loop {
            render_engine.render_notify.notified().await;
            tokio::time::sleep(Duration::from_millis(5)).await;
            if render_engine.render_dirty.swap(false, Ordering::AcqRel) {
                let next = render_engine.core.view();
                let delta = ViewDelta::between(last_view.as_ref(), &next);
                last_view = Some(next);
                if render_tx.send(HostMsg::Delta(Box::new(delta))).await.is_err() {
                    break;
                }
            }
        }
    });

    tokio::spawn(async move {
        while let Some(cmd) = cmd_rx.recv().await {
            match cmd {
                HostCmd::Event(ev) => engine.process_event(*ev),
                HostCmd::FetchImage { url } => engine.fetch_image(url).await,
                HostCmd::FocusContext { focused, thread_id } => {
                    let mut ctx = engine.focus.lock().await;
                    ctx.focused = focused;
                    ctx.thread_id = thread_id;
                }
                HostCmd::Shutdown => {
                    engine.close_socket().await;
                    engine.stop_tunnel().await;
                    break;
                }
            }
        }
    });

    (HostHandle { tx: cmd_tx }, ui_rx)
}
