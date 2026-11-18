//! Remote access over iroh (Domovoi only; t3code reaches remote machines through
//! T3 Connect). The server's iroh key lives in `<home>/remote/secret.key`, so
//! its endpoint id, which clients dial, survives restarts. Settings live in
//! `<home>/remote.json`, apart from t3code's settings schema.
//!
//! Pairing links look like `domovoi://connect?ticket=…&code=…&name=…`: an iroh
//! ticket (or bare endpoint id), a one-time pairing code and a label.

use crate::util::locked;
use serde::{Deserialize, Serialize};
use domovoi_model::typed::{RemoteAccessState, RemoteAccessStatus, RemoteDevice, RemotePushStatus};
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use domovoi_remote::{EndpointId, Network, RelayUrl, RemoteServer, SecretKey, Ticket};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

/// Pairing attempts allowed per minute from iroh peers that hold no session:
/// per peer, and for all of them together (keys cost nothing, so the total
/// is what bounds guessing). Paired devices are never limited, so a stranger
/// cannot lock them out.
const ATTEMPTS_PER_PEER: usize = 6;
const ATTEMPTS_TOTAL: usize = 60;
const ATTEMPT_WINDOW: Duration = Duration::from_secs(60);
/// How long the first ticket waits for a relay, so it can name one.
const RELAY_WAIT: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteSettings {
    pub enabled: bool,
    /// Self-hosted relay URLs; empty uses n0's public relays, `["disabled"]`
    /// the local network only.
    #[serde(default)]
    pub relays: Vec<String>,
    /// The UDP port used on the local network (`disabled` relays). Kept so
    /// links saved on other devices still work after a restart.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lan_port: Option<u16>,
}

/// Where remote access stands.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Phase {
    #[default]
    Off,
    Starting,
    /// Serving. `relay` is the relay peers can reach it through, if any;
    /// `lan_only` when relays are turned off.
    Online { relay: Option<String>, lan_only: bool },
    /// The last start failed.
    Failed(String),
}

#[derive(Debug, thiserror::Error)]
pub enum RemoteError {
    /// A relay setting that is not a URL (the caller's input).
    #[error("{0}")]
    InvalidRelay(String),
    /// The endpoint could not start (details are for the local log).
    #[error("{0}")]
    Start(String),
    #[error("saving remote.json: {0}")]
    Save(String),
}

pub struct Remote {
    dir: PathBuf,
    settings_path: PathBuf,
    key: SecretKey,
    settings: Mutex<RemoteSettings>,
    /// Serializes start, stop and configure.
    ops: tokio::sync::Mutex<()>,
    /// The running endpoint and the task that keeps its saved ticket fresh.
    server: Mutex<Option<(Arc<RemoteServer>, CancellationToken)>>,
    phase: watch::Sender<Phase>,
    attempts: Mutex<Attempts>,
}

#[derive(Default)]
struct Attempts {
    all: VecDeque<Instant>,
    by_peer: HashMap<String, VecDeque<Instant>>,
}

impl Remote {
    /// Loads (or creates) the server key and the settings. `relays` from the
    /// command line replace the saved ones for this run.
    pub fn open(home: &Path, relays: Vec<String>) -> std::io::Result<Self> {
        let dir = home.join("remote");
        let key = load_or_create_key(&dir.join("secret.key"))?;
        let settings_path = home.join("remote.json");
        let mut settings: RemoteSettings =
            std::fs::read(&settings_path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
        if !relays.is_empty() {
            settings.relays = relays;
        }
        Ok(Remote {
            dir,
            settings_path,
            key,
            settings: Mutex::new(settings),
            ops: Default::default(),
            server: Default::default(),
            phase: watch::Sender::new(Phase::Off),
            attempts: Default::default(),
        })
    }

    pub fn endpoint_id(&self) -> EndpointId {
        self.key.public()
    }

    pub fn settings(&self) -> RemoteSettings {
        locked(&self.settings).clone()
    }

    pub fn phase(&self) -> Phase {
        self.phase.borrow().clone()
    }

    /// Changes of [`Phase`], for the status bar icon and status readers.
    pub fn watch(&self) -> watch::Receiver<Phase> {
        self.phase.subscribe()
    }

    fn running(&self) -> Option<Arc<RemoteServer>> {
        locked(&self.server).as_ref().map(|(s, _)| s.clone())
    }

    pub fn is_running(&self) -> bool {
        locked(&self.server).is_some()
    }

    fn save(&self, settings: &RemoteSettings) -> Result<(), RemoteError> {
        std::fs::write(&self.settings_path, serde_json::to_vec_pretty(settings).unwrap_or_default()).map_err(|e| RemoteError::Save(e.to_string()))
    }

    /// Saves the on/off switch without a running server (`domovoi-server remote
    /// on|off` with nothing to talk to); it takes effect at the next start.
    pub fn save_enabled(&self, enabled: bool) -> Result<(), RemoteError> {
        let settings = {
            let mut s = locked(&self.settings);
            s.enabled = enabled;
            s.clone()
        };
        self.save(&settings)
    }

    /// Starts serving `router` to iroh peers (restarting if running).
    pub async fn start(self: &Arc<Self>, router: axum::Router) -> Result<(), RemoteError> {
        let _op = self.ops.lock().await;
        self.start_locked(router).await
    }

    async fn start_locked(self: &Arc<Self>, router: axum::Router) -> Result<(), RemoteError> {
        let settings = self.settings();
        let network = server_network(&settings).map_err(RemoteError::InvalidRelay)?;
        self.halt().await;
        self.phase.send_replace(Phase::Starting);
        let lan_only = matches!(network, Network::Direct { .. });
        let started = match RemoteServer::start(self.key.clone(), network, router.clone()).await {
            Ok(s) => s,
            // The saved LAN port may be taken now: pick a new one.
            Err(_) if lan_only && settings.lan_port.is_some() => {
                match RemoteServer::start(self.key.clone(), Network::Direct { port: 0 }, router).await {
                    Ok(s) => s,
                    Err(e) => return Err(self.failed(e.to_string())),
                }
            }
            Err(e) => return Err(self.failed(e.to_string())),
        };
        if lan_only && started.local_port() != settings.lan_port {
            let saved = {
                let mut s = locked(&self.settings);
                s.lan_port = started.local_port();
                s.clone()
            };
            if let Err(e) = self.save(&saved) {
                tracing::warn!("Remote access: {e}");
            }
        }
        tracing::info!("Remote access on, endpoint {}", started.id());
        let started = Arc::new(started);
        let cancel = CancellationToken::new();
        *locked(&self.server) = Some((started.clone(), cancel.clone()));
        self.phase.send_replace(Phase::Online { relay: None, lan_only });
        // Local-only tickets are complete at once; others are saved once a
        // relay is known (or the wait runs out), then kept fresh.
        if lan_only {
            self.write_ticket(&started);
        }
        tokio::spawn(self.clone().keep_ticket_fresh(started, lan_only, cancel));
        Ok(())
    }

    fn failed(&self, detail: String) -> RemoteError {
        tracing::error!("Remote access failed to start: {detail}");
        self.phase.send_replace(Phase::Failed(detail.clone()));
        RemoteError::Start(detail)
    }

    fn write_ticket(&self, server: &RemoteServer) -> String {
        let ticket = server.ticket().encode_string();
        // Remembered for `domovoi-server pair --remote` in another process.
        let _ = std::fs::write(self.dir.join("ticket"), &ticket);
        ticket
    }

    /// Rewrites the saved ticket and the phase when the relay or the
    /// addresses change (a new network, a relay coming or going).
    async fn keep_ticket_fresh(self: Arc<Self>, server: Arc<RemoteServer>, lan_only: bool, cancel: CancellationToken) {
        if !lan_only {
            let online = tokio::select! {
                () = cancel.cancelled() => return,
                online = server.online(RELAY_WAIT) => online,
            };
            if !online {
                tracing::warn!("Remote access: no relay reached yet; peers may only find this server on the local network");
            }
        }
        let mut last = String::new();
        loop {
            let ticket = server.ticket().encode_string();
            if ticket != last {
                self.write_ticket(&server);
                last = ticket;
            }
            self.phase.send_if_modified(|phase| {
                let next = Phase::Online { relay: server.status().relay, lan_only };
                let changed = matches!(phase, Phase::Online { .. }) && *phase != next;
                if changed {
                    *phase = next;
                }
                changed
            });
            tokio::select! {
                () = cancel.cancelled() => return,
                () = tokio::time::sleep(Duration::from_secs(5)) => {}
            }
        }
    }

    /// Stops the endpoint and forgets the saved ticket; the phase is left to
    /// the caller.
    async fn halt(&self) {
        let running = locked(&self.server).take();
        if let Some((server, cancel)) = running {
            cancel.cancel();
            server.shutdown().await;
            tracing::info!("Remote access off");
        }
        let _ = std::fs::remove_file(self.dir.join("ticket"));
    }

    pub async fn stop(&self) {
        let _op = self.ops.lock().await;
        self.halt().await;
        self.phase.send_replace(Phase::Off);
    }

    /// Saves new settings and starts or stops serving to match. A request
    /// that changes nothing leaves a running endpoint (and its peers) alone.
    pub async fn configure(self: &Arc<Self>, enabled: bool, relays: Option<Vec<String>>, router: axum::Router) -> Result<(), RemoteError> {
        if let Some(relays) = &relays {
            network(relays).map_err(RemoteError::InvalidRelay)?;
        }
        let _op = self.ops.lock().await;
        let (settings, changed) = {
            let mut s = locked(&self.settings);
            let before = s.clone();
            s.enabled = enabled;
            if let Some(relays) = relays {
                if s.relays != relays {
                    s.lan_port = None;
                }
                s.relays = relays;
            }
            (s.clone(), *s != before)
        };
        if changed {
            self.save(&settings)?;
        }
        if !enabled {
            self.halt().await;
            self.phase.send_replace(Phase::Off);
            return Ok(());
        }
        if !changed && self.is_running() {
            return Ok(());
        }
        self.start_locked(router).await
    }

    /// The best ticket available: the running server's (with its relay and
    /// addresses), else the one saved by a running server in another process,
    /// else the bare endpoint id (found through n0's address lookup).
    pub async fn ticket(&self) -> String {
        if let Some(server) = self.running() {
            // Just started: give the relay a moment, so the link names it.
            if matches!(self.phase(), Phase::Online { relay: None, lan_only: false }) {
                server.online(Duration::from_secs(5)).await;
            }
            return server.ticket().encode_string();
        }
        std::fs::read_to_string(self.dir.join("ticket"))
            .ok()
            .map(|t| t.trim().to_owned())
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| self.endpoint_id().to_string())
    }

    /// Where remote access stands, with the paired `devices` and the `push`
    /// state the caller knows. `detail` adds the last start error (for the
    /// local owner only).
    pub fn status(&self, detail: bool, push: RemotePushStatus, devices: Vec<RemoteDevice>) -> RemoteAccessStatus {
        let settings = self.settings();
        let live = self.running().map(|s| s.status());
        let phase = self.phase();
        RemoteAccessStatus {
            enabled: settings.enabled,
            running: live.is_some(),
            state: match &phase {
                Phase::Off => RemoteAccessState::Off,
                Phase::Starting => RemoteAccessState::Starting,
                Phase::Online { lan_only: true, .. } => RemoteAccessState::LanOnly,
                Phase::Online { .. } => RemoteAccessState::Online,
                Phase::Failed(_) => RemoteAccessState::Error,
            },
            error: match &phase {
                Phase::Failed(e) if detail => Some(e.clone()),
                Phase::Failed(_) => Some("Remote access could not start. See the server log.".into()),
                _ => None,
            },
            endpoint_id: self.endpoint_id().to_string(),
            relays: settings.relays,
            home_relay: live.as_ref().and_then(|s| s.relay.clone()),
            direct_addresses: live.map(|s| s.direct_addresses).unwrap_or_default(),
            push,
            devices,
        }
    }

    /// Counts a pairing attempt from an iroh peer that holds no session;
    /// `false` when it must be refused. Counting happens before the attempt
    /// runs, so parallel requests cannot slip past the limit.
    pub fn allow_attempt(&self, peer: &str) -> bool {
        let mut a = locked(&self.attempts);
        let now = Instant::now();
        let fresh = |q: &mut VecDeque<Instant>| {
            while q.front().is_some_and(|t| now.duration_since(*t) > ATTEMPT_WINDOW) {
                q.pop_front();
            }
        };
        fresh(&mut a.all);
        a.by_peer.retain(|_, q| {
            fresh(q);
            !q.is_empty()
        });
        if a.all.len() >= ATTEMPTS_TOTAL || a.by_peer.get(peer).is_some_and(|q| q.len() >= ATTEMPTS_PER_PEER) {
            return false;
        }
        a.all.push_back(now);
        a.by_peer.entry(peer.to_owned()).or_default().push_back(now);
        true
    }
}

/// n0's relays, or the listed ones; `disabled` keeps traffic on the local
/// network (no relays, no address lookup; tickets carry LAN addresses).
pub fn network(relays: &[String]) -> Result<Network, String> {
    if relays.iter().any(|r| r == "disabled") {
        return Ok(Network::Direct { port: 0 });
    }
    // Debugging: no direct paths, everything through n0's relays.
    if relays.iter().any(|r| r == "relay-only") {
        return Ok(Network::RelayOnly);
    }
    let relays = relays
        .iter()
        .map(|r| r.parse::<RelayUrl>().map_err(|e| format!("relay {r}: {e}")))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Network::Internet { relays })
}

/// The server's network: as [`network`], on the saved LAN port when there is one.
fn server_network(settings: &RemoteSettings) -> Result<Network, String> {
    Ok(match network(&settings.relays)? {
        Network::Direct { .. } => Network::Direct { port: settings.lan_port.unwrap_or(0) },
        other => other,
    })
}

/// `domovoi://connect?ticket=…&code=…&name=…`
pub fn pairing_link(ticket: &str, code: &str, name: &str) -> String {
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("ticket", ticket)
        .append_pair("code", code)
        .append_pair("name", name)
        .finish();
    format!("domovoi://connect?{query}")
}

/// The parts of a pairing link, or a bare ticket (no code).
pub fn parse_link(link: &str) -> (String, Option<String>) {
    let link = link.trim();
    match link.strip_prefix("domovoi://connect?").map(|q| url::form_urlencoded::parse(q.as_bytes())) {
        Some(pairs) => {
            let (mut ticket, mut code) = (String::new(), None);
            for (k, v) in pairs {
                match k.as_ref() {
                    "ticket" => ticket = v.into_owned(),
                    "code" => code = Some(v.into_owned()),
                    _ => {}
                }
            }
            (ticket, code)
        }
        None => (link.to_owned(), None),
    }
}

/// A QR code for the terminal (two rows per character). On a dark
/// background (`dark_background`) the code's light modules are the drawn
/// blocks and its dark ones the background, so it scans the right way round.
pub fn terminal_qr(text: &str, dark_background: bool) -> Option<String> {
    use qrcode::render::unicode::Dense1x2;
    let code = qrcode::QrCode::new(text.as_bytes()).ok()?;
    let mut render = code.render::<Dense1x2>();
    render.quiet_zone(true);
    if dark_background {
        render.dark_color(Dense1x2::Light).light_color(Dense1x2::Dark);
    }
    Some(render.build())
}

/// Reads a 32-byte key (hex) or creates one, readable only by the owner.
pub fn load_or_create_key(path: &Path) -> std::io::Result<SecretKey> {
    if let Ok(hex) = std::fs::read_to_string(path)
        && let Some(bytes) = from_hex(hex.trim())
    {
        return Ok(SecretKey::from_bytes(&bytes));
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let key = SecretKey::generate();
    let hex: String = key.to_bytes().iter().map(|b| format!("{b:02x}")).collect();
    write_private(path, hex.as_bytes())?;
    Ok(key)
}

fn from_hex(s: &str) -> Option<[u8; 32]> {
    if s.len() != 64 || !s.is_ascii() {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
}

pub(crate) fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    options.mode(0o600);
    options.open(path)?.write_all(data)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_persist_and_stay_private() {
        let dir = std::env::temp_dir().join(format!("domovoi-remote-key-{}", crate::util::new_uuid()));
        let path = dir.join("secret.key");
        let a = load_or_create_key(&path).unwrap();
        let b = load_or_create_key(&path).unwrap();
        assert_eq!(a.public(), b.public());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn links_round_trip() {
        let link = pairing_link("endpointabc", "CODE 1", "My Mac & co");
        assert!(link.starts_with("domovoi://connect?ticket=endpointabc&code=CODE+1"));
        assert_eq!(parse_link(&link), ("endpointabc".into(), Some("CODE 1".into())));
        assert_eq!(parse_link(" endpointxyz "), ("endpointxyz".into(), None));
        assert!(terminal_qr(&link, false).is_some_and(|q| q.lines().count() > 10));
        // The quiet zone is light: drawn on a dark background, spaces on a light one.
        let dark = terminal_qr(&link, true).expect("renders");
        let light = terminal_qr(&link, false).expect("renders");
        assert!(dark.lines().next().is_some_and(|row| row.chars().all(|c| c == '█')), "{dark}");
        assert!(light.lines().next().is_some_and(|row| row.chars().all(|c| c == ' ')), "{light}");
    }

    #[test]
    fn relay_urls_are_validated() {
        assert!(network(&[]).is_ok());
        assert!(network(&["https://relay.example.com".into()]).is_ok());
        assert!(network(&["not a url".into()]).is_err());
        assert_eq!(network(&["disabled".into()]).unwrap(), Network::Direct { port: 0 });
        let lan = RemoteSettings { enabled: true, relays: vec!["disabled".into()], lan_port: Some(4444) };
        assert_eq!(server_network(&lan).unwrap(), Network::Direct { port: 4444 });
    }

    #[test]
    fn pairing_attempts_are_limited_per_peer_and_in_total() {
        let dir = std::env::temp_dir().join(format!("domovoi-remote-limit-{}", crate::util::new_uuid()));
        let remote = Remote::open(&dir, vec![]).unwrap();
        for _ in 0..ATTEMPTS_PER_PEER {
            assert!(remote.allow_attempt("peer-a"));
        }
        assert!(!remote.allow_attempt("peer-a"), "one peer is cut off first");
        assert!(remote.allow_attempt("peer-b"), "other peers are not");
        let mut allowed = ATTEMPTS_PER_PEER + 1;
        for i in 0..ATTEMPTS_TOTAL {
            if remote.allow_attempt(&format!("stranger-{i}")) {
                allowed += 1;
            }
        }
        assert_eq!(allowed, ATTEMPTS_TOTAL, "then everyone together");
        let _ = std::fs::remove_dir_all(dir);
    }
}
