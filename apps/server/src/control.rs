//! What the status bar icon, the `domovoi-server remote …` commands and the
//! `/api/domovoi/…` routes share: one status snapshot and one set of actions, so
//! the three cannot drift apart.

use crate::auth::{AuthError, Grant, session_id};
use crate::remote::{Phase, RemoteError};
use crate::state::{AppState, SharedState};
use rusqlite::params;
use std::time::Duration;
use crate::time_fmt::{iso_from_millis, now_millis};
use domovoi_model::typed::{RemoteAccessStatus, RemoteDevice, RemotePairingLink, RemotePushStatus};
use tokio::sync::watch;

/// The label the command-line tools pair with; their short-lived sessions
/// are not devices.
pub const CLI_LABEL: &str = "domovoi-server CLI";

/// A paired session as the status lists it: its public id (what revoke
/// takes), label and kind, whether it was paired over iroh (bound to a device
/// key) rather than local TCP, and whether it is connected.
pub type Device = RemoteDevice;

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ServerStatus {
    pub phase: Phase,
    pub enabled: bool,
    pub endpoint_id: String,
    pub devices: Vec<Device>,
    pub push_configured: bool,
    pub push_devices: usize,
}

#[cfg_attr(not(feature = "tray"), expect(dead_code, reason = "the status bar icon (the `tray` feature) is what uses it"))]
impl ServerStatus {
    pub fn remote_devices(&self) -> usize {
        self.devices.iter().filter(|d| d.remote).count()
    }

    /// One line for people, e.g. "Remote access on · 2 devices, 1 connected".
    pub fn summary(&self) -> String {
        let devices = self.remote_devices();
        let counts = match devices {
            0 => "no devices paired".to_owned(),
            n => format!("{n} device{}, {} connected", if n == 1 { "" } else { "s" }, self.devices.iter().filter(|d| d.remote && d.connected).count()),
        };
        match &self.phase {
            Phase::Off => "Remote access off".into(),
            Phase::Starting => "Remote access starting…".into(),
            Phase::Online { lan_only: true, .. } => format!("Remote access on (local network) · {counts}"),
            Phase::Online { relay: None, .. } => format!("Remote access on (no relay yet) · {counts}"),
            Phase::Online { .. } => format!("Remote access on · {counts}"),
            Phase::Failed(_) => "Remote access failed to start".into(),
        }
    }
}

/// Paired sessions, oldest first.
pub fn devices(state: &AppState) -> Vec<Device> {
    devices_in(&state.store.db(), |hash| (state.connections.is_live(hash), state.connections.last_seen(hash)))
}

/// As [`devices`], straight from a database; `live` says whether a session
/// (by token hash) has a socket open and when it was last seen.
pub fn devices_in(db: &crate::db::Db, live: impl Fn(&str) -> (bool, Option<i64>)) -> Vec<Device> {
    let now = now_millis();
    type Row = (String, Option<String>, String);
    let rows: Vec<Row> = db
        .with(|c| {
            let mut st = c.prepare("SELECT token_hash, bound_endpoint, client FROM auth_sessions WHERE expires_at_ms > ?1 ORDER BY created_at")?;
            let rows = st.query_map(params![now], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
            rows.collect()
        })
        .unwrap_or_default();
    rows.into_iter()
        .filter_map(|(hash, bound, client)| {
            // What the client said of itself when it paired.
            let meta: crate::auth::ClientMeta = serde_json::from_str(&client).unwrap_or_default();
            let label = meta.label;
            if label.as_deref() == Some(CLI_LABEL) {
                return None;
            }
            let (connected, last_seen) = live(&hash);
            Some(Device {
                id: session_id(&hash),
                label: label.unwrap_or_else(|| "Device".into()),
                kind: meta.device_type.as_deref().unwrap_or("unknown").into(),
                remote: bound.is_some(),
                connected,
                last_seen: last_seen.map(|seen| iso_from_millis(seen).into()),
            })
        })
        .collect()
}

pub fn snapshot(state: &AppState) -> ServerStatus {
    ServerStatus {
        phase: state.remote.phase(),
        enabled: state.remote.settings().enabled,
        endpoint_id: state.remote.endpoint_id().to_string(),
        devices: devices(state),
        push_configured: state.push_configured,
        push_devices: crate::push::device_count(&state.store.db()),
    }
}

/// Keeps `state.status` current: on every pulse (a pairing, a revoke, a
/// socket opening or closing), on remote access changing phase, and every
/// few seconds regardless.
pub fn spawn_status(state: SharedState) {
    tokio::spawn(async move {
        let mut phase = state.remote.watch();
        loop {
            let next = snapshot(&state);
            state.status.send_if_modified(|current| {
                let changed = *current != next;
                if changed {
                    *current = next;
                }
                changed
            });
            tokio::select! {
                () = state.shutdown.cancelled() => return,
                () = state.pulse.notified() => {}
                _ = phase.changed() => {}
                () = tokio::time::sleep(Duration::from_secs(10)) => {}
            }
            // Bursts (a socket closing and reopening) settle into one update.
            tokio::time::sleep(Duration::from_millis(80)).await;
        }
    });
}

/// Housekeeping every few minutes: sessions with a socket open count as in
/// use (and renew), expired ones are deleted along with their sockets and
/// push devices, and leftovers of sessions that are gone are dropped.
pub fn spawn_maintenance(state: SharedState) {
    tokio::spawn(async move {
        loop {
            sweep(&state);
            tokio::select! {
                () = state.shutdown.cancelled() => return,
                () = tokio::time::sleep(Duration::from_secs(300)) => {}
            }
        }
    });
}

/// How long a deleted thread's history is kept, and command receipts (which
/// only answer a client retrying the same command).
const DELETED_THREAD_RETENTION_DAYS: i64 = 30;
const RECEIPT_RETENTION_DAYS: i64 = 7;

pub fn sweep(state: &AppState) {
    let now = crate::time_fmt::now_millis();
    let ago = |days: i64| crate::time_fmt::iso_from_millis(now - days * 86_400_000);
    match state.store.prune(&ago(DELETED_THREAD_RETENTION_DAYS), &ago(RECEIPT_RETENTION_DAYS)) {
        Ok((0, 0, 0)) => {}
        Ok((events, entities, receipts)) => tracing::info!(events, entities, receipts, "pruned the event log"),
        Err(e) => tracing::warn!("event log pruning failed: {e}"),
    }
    match state.store.trim_event_log(crate::orchestration::events::EVENT_LOG_KEEPS) {
        Ok(0) => {}
        Ok(events) => tracing::debug!(events, "trimmed the event log"),
        Err(e) => tracing::warn!("event log trimming failed: {e}"),
    }
    for hash in state.connections.live_hashes() {
        let _ = state.auth.session_for_hash(&hash);
    }
    match state.auth.purge_expired() {
        Ok(gone) if !gone.is_empty() => {
            tracing::info!("{} expired session(s) removed", gone.len());
            forget(state, &gone);
        }
        Ok(_) => {}
        Err(e) => tracing::warn!("session cleanup failed: {e}"),
    }
    if let Ok(live) = state.auth.live_hashes() {
        state.connections.retain_seen(|hash| live.contains(hash));
        // A socket whose session is gone (deleted behind our back) closes too.
        for hash in state.connections.live_hashes() {
            if !live.contains(&hash) {
                state.connections.close_all(&hash);
            }
        }
        let _ = crate::push::forget_orphans(&state.store.db());
    }
}

/// `/api/domovoi/remote` and `domovoi-server remote status`. `detail` (the local
/// owner) adds the last start error and device ids.
pub fn remote_status(state: &AppState, detail: bool) -> RemoteAccessStatus {
    let push = RemotePushStatus { configured: state.push_configured, devices: crate::push::device_count(&state.store.db()) as u64 };
    state.remote.status(detail, push, devices(state))
}

pub async fn set_remote(state: &SharedState, enabled: bool, relays: Option<Vec<String>>) -> Result<(), RemoteError> {
    let result = state.remote.configure(enabled, relays, crate::http::router(state.clone())).await;
    state.pulse.notify_one();
    result
}

/// A one-time code for a new device, as the link that carries it.
pub type PairingLink = RemotePairingLink;

/// A one-time code granting `grant`, as a `domovoi://connect` link.
pub async fn pairing_link(state: &AppState, ttl: Duration, grant: &Grant) -> Result<PairingLink, AuthError> {
    let ttl = ttl.min(crate::auth::MAX_PAIRING_TTL);
    let code = state.auth.issue_pairing_code_with(ttl, grant)?;
    let ticket = state.remote.ticket().await;
    Ok(PairingLink {
        link: crate::remote::pairing_link(&ticket, &code, &state.environment.label),
        code,
        ticket,
        endpoint_id: state.remote.endpoint_id().to_string(),
        expires_at: iso_from_millis(now_millis() + ttl.as_millis() as i64).into(),
    })
}

pub enum Revoked {
    Done,
    NotFound,
    /// A remote device may not sign out a session on the machine itself.
    Forbidden,
}

/// Signs a session out: its row, its open sockets and its push devices.
/// `remote_only` (the caller is a remote device) spares local sessions.
pub fn revoke(state: &AppState, id: &str, remote_only: bool) -> Result<Revoked, AuthError> {
    let outcome = match state.auth.revoke(id, remote_only)? {
        crate::auth::Revoke::Done(hash) => {
            forget(state, std::slice::from_ref(&hash));
            Revoked::Done
        }
        crate::auth::Revoke::NotFound => Revoked::NotFound,
        crate::auth::Revoke::Forbidden => Revoked::Forbidden,
    };
    Ok(outcome)
}

/// Signs out every session but `keep` (a token hash); returns how many.
pub fn revoke_others(state: &AppState, keep: &str, remote_only: bool) -> Result<usize, AuthError> {
    let hashes = state.auth.revoke_others(keep, remote_only)?;
    forget(state, &hashes);
    Ok(hashes.len())
}

/// Closes the sockets and drops the push devices of ended sessions.
pub fn forget(state: &AppState, hashes: &[String]) {
    for hash in hashes {
        state.connections.close_all(hash);
    }
    let _ = crate::push::forget_sessions(&state.store.db(), hashes);
    state.pulse.notify_one();
}

/// The in-process handle the status bar icon drives the server with.
#[derive(Clone)]
#[cfg_attr(not(feature = "tray"), expect(dead_code, reason = "the status bar icon (the `tray` feature) is what uses it"))]
pub struct Control {
    state: SharedState,
    /// What "Launch at Login" would run.
    login_entry: Option<crate::autostart::Entry>,
}

#[cfg_attr(not(feature = "tray"), expect(dead_code, reason = "the status bar icon (the `tray` feature) is what uses it"))]
impl Control {
    pub fn new(state: SharedState, login_entry: Option<crate::autostart::Entry>) -> Self {
        Control { state, login_entry }
    }

    pub fn launch_at_login(&self) -> bool {
        crate::autostart::is_enabled()
    }

    /// `Ok(Some(note))`: on, but the user has something left to do.
    pub fn set_launch_at_login(&self, on: bool) -> Result<Option<String>, String> {
        match (on, &self.login_entry) {
            (true, Some(entry)) => crate::autostart::enable(entry),
            (true, None) => Err("this executable's path is unknown".into()),
            (false, _) => crate::autostart::disable().map(|()| None),
        }
    }

    pub fn status(&self) -> watch::Receiver<ServerStatus> {
        self.state.status.subscribe()
    }

    pub async fn set_remote(&self, enabled: bool) -> Result<(), String> {
        set_remote(&self.state, enabled, None).await.map_err(|e| e.to_string())
    }

    /// A full-access link, valid ten minutes: the icon belongs to the owner.
    pub async fn pairing_link(&self) -> Result<PairingLink, String> {
        pairing_link(&self.state, Duration::from_secs(600), &Grant::full()).await.map_err(|e| e.to_string())
    }

    pub fn revoke(&self, id: &str) -> Result<bool, String> {
        revoke(&self.state, id, false).map(|r| matches!(r, Revoked::Done)).map_err(|e| e.to_string())
    }

    pub fn logs_dir(&self) -> std::path::PathBuf {
        self.state.paths.logs.clone()
    }

    pub fn home_dir(&self) -> std::path::PathBuf {
        self.state.paths.home.clone()
    }

    pub fn shutdown(&self) {
        self.state.shutdown.cancel();
    }
}
