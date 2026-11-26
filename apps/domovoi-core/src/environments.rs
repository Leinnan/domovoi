//! Saved environments (servers) and connecting to them.
//!
//! An environment is local (an HTTP URL) or remote (an iroh ticket, reached
//! through the shell's loopback tunnel). Either way the client pairs once
//! with a one-time code through t3code's token exchange (`/oauth/token`),
//! keeps the bearer token in the shell's secure store, and opens each RPC
//! socket with a fresh one-time ticket.
//!
//! Connecting: (tunnel) → pair (only with a code) → mint ticket → socket.

use crate::app::Event;
use crate::model::{Connection, Environment, Model, TunnelState};
use crate::offline::{cache_key, drafts_key, save_cache, save_drafts};
use crate::replies::http::{Answer, HttpResult, answer, is_servers_own, read, rejection_message};
use crate::requests;
use crate::socket::SocketOperation;
use crate::store::{KEY_ACTIVE, KEY_ENVIRONMENTS, StoreOperation, StoreOutput, token_key};
use crate::tunnel::{TunnelOperation, TunnelOutput};
use crate::update::{Cmd, open_socket};
use crux_core::{Command, render::render};
use crux_http::command::Http;
use domovoi_model::typed::{AuthAccessTokenResult, AuthWebSocketTicketResult, RemoteAccessStatus, RemotePairingLink};

/// A pairing link: `domovoi://connect?ticket=…&code=…&name=…`, or a bare ticket.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub ticket: String,
    pub code: Option<String>,
    pub name: Option<String>,
}

pub fn parse_link(text: &str) -> Option<Link> {
    let text = text.trim();
    let Some(query) = text.strip_prefix("domovoi://connect?") else {
        // A ticket is plain ASCII (base32); anything else is not one.
        let ok = !text.is_empty() && text.is_ascii() && !text.contains(char::is_whitespace) && !text.contains("://");
        return ok.then(|| Link { ticket: text.to_owned(), code: None, name: None });
    };
    let mut link = Link { ticket: String::new(), code: None, name: None };
    for pair in query.split('&') {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        let v = form_decode(v);
        match k {
            "ticket" => link.ticket = v,
            "code" if !v.is_empty() => link.code = Some(v),
            "name" if !v.is_empty() => link.name = Some(v),
            _ => {}
        }
    }
    (!link.ticket.is_empty() && link.ticket.is_ascii()).then_some(link)
}

/// The first `n` characters of `s` (a label never cuts a character in two).
pub fn prefix(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// The server's endpoint id inside a ticket, so a newer ticket for the same
/// server updates its saved environment instead of adding another.
pub fn endpoint_key(ticket: &str) -> String {
    #[cfg(feature = "remote")]
    if let Ok(t) = domovoi_remote::parse_ticket(ticket) {
        return t.endpoint_addr().id.to_string();
    }
    ticket.to_owned()
}

fn form_encode(pairs: &[(&str, &str)]) -> String {
    let enc = |s: &str| -> String {
        s.bytes()
            .map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => (b as char).to_string(),
                b' ' => "+".into(),
                _ => format!("%{b:02X}"),
            })
            .collect()
    };
    pairs.iter().map(|(k, v)| format!("{}={}", enc(k), enc(v))).collect::<Vec<_>>().join("&")
}

fn form_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    let hex = |b: u8| (b as char).to_digit(16).map(|d| d as u8);
    while i < bytes.len() {
        match bytes[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < bytes.len() => match (hex(bytes[i + 1]), hex(bytes[i + 2])) {
                (Some(hi), Some(lo)) => {
                    out.push(hi << 4 | lo);
                    i += 2;
                }
                _ => out.push(b'%'),
            },
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

pub(crate) fn value_of(out: StoreOutput) -> Option<String> {
    match out {
        StoreOutput::Value { value } => value,
    }
}

pub(crate) fn store(op: StoreOperation) -> Cmd {
    Command::request_from_shell(op).then_send(|_| Event::Stored)
}

pub(crate) fn save_environments(model: &Model) -> Cmd {
    store(StoreOperation::Save { key: KEY_ENVIRONMENTS.into(), value: serde_json::to_string(&model.environments).expect("plain data serializes") })
}

fn save_active(model: &Model) -> Cmd {
    match &model.active_env {
        Some(id) => store(StoreOperation::Save { key: KEY_ACTIVE.into(), value: id.clone() }),
        None => store(StoreOperation::Delete { key: KEY_ACTIVE.into() }),
    }
}

fn loopback(port: u16) -> String {
    format!("http://127.0.0.1:{port}")
}

type Request = crux_http::command::RequestBuilder<crate::Effect, Event>;

fn authorized(model: &Model, req: Request) -> Request {
    match &model.token {
        Some(t) => req.header("Authorization", format!("Bearer {t}").as_str()),
        None => req,
    }
}

// ---- launch and switching ----

/// Restores the saved environments and reconnects to the last one.
pub fn launch(model: &mut Model, surface: String, device_label: String, device_os: String) -> Cmd {
    model.surface = surface;
    model.device_label = device_label;
    model.device_os = device_os;
    model.launching = true;
    let layout = Command::request_from_shell(StoreOperation::Load { key: crate::sidebar::KEY_SIDEBAR_LAYOUT.into() })
        .then_send(|out| Event::SidebarLayoutLoaded { value: value_of(out) });
    Command::request_from_shell(StoreOperation::Load { key: KEY_ENVIRONMENTS.into() })
        .then_send(|out| Event::EnvironmentsLoaded { value: value_of(out) })
        .and(layout)
        .and(Command::request_from_shell(StoreOperation::Load { key: crate::terminal::SPLIT_KEY.into() })
            .then_send(|out| Event::TerminalSplitsLoaded { value: value_of(out) }))
}

pub fn environments_loaded(model: &mut Model, value: Option<String>) -> Cmd {
    model.environments = value.and_then(|v| serde_json::from_str(&v).ok()).unwrap_or_default();
    Command::request_from_shell(StoreOperation::Load { key: KEY_ACTIVE.into() })
        .then_send(|out| Event::ActiveEnvironmentLoaded { value: value_of(out) })
        .and(render())
}

/// Saved servers are in: reconnect to the last one, then handle the links
/// and notification taps that arrived meanwhile (a cold start from a tap).
pub fn active_loaded(model: &mut Model, value: Option<String>) -> Cmd {
    model.launching = false;
    let mut cmds = vec![match value.filter(|id| model.environments.iter().any(|e| &e.id == id)) {
        Some(id) => select(model, id, None),
        None => render(),
    }];
    for event in std::mem::take(&mut model.deferred) {
        cmds.push(crate::update::update(event, model));
    }
    Command::all(cmds)
}

/// Makes `id` the active environment and connects: pairing with `code`, or
/// with its saved token.
pub fn select(model: &mut Model, id: String, code: Option<String>) -> Cmd {
    let mut cmds = vec![teardown(model)];
    model.clear_server_state();
    model.active_env = Some(id.clone());
    model.unpark();
    model.token = None;
    cmds.push(save_active(model));
    let env_id = id.clone();
    cmds.push(Command::request_from_shell(StoreOperation::Load { key: drafts_key(&id) }).then_send(move |out| Event::DraftsLoaded { environment_id: env_id, value: value_of(out) }));
    let env_id = id.clone();
    cmds.push(Command::request_from_shell(StoreOperation::Load { key: cache_key(&id) }).then_send(move |out| Event::CacheLoaded { environment_id: env_id, value: value_of(out) }));
    match code {
        Some(code) => cmds.push(begin(model, Some(code))),
        None => {
            model.connection = Connection::Connecting;
            cmds.push(
                Command::request_from_shell(StoreOperation::Load { key: token_key(&id) })
                    .then_send(move |out| Event::TokenLoaded { environment_id: id, value: value_of(out) }),
            );
        }
    }
    cmds.push(render());
    Command::all(cmds)
}

pub fn token_loaded(model: &mut Model, environment_id: String, value: Option<String>) -> Cmd {
    // Another server was picked meanwhile, or this one is already connecting.
    if model.active_env.as_deref() != Some(environment_id.as_str()) || model.in_flight || model.connection != Connection::Connecting {
        return Command::done();
    }
    model.token = value;
    begin(model, None)
}

/// Only nearby servers may be reached without TLS: the pairing code and the
/// device's sign-in travel in the clear over `http://`.
fn plain_http_allowed(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("http://") else { return true };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host = if let Some(v6) = authority.strip_prefix('[') { v6.split(']').next().unwrap_or_default() } else { authority.rsplit_once(':').map_or(authority, |(h, _)| h) };
    let host = host.to_ascii_lowercase();
    if host == "localhost" || host.ends_with(".localhost") || host.ends_with(".local") || host.ends_with(".ts.net") || host.ends_with(".internal") || host.ends_with(".lan") || host.ends_with(".home.arpa") {
        return true;
    }
    match host.parse::<std::net::IpAddr>() {
        Ok(std::net::IpAddr::V4(ip)) => {
            let [a, b, ..] = ip.octets();
            // Loopback, private ranges, link-local and CGNAT (Tailscale).
            ip.is_loopback() || ip.is_private() || ip.is_link_local() || (a == 100 && (64..128).contains(&b))
        }
        Ok(std::net::IpAddr::V6(ip)) => ip.is_loopback() || (ip.segments()[0] & 0xfe00) == 0xfc00 || (ip.segments()[0] & 0xffc0) == 0xfe80,
        // A plain name with no dots (`mymac`) is on the local network.
        Err(_) => !host.contains('.'),
    }
}

/// A pairing input that can't be used. Connected (or on the way) to a
/// server, that stays as it is and only a toast says so; otherwise the
/// pairing screen shows the error.
fn bad_input(model: &mut Model, message: &str) -> Cmd {
    let in_use = model.active_env.is_some() && model.token.is_some() && model.connection != Connection::Failed && model.connection != Connection::Disconnected;
    if in_use {
        model.toast("error", message);
    } else {
        model.connection = Connection::Failed;
        model.error = Some(message.to_owned());
    }
    render()
}

/// `Connect`: a local server by URL.
pub fn connect_local(model: &mut Model, url: String, code: String) -> Cmd {
    let url = url.trim().trim_end_matches('/').to_owned();
    if url.is_empty() {
        return bad_input(model, "Enter the server address.");
    }
    if !plain_http_allowed(&url) {
        return bad_input(model, "That address is outside your network, where http:// would send your sign-in unencrypted. Use https://, or remote access with a domovoi:// link.");
    }
    let id = match model.environments.iter().find(|e| !e.is_remote() && e.url == url) {
        Some(e) => e.id.clone(),
        None => {
            let label = url.split("://").nth(1).unwrap_or(&url).to_owned();
            let env = Environment { id: crate::update::uuid(), label, url, ..Default::default() };
            let id = env.id.clone();
            model.environments.push(env);
            id
        }
    };
    let code = Some(code.trim().to_owned()).filter(|c| !c.is_empty());
    save_environments(model).and(select(model, id, code))
}

/// `ConnectRemote`: a remote server from a pairing link (or a bare ticket of
/// a server this device already paired with).
pub fn connect_remote(model: &mut Model, link: String) -> Cmd {
    let Some(link) = parse_link(&link) else {
        return bad_input(model, "That is not a Domovoi pairing link. On the server, run `domovoi-server pair --remote` or open Settings → Remote Access.");
    };
    let key = endpoint_key(&link.ticket);
    let id = match model.environments.iter_mut().find(|e| e.is_remote() && endpoint_key(&e.ticket) == key) {
        Some(e) => {
            e.ticket = link.ticket.clone();
            if let Some(name) = &link.name {
                e.label = name.clone();
            }
            e.id.clone()
        }
        None => {
            let label = link.name.clone().unwrap_or_else(|| format!("Remote {}", prefix(&key, 8)));
            let env = Environment { id: crate::update::uuid(), label, ticket: link.ticket.clone(), ..Default::default() };
            let id = env.id.clone();
            model.environments.push(env);
            id
        }
    };
    save_environments(model).and(select(model, id, link.code))
}

/// Forgets a server. Connected to it, this device first signs itself out
/// there, which also stops its pushes; the rest happens in `signed_out`.
pub fn forget(model: &mut Model, id: String) -> Cmd {
    let active = model.active_env.as_deref() == Some(id.as_str());
    if active && model.connection == Connection::Connected && model.token.is_some() && !model.server_url.is_empty() {
        model.toast("info", "Signing out of this server…");
        let id2 = id.clone();
        return authorized(model, Http::post(format!("{}/api/domovoi/session/end", model.server_url)))
            .build()
            .then_send(move |_| Event::SignedOut { id: id2 })
            .and(render());
    }
    remove_environment(model, &id)
}

/// The server has been told (or could not be reached): forget it here.
pub fn signed_out(model: &mut Model, id: String) -> Cmd {
    remove_environment(model, &id)
}

fn remove_environment(model: &mut Model, id: &str) -> Cmd {
    model.environments.retain(|e| e.id != id);
    model.parked.remove(id);
    let mut cmds = vec![save_environments(model), store(StoreOperation::Delete { key: token_key(id) })];
    if model.active_env.as_deref() == Some(id) {
        cmds.push(teardown(model));
        model.clear_server_state();
        model.parked.remove(id);
        model.active_env = None;
        model.token = None;
        model.connection = Connection::Disconnected;
        cmds.push(save_active(model));
    }
    cmds.push(render());
    Command::all(cmds)
}

pub fn rename(model: &mut Model, id: String, label: String) -> Cmd {
    let label = label.trim();
    if label.is_empty() {
        return render();
    }
    let Some(env) = model.environments.iter_mut().find(|e| e.id == id) else { return Command::done() };
    env.label = label.to_owned();
    save_environments(model).and(render())
}

pub fn move_environment(model: &mut Model, id: String, delta: i32) -> Cmd {
    let Some(from) = model.environments.iter().position(|e| e.id == id) else { return Command::done() };
    let to = (from as i64 + i64::from(delta)).clamp(0, model.environments.len() as i64 - 1) as usize;
    if to == from {
        return Command::done();
    }
    let env = model.environments.remove(from);
    model.environments.insert(to, env);
    save_environments(model).and(render())
}

// ---- connecting ----

/// Connects to the active environment; `code` pairs this device first.
fn begin(model: &mut Model, code: Option<String>) -> Cmd {
    let Some(env) = model.active_environment().cloned() else { return render() };
    model.error = None;
    model.manual_disconnect = false;
    model.suspended = false;
    model.server_changed = false;
    model.reconnect_attempts = 0;
    model.retry_in = 0;
    model.refusals = 0;
    model.pending_code = code;
    if model.token.is_none() && model.pending_code.is_none() {
        model.connection = Connection::Failed;
        model.error = Some(format!("Pair this device with {} using a pairing code.", env.label));
        return render();
    }
    model.connection = if model.pending_code.is_some() { Connection::Pairing } else { Connection::Connecting };
    model.attempt += 1;
    model.in_flight = true;
    transport(model, &env).and(render())
}

/// Reaches the server: through the tunnel for remote environments.
fn transport(model: &mut Model, env: &Environment) -> Cmd {
    if !env.is_remote() {
        model.server_url = env.url.clone();
        return authenticate(model);
    }
    if let Some(port) = model.tunnel.port {
        model.server_url = loopback(port);
        return authenticate(model);
    }
    model.tunnel = TunnelState::default();
    model.tunnel_gen += 1;
    let tunnel_gen = model.tunnel_gen;
    Command::stream_from_shell(TunnelOperation::Start { ticket: env.ticket.clone() }).then_send(move |out| Event::Tunnel { tunnel_gen, out })
}

fn authenticate(model: &mut Model) -> Cmd {
    match model.pending_code.take() {
        Some(code) => exchange(model, &code),
        None => mint_ticket(model),
    }
}

const GRANT_TOKEN_EXCHANGE: &str = "urn:ietf:params:oauth:grant-type:token-exchange";
const TOKEN_TYPE_BOOTSTRAP: &str = "urn:t3:params:oauth:token-type:environment-bootstrap";
const TOKEN_TYPE_ACCESS: &str = "urn:ietf:params:oauth:token-type:access_token";

fn exchange(model: &mut Model, code: &str) -> Cmd {
    let device_type = if model.surface == "mobile" { "mobile" } else { "desktop" };
    let mut pairs = vec![
        ("grant_type", GRANT_TOKEN_EXCHANGE),
        ("subject_token", code),
        ("subject_token_type", TOKEN_TYPE_BOOTSTRAP),
        ("requested_token_type", TOKEN_TYPE_ACCESS),
        ("client_device_type", device_type),
    ];
    if !model.device_label.is_empty() {
        pairs.push(("client_label", &model.device_label));
    }
    if !model.device_os.is_empty() {
        pairs.push(("client_os", &model.device_os));
    }
    let attempt = model.attempt;
    let environment_id = model.active_env.clone().unwrap_or_default();
    Http::post(format!("{}/oauth/token", model.server_url))
        .body_string(form_encode(&pairs))
        .header("Content-Type", "application/x-www-form-urlencoded")
        .build()
        .then_send(move |res| Event::TokenIssued { attempt, environment_id: environment_id, res })
}

pub fn token_issued(model: &mut Model, attempt: u64, environment_id: String, res: HttpResult) -> Cmd {
    let current = attempt == model.attempt && model.active_env.as_deref() == Some(environment_id.as_str());
    match answer(res) {
        Answer::Ok(body) => {
            let token = body.read::<AuthAccessTokenResult>().map(|issued| issued.access_token).unwrap_or_default();
            if token.is_empty() {
                return if current { failed(model, "The server answered without a token.".into()) } else { Command::done() };
            }
            // The code is spent either way: keep the sign-in it bought, even
            // if this attempt was abandoned (the app went to the background).
            if !model.environments.iter().any(|e| e.id == environment_id) {
                return Command::done();
            }
            let save = store(StoreOperation::Save { key: token_key(&environment_id), value: token.clone() });
            if model.active_env.as_deref() == Some(environment_id.as_str()) {
                model.token = Some(token);
            }
            if current { save.and(mint_ticket(model)) } else { save }
        }
        _ if !current => Command::done(),
        // A used or wrong code for a server this device is already signed in
        // to: carry on with the saved sign-in.
        Answer::Rejected(401, _) => Command::request_from_shell(StoreOperation::Load { key: token_key(&environment_id) })
            .then_send(move |out| Event::FallbackTokenLoaded { attempt, environment_id: environment_id, value: value_of(out) }),
        Answer::Rejected(429, _) => failed(model, "Too many pairing attempts. Wait a minute and try again.".into()),
        Answer::Rejected(code, _) => failed(model, format!("Pairing failed ({code}).")),
        Answer::Unreachable(e) => failed(model, format!("Could not reach the server: {e}")),
    }
}

pub fn fallback_token_loaded(model: &mut Model, attempt: u64, environment_id: String, value: Option<String>) -> Cmd {
    if attempt != model.attempt || model.active_env.as_deref() != Some(environment_id.as_str()) {
        return Command::done();
    }
    match value {
        Some(token) => {
            model.token = Some(token);
            model.toast("info", "That pairing code was already used; this device is still signed in.");
            mint_ticket(model).and(render())
        }
        None => failed(model, "The pairing code was rejected or already used.".into()),
    }
}

fn mint_ticket(model: &mut Model) -> Cmd {
    if model.token.is_none() {
        return failed(model, "Pair this device using a pairing code.".into());
    }
    let attempt = model.attempt;
    authorized(model, Http::post(format!("{}/api/auth/websocket-ticket/rpc", model.server_url)))
        .build()
        .then_send(move |res| Event::TicketIssued { attempt, res })
}

/// The ticket answer of the current attempt (stale ones are dropped before).
pub fn ticket_issued(model: &mut Model, res: HttpResult) -> Cmd {
    match answer(res) {
        Answer::Ok(body) => {
            model.refusals = 0;
            let ticket = read::<AuthWebSocketTicketResult>(model, "the server's sign-in answer", body).map(|issued| issued.ticket).unwrap_or_default();
            open_socket(model, &ticket).and(render())
        }
        Answer::Rejected(401, _) => {
            // Revoked or expired: pairing again is the only way back.
            model.token = None;
            let label = model.active_environment().map(|e| e.label.clone()).unwrap_or_default();
            let forget = match &model.active_env {
                Some(id) => store(StoreOperation::Delete { key: token_key(id) }),
                None => Command::done(),
            };
            forget.and(failed(model, format!("This device is no longer signed in to {label}. Pair it again with a new code.")))
        }
        // Not a Domovoi server any more, or it refuses this device: retrying will not help.
        // A captive portal or proxy can answer these too: only the server's
        // own error, or the same answer three times running, means it.
        Answer::Rejected(code @ (400 | 403 | 404), body) => {
            model.refusals += 1;
            let from_server = is_servers_own(&body);
            if !from_server && model.refusals < 3 {
                return retry_later(model, format!("The server answered {code}."));
            }
            model.server_changed = true;
            failed(model, format!("This server no longer accepts this device (it answered {code}). If it was reinstalled or replaced, pair it again; otherwise forget it."))
        }
        Answer::Rejected(code, _) => retry_later(model, format!("The server answered {code}.")),
        // Through a tunnel this is the bridge failing to reach the server: the
        // next attempt starts a new one rather than retrying a stuck bridge.
        Answer::Unreachable(e) => restart_tunnel(model).and(retry_later(model, format!("Could not reach the server: {e}"))),
    }
}

/// Drops the running tunnel, so the next attempt starts a new bridge with
/// new sockets. iOS takes a suspended app's sockets, and a bridge left on
/// them can keep failing every dial, never reaching the server, until the
/// app is restarted.
fn restart_tunnel(model: &mut Model) -> Cmd {
    if model.tunnel.port.is_none() {
        return Command::done();
    }
    model.tunnel = TunnelState::default();
    // Its stream is over; a new tunnel gets a new number.
    model.tunnel_gen += 1;
    model.server_url.clear();
    Command::notify_shell(TunnelOperation::Stop).into()
}

fn failed(model: &mut Model, message: String) -> Cmd {
    model.connection = Connection::Failed;
    model.error = Some(message);
    model.in_flight = false;
    model.retry_in = 0;
    render()
}

/// Counts a failed attempt and sets when the next one happens.
pub fn schedule_retry(model: &mut Model) {
    model.reconnect_attempts += 1;
    let jitter = uuid::Uuid::new_v4().as_u128() as u16 as f64 / f64::from(u16::MAX);
    model.retry_in = crate::model::retry_delay(model.reconnect_attempts, jitter);
    model.retry_gen += 1;
}

/// A failure that may pass (server restarting, network down): a paired device
/// keeps retrying with backoff; an unpaired one stops.
fn retry_later(model: &mut Model, message: String) -> Cmd {
    model.in_flight = false;
    model.attempt += 1;
    if model.token.is_some() && !model.manual_disconnect {
        model.connection = Connection::Reconnecting;
        model.error = Some(message);
        schedule_retry(model);
        render()
    } else {
        failed(model, message)
    }
}

pub fn tunnel_output(model: &mut Model, out: TunnelOutput) -> Cmd {
    match out {
        TunnelOutput::Started { port } => {
            model.tunnel.port = Some(port);
            model.tunnel.error.clear();
            model.server_url = loopback(port);
            let waiting = matches!(model.connection, Connection::Pairing | Connection::Connecting | Connection::Reconnecting);
            if waiting && model.in_flight && !model.suspended { authenticate(model).and(render()) } else { render() }
        }
        // The loopback port is gone: the tunnel must be started again.
        TunnelOutput::Status { listening: false, .. } => tunnel_output(model, TunnelOutput::Failed { reason: "the tunnel's local port was closed".into() }),
        TunnelOutput::Status { connected, path, rtt_ms, error, .. } => {
            model.tunnel.connected = connected;
            model.tunnel.path = path;
            model.tunnel.rtt_ms = rtt_ms;
            model.tunnel.error = error;
            render()
        }
        TunnelOutput::Failed { reason } => {
            model.tunnel = TunnelState { error: reason.clone(), ..Default::default() };
            // Its stream is over; a new tunnel gets a new number.
            model.tunnel_gen += 1;
            if model.manual_disconnect || model.suspended {
                return render();
            }
            let close: Cmd = if model.connection == Connection::Connected { Command::notify_shell(SocketOperation::Close).into() } else { Command::done() };
            crate::update::requests_lost(model);
            close.and(retry_later(model, format!("Could not reach the remote server: {reason}")))
        }
    }
}

/// The shell's backoff timer for retry `retry_gen` fired. A timer of an
/// earlier retry (one an attempt has overtaken since) does nothing.
pub fn retry_timer(model: &mut Model, retry_gen: u64) -> Cmd {
    if retry_gen != model.retry_gen || model.connection != Connection::Reconnecting || model.offline {
        return Command::done();
    }
    reconnect(model)
}

/// The user asked to connect now ("Retry Now", "Connect", pull to refresh):
/// skips the backoff, and undoes a manual disconnect.
pub fn reconnect_now(model: &mut Model) -> Cmd {
    if model.connection == Connection::Connected || model.in_flight || model.server_changed {
        return Command::done();
    }
    model.manual_disconnect = false;
    model.reconnect_attempts = 0;
    if model.connection == Connection::Failed {
        // Failed for good (signed out, pairing needed): only pairing helps.
        if model.token.is_none() {
            return Command::done();
        }
    }
    reconnect(model)
}

/// Starts a reconnect attempt now.
pub fn reconnect(model: &mut Model) -> Cmd {
    if model.manual_disconnect || model.suspended || model.in_flight || model.server_changed || model.connection == Connection::Connected {
        return Command::done();
    }
    let Some(env) = model.active_environment().cloned() else { return Command::done() };
    if model.token.is_none() {
        return Command::done();
    }
    model.connection = Connection::Reconnecting;
    model.retry_in = 0;
    model.attempt += 1;
    model.in_flight = true;
    transport(model, &env).and(render())
}

/// Closes the socket and the tunnel; replies still on their way are dropped.
fn teardown(model: &mut Model) -> Cmd {
    crate::update::requests_lost(model);
    model.attempt += 1;
    model.tunnel_gen += 1;
    model.in_flight = false;
    model.retry_in = 0;
    let mut cmds: Vec<Cmd> = vec![Command::notify_shell(SocketOperation::Close).into()];
    if model.tunnel.port.is_some() || model.active_environment().is_some_and(Environment::is_remote) {
        cmds.push(Command::notify_shell(TunnelOperation::Stop).into());
    }
    model.tunnel = TunnelState::default();
    model.server_url.clear();
    Command::all(cmds)
}

pub fn disconnect(model: &mut Model) -> Cmd {
    model.manual_disconnect = true;
    model.connection = Connection::Disconnected;
    teardown(model).and(render())
}

/// The app went to the background: drop the socket (the OS would) and
/// abandon an attempt under way. The tunnel stays; its connection redials
/// on demand.
pub fn suspend(model: &mut Model) -> Cmd {
    if model.active_env.is_none() || model.manual_disconnect {
        return Command::done();
    }
    model.suspended = true;
    let save = save_drafts(model).and(if model.cache_dirty { save_cache(model) } else { Command::done() });
    crate::update::requests_lost(model);
    model.attempt += 1;
    model.in_flight = false;
    model.retry_in = 0;
    if model.connection != Connection::Failed {
        model.connection = Connection::Disconnected;
    }
    let close: Cmd = Command::notify_shell(SocketOperation::Close).into();
    save.and(close).and(render())
}

/// The app is in the foreground again after a suspension (not a glance
/// away within the grace period, which never suspends): reconnect at once.
pub fn resume(model: &mut Model) -> Cmd {
    if !model.suspended {
        return Command::done();
    }
    model.suspended = false;
    if model.manual_disconnect || model.server_changed {
        return render();
    }
    if model.token.is_none() {
        if model.connection == Connection::Disconnected && model.active_env.is_some() {
            // Pairing was cut short before the server answered.
            return failed(model, "Pairing was interrupted. Use a new pairing code.".into());
        }
        return render();
    }
    // The tunnel's sockets may be gone with the suspension, and the server
    // has closed its connection by now anyway: start a new one.
    let restart = restart_tunnel(model);
    model.reconnect_attempts = 0;
    restart.and(reconnect(model))
}

/// The device's network went away or came back. Offline, retries wait
/// (they would only fail); back online, a waiting reconnect goes now.
pub fn network_status(model: &mut Model, online: bool) -> Cmd {
    let was = model.offline;
    model.offline = !online;
    if was && online {
        return network_changed(model).and(render());
    }
    render()
}

/// The device's network changed: re-probe the tunnel's paths, and retry a
/// waiting reconnect now instead of at the end of its backoff.
pub fn network_changed(model: &mut Model) -> Cmd {
    let probe: Cmd = if model.tunnel.port.is_some() { Command::notify_shell(TunnelOperation::NetworkChanged).into() } else { Command::done() };
    if model.connection == Connection::Reconnecting && !model.in_flight {
        model.reconnect_attempts = 0;
        return probe.and(reconnect(model));
    }
    probe
}

// ---- push notifications ----

/// Connected: the server answered, so backoff starts over; remember the
/// server's own id (dropping an older entry for the same server), and
/// register this device's push token with it.
pub fn on_welcome(model: &mut Model) -> Cmd {
    model.reconnect_attempts = 0;
    // The server answered, so there is a network, whatever the device last
    // said (a change while suspended may never have been reported).
    model.offline = false;
    let server_id = model.environment.as_ref().map(|environment| environment.environment_id.as_str().to_owned()).unwrap_or_default();
    let mut cmds = vec![];
    if let Some(id) = model.active_env.clone()
        && !server_id.is_empty()
        && let Some(active) = model.environments.iter().find(|e| e.id == id).cloned()
    {
        // The same server under an older entry of the same kind (its iroh key
        // changed, say): the new entry replaces it.
        let stale: Vec<String> =
            model.environments.iter().filter(|e| e.id != id && e.server_id == server_id && e.is_remote() == active.is_remote()).map(|e| e.id.clone()).collect();
        if active.server_id != server_id || !stale.is_empty() {
            model.environments.retain(|e| !stale.contains(&e.id));
            if let Some(env) = model.environments.iter_mut().find(|e| e.id == id) {
                env.server_id = server_id;
            }
            for old in &stale {
                cmds.push(store(StoreOperation::Delete { key: token_key(old) }));
            }
            cmds.push(save_environments(model));
        }
    }
    cmds.push(register_push(model));
    cmds.push(register_push_endpoint(model));
    for (thread, token, environment) in model.activity_tokens.clone() {
        cmds.push(register_activity(model, &thread, &token, &environment));
    }
    Command::all(cmds)
}

/// A Live Activity's push token: the server sends the thread's status to it.
pub fn activity_token(model: &mut Model, thread_id: String, token: String, environment: String) -> Cmd {
    model.activity_tokens.retain(|(t, _, _)| t != &thread_id);
    model.activity_tokens.push((thread_id.clone(), token.clone(), environment.clone()));
    if model.connection == Connection::Connected { register_activity(model, &thread_id, &token, &environment) } else { Command::done() }
}

fn register_activity(model: &Model, thread_id: &str, token: &str, environment: &str) -> Cmd {
    if model.server_url.is_empty() {
        return Command::done();
    }
    match authorized(model, Http::post(format!("{}/api/domovoi/push/activities", model.server_url))).body_json(&requests::push_activity(token, &thread_id.into(), environment)) {
        Ok(req) => req.build().then_send(Event::PushRegistered),
        Err(_) => Command::done(),
    }
}

pub fn push_token(model: &mut Model, token: String, environment: String) -> Cmd {
    model.push_token = Some((token, environment));
    if model.connection == Connection::Connected { register_push(model) } else { Command::done() }
}

fn register_push(model: &Model) -> Cmd {
    let (Some((token, environment)), false) = (&model.push_token, model.server_url.is_empty()) else { return Command::done() };
    match authorized(model, Http::post(format!("{}/api/domovoi/push/devices", model.server_url))).body_json(&requests::push_device(token, environment)) {
        Ok(req) => req.build().then_send(Event::PushRegistered),
        Err(_) => Command::done(),
    }
}

pub fn push_endpoint(model: &mut Model, endpoint: String, p256dh: String, auth: String) -> Cmd {
    model.push_endpoint = (!endpoint.is_empty()).then_some((endpoint, p256dh, auth));
    if model.connection == Connection::Connected { register_push_endpoint(model) } else { Command::done() }
}

/// Registers this device's UnifiedPush endpoint with the server in use.
fn register_push_endpoint(model: &Model) -> Cmd {
    let (Some((endpoint, p256dh, auth)), false) = (&model.push_endpoint, model.server_url.is_empty()) else { return Command::done() };
    match authorized(model, Http::post(format!("{}/api/domovoi/push/webpush", model.server_url))).body_json(&requests::push_webpush(endpoint, p256dh, auth)) {
        Ok(req) => req.build().then_send(Event::PushRegistered),
        Err(_) => Command::done(),
    }
}

/// Whether a notification's server (`environmentId`; empty: a local
/// notification) is the one in use.
pub fn is_current_server(model: &Model, environment_id: &str) -> bool {
    let active_server = model.active_environment().map(|e| e.server_id.clone()).unwrap_or_default();
    let server = model.environment.as_ref().map(|environment| environment.environment_id.as_str());
    model.active_env.is_some() && (environment_id.is_empty() || server == Some(environment_id) || active_server == environment_id)
}

/// Opens the thread a push named, switching to its server if needed.
pub fn open_notification(model: &mut Model, environment_id: String, thread_id: String) -> Cmd {
    if is_current_server(model, &environment_id) || (environment_id.is_empty() && model.active_env.is_none()) {
        // Opened as any thread is, connected or not: still connecting, what
        // it sends waits, and its stream starts once the socket opens
        // (`subscribe_core`). Only the selection changed here once, and the
        // thread open before stayed held under it: shown as this one, and
        // its stream resumed into that thread's records.
        return crate::update::select_thread(model, thread_id);
    }
    // Saved twice (by address and by link)? The remote entry works from anywhere.
    let mut matches: Vec<&Environment> = model.environments.iter().filter(|e| e.server_id == environment_id).collect();
    matches.sort_by_key(|e| !e.is_remote());
    let Some(env) = matches.first().map(|e| e.id.clone()) else {
        // A server this device has forgotten: nothing to open.
        return Command::done();
    };
    let cmd = select(model, env, None);
    // Subscribed once the socket opens (`subscribe_core`).
    model.open_thread(Some(thread_id));
    model.route = "thread".into();
    cmd
}

// ---- remote access settings (the server's side) ----

pub fn load_remote_access(model: &mut Model) -> Cmd {
    if model.server_url.is_empty() {
        return Command::done();
    }
    model.pages.remote_busy = true;
    authorized(model, Http::get(format!("{}/api/domovoi/remote", model.server_url))).build().then_send(Event::RemoteAccessLoaded).and(render())
}

pub fn set_remote_access(model: &mut Model, enabled: bool) -> Cmd {
    model.pages.remote_busy = true;
    model.pages.remote_error = None;
    match authorized(model, Http::post(format!("{}/api/domovoi/remote/configure", model.server_url))).body_json(&requests::remote_configure(enabled)) {
        Ok(req) => req.build().then_send(Event::RemoteAccessLoaded).and(render()),
        Err(e) => {
            model.pages.remote_busy = false;
            model.pages.remote_error = Some(e.to_string());
            render()
        }
    }
}

pub fn create_pairing_link(model: &mut Model) -> Cmd {
    model.pages.remote_busy = true;
    model.pages.remote_error = None;
    match authorized(model, Http::post(format!("{}/api/domovoi/remote/pairing", model.server_url))).body_json(&requests::remote_pairing(10)) {
        Ok(req) => req.build().then_send(Event::PairingLinkCreated).and(render()),
        Err(e) => {
            model.pages.remote_busy = false;
            model.pages.remote_error = Some(e.to_string());
            render()
        }
    }
}

pub fn revoke_client(model: &mut Model, session_id: String) -> Cmd {
    match authorized(model, Http::post(format!("{}/api/auth/clients/revoke", model.server_url))).body_json(&requests::auth_clients_revoke(&session_id)) {
        Ok(req) => req.build().then_send(Event::ClientRevoked),
        Err(_) => Command::done(),
    }
}

pub fn remote_access_loaded(model: &mut Model, res: HttpResult) -> Cmd {
    model.pages.remote_busy = false;
    match answer(res) {
        Answer::Ok(body) => {
            model.pages.remote = read::<RemoteAccessStatus>(model, "the server's remote access", body);
            model.pages.remote_error = None;
            if !model.pages.remote.as_ref().is_some_and(|remote| remote.running) {
                model.pages.remote_link = None;
            }
        }
        // A t3code server has no remote access routes.
        Answer::Rejected(404, _) => model.pages.remote = None,
        Answer::Rejected(code, body) => model.pages.remote_error = Some(rejection_message(code, &body)),
        Answer::Unreachable(e) => model.pages.remote_error = Some(e),
    }
    render()
}

pub fn pairing_link_created(model: &mut Model, res: HttpResult) -> Cmd {
    model.pages.remote_busy = false;
    match answer(res) {
        Answer::Ok(body) => model.pages.remote_link = read::<RemotePairingLink>(model, "the pairing link", body),
        Answer::Rejected(code, body) => model.pages.remote_error = Some(rejection_message(code, &body)),
        Answer::Unreachable(e) => model.pages.remote_error = Some(e),
    }
    render()
}

pub fn client_revoked(model: &mut Model, res: HttpResult) -> Cmd {
    match answer(res) {
        Answer::Ok(_) => model.toast("success", "Device signed out."),
        Answer::Rejected(code, body) => model.toast("error", format!("Could not sign the device out: {}", rejection_message(code, &body))),
        Answer::Unreachable(e) => model.toast("error", format!("Could not sign the device out: {e}")),
    }
    render()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn links_parse() {
        let link = parse_link("domovoi://connect?ticket=endpointabc&code=CODE+1&name=My%20Mac%20%26%20co").unwrap();
        assert_eq!(link, Link { ticket: "endpointabc".into(), code: Some("CODE 1".into()), name: Some("My Mac & co".into()) });
        assert_eq!(parse_link(" endpointxyz ").unwrap().ticket, "endpointxyz");
        assert!(parse_link("https://example.com").is_none());
        assert!(parse_link("domovoi://connect?code=X").is_none());
        assert!(parse_link("two words").is_none());
    }

    #[test]
    fn tickets_that_are_not_ascii_are_not_tickets() {
        assert!(parse_link("abcdefgh€ijk").is_none());
        assert!(parse_link("domovoi://connect?ticket=%C3%A9abc").is_none());
        // Labels cut by character, never inside one (a ticket saved before
        // this check still shows).
        assert_eq!(prefix("abcdefgh€ijk", 10), "abcdefgh€i");
        assert_eq!(prefix("ab", 8), "ab");
    }

    #[test]
    fn plain_http_only_nearby() {
        for ok in ["http://127.0.0.1:13773", "http://localhost:1", "http://192.168.1.5:13773", "http://10.0.0.2", "http://mymac:13773", "http://studio.local", "http://box.tail1234.ts.net", "http://100.101.102.103", "http://[::1]:1", "http://[fd00::5]:1", "https://example.com"] {
            assert!(plain_http_allowed(ok), "{ok}");
        }
        for bad in ["http://example.com", "http://8.8.8.8:13773", "http://[2001:db8::1]:1", "http://100.200.1.1"] {
            assert!(!plain_http_allowed(bad), "{bad}");
        }
    }

    #[test]
    fn retry_delays_back_off_with_jitter() {
        use crate::model::retry_delay;
        assert_eq!([1, 2, 3, 4, 5, 9].map(|n| retry_delay(n, 0.5)), [2, 4, 8, 16, 30, 30]);
        assert_eq!(retry_delay(5, 0.0), 23);
        assert_eq!(retry_delay(5, 1.0), 38);
        assert_eq!(retry_delay(1, 0.0), 2);
    }

    #[test]
    fn form_encoding_round_trips() {
        let s = form_encode(&[("a b", "c&d=é")]);
        assert_eq!(s, "a+b=c%26d%3D%C3%A9");
        assert_eq!(form_decode("c%26d%3D%C3%A9"), "c&d=é");
        assert_eq!(form_decode("100%"), "100%");
    }
}
