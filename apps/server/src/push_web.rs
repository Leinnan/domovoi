//! Web Push to UnifiedPush distributors, for Android (Domovoi only; t3code
//! pushes through its relay). The same alerts as APNs (`push.rs`): when a
//! thread starts needing approval, finishes or fails, every registered
//! endpoint whose app has no socket open gets an encrypted message.
//!
//! A device registers `POST /api/domovoi/push/webpush` with its session:
//! `{ endpoint, p256dh, auth }` as a UnifiedPush distributor gave them (the
//! endpoint URL, the device's P-256 public key and auth secret, base64url);
//! `DELETE` with `{ endpoint }` removes the session's own registration, and
//! `GET` answers this server's VAPID public key (`vapidPublicKey`), which a
//! distributor may ask for. Messages are RFC 8030 POSTs to the endpoint,
//! encrypted as RFC 8291 (`aes128gcm`, one record), signed with VAPID
//! (RFC 8292, ES256) by a key the server makes on first use
//! (`<home>/webpush-vapid.p8`, owner-only). An endpoint answering 404 or 410
//! is forgotten. The plaintext is JSON: `{ environmentId, threadId, title,
//! body, requestId? }`; only the device can read it, so thread titles are
//! sent (unlike APNs' `privateAlerts` option, which does not apply here).
//!
//! Registrations live in `webpush_devices` (made here when missing, not a
//! numbered migration), one row per endpoint and session; a session that
//! expired gets nothing, and rows of sessions that are gone (revoked) are
//! dropped before each send.
//!
//! Limits: an approval's `requestId` lets the Android notification answer
//! Allow or Decline, but a question cannot be answered from it; no badge
//! counts and no Live Activity counterpart; `Urgency` is `high` for
//! approvals and `normal` otherwise; messages wait up to a day (`TTL`) at the
//! push service; no retry when an endpoint is unreachable; endpoints may be
//! plain `http` (a distributor on the local network), and a paired device
//! can point one anywhere; FCM is not supported (it needs a Google project
//! and its credentials).

#![expect(clippy::disallowed_types, clippy::disallowed_macros, reason = "the push message is JSON the device reads, and the registration bodies")]

use crate::auth::Session;
use crate::db::Db;
use crate::state::SharedState;
use axum::extract::{Extension, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use base64::Engine;
use ring::aead::{AES_128_GCM, Aad, LessSafeKey, Nonce, UnboundKey};
use ring::agreement::{ECDH_P256, EphemeralPrivateKey, UnparsedPublicKey, agree_ephemeral};
use ring::hkdf;
use ring::rand::{SecureRandom, SystemRandom};
use ring::signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair};
use rusqlite::params;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use domovoi_contracts::time_fmt::{iso_from_millis, now_millis};
use domovoi_model::typed::{RuntimeRequestKind, ShellStatus, notice, shell_status};
use domovoi_remote::RemotePeer;

/// As `push.rs`: statuses are looked at this long after a burst's first event.
const DEBOUNCE: Duration = Duration::from_millis(800);
/// Pushes in flight at once.
const SENDS_AT_ONCE: usize = 4;
/// How long a push service keeps an undelivered message (seconds).
const TTL_SECS: u32 = 24 * 60 * 60;
/// The record size written in the header (one record holds the message).
const RECORD_SIZE: u32 = 4096;
/// Thread titles are user text: the message stays well inside 4 KB.
const TITLE_CHARS: usize = 120;
/// A VAPID token's lifetime (RFC 8292: at most 24 hours).
const VAPID_LIFETIME_SECS: i64 = 12 * 60 * 60;

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// Base64url, padded or not.
fn unb64(text: &str) -> Option<Vec<u8>> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(text.trim().trim_end_matches('=')).ok()
}

// ---- RFC 8291 ----

/// `hkdf::KeyType` for an output of `n` bytes.
struct Len(usize);

impl hkdf::KeyType for Len {
    fn len(&self) -> usize {
        self.0
    }
}

fn expand(prk: &hkdf::Prk, info: &[u8], len: usize) -> Vec<u8> {
    let mut out = vec![0u8; len];
    // A length within HKDF-SHA256's limit (255 × 32) always expands.
    if let Ok(okm) = prk.expand(&[info], Len(len)) {
        let _ = okm.fill(&mut out);
    }
    out
}

/// The `aes128gcm` body for `plaintext`, from the ECDH secret the sender and
/// the device share (RFC 8291 §3.4, RFC 8188): `salt ‖ rs ‖ idlen ‖ as_public`
/// then one record, padded with its last-record delimiter.
fn encrypt_with(ecdh_secret: &[u8], auth_secret: &[u8], ua_public: &[u8], as_public: &[u8], salt: &[u8; 16], plaintext: &[u8]) -> Result<Vec<u8>, String> {
    let prk_key = hkdf::Salt::new(hkdf::HKDF_SHA256, auth_secret).extract(ecdh_secret);
    let key_info = [b"WebPush: info\0".as_slice(), ua_public, as_public].concat();
    let ikm = expand(&prk_key, &key_info, 32);
    let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, salt).extract(&ikm);
    let cek = expand(&prk, b"Content-Encoding: aes128gcm\0", 16);
    let nonce = expand(&prk, b"Content-Encoding: nonce\0", 12);
    let key = LessSafeKey::new(UnboundKey::new(&AES_128_GCM, &cek).map_err(|_| "content key")?);
    let nonce = Nonce::try_assume_unique_for_key(&nonce).map_err(|_| "nonce")?;
    let mut record = [plaintext, &[2u8]].concat();
    key.seal_in_place_append_tag(nonce, Aad::empty(), &mut record).map_err(|_| "encrypting the message")?;
    let id_len = u8::try_from(as_public.len()).map_err(|_| "key id too long")?;
    Ok([salt.as_slice(), &RECORD_SIZE.to_be_bytes(), &[id_len], as_public, &record].concat())
}

/// `plaintext` encrypted for a device with public key `ua_public` (65 bytes,
/// uncompressed P-256) and auth secret `auth_secret` (16 bytes), with a new
/// sender key and salt.
pub fn encrypt(ua_public: &[u8], auth_secret: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, String> {
    let rng = SystemRandom::new();
    let ephemeral = EphemeralPrivateKey::generate(&ECDH_P256, &rng).map_err(|_| "making a sender key")?;
    let as_public = ephemeral.compute_public_key().map_err(|_| "the sender key's public half")?;
    let mut salt = [0u8; 16];
    rng.fill(&mut salt).map_err(|_| "making a salt")?;
    let ecdh_secret = agree_ephemeral(ephemeral, &UnparsedPublicKey::new(&ECDH_P256, ua_public), |secret| secret.to_vec())
        .map_err(|_| "the device's key is not a P-256 point")?;
    encrypt_with(&ecdh_secret, auth_secret, ua_public, as_public.as_ref(), &salt, plaintext)
}

// ---- VAPID ----

/// This server's VAPID key (RFC 8292), made on first use.
pub struct Vapid {
    key: EcdsaKeyPair,
    rng: SystemRandom,
}

impl Vapid {
    /// The key at `path`, or a new one written there (owner-only).
    pub fn load_or_create(path: &Path) -> Result<Vapid, String> {
        let rng = SystemRandom::new();
        let der = match std::fs::read(path) {
            Ok(der) => der,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).map_err(|_| "making the VAPID key")?;
                write_owner_only(path, pkcs8.as_ref()).map_err(|e| format!("writing {}: {e}", path.display()))?;
                pkcs8.as_ref().to_vec()
            }
            Err(e) => return Err(format!("reading {}: {e}", path.display())),
        };
        let key = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &der, &rng).map_err(|e| format!("{} is not a P-256 key: {e}", path.display()))?;
        Ok(Vapid { key, rng })
    }

    /// The public key, base64url (`applicationServerKey`).
    pub fn public_key(&self) -> String {
        b64(self.key.public_key().as_ref())
    }

    /// `Authorization` for a push to `endpoint`: `vapid t=<jwt>, k=<key>`.
    fn authorization(&self, endpoint: &str, now_secs: i64) -> Result<String, String> {
        let header = b64(json!({ "typ": "JWT", "alg": "ES256" }).to_string().as_bytes());
        let claims = b64(json!({ "aud": origin(endpoint), "exp": now_secs + VAPID_LIFETIME_SECS, "sub": "mailto:push@domovoi.invalid" }).to_string().as_bytes());
        let input = format!("{header}.{claims}");
        let signature = self.key.sign(&self.rng, input.as_bytes()).map_err(|_| "signing the VAPID token")?;
        Ok(format!("vapid t={input}.{}, k={}", b64(signature.as_ref()), self.public_key()))
    }
}

fn write_owner_only(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    crate::util::create_private(path, true)?.write_all(bytes)
}

/// `scheme://host[:port]` of a URL (VAPID's audience).
fn origin(url: &str) -> &str {
    let rest = url.find("://").map_or(0, |at| at + 3);
    url[rest..].find('/').map_or(url, |end| &url[..rest + end])
}

// ---- devices ----

/// An endpoint to push to, with the device's keys and the sessions that registered it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebPushDevice {
    pub endpoint: String,
    pub p256dh: String,
    pub auth: String,
    pub sessions: Vec<String>,
}

fn ensure_table(db: &Db) -> rusqlite::Result<()> {
    db.with(|c| {
        c.execute_batch(
            "CREATE TABLE IF NOT EXISTS webpush_devices (
                endpoint TEXT NOT NULL, session_hash TEXT NOT NULL, p256dh TEXT NOT NULL, auth TEXT NOT NULL, created_at TEXT NOT NULL,
                PRIMARY KEY (endpoint, session_hash));
             CREATE INDEX IF NOT EXISTS webpush_devices_session ON webpush_devices (session_hash);",
        )
    })
}

pub fn register(db: &Db, endpoint: &str, p256dh: &str, auth: &str, session_hash: &str) -> rusqlite::Result<()> {
    ensure_table(db)?;
    db.with(|c| {
        c.execute(
            "INSERT INTO webpush_devices (endpoint, session_hash, p256dh, auth, created_at) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(endpoint, session_hash) DO UPDATE SET p256dh = excluded.p256dh, auth = excluded.auth",
            params![endpoint, session_hash, p256dh, auth, iso_from_millis(now_millis())],
        )
        .map(drop)
    })
}

pub fn unregister(db: &Db, endpoint: &str, session_hash: &str) -> rusqlite::Result<()> {
    ensure_table(db)?;
    db.with(|c| c.execute("DELETE FROM webpush_devices WHERE endpoint = ?1 AND session_hash = ?2", params![endpoint, session_hash]).map(drop))
}

/// Forgets an endpoint its push service no longer knows, whoever registered it.
fn forget_endpoint(db: &Db, endpoint: &str) -> rusqlite::Result<()> {
    db.with(|c| c.execute("DELETE FROM webpush_devices WHERE endpoint = ?1", params![endpoint]).map(drop))
}

/// Endpoints with at least one live session, one entry per endpoint.
pub fn devices(db: &Db) -> rusqlite::Result<Vec<WebPushDevice>> {
    ensure_table(db)?;
    db.with(|c| c.execute("DELETE FROM webpush_devices WHERE session_hash NOT IN (SELECT token_hash FROM auth_sessions)", []).map(drop))?;
    let now = now_millis();
    let rows: Vec<(String, String, String, String)> = db.with(|c| {
        let mut st = c.prepare(
            "SELECT d.endpoint, d.session_hash, d.p256dh, d.auth FROM webpush_devices d
             JOIN auth_sessions s ON s.token_hash = d.session_hash WHERE s.expires_at_ms > ?1 ORDER BY d.endpoint",
        )?;
        let rows = st.query_map(params![now], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
        rows.collect()
    })?;
    let mut out: Vec<WebPushDevice> = Vec::new();
    for (endpoint, session, p256dh, auth) in rows {
        match out.last_mut() {
            Some(last) if last.endpoint == endpoint => last.sessions.push(session),
            _ => out.push(WebPushDevice { endpoint, p256dh, auth, sessions: vec![session] }),
        }
    }
    Ok(out)
}

/// What a registration must be: an http(s) URL and the device's keys.
fn valid(endpoint: &str, p256dh: &str, auth: &str) -> bool {
    let url_ok = (endpoint.starts_with("https://") || endpoint.starts_with("http://")) && endpoint.len() <= 2048 && !endpoint.chars().any(char::is_whitespace);
    let key_ok = unb64(p256dh).is_some_and(|k| k.len() == 65 && k[0] == 4);
    let auth_ok = unb64(auth).is_some_and(|a| a.len() == 16);
    url_ok && key_ok && auth_ok
}

// ---- routes ----

#[derive(Deserialize)]
struct Registration {
    endpoint: String,
    #[serde(default)]
    p256dh: String,
    #[serde(default)]
    auth: String,
}

/// `/api/domovoi/push/webpush`, merged into the HTTP router.
pub fn routes() -> Router<SharedState> {
    Router::new().route("/api/domovoi/push/webpush", post(register_route).delete(unregister_route).get(key_route))
}

fn reject(status: StatusCode, code: &str) -> Response {
    (status, Json(json!({ "error": code }))).into_response()
}

/// The request's session, with `orchestration:read` (as APNs registration).
fn session(state: &SharedState, headers: &HeaderMap, peer: Option<&Extension<RemotePeer>>) -> Result<Session, Response> {
    let peer = peer.map(|Extension(RemotePeer(id))| id.to_string());
    match state.auth.authenticate(headers, peer.as_deref()) {
        Ok(Some(s)) if s.permissions.iter().any(|p| p == "orchestration:read") => Ok(s),
        Ok(Some(_)) => Err(reject(StatusCode::FORBIDDEN, "insufficient_scope")),
        _ => Err(reject(StatusCode::UNAUTHORIZED, "unauthorized")),
    }
}

/// The VAPID key, made when missing.
fn vapid(state: &SharedState) -> Result<Vapid, String> {
    Vapid::load_or_create(&state.paths.home.join("webpush-vapid.p8"))
}

async fn key_route(State(state): State<SharedState>, headers: HeaderMap, peer: Option<Extension<RemotePeer>>) -> Response {
    if let Err(r) = session(&state, &headers, peer.as_ref()) {
        return r;
    }
    match vapid(&state) {
        Ok(v) => Json(json!({ "vapidPublicKey": v.public_key() })).into_response(),
        Err(e) => {
            tracing::error!("web push: {e}");
            reject(StatusCode::INTERNAL_SERVER_ERROR, "internal_error")
        }
    }
}

async fn register_route(State(state): State<SharedState>, headers: HeaderMap, peer: Option<Extension<RemotePeer>>, Json(body): Json<Registration>) -> Response {
    let session = match session(&state, &headers, peer.as_ref()) {
        Ok(s) => s,
        Err(r) => return r,
    };
    if !valid(&body.endpoint, &body.p256dh, &body.auth) {
        return reject(StatusCode::BAD_REQUEST, "invalid_request");
    }
    let key = match vapid(&state) {
        Ok(v) => v.public_key(),
        Err(e) => {
            tracing::error!("web push: {e}");
            return reject(StatusCode::INTERNAL_SERVER_ERROR, "internal_error");
        }
    };
    match register(&state.store.db(), body.endpoint.trim(), body.p256dh.trim(), body.auth.trim(), &session.token_hash) {
        Ok(()) => Json(json!({ "registered": true, "vapidPublicKey": key })).into_response(),
        Err(e) => {
            tracing::error!("web push registration: {e}");
            reject(StatusCode::INTERNAL_SERVER_ERROR, "internal_error")
        }
    }
}

async fn unregister_route(State(state): State<SharedState>, headers: HeaderMap, peer: Option<Extension<RemotePeer>>, Json(body): Json<Registration>) -> Response {
    let session = match session(&state, &headers, peer.as_ref()) {
        Ok(s) => s,
        Err(r) => return r,
    };
    match unregister(&state.store.db(), body.endpoint.trim(), &session.token_hash) {
        Ok(()) => Json(json!({ "registered": false })).into_response(),
        Err(e) => {
            tracing::error!("web push: {e}");
            reject(StatusCode::INTERNAL_SERVER_ERROR, "internal_error")
        }
    }
}

// ---- sending ----

/// What became of a push.
#[derive(Debug, PartialEq, Eq)]
pub enum Delivery {
    Sent,
    /// The push service no longer knows the endpoint: forget it.
    Gone(u16),
    Failed(String),
}

/// A Web Push sender: VAPID-signed, encrypted POSTs.
pub struct WebPush {
    client: reqwest::Client,
    vapid: Vapid,
}

impl WebPush {
    pub fn new(vapid: Vapid) -> Result<WebPush, String> {
        // reqwest's rustls backend comes without a crypto provider (see main).
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = reqwest::Client::builder().timeout(Duration::from_secs(20)).build().map_err(|e| e.to_string())?;
        Ok(WebPush { client, vapid })
    }

    /// Sends `message` to `device`; `topic` replaces an undelivered message
    /// with the same topic, `urgent` asks the service to wake the device.
    pub async fn deliver(&self, device: &WebPushDevice, message: &Value, topic: &str, urgent: bool) -> Delivery {
        let (Some(ua_public), Some(auth)) = (unb64(&device.p256dh), unb64(&device.auth)) else {
            return Delivery::Failed("the registration's keys are not base64url".into());
        };
        let body = match encrypt(&ua_public, &auth, message.to_string().as_bytes()) {
            Ok(body) => body,
            Err(e) => return Delivery::Failed(e),
        };
        let authorization = match self.vapid.authorization(&device.endpoint, now_millis() / 1000) {
            Ok(a) => a,
            Err(e) => return Delivery::Failed(e),
        };
        let sent = self
            .client
            .post(&device.endpoint)
            .header("TTL", TTL_SECS.to_string())
            .header("Content-Encoding", "aes128gcm")
            .header("Content-Type", "application/octet-stream")
            .header("Urgency", if urgent { "high" } else { "normal" })
            .header("Topic", topic)
            .header("Authorization", authorization)
            .body(body)
            .send()
            .await;
        match sent {
            Ok(r) if r.status().is_success() => Delivery::Sent,
            Ok(r) if matches!(r.status().as_u16(), 404 | 410) => Delivery::Gone(r.status().as_u16()),
            Ok(r) => Delivery::Failed(format!("HTTP {}", r.status())),
            Err(e) => Delivery::Failed(e.to_string()),
        }
    }
}

/// The message for a thread that needs the user (or finished, or failed).
pub fn message(environment_id: &str, thread_id: &str, title: &str, body: &str, request_id: Option<&str>) -> Value {
    let title: String = title.chars().take(TITLE_CHARS).collect();
    let mut m = json!({ "environmentId": environment_id, "threadId": thread_id, "title": title, "body": body });
    if let Some(id) = request_id {
        m["requestId"] = json!(id);
    }
    m
}

/// `Topic` (at most 32 base64url characters) for a thread: one waiting
/// message per thread at the push service.
fn topic(thread_id: &str) -> String {
    crate::util::sha256_hex(thread_id.as_bytes())[..32].to_owned()
}

/// Watches thread events and pushes status changes to registered endpoints,
/// as `push::spawn` does for APNs.
pub fn spawn(state: SharedState) {
    tokio::spawn(async move {
        let sender = match vapid(&state).and_then(WebPush::new) {
            Ok(s) => Arc::new(s),
            Err(e) => {
                tracing::warn!("web push off: {e}");
                return;
            }
        };
        let sends = Arc::new(tokio::sync::Semaphore::new(SENDS_AT_ONCE));
        let mut events = state.store.subscribe();
        let mut statuses: HashMap<String, ShellStatus> = {
            let state = state.clone();
            tokio::task::spawn_blocking(move || seed(&state)).await.unwrap_or_default()
        };
        let mut dirty: HashSet<String> = HashSet::new();
        let mut due: Option<tokio::time::Instant> = None;
        loop {
            let wait = async {
                match due {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                () = state.shutdown.cancelled() => return,
                event = events.recv() => match event {
                    Ok(e) if e.aggregate_kind == "thread" => {
                        dirty.insert(e.aggregate_id.clone());
                        due.get_or_insert_with(|| tokio::time::Instant::now() + DEBOUNCE);
                    }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        dirty.extend(statuses.keys().cloned());
                        dirty.extend(state.store.thread_ids().unwrap_or_default().into_iter().map(|(id, _)| id));
                        due.get_or_insert_with(|| tokio::time::Instant::now() + DEBOUNCE);
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                },
                () = wait => {
                    due = None;
                    for thread_id in std::mem::take(&mut dirty) {
                        check(&state, &sender, &sends, &mut statuses, &thread_id);
                    }
                }
            }
        }
    });
}

fn seed(state: &SharedState) -> HashMap<String, ShellStatus> {
    let mut out = HashMap::new();
    for (id, _) in state.store.thread_ids().unwrap_or_default() {
        if let Ok(Some(shell)) = crate::orchestration::projection::thread_shell(&state.store, &id) {
            out.insert(id, shell_status(&shell));
        }
    }
    out
}

fn check(state: &SharedState, sender: &Arc<WebPush>, sends: &Arc<tokio::sync::Semaphore>, statuses: &mut HashMap<String, ShellStatus>, thread_id: &str) {
    let Ok(Some(shell)) = crate::orchestration::projection::thread_shell(&state.store, thread_id) else { return };
    let status = shell_status(&shell);
    let previous = statuses.insert(thread_id.to_owned(), status).unwrap_or(ShellStatus::Idle);
    let Some(body) = notice(Some(previous), status) else { return };
    let pending = shell.pending_runtime_request.as_ref().filter(|p| status == ShellStatus::Approval && p.kind != RuntimeRequestKind::UserInput);
    let urgent = status == ShellStatus::Approval;
    let message = Arc::new(message(&state.environment.environment_id, thread_id, &shell.title, body, pending.map(|p| p.id.as_str())));
    let topic = topic(thread_id);
    let db = state.store.db();
    for device in devices(&db).unwrap_or_default() {
        // An open app (any of the device's sessions has a socket) notifies itself.
        if device.sessions.iter().any(|s| state.connections.is_live(s)) {
            continue;
        }
        let (sender, sends, message, db, topic, thread_id) = (sender.clone(), sends.clone(), message.clone(), db.clone(), topic.clone(), thread_id.to_owned());
        tokio::spawn(async move {
            let Ok(_permit) = sends.acquire().await else { return };
            match sender.deliver(&device, &message, &topic, urgent).await {
                Delivery::Sent => tracing::info!(thread = %thread_id, "web push sent: {body}"),
                Delivery::Gone(status) => {
                    tracing::info!("web push endpoint dropped (HTTP {status})");
                    let _ = forget_endpoint(&db, &device.endpoint);
                }
                Delivery::Failed(e) => tracing::warn!("web push failed: {e}"),
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(text: &str) -> Vec<u8> {
        unb64(text).expect("base64url")
    }

    /// RFC 8291 Appendix A: from its shared secret, keys and salt, the
    /// header and ciphertext it prints.
    #[test]
    fn encryption_matches_rfc_8291_appendix_a() {
        let as_public = d("BP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A8");
        let ua_public = d("BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4");
        let salt: [u8; 16] = d("DGv6ra1nlYgDCS1FRnbzlw").try_into().expect("16 bytes");
        let auth = d("BTBZMqHH6r4Tts7J_aSIgg");
        let ecdh = d("kyrL1jIIOHEzg3sM2ZWRHDRB62YACZhhSlknJ672kSs");
        let plaintext = d("V2hlbiBJIGdyb3cgdXAsIEkgd2FudCB0byBiZSBhIHdhdGVybWVsb24");
        let body = encrypt_with(&ecdh, &auth, &ua_public, &as_public, &salt, &plaintext).expect("encrypted");
        let header = d("DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A8");
        assert_eq!(&body[..header.len()], header.as_slice());
        assert_eq!(b64(&body[header.len()..]), "8pfeW0KbunFT06SuDKoJH9Ql87S1QUrdirN6GcG7sFz1y1sqLgVi1VhjVkHsUoEsbI_0LpXMuGvnzQ");
    }

    #[test]
    fn registrations_are_checked_and_vapid_signs_for_the_endpoints_origin() {
        let key = b64(&[[4u8].as_slice(), &[7u8; 64]].concat());
        let auth = b64(&[1u8; 16]);
        assert!(valid("https://ntfy.sh/upAbc?up=1", &key, &auth));
        assert!(!valid("ftp://x", &key, &auth) && !valid("https://x", &auth, &auth) && !valid("https://x", &key, &key));
        assert_eq!(origin("https://push.example.org:8443/wpush/v2/abc"), "https://push.example.org:8443");
        assert_eq!(topic("6421f1e6-14f6-4d7a-89b3-6e82dac38210").len(), 32);

        let dir = std::env::temp_dir().join(format!("domovoi-vapid-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("webpush-vapid.p8");
        let vapid = Vapid::load_or_create(&path).expect("made");
        assert_eq!(Vapid::load_or_create(&path).expect("read again").public_key(), vapid.public_key(), "the same key next time");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).expect("file").permissions().mode() & 0o777, 0o600);
        }
        let auth = vapid.authorization("https://push.example.org/wpush/abc", 1000).expect("signed");
        let token = auth.strip_prefix("vapid t=").and_then(|rest| rest.split(", k=").next()).expect("vapid t=…, k=…");
        let claims: Value = serde_json::from_slice(&d(token.split('.').nth(1).expect("claims"))).expect("JSON");
        assert_eq!(claims["aud"], "https://push.example.org");
        assert_eq!(claims["exp"], 1000 + VAPID_LIFETIME_SECS);
        // The signature verifies with the public key the token names.
        let (input, signature) = token.rsplit_once('.').expect("signed token");
        let public = d(auth.rsplit("k=").next().expect("key"));
        ring::signature::UnparsedPublicKey::new(&ring::signature::ECDSA_P256_SHA256_FIXED, public).verify(input.as_bytes(), &d(signature)).expect("valid ES256");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
