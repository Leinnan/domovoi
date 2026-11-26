//! Saved environments (servers) and connecting to them.
//!
//! An environment is local (an HTTP URL) or remote (an iroh ticket, reached
//! through the shell's loopback tunnel). Either way the client pairs once
//! with a one-time code through a token exchange (`/oauth/token`),
//! keeps the bearer token in the shell's secure store, and opens each RPC
//! socket with a fresh one-time ticket.
//!
//! Connecting: (tunnel) → pair (only with a code) → mint ticket → socket.
//!
//! What is saved, and when:
//!
//! - A server is saved once this device has signed in to it. Until then a
//!   new one lives in memory only (`Model::unsaved`): a wrong code, an
//!   unreachable address or a declined link leaves the saved list as it was.
//! - A link for a server already saved changes nothing saved until the
//!   server has answered through it: its newer ticket is tried first
//!   (`Model::pending_ticket`), and the saved name is never the link's.
//! - A link that reached the app from outside (`OpenLink`) connects nothing
//!   by itself: the user is asked, and what is connected stays so meanwhile.
//!
//! The server on this machine (desktop, `crate::local`): the shell reports
//! its descriptor, and the core signs in to it without a code by having the
//! shell write a request file and posting its id. The answer counts only if
//! it carries the secret in that file, which nothing but the real server can
//! read. That happens by itself once per install (a first launch with no
//! saved servers), when the user asks (`ConnectLocalServer`), and once,
//! silently, when a saved entry for that server has lost its sign-in.

use crate::app::Event;
use crate::local::{LocalAttempt, LocalOperation, LocalOutput, LocalServer};
use crate::model::{Connection, Environment, LinkRequest, Model, TunnelState};
use crate::offline::{cache_key, drafts_key, save_cache, save_drafts};
use crate::replies::http::{Answer, HttpResult, answer, host_not_allowed, is_servers_own, read, rejection_message};
use crate::requests;
use crate::socket::SocketOperation;
use crate::store::{KEY_ACTIVE, KEY_ENVIRONMENTS, KEY_LOCAL_AUTO, StoreOperation, StoreOutput, token_key};
use crate::tunnel::{TunnelOperation, TunnelOutput};
use crate::update::{Cmd, open_socket};
use crux_core::{Command, render::render};
use crux_http::command::Http;
use domovoi_model::typed::{AuthAccessTokenResult, AuthWebSocketTicketResult, LocalSessionRequest, LocalSessionResult, RemoteAccessStatus, RemotePairingLink};

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

/// The address and code in the pairing URL a server prints for a client on
/// its machine, `http://localhost:13773/pair#token=CODE`: what people copy
/// when asked for "the pairing code".
pub fn parse_pairing_url(text: &str) -> Option<(String, String)> {
    let text = text.trim();
    let scheme = ["http://", "https://"].into_iter().find(|scheme| text.starts_with(scheme))?.len();
    let code_at = ["#token=", "?token=", "&token="].into_iter().filter_map(|mark| text.find(mark).map(|at| at + mark.len())).min()?;
    let code = form_decode(text[code_at..].split(['&', '#', '?']).next().unwrap_or_default().trim());
    let authority_end = text[scheme..].find(['/', '?', '#']).map_or(text.len(), |at| at + scheme);
    (authority_end > scheme && !code.is_empty() && !text.contains(char::is_whitespace)).then(|| (text[..authority_end].to_owned(), code))
}

/// A name someone else chose (a link's `name`, a descriptor's label) as it
/// may be shown: no control characters, none that reorder or hide text, one
/// line, at most 40 characters. `None` when nothing is left.
pub fn shown_name(name: &str) -> Option<String> {
    let hidden = |c: char| c.is_control() || matches!(c, '\u{200b}' | '\u{200e}' | '\u{200f}' | '\u{061c}' | '\u{2060}' | '\u{feff}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}');
    let visible: String = name.chars().map(|c| if c.is_whitespace() { ' ' } else { c }).filter(|c| !hidden(*c)).collect();
    let line = prefix(&visible.split(' ').filter(|word| !word.is_empty()).collect::<Vec<_>>().join(" "), 40);
    let line = line.trim_end().to_owned();
    (!line.is_empty()).then_some(line)
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

/// Saves the list of servers: the ones this device has signed in to.
pub(crate) fn save_environments(model: &Model) -> Cmd {
    let saved: Vec<&Environment> = model.environments.iter().filter(|e| !model.unsaved.contains(&e.id)).collect();
    store(StoreOperation::Save { key: KEY_ENVIRONMENTS.into(), value: serde_json::to_string(&saved).expect("plain data serializes") })
}

fn save_active(model: &Model) -> Cmd {
    match &model.active_env {
        // Not a saved server yet: the last one stays the one to come back to.
        Some(id) if model.unsaved.contains(id) => Command::done(),
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
        .and(Command::request_from_shell(StoreOperation::Load { key: KEY_LOCAL_AUTO.into() }).then_send(|out| Event::LocalAutoLoaded { value: value_of(out) }))
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
    // Only now, so the server on this machine is never heard of before the
    // saved ones are known; and only on a desktop, where there can be one.
    if model.surface == "desktop" {
        cmds.push(Command::stream_from_shell(LocalOperation::Watch).then_send(|out| Event::LocalServer { out }));
    }
    cmds.push(local_auto(model));
    Command::all(cmds)
}

/// Makes `id` the active environment, with nothing connecting yet: what was
/// connected is closed, and what is saved for `id` on this device is loaded.
fn activate(model: &mut Model, id: &str) -> Vec<Cmd> {
    let mut cmds = vec![teardown(model)];
    model.clear_server_state();
    // A server made for a sign-in that never happened goes when another is picked.
    let dropped: Vec<String> = model.unsaved.iter().filter(|unsaved| unsaved.as_str() != id).cloned().collect();
    model.environments.retain(|e| !dropped.contains(&e.id));
    for gone in &dropped {
        model.unsaved.remove(gone);
        model.parked.remove(gone);
    }
    if model.pending_ticket.as_ref().is_some_and(|(environment, _)| environment != id) {
        model.pending_ticket = None;
    }
    model.active_env = Some(id.to_owned());
    model.unpark();
    model.token = None;
    cmds.push(save_active(model));
    let env_id = id.to_owned();
    cmds.push(Command::request_from_shell(StoreOperation::Load { key: drafts_key(id) }).then_send(move |out| Event::DraftsLoaded { environment_id: env_id, value: value_of(out) }));
    let env_id = id.to_owned();
    cmds.push(Command::request_from_shell(StoreOperation::Load { key: cache_key(id) }).then_send(move |out| Event::CacheLoaded { environment_id: env_id, value: value_of(out) }));
    cmds
}

/// Makes `id` the active environment and connects: pairing with `code`, or
/// with its saved token.
pub fn select(model: &mut Model, id: String, code: Option<String>) -> Cmd {
    let mut cmds = activate(model, &id);
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
    // The pairing URL the server prints, pasted whole where the address or
    // the code goes. In the code's place it gives the code, and the address
    // only when none was typed (the server prints `localhost`, which from
    // another machine is the wrong one).
    let (url, code) = match (parse_pairing_url(&url), parse_pairing_url(&code)) {
        (Some((address, code)), _) => (address, code),
        (None, Some((address, code))) => (if url.trim().is_empty() { address } else { url }, code),
        (None, None) => (url, code),
    };
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
            // Saved once this device has signed in to it (`signed_in`).
            model.unsaved.insert(id.clone());
            model.environments.push(env);
            id
        }
    };
    let code = Some(code.trim().to_owned()).filter(|c| !c.is_empty());
    select(model, id, code)
}

/// `ConnectRemote`: a remote server from a pairing link (or a bare ticket of
/// a server this device already paired with) that the user gave the app.
pub fn connect_remote(model: &mut Model, link: String) -> Cmd {
    // The local pairing URL, pasted where a link goes.
    if let Some((address, code)) = parse_pairing_url(&link) {
        return connect_local(model, address, code);
    }
    let Some(link) = parse_link(&link) else {
        return bad_input(model, "That is not a Domovoi pairing link. On the server, run `domovoi-server pair --remote` or open Settings → Remote Access.");
    };
    connect_parsed(model, link)
}

/// Connects to the server a link names. One this device has saved keeps its
/// name, and its saved ticket until the server has answered through the
/// link's; a new one is saved once this device has signed in to it.
fn connect_parsed(model: &mut Model, link: Link) -> Cmd {
    let key = endpoint_key(&link.ticket);
    let found = model.environments.iter_mut().find(|e| e.is_remote() && endpoint_key(&e.ticket) == key);
    let id = match found {
        Some(e) => {
            let id = e.id.clone();
            if model.unsaved.contains(&id) {
                e.ticket = link.ticket.clone();
                model.pending_ticket = None;
            } else {
                model.pending_ticket = (e.ticket != link.ticket).then(|| (id.clone(), link.ticket.clone()));
            }
            id
        }
        None => {
            let label = link.name.as_deref().and_then(shown_name).unwrap_or_else(|| format!("Remote {}", prefix(&key, 8)));
            let env = Environment { id: crate::update::uuid(), label, ticket: link.ticket.clone(), ..Default::default() };
            let id = env.id.clone();
            model.unsaved.insert(id.clone());
            model.environments.push(env);
            id
        }
    };
    select(model, id, link.code)
}

/// `OpenLink`: a link the system handed the app. A page, a mail or another
/// app can make the system do that, so nothing connects here: the link waits
/// for the user's answer (`ConnectionView.link_request`), and a server in use
/// stays in use. A link that is none only says so.
pub fn open_link(model: &mut Model, text: String) -> Cmd {
    let link = parse_link(&text).filter(|_| text.trim().starts_with("domovoi://"));
    match link {
        Some(link) => {
            model.link_counter += 1;
            model.link_request = Some(LinkRequest { id: model.link_counter, link });
        }
        None => model.toast("error", "That link is not a Domovoi pairing link."),
    }
    render()
}

/// `AnswerLinkRequest`: the user's yes or no to the link waiting. An answer
/// to an earlier request (another link came meanwhile) changes nothing.
pub fn answer_link_request(model: &mut Model, id: u32, accept: bool) -> Cmd {
    let Some(request) = model.link_request.take_if(|request| request.id == id) else { return Command::done() };
    if accept { connect_parsed(model, request.link) } else { render() }
}

/// This device has signed in to `id`: its token is saved, and so is the
/// server itself if it was not yet. The one place a new server is saved.
fn signed_in(model: &mut Model, id: &str, token: String) -> Cmd {
    let mut cmds = vec![store(StoreOperation::Save { key: token_key(id), value: token.clone() })];
    let active = model.active_env.as_deref() == Some(id);
    if model.unsaved.remove(id) {
        cmds.push(save_environments(model));
        if active {
            cmds.push(save_active(model));
        }
    }
    if active {
        model.token = Some(token);
    }
    Command::all(cmds)
}

/// The server answered this device through the ticket a link brought: that
/// ticket is the saved one from now on.
fn adopt_ticket(model: &mut Model) -> Cmd {
    let Some((id, ticket)) = model.pending_ticket.take_if(|(id, _)| model.active_env.as_deref() == Some(id.as_str())) else { return Command::done() };
    let Some(env) = model.environments.iter_mut().find(|e| e.id == id) else { return Command::done() };
    env.ticket = ticket;
    save_environments(model)
}

/// What this device keeps for a server besides its entry: its drafts and its
/// offline copy, and (`token`) its sign-in.
fn forget_keys(id: &str, token: bool) -> Vec<Cmd> {
    let mut cmds = vec![store(StoreOperation::Delete { key: drafts_key(id) }), store(StoreOperation::Delete { key: cache_key(id) })];
    if token {
        cmds.push(store(StoreOperation::Delete { key: token_key(id) }));
    }
    cmds
}

/// Forgets a server. Connected to it, this device first signs itself out
/// there, which also stops its pushes; the rest happens in `signed_out`.
///
/// A server that is not the connected one is forgotten at once, and what
/// then happens to this device's sign-in there is said, not assumed: a
/// server reached by address is asked to end it, with that server's own
/// token (`forget_token_loaded`); a remote one is not dialled for it.
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
    let Some(env) = model.environments.iter().find(|e| e.id == id).cloned() else { return Command::done() };
    if model.unsaved.contains(&id) {
        // Never signed in to: there is nothing saved and no sign-in to end.
        return remove_environment(model, &id, true);
    }
    let url = if env.is_remote() { String::new() } else { env.url.clone() };
    let (label, forgotten) = (env.label.clone(), id.clone());
    // The token is deleted once it has been read (`forget_token_loaded`).
    let load = Command::request_from_shell(StoreOperation::Load { key: token_key(&id) })
        .then_send(move |out| Event::ForgetTokenLoaded { id: forgotten, label, url, value: value_of(out) });
    remove_environment(model, &id, false).and(load)
}

/// The server has been told (or could not be reached): forget it here.
pub fn signed_out(model: &mut Model, id: String) -> Cmd {
    remove_environment(model, &id, true)
}

/// The sign-in of a server just forgotten (`forget`), read before it is
/// deleted here: a server reached by address is asked to end it.
pub fn forget_token_loaded(model: &mut Model, id: String, label: String, url: String, value: Option<String>) -> Cmd {
    let delete = store(StoreOperation::Delete { key: token_key(&id) });
    let Some(token) = value else { return delete };
    if url.is_empty() {
        model.toast("info", format!("Forgot {label}. This device stays signed in there until that expires or you remove it in that server's access settings."));
        return delete.and(render());
    }
    let asked = Http::post(format!("{url}/api/domovoi/session/end"))
        .header("Authorization", format!("Bearer {token}").as_str())
        .build()
        .then_send(move |res| Event::ForgottenSignedOut { label, res });
    delete.and(asked)
}

pub fn forgotten_signed_out(model: &mut Model, label: String, res: HttpResult) -> Cmd {
    match answer(res) {
        Answer::Ok(_) => model.toast("success", format!("Forgot {label} and signed this device out of it.")),
        // The sign-in was gone already.
        Answer::Rejected(401, _) => return Command::done(),
        _ => model.toast("info", format!("Forgot {label}. It could not be reached to sign this device out: that lasts until it expires or you remove this device in that server's access settings.")),
    }
    render()
}

/// Takes `id` out of the saved servers, with what this device kept for it
/// (its sign-in too, with `token`; otherwise that is still to be read).
fn remove_environment(model: &mut Model, id: &str, token: bool) -> Cmd {
    model.environments.retain(|e| e.id != id);
    model.parked.remove(id);
    let was_saved = !model.unsaved.remove(id);
    if model.pending_ticket.as_ref().is_some_and(|(environment, _)| environment == id) {
        model.pending_ticket = None;
    }
    let mut cmds = vec![];
    if was_saved {
        cmds.push(save_environments(model));
        cmds.extend(forget_keys(id, token));
        // Forgetting is a choice: the app does not sign in to this machine's server by itself after it.
        if !model.local_auto_done {
            model.local_auto_done = true;
            cmds.push(store(StoreOperation::Save { key: KEY_LOCAL_AUTO.into(), value: "1".into() }));
        }
    }
    if model.active_env.as_deref() == Some(id) {
        cmds.push(teardown(model));
        model.clear_server_state();
        model.parked.remove(id);
        model.active_env = None;
        model.token = None;
        model.connection = Connection::Disconnected;
        // A server never saved was never the one to come back to either.
        if was_saved {
            cmds.push(save_active(model));
        }
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
        // The server on this machine signs this app in again by itself.
        if let Some(renewing) = local_renew(model, &env) {
            return renewing;
        }
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
    // A link's newer ticket for this server is the one to try.
    let ticket = match &model.pending_ticket {
        Some((id, ticket)) if *id == env.id => ticket.clone(),
        _ => env.ticket.clone(),
    };
    Command::stream_from_shell(TunnelOperation::Start { ticket }).then_send(move |out| Event::Tunnel { tunnel_gen, out })
}

fn authenticate(model: &mut Model) -> Cmd {
    match model.pending_code.take() {
        Some(code) => exchange(model, &code),
        None => mint_ticket(model),
    }
}

const GRANT_TOKEN_EXCHANGE: &str = "urn:ietf:params:oauth:grant-type:token-exchange";
const TOKEN_TYPE_BOOTSTRAP: &str = "urn:domovoi:params:oauth:token-type:environment-bootstrap";
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
            let save = signed_in(model, &environment_id, token);
            if current { save.and(mint_ticket(model)) } else { save }
        }
        _ if !current => Command::done(),
        // A used or wrong code for a server this device is already signed in
        // to: carry on with the saved sign-in.
        Answer::Rejected(401, _) => Command::request_from_shell(StoreOperation::Load { key: token_key(&environment_id) })
            .then_send(move |out| Event::FallbackTokenLoaded { attempt, environment_id: environment_id, value: value_of(out) }),
        Answer::Rejected(429, _) => failed(model, "Too many pairing attempts. Wait a minute and try again.".into()),
        // Reached under a name it does not answer to: the server says what to do.
        Answer::Rejected(code, body) => failed(model, host_not_allowed(&body).unwrap_or_else(|| format!("Pairing failed ({code})."))),
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
            let issued = read::<AuthWebSocketTicketResult>(model, "the server's sign-in answer", body);
            // The server took this device's sign-in: it is the one the link named.
            let adopted = adopt_ticket(model);
            // A server of another protocol version and this app would
            // misread each other: say which of the two to update, and keep
            // asking (the server may be updated while the app waits).
            if let Some(advice) = issued.as_ref().and_then(|issued| update_advice(issued.protocol)) {
                return adopted.and(retry_later(model, advice.into()));
            }
            let ticket = issued.map(|issued| issued.ticket).unwrap_or_default();
            adopted.and(open_socket(model, &ticket)).and(render())
        }
        Answer::Rejected(401, _) => {
            // Revoked or expired: pairing again is the only way back, unless
            // it is the server on this machine, which signs this app in again.
            model.token = None;
            let label = model.active_environment().map(|e| e.label.clone()).unwrap_or_default();
            let forget = match &model.active_env {
                Some(id) => store(StoreOperation::Delete { key: token_key(id) }),
                None => Command::done(),
            };
            if let Some(env) = model.active_environment().cloned()
                && let Some(renewing) = local_renew(model, &env)
            {
                return forget.and(renewing);
            }
            forget.and(failed(model, format!("This device is no longer signed in to {label}. Pair it again with a new code.")))
        }
        // Not a Domovoi server any more, or it refuses this device: retrying will not help.
        // A captive portal or proxy can answer these too: only the server's
        // own error, or the same answer three times running, means it.
        Answer::Rejected(code @ (400 | 403 | 404), body) => {
            // The server's own word that it was reached under a name it does
            // not answer to: it is there, and so is this device's sign-in.
            // What changes is how the server is started, so no "pair again".
            if let Some(message) = host_not_allowed(&body) {
                return failed(model, message);
            }
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

/// What to tell the user when the server's protocol version (none from a
/// server older than the versions) is not this app's; `None` when it is.
fn update_advice(server: Option<u32>) -> Option<&'static str> {
    match server {
        Some(version) if version == domovoi_model::PROTOCOL_VERSION => None,
        Some(version) if version > domovoi_model::PROTOCOL_VERSION => Some("This app is too old for this server. Update Domovoi on this device."),
        _ => Some("This server is older than this app. Update domovoi-server on the machine it runs on."),
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
    model.local_attempt = None;
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
    model.local_attempt = None;
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

// ---- the server on this machine ----

/// The port of a URL on this machine (`http://localhost:13773`).
pub(crate) fn loopback_port(url: &str) -> Option<u16> {
    let authority = url.strip_prefix("http://")?.split(['/', '?', '#']).next()?;
    let (host, port) = authority.rsplit_once(':')?;
    let host = host.trim_start_matches('[').trim_end_matches(']');
    let loopback = host.eq_ignore_ascii_case("localhost") || host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback());
    port.parse().ok().filter(|_| loopback)
}

/// Whether `env` is the entry for this machine's server: the one with its
/// id, or, never connected yet, one at its port on this machine.
fn is_local_server(env: &Environment, server: &LocalServer) -> bool {
    if env.is_remote() {
        return false;
    }
    if env.server_id.is_empty() { loopback_port(&env.url) == Some(server.port) } else { env.server_id == server.environment_id }
}

/// The entry for this machine's server, if there is one.
pub fn local_environment<'a>(model: &'a Model, server: &LocalServer) -> Option<&'a Environment> {
    let by_id = model.environments.iter().find(|e| !e.is_remote() && e.server_id == server.environment_id);
    by_id.or_else(|| model.environments.iter().find(|e| is_local_server(e, server)))
}

/// `LocalAutoLoaded`: whether this install signed in to its machine's server by itself before.
pub fn local_auto_loaded(model: &mut Model, value: Option<String>) -> Cmd {
    model.local_auto_known = true;
    model.local_auto_done = model.local_auto_done || value.is_some();
    local_auto(model)
}

/// `LocalServer`: the shell's word on this machine's server, now or after a change.
pub fn local_server_output(model: &mut Model, out: LocalOutput) -> Cmd {
    let LocalOutput::Descriptor { text } = out else { return Command::done() };
    let server = text.as_deref().and_then(LocalServer::parse);
    if server == model.local_server {
        return Command::done();
    }
    model.local_server = server;
    // A server that has only now appeared has refused nothing yet.
    model.local_tried = None;
    let auto = local_auto(model);
    // An entry for it that waits to be paired (the server was not running when
    // the app tried) is signed in now that the server is there.
    let waiting = model.token.is_none() && !model.in_flight && !model.server_changed && !model.manual_disconnect && !model.suspended && model.connection == Connection::Failed;
    let renew = match model.active_environment().cloned().filter(|_| waiting) {
        Some(env) => local_renew(model, &env).unwrap_or_else(Command::done),
        None => Command::done(),
    };
    auto.and(renew).and(render())
}

/// On a first launch with no saved servers, the app signs in to the server
/// on its machine by itself: once per install (`KEY_LOCAL_AUTO`), so a user
/// who then forgets that server is not signed back in at the next launch.
fn local_auto(model: &mut Model) -> Cmd {
    let first_run = !model.launching && model.local_auto_known && !model.local_auto_done && model.environments.is_empty() && model.active_env.is_none() && !model.in_flight;
    let Some(server) = model.local_server.clone().filter(|_| first_run) else { return Command::done() };
    model.local_auto_done = true;
    store(StoreOperation::Save { key: KEY_LOCAL_AUTO.into(), value: "1".into() }).and(connect_local_server(model, server, false))
}

/// `ConnectLocalServer`: the user picked this machine's server.
pub fn connect_local_asked(model: &mut Model) -> Cmd {
    match model.local_server.clone() {
        Some(server) => connect_local_server(model, server, true),
        None => {
            model.toast("error", "No Domovoi server is running on this computer.");
            render()
        }
    }
}

/// Makes this machine's server the active one (its saved entry, or a new
/// one) and signs in to it without a code.
fn connect_local_server(model: &mut Model, server: LocalServer, asked: bool) -> Cmd {
    let (id, made) = match local_environment(model, &server) {
        Some(env) => (env.id.clone(), model.unsaved.contains(&env.id)),
        None => {
            let env = Environment { id: crate::update::uuid(), label: server.label.clone(), url: server.url.clone(), server_id: server.environment_id.clone(), ..Default::default() };
            let id = env.id.clone();
            model.unsaved.insert(id.clone());
            model.environments.push(env);
            (id, true)
        }
    };
    let mut cmds = activate(model, &id);
    model.local_tried = Some(server.clone());
    cmds.push(local_sign_in(model, id, server, asked, made));
    Command::all(cmds)
}

/// A saved entry for this machine's server has no sign-in (never had, or
/// the server ended it): the server gives it a new one, with nobody asked.
/// Once per server until a connection succeeds; `None` when `env` is not
/// that server's, or it was tried already.
fn local_renew(model: &mut Model, env: &Environment) -> Option<Cmd> {
    let server = model.local_server.clone().filter(|server| is_local_server(env, server) && model.local_tried.as_ref() != Some(server))?;
    model.local_tried = Some(server.clone());
    Some(local_sign_in(model, env.id.clone(), server, false, model.unsaved.contains(&env.id)))
}

/// Starts the sign-in to this machine's server for the active environment
/// `id`: the shell writes the request file first.
fn local_sign_in(model: &mut Model, id: String, server: LocalServer, asked: bool, made: bool) -> Cmd {
    model.error = None;
    model.manual_disconnect = false;
    model.suspended = false;
    model.server_changed = false;
    model.reconnect_attempts = 0;
    model.retry_in = 0;
    model.refusals = 0;
    model.pending_code = None;
    // Connecting, not pairing: there is no code, and nothing for the user to do.
    model.connection = Connection::Connecting;
    model.attempt += 1;
    model.in_flight = true;
    let attempt = model.attempt;
    model.local_attempt = Some(LocalAttempt { attempt, environment_id: id, server, secret: None, asked, made });
    Command::request_from_shell(LocalOperation::Request).then_send(move |out| Event::LocalRequested { attempt, out }).and(render())
}

/// The sign-in under way, if `attempt` is its and still the current one.
fn local_attempt(model: &Model, attempt: u64) -> Option<LocalAttempt> {
    model.local_attempt.clone().filter(|local| local.attempt == attempt && attempt == model.attempt && model.active_env.as_deref() == Some(local.environment_id.as_str()))
}

/// `LocalRequested`: the request file is written; its id goes to the server.
pub fn local_requested(model: &mut Model, attempt: u64, out: LocalOutput) -> Cmd {
    let Some(local) = local_attempt(model, attempt) else { return Command::done() };
    let LocalOutput::Request { id, secret } = out else { return local_failed(model) };
    if let Some(under_way) = &mut model.local_attempt {
        under_way.secret = Some(secret);
    }
    let said = |value: &str| (!value.is_empty()).then(|| value.to_owned());
    let body = LocalSessionRequest {
        request: id,
        client_label: said(&model.device_label),
        client_device_type: Some(if model.surface == "mobile" { "mobile" } else { "desktop" }.to_owned()),
        client_os: said(&model.device_os),
    };
    model.server_url = local.server.url.clone();
    match Http::post(format!("{}/api/domovoi/local/session", local.server.url)).body_json(&body) {
        Ok(request) => request.build().then_send(move |res| Event::LocalSignedIn { attempt, res }),
        Err(_) => local_failed(model),
    }
}

/// `LocalSignedIn`: the answer to the sign-in. It counts only if it carries
/// the secret from the request file: the real server read it there, and
/// another program listening on the server's port cannot know it. Anything
/// else is dropped, token and all.
pub fn local_signed_in(model: &mut Model, attempt: u64, res: HttpResult) -> Cmd {
    let Some(local) = local_attempt(model, attempt) else { return Command::done() };
    let issued = match answer(res) {
        Answer::Ok(body) => body.read::<LocalSessionResult>(),
        _ => None,
    };
    let proven = |issued: &LocalSessionResult| local.secret.as_deref().is_some_and(|secret| !secret.is_empty() && secret == issued.proof) && issued.environment_id == local.server.environment_id && !issued.access_token.is_empty();
    let Some(issued) = issued.filter(proven) else { return local_failed(model) };
    model.local_attempt = None;
    let id = local.environment_id;
    let was_saved = !model.unsaved.contains(&id);
    let mut cmds = vec![];
    // A saved entry found by its port (or saved under another address of
    // this machine) is this server's from now on.
    if let Some(env) = model.environments.iter_mut().find(|e| e.id == id)
        && (env.url != local.server.url || env.server_id != local.server.environment_id)
    {
        env.url = local.server.url.clone();
        env.server_id = local.server.environment_id.clone();
        if was_saved {
            cmds.push(save_environments(model));
        }
    }
    model.server_url = local.server.url;
    cmds.push(signed_in(model, &id, issued.access_token));
    cmds.push(mint_ticket(model));
    Command::all(cmds)
}

/// The sign-in to this machine's server did not happen (no request file, no
/// answer, or one without the proof). An entry made for it goes, and the
/// plain connect screen is back; a saved one waits to be paired with a code.
/// Said only when the user asked for the sign-in.
fn local_failed(model: &mut Model) -> Cmd {
    let Some(local) = model.local_attempt.take() else { return Command::done() };
    model.token = None;
    let said = "Could not sign in to the server on this computer. Is it still running? You can pair with a code instead.";
    if !local.made {
        if local.asked {
            model.toast("error", said);
        }
        let label = model.active_environment().map(|e| e.label.clone()).unwrap_or_default();
        return failed(model, format!("Pair this device with {label} using a pairing code."));
    }
    let close = teardown(model);
    model.clear_server_state();
    model.environments.retain(|e| e.id != local.environment_id);
    model.unsaved.remove(&local.environment_id);
    model.parked.remove(&local.environment_id);
    model.active_env = None;
    model.token = None;
    model.connection = Connection::Disconnected;
    if local.asked {
        model.toast("error", said);
    }
    close.and(render())
}

// ---- push notifications ----

/// Connected: the server answered, so backoff starts over; remember the
/// server's own id (dropping an older entry for the same server), and
/// register this device's push token with it.
pub fn on_welcome(model: &mut Model) -> Cmd {
    model.reconnect_attempts = 0;
    // Connected: this machine's server may be asked again, should the sign-in go.
    model.local_tried = None;
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
                model.parked.remove(old);
                cmds.extend(forget_keys(old, true));
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
        // A server built without remote access has no such route.
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
