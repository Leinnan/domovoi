//! The effects of `domovoi_core::DomovoiApp`, run on Tokio.
//!
//! Runs all six `domovoi_core::Effect` capabilities (`Render`, `Http`,
//! `Socket`, `Tunnel`, `Store`, `Notify`). Bursts of renders are merged into
//! one `ViewDelta`. Also fetches images for the UI.

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
use domovoi_remote::{Bridge, ClientEndpoint, Network, RelayUrl};
use futures::{SinkExt, StreamExt};
use tokio::sync::{Mutex, Notify, mpsc};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::{Bytes, Message as WsMessage};

use crate::Identity;
use crate::store::SecureStore;

const HTTP_TIMEOUT: Duration = Duration::from_secs(30);
const WS_CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const WS_PING_INTERVAL: Duration = Duration::from_secs(20);
const WS_PONG_TIMEOUT: Duration = Duration::from_secs(10);
const WS_MAX_MESSAGE_BYTES: usize = 64 * 1024 * 1024;
const TUNNEL_POLL_INTERVAL: Duration = Duration::from_secs(3);
/// The UI is slower than the core: this many events waiting is a stuck UI, and
/// later deltas are queued behind them, not dropped.
const EVENT_QUEUE: usize = 128;
/// Commands from the UI (each user action is one).
const COMMAND_QUEUE: usize = 512;

/// How a client runs the host.
#[derive(Clone, Debug)]
pub struct HostConfig {
    pub identity: Identity,
    /// How long after a render request the view is read: renders requested
    /// in between become one delta.
    pub render_coalesce: Duration,
    /// Posts the core's notifications to the OS (off in tests).
    pub notifications: bool,
}

impl HostConfig {
    pub fn new(identity: Identity) -> Self {
        Self { identity, render_coalesce: Duration::from_millis(16), notifications: true }
    }

    /// Quick renders and no OS notifications.
    pub fn for_tests(identity: Identity) -> Self {
        Self { identity, render_coalesce: Duration::from_millis(5), notifications: false }
    }
}

#[derive(Clone, Debug)]
pub enum TerminalProbe {
    Current(Vec<domovoi_core::view::TerminalDetailView>),
    Unavailable,
    Unauthorized,
}

/// What the host tells the UI, in order.
#[derive(Clone)]
pub enum HostEvent {
    TerminalDetails { environment_id: String, outcome: TerminalProbe },
    /// A change to the view. The first is the whole view.
    Delta(Box<ViewDelta>),
    /// The bytes of an image the UI asked for with [`HostHandle::fetch_image`].
    ImageLoaded { url: String, bytes: Vec<u8> },
    /// The image couldn't be fetched; asking for it again tries again.
    ImageFailed { url: String },
    /// A click on a notification (freedesktop and Windows report them).
    OpenThreadFromNotification { environment_id: String, thread_id: String },
    /// Allow or Decline on an approval's notification.
    RespondFromNotification { environment_id: String, thread_id: String, request_id: String, decision: String },
}

impl std::fmt::Debug for HostEvent {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HostEvent::Delta(_) => f.write_str("Delta(..)"),
            HostEvent::TerminalDetails { environment_id, .. } => write!(f, "TerminalDetails({environment_id})"),
            HostEvent::ImageLoaded { url, .. } => write!(f, "ImageLoaded({url})"),
            HostEvent::ImageFailed { url } => write!(f, "ImageFailed({url})"),
            HostEvent::OpenThreadFromNotification { thread_id, .. } => write!(f, "OpenThreadFromNotification({thread_id})"),
            HostEvent::RespondFromNotification { request_id, decision, .. } => write!(f, "RespondFromNotification({request_id}, {decision})"),
        }
    }
}

/// The UI's side of a running host. Cheap to clone; the host stops once every
/// handle is gone (or after [`HostHandle::shutdown`]).
#[derive(Clone)]
pub struct HostHandle {
    tx: mpsc::Sender<HostCmd>,
}

impl HostHandle {
    pub fn send(&self, event: Event) {
        let _ = self.tx.try_send(HostCmd::Event(Box::new(event)));
    }

    pub fn probe_terminals(&self, environment_id: String) -> bool {
        self.tx.try_send(HostCmd::ProbeTerminals { environment_id }).is_ok()
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
    ProbeTerminals { environment_id: String },
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
    config: HostConfig,
    http: reqwest::Client,
    ui_tx: mpsc::Sender<HostEvent>,
    render_dirty: AtomicBool,
    render_notify: Notify,
    socket: Mutex<Option<ActiveSocket>>,
    socket_gen: AtomicU64,
    tunnel: Mutex<Option<ActiveTunnel>>,
    client_endpoint: tokio::sync::OnceCell<ClientEndpoint>,
    tunnel_gen: AtomicU64,
    focus: Mutex<FocusContext>,
    /// Images being fetched now. Only in-flight ones: the UI keeps what has
    /// loaded (and what failed), so a URL it drops can be fetched again.
    fetching_images: Arc<Mutex<HashSet<String>>>,
    trace_ws: Option<String>,
}

impl Engine {
    fn new(store: Arc<SecureStore>, config: HostConfig, ui_tx: mpsc::Sender<HostEvent>) -> Arc<Self> {
        let http = reqwest::Client::builder()
            .timeout(HTTP_TIMEOUT)
            .build()
            .expect("build reqwest client");
        Arc::new(Self {
            core: Core::new(),
            store,
            config,
            http,
            ui_tx,
            render_dirty: AtomicBool::new(false),
            render_notify: Notify::new(),
            socket: Mutex::new(None),
            socket_gen: AtomicU64::new(0),
            tunnel: Mutex::new(None),
            client_endpoint: tokio::sync::OnceCell::new(),
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
                let parsed_ticket = match domovoi_remote::parse_ticket(&ticket) {
                    Ok(t) => t,
                    Err(err) => {
                        self.resolve(&mut req, TunnelOutput::Failed { reason: err.to_string() });
                        return;
                    }
                };
                let endpoint = match self.client_endpoint().await {
                    Ok(endpoint) => endpoint,
                    Err(reason) => {
                        self.resolve(&mut req, TunnelOutput::Failed { reason });
                        return;
                    }
                };
                let bridge = match Bridge::start_on(endpoint, parsed_ticket, 0).await {
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
        if !self.config.notifications {
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
            #[cfg(target_os = "windows")]
            let app_id = crate::windows::toast_app_id();
            drop(tokio::task::spawn_blocking(move || {
                let mut notification = notify_rust::Notification::new();
                notification.appname("Domovoi").summary(&title).body(&body);
                // Without an app id of its own a toast is shown as PowerShell's.
                #[cfg(target_os = "windows")]
                if let Some(app_id) = app_id {
                    notification.app_id(app_id);
                }
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
                        NotificationResponse::Default => Some(HostEvent::OpenThreadFromNotification { environment_id, thread_id }),
                        NotificationResponse::Action(action) if action == "default" => Some(HostEvent::OpenThreadFromNotification { environment_id, thread_id }),
                        NotificationResponse::Action(action) if action == "accept" || action == "decline" => {
                            request_id.map(|request_id| HostEvent::RespondFromNotification { environment_id, thread_id, request_id, decision: action.clone() })
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

    async fn client_endpoint(&self) -> Result<&ClientEndpoint, String> {
        self.client_endpoint.get_or_try_init(|| async {
            let key = self.store.device_key()?;
            ClientEndpoint::bind(key, Network::Internet { relays: parse_relays_env() }).await.map_err(|error| error.to_string())
        }).await
    }

    fn probe_terminals(self: &Arc<Self>, environment_id: String) {
        let engine = Arc::clone(self);
        tokio::spawn(async move {
            let outcome = tokio::time::timeout(Duration::from_secs(8), engine.fetch_terminals(&environment_id))
                .await.unwrap_or(TerminalProbe::Unavailable);
            let _ = engine.ui_tx.send(HostEvent::TerminalDetails { environment_id, outcome }).await;
        });
    }

    async fn fetch_terminals(&self, environment_id: &str) -> TerminalProbe {
        let Some(token) = self.store.load(&domovoi_core::store::token_key(environment_id)) else { return TerminalProbe::Unauthorized };
        let Some(environments) = self.store.load(domovoi_core::store::KEY_ENVIRONMENTS)
            .and_then(|json| serde_json::from_str::<Vec<serde_json::Value>>(&json).ok()) else { return TerminalProbe::Unavailable };
        let Some(environment) = environments.iter().find(|value| value["id"].as_str() == Some(environment_id)) else { return TerminalProbe::Unauthorized };
        let _bridge;
        let url = if let Some(ticket) = environment["ticket"].as_str().filter(|ticket| !ticket.is_empty()) {
            let Ok(ticket) = domovoi_remote::parse_ticket(ticket) else { return TerminalProbe::Unavailable };
            let Ok(endpoint) = self.client_endpoint().await else { return TerminalProbe::Unavailable };
            let Ok(remote) = Bridge::start_on(endpoint, ticket, 0).await else { return TerminalProbe::Unavailable };
            let port = remote.port();
            _bridge = remote;
            format!("http://127.0.0.1:{port}")
        } else {
            let Some(url) = environment["url"].as_str().filter(|url| !url.is_empty()) else { return TerminalProbe::Unavailable };
            url.trim_end_matches('/').to_owned()
        };
        let Ok(client) = reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build() else { return TerminalProbe::Unavailable };
        let Ok(mut response) = client.get(format!("{url}/api/domovoi/terminals")).bearer_auth(token).send().await else { return TerminalProbe::Unavailable };
        match response.status().as_u16() {
            200 => {}
            401 | 403 | 404 => return TerminalProbe::Unauthorized,
            _ => return TerminalProbe::Unavailable,
        }
        let mut body = Vec::new();
        loop {
            match response.chunk().await {
                Ok(Some(chunk)) if body.len().saturating_add(chunk.len()) <= 512 * 1024 => body.extend_from_slice(&chunk),
                Ok(None) => break,
                _ => return TerminalProbe::Unavailable,
            }
        }
        serde_json::from_slice(&body).map(TerminalProbe::Current).unwrap_or(TerminalProbe::Unavailable)
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
                Some(bytes) => HostEvent::ImageLoaded { url, bytes: bytes.to_vec() },
                None => HostEvent::ImageFailed { url },
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


/// Starts the core and its effects on the current Tokio runtime. The first
/// event on the channel is the whole view; later ones follow as they happen.
pub fn start(store: Arc<SecureStore>, config: HostConfig) -> (HostHandle, mpsc::Receiver<HostEvent>) {
    let (ui_tx, ui_rx) = mpsc::channel::<HostEvent>(EVENT_QUEUE);
    let (cmd_tx, mut cmd_rx) = mpsc::channel::<HostCmd>(COMMAND_QUEUE);
    let coalesce = config.render_coalesce;
    let engine = Engine::new(store, config, ui_tx.clone());

    let initial_vm = engine.core.view();
    let initial_delta = ViewDelta::between(None, &initial_vm);
    // The channel is new and empty, so this fits.
    let _ = ui_tx.try_send(HostEvent::Delta(Box::new(initial_delta)));

    let render_engine = Arc::clone(&engine);
    let render_task = tokio::spawn(async move {
        let mut last_view: Option<ViewModel> = Some(initial_vm);
        loop {
            render_engine.render_notify.notified().await;
            tokio::time::sleep(coalesce).await;
            if render_engine.render_dirty.swap(false, Ordering::AcqRel) {
                let next = render_engine.core.view();
                let delta = ViewDelta::between(last_view.as_ref(), &next);
                last_view = Some(next);
                if ui_tx.send(HostEvent::Delta(Box::new(delta))).await.is_err() {
                    break;
                }
            }
        }
    });

    tokio::spawn(async move {
        while let Some(cmd) = cmd_rx.recv().await {
            match cmd {
                HostCmd::Event(ev) => engine.process_event(*ev),
                HostCmd::ProbeTerminals { environment_id } => engine.probe_terminals(environment_id),
                HostCmd::FetchImage { url } => engine.fetch_image(url).await,
                HostCmd::FocusContext { focused, thread_id } => {
                    let mut ctx = engine.focus.lock().await;
                    ctx.focused = focused;
                    ctx.thread_id = thread_id;
                }
                HostCmd::Shutdown => break,
            }
        }
        // Shut down, or every handle is gone: nobody can use the host now.
        engine.close_socket().await;
        engine.stop_tunnel().await;
        render_task.abort();
    });

    (HostHandle { tx: cmd_tx }, ui_rx)
}
