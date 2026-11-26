//! Signing in: a one-time pairing code is exchanged for a session, whose
//! bearer token the client keeps (RFC 8693 token exchange, `/oauth/token`).
//! It opens the RPC WebSocket with the token, or with a short-lived ticket
//! where it cannot attach a header to the upgrade. There are no cookies: a
//! web page has nothing of a signed-in client's to ride on.
//!
//! A session created by an iroh peer is bound to that peer's endpoint id: it
//! works only on connections from the same key, never from plain TCP, and
//! sessions created over TCP never work over iroh. A leaked token is useless
//! without the device's iroh secret key.
//!
//! Only token hashes are stored.

use crate::db::Db;
use crate::util::{pairing_code, random_bytes, sha256_hex};
use axum::http::HeaderMap;
use base64::Engine;
use rusqlite::{OptionalExtension, params};
use std::collections::HashMap;
use std::sync::Mutex;
use std::time::Duration;
use domovoi_contracts::auth::{AuthDescriptor, AuthSessionState, DEFAULT_PERMISSIONS, DEFAULT_SCOPES, strings};
use domovoi_contracts::time_fmt::{iso_from_millis, now_millis};

const SESSION_TTL_MS: i64 = 30 * 24 * 60 * 60 * 1000;
/// A session in use is extended once less than this remains, so a device
/// that keeps coming back never has to pair again.
const SESSION_RENEW_BELOW_MS: i64 = 20 * 24 * 60 * 60 * 1000;
/// A WebSocket ticket is redeemed right after it is minted.
const WS_TICKET_TTL_MS: i64 = 60 * 1000;

/// How every session is presented. (A home from before holds sessions of
/// the web client, `browser-session-cookie`, until they expire: no request
/// can present one.)
pub const METHOD_BEARER: &str = "bearer-access-token";
/// The startup pairing code stays valid for a working day; ad-hoc codes for minutes.
pub const STARTUP_PAIRING_TTL: Duration = Duration::from_secs(24 * 60 * 60);
/// The longest a pairing code may live (a week).
pub const MAX_PAIRING_TTL: Duration = Duration::from_secs(7 * 24 * 60 * 60);

#[derive(Debug, Clone)]
pub struct Session {
    pub subject: String,
    pub scopes: Vec<String>,
    pub permissions: Vec<String>,
    pub expires_at: String,
    /// Hash of the session token (the `auth_sessions` key).
    pub token_hash: String,
    /// [`METHOD_BEARER`].
    pub method: String,
    /// The iroh endpoint id the session is bound to, if created over iroh.
    pub bound_endpoint: Option<String>,
}

/// What a client says about itself when pairing (t3code
/// `AuthClientPresentationMetadata`), shown in the access list.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ClientMeta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub device_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub os: Option<String>,
}

/// How a pairing code becomes a session.
#[derive(Debug, Clone, Default)]
pub struct NewSession {
    pub client: ClientMeta,
    /// The iroh peer exchanging the code, if any.
    pub peer: Option<String>,
    /// OAuth `scope`: narrows the grant to these scopes and permissions.
    pub scope: Option<Vec<String>>,
}

pub struct Auth {
    db: Db,
    /// WebSocket tickets: hash -> (session token hash, expiry ms). Single use.
    tickets: Mutex<HashMap<String, (String, i64)>>,
    /// Pairing attempts over plain TCP, by address.
    tcp_pairing: Mutex<TcpPairing>,
}

/// The window the pairing budgets over plain TCP are counted in.
const TCP_PAIRING_WINDOW: Duration = Duration::from_secs(60);
/// Failed pairing attempts one address may make in a window before its
/// wrong codes are answered with 429 instead of being tried.
const TCP_FAILURES_PER_ADDRESS: usize = 10;
/// The same, for every address together.
const TCP_FAILURES_TOTAL: usize = 100;
/// With the failure budget spent, how many codes an address may still have
/// looked at in a window (a live one is let through).
const TCP_CHECKS_PER_ADDRESS: usize = 120;
/// The same, for every address together.
const TCP_CHECKS_TOTAL: usize = 600;

/// What [`Auth::begin_tcp_pairing`] says of a pairing attempt over plain TCP.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TcpAttempt {
    /// Within the budget: it runs, and counts as a failure until
    /// [`Auth::tcp_pairing_succeeded`] says otherwise.
    Counted,
    /// The budget is spent, but the code is a live one: it runs.
    Live,
    /// The budget is spent: answered with 429.
    Refused,
}

/// When each counted attempt was made, in all and per key.
#[derive(Default)]
struct Window {
    all: std::collections::VecDeque<std::time::Instant>,
    by_address: HashMap<std::net::IpAddr, std::collections::VecDeque<std::time::Instant>>,
}

impl Window {
    fn forget_old(&mut self, now: std::time::Instant) {
        let fresh = |q: &mut std::collections::VecDeque<std::time::Instant>| {
            while q.front().is_some_and(|t| now.duration_since(*t) > TCP_PAIRING_WINDOW) {
                q.pop_front();
            }
        };
        fresh(&mut self.all);
        self.by_address.retain(|_, q| {
            fresh(q);
            !q.is_empty()
        });
    }

    fn spent(&self, address: std::net::IpAddr, per_address: usize, total: usize) -> bool {
        self.all.len() >= total || self.by_address.get(&address).is_some_and(|q| q.len() >= per_address)
    }

    fn count(&mut self, address: std::net::IpAddr, now: std::time::Instant) {
        self.all.push_back(now);
        self.by_address.entry(address).or_default().push_back(now);
    }

    /// Takes back the newest count of `address`.
    fn uncount(&mut self, address: std::net::IpAddr) {
        let Some(at) = self.by_address.get_mut(&address).and_then(|q| q.pop_back()) else { return };
        if let Some(i) = self.all.iter().rposition(|t| *t == at) {
            self.all.remove(i);
        }
    }
}

#[derive(Default)]
struct TcpPairing {
    /// Attempts that failed, or have not succeeded yet.
    failures: Window,
    /// Codes looked at with the failure budget spent.
    checks: Window,
}

/// What a pairing code grants once exchanged.
#[derive(Debug, Clone)]
pub struct Grant {
    pub scopes: Vec<String>,
    pub permissions: Vec<String>,
}

impl Grant {
    /// t3code's default one-time-token grant.
    pub fn full() -> Self {
        Grant { scopes: strings(DEFAULT_SCOPES), permissions: strings(DEFAULT_PERMISSIONS) }
    }

    /// A grant limited to `permissions`; scopes are the legacy names covering them.
    pub fn limited(permissions: Vec<String>) -> Self {
        let mut scopes: Vec<String> = permissions
            .iter()
            .filter(|p| DEFAULT_SCOPES.contains(&p.as_str()))
            .cloned()
            .collect();
        if scopes.is_empty() {
            scopes.push("orchestration:read".into());
        }
        Grant { scopes, permissions }
    }
}

/// The outcome of [`Auth::revoke`].
pub enum Revoke {
    Done(String),
    NotFound,
    Forbidden,
}

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("invalid_credential")]
    InvalidCredential,
    /// The requested `scope` names nothing the code grants.
    #[error("invalid_scope")]
    InvalidScope,
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
}

impl Auth {
    pub fn new(db: Db) -> Self {
        Auth { db, tickets: Mutex::new(HashMap::new()), tcp_pairing: Mutex::default() }
    }

    /// Whether `code` is a pairing code that would still be accepted.
    pub fn code_is_live(&self, code: &str) -> Result<bool, AuthError> {
        let key = sha256_hex(code.trim().as_bytes());
        let now = now_millis();
        let found = self.db.with(|c| {
            c.query_row("SELECT 1 FROM pairing_grants WHERE code_hash = ?1 AND consumed_at IS NULL AND expires_at_ms > ?2", params![key, now], |_| Ok(())).optional()
        })?;
        Ok(found.is_some())
    }

    /// Counts a pairing attempt with `code` over plain TCP from `address`
    /// (`None` when the listener gave none: one bucket for all of those).
    /// iroh peers are limited per device instead (`Remote::allow_attempt`).
    ///
    /// Only failures use up the budget: the attempt is counted before it
    /// runs (so parallel ones cannot slip past) and uncounted when it ends in
    /// a session. With the budget spent a wrong code is answered with 429,
    /// while a live one still pairs: `domovoi-server pair`, the command line
    /// and the app on this machine all come from 127.0.0.1, and a program
    /// that floods the server with guesses must not lock them out. Those
    /// lookups have a budget of their own, far larger, so guessing is capped
    /// either way; what makes a guess hopeless is the code's 60 bits.
    pub fn begin_tcp_pairing(&self, address: Option<std::net::IpAddr>, code: &str) -> Result<TcpAttempt, AuthError> {
        let address = address.unwrap_or(std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED));
        let now = std::time::Instant::now();
        {
            let mut pairing = crate::util::locked(&self.tcp_pairing);
            pairing.failures.forget_old(now);
            if !pairing.failures.spent(address, TCP_FAILURES_PER_ADDRESS, TCP_FAILURES_TOTAL) {
                pairing.failures.count(address, now);
                return Ok(TcpAttempt::Counted);
            }
            pairing.checks.forget_old(now);
            if pairing.checks.spent(address, TCP_CHECKS_PER_ADDRESS, TCP_CHECKS_TOTAL) {
                return Ok(TcpAttempt::Refused);
            }
            pairing.checks.count(address, now);
        }
        Ok(if self.code_is_live(code)? { TcpAttempt::Live } else { TcpAttempt::Refused })
    }

    /// A [`TcpAttempt::Counted`] attempt from `address` ended in a session.
    pub fn tcp_pairing_succeeded(&self, address: Option<std::net::IpAddr>) {
        let address = address.unwrap_or(std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED));
        crate::util::locked(&self.tcp_pairing).failures.uncount(address);
    }

    pub fn descriptor(&self) -> AuthDescriptor {
        AuthDescriptor::pairing_code()
    }

    #[cfg(test)]
    pub fn issue_pairing_code(&self, ttl: Duration) -> Result<String, AuthError> {
        self.issue_pairing_code_with(ttl, &Grant::full())
    }

    /// A full-access code that only works from this machine (plain TCP),
    /// never over iroh: the one printed at start lives for a day.
    pub fn issue_local_pairing_code(&self, ttl: Duration) -> Result<String, AuthError> {
        self.issue(ttl, &Grant::full(), true)
    }

    pub fn issue_pairing_code_with(&self, ttl: Duration, grant: &Grant) -> Result<String, AuthError> {
        self.issue(ttl, grant, false)
    }

    fn issue(&self, ttl: Duration, grant: &Grant, local_only: bool) -> Result<String, AuthError> {
        let ttl = ttl.min(MAX_PAIRING_TTL);
        let code = pairing_code();
        let now = now_millis();
        self.db.with(|c| {
            c.execute("DELETE FROM pairing_grants WHERE expires_at_ms <= ?1", params![now])?;
            c.execute(
                "INSERT INTO pairing_grants (code_hash, scopes, permissions, created_at, expires_at_ms, local_only) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    sha256_hex(code.as_bytes()),
                    serde_json::to_string(&grant.scopes).expect("plain data serializes"),
                    serde_json::to_string(&grant.permissions).expect("plain data serializes"),
                    iso_from_millis(now),
                    now + ttl.as_millis() as i64,
                    local_only
                ],
            )
            .map(|_| ())
        })?;
        Ok(code)
    }

    /// Consumes a pairing code and creates a session over plain TCP with its
    /// grant. Returns the raw session token and the session.
    #[cfg(test)]
    pub fn create_local_session(&self, credential: &str) -> Result<(String, Session), AuthError> {
        self.create_session(credential, NewSession::default())
    }

    /// Consumes a pairing code and creates a session with its grant (narrowed
    /// to `new.scope`). Returns the raw session token and the session.
    pub fn create_session(&self, credential: &str, new: NewSession) -> Result<(String, Session), AuthError> {
        let key = sha256_hex(credential.trim().as_bytes());
        let now = now_millis();
        // Consume atomically: of two concurrent exchanges only one updates the row.
        // A code asked for a scope it does not grant stays unused.
        type Granted = Result<Option<(Vec<String>, Vec<String>)>, AuthError>;
        let grant: Granted = self.db.with(|c| {
            let tx = c.savepoint()?;
            let row: Option<(String, String, bool)> = tx
                .query_row(
                    "SELECT scopes, permissions, local_only FROM pairing_grants WHERE code_hash = ?1 AND consumed_at IS NULL AND expires_at_ms > ?2",
                    params![key, now],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?;
            let Some((scopes, permissions, local_only)) = row else { return Ok(Ok(None)) };
            // A local-only code offered over iroh is refused and stays unused.
            if local_only && new.peer.is_some() {
                return Ok(Ok(None));
            }
            let mut scopes: Vec<String> = serde_json::from_str(&scopes).unwrap_or_default();
            let mut permissions: Vec<String> = serde_json::from_str(&permissions).unwrap_or_default();
            if let Some(requested) = &new.scope {
                scopes.retain(|s| requested.contains(s));
                permissions.retain(|p| requested.contains(p));
                if permissions.is_empty() {
                    return Ok(Err(AuthError::InvalidScope));
                }
                if scopes.is_empty() {
                    scopes.push("orchestration:read".into());
                }
            }
            tx.execute("UPDATE pairing_grants SET consumed_at = ?2 WHERE code_hash = ?1", params![key, iso_from_millis(now)])?;
            tx.commit()?;
            Ok(Ok(Some((scopes, permissions))))
        })?;
        let Some((scopes, permissions)) = grant? else {
            return Err(AuthError::InvalidCredential);
        };
        let token = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(random_bytes::<32>());
        let session = Session {
            subject: "one-time-token".into(),
            scopes,
            permissions,
            expires_at: iso_from_millis(now + SESSION_TTL_MS),
            token_hash: sha256_hex(token.as_bytes()),
            method: METHOD_BEARER.into(),
            bound_endpoint: new.peer,
        };
        self.db.with(|c| {
            c.execute(
                "INSERT INTO auth_sessions (token_hash, subject, scopes, permissions, created_at, expires_at, expires_at_ms, method, bound_endpoint, client)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![
                    session.token_hash,
                    session.subject,
                    serde_json::to_string(&session.scopes).expect("plain data serializes"),
                    serde_json::to_string(&session.permissions).expect("plain data serializes"),
                    iso_from_millis(now),
                    session.expires_at,
                    now + SESSION_TTL_MS,
                    session.method,
                    session.bound_endpoint,
                    serde_json::to_string(&new.client).expect("plain data serializes"),
                ],
            )
        })?;
        Ok((token, session))
    }

    pub fn session_for_token(&self, token: &str) -> Result<Option<Session>, AuthError> {
        self.session_for_hash(&sha256_hex(token.as_bytes()))
    }

    /// The live session with this token hash. Using a session renews it once
    /// it is past a third of its life.
    pub fn session_for_hash(&self, hash: &str) -> Result<Option<Session>, AuthError> {
        let now = now_millis();
        Ok(self.db.with(|c| {
            let found = c
                .query_row(
                    "SELECT subject, scopes, permissions, expires_at, method, bound_endpoint, expires_at_ms FROM auth_sessions
                     WHERE token_hash = ?1 AND expires_at_ms > ?2",
                    params![hash, now],
                    |r| {
                        Ok((
                            Session {
                                subject: r.get(0)?,
                                scopes: serde_json::from_str(&r.get::<_, String>(1)?).unwrap_or_default(),
                                permissions: serde_json::from_str(&r.get::<_, String>(2)?).unwrap_or_default(),
                                expires_at: r.get(3)?,
                                token_hash: hash.to_owned(),
                                method: r.get(4)?,
                                bound_endpoint: r.get(5)?,
                            },
                            r.get::<_, i64>(6)?,
                        ))
                    },
                )
                .optional()?;
            let Some((mut session, expires_ms)) = found else { return Ok(None) };
            if expires_ms - now < SESSION_RENEW_BELOW_MS {
                session.expires_at = iso_from_millis(now + SESSION_TTL_MS);
                c.execute(
                    "UPDATE auth_sessions SET expires_at = ?2, expires_at_ms = ?3 WHERE token_hash = ?1",
                    params![hash, session.expires_at, now + SESSION_TTL_MS],
                )?;
            }
            Ok(Some(session))
        })?)
    }

    /// Deletes expired sessions; returns their token hashes (to close their
    /// sockets and drop their push devices). Also drops spent pairing codes
    /// and WebSocket tickets.
    pub fn purge_expired(&self) -> Result<Vec<String>, AuthError> {
        let now = now_millis();
        crate::util::locked(&self.tickets).retain(|_, (_, exp)| *exp > now);
        Ok(self.db.with(|c| {
            let gone: Vec<String> = {
                let mut st = c.prepare("SELECT token_hash FROM auth_sessions WHERE expires_at_ms <= ?1")?;
                let rows = st.query_map(params![now], |r| r.get(0))?;
                rows.collect::<rusqlite::Result<_>>()?
            };
            c.execute("DELETE FROM auth_sessions WHERE expires_at_ms <= ?1", params![now])?;
            c.execute("DELETE FROM pairing_grants WHERE expires_at_ms <= ?1", params![now])?;
            Ok(gone)
        })?)
    }

    /// Token hashes of every live session.
    pub fn live_hashes(&self) -> Result<std::collections::HashSet<String>, AuthError> {
        Ok(self.session_hashes()?.into_iter().map(|(_, hash, _)| hash).collect())
    }

    /// Authenticates a request by its bearer token, on a connection from
    /// `peer` (`None` for plain TCP).
    pub fn authenticate(&self, headers: &HeaderMap, peer: Option<&str>) -> Result<Option<Session>, AuthError> {
        match bearer_token(headers) {
            Some(token) => Ok(self.session_for_token(&token)?.filter(|s| bound_to(s, peer))),
            None => Ok(None),
        }
    }

    /// A single-use ticket for opening the RPC WebSocket as `session`
    /// (clients that cannot attach headers to the upgrade). Returns the
    /// ticket and its expiry (ISO).
    pub fn mint_ws_ticket(&self, session: &Session) -> (String, String) {
        let ticket = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(random_bytes::<32>());
        let now = now_millis();
        let expires = now + WS_TICKET_TTL_MS;
        let mut tickets = crate::util::locked(&self.tickets);
        tickets.retain(|_, (_, exp)| *exp > now);
        tickets.insert(sha256_hex(ticket.as_bytes()), (session.token_hash.clone(), expires));
        (ticket, iso_from_millis(expires))
    }

    /// Redeems a WebSocket ticket on a connection from `peer`.
    pub fn redeem_ws_ticket(&self, ticket: &str, peer: Option<&str>) -> Result<Option<Session>, AuthError> {
        let entry = crate::util::locked(&self.tickets).remove(&sha256_hex(ticket.as_bytes()));
        match entry {
            Some((hash, expires)) if expires > now_millis() => Ok(self.session_for_hash(&hash)?.filter(|s| bound_to(s, peer))),
            _ => Ok(None),
        }
    }

    /// Whether an iroh peer holds a live session (unknown peers may only pair).
    pub fn known_peer(&self, endpoint: &str) -> Result<bool, AuthError> {
        let now = now_millis();
        Ok(self.db.with(|c| {
            c.query_row(
                "SELECT 1 FROM auth_sessions WHERE bound_endpoint = ?1 AND expires_at_ms > ?2 LIMIT 1",
                params![endpoint, now],
                |_| Ok(()),
            )
            .optional()
        })?
        .is_some())
    }

    /// Live sessions: public id, token hash, and whether bound to an iroh peer.
    fn session_hashes(&self) -> Result<Vec<(String, String, bool)>, AuthError> {
        let now = now_millis();
        let rows: Vec<(String, Option<String>)> = self.db.with(|c| {
            let mut st = c.prepare("SELECT token_hash, bound_endpoint FROM auth_sessions WHERE expires_at_ms > ?1")?;
            let rows = st.query_map(params![now], |r| Ok((r.get(0)?, r.get(1)?)))?;
            rows.collect()
        })?;
        Ok(rows.into_iter().map(|(h, bound)| (session_id(&h), h, bound.is_some())).collect())
    }

    /// Ends the session with public id `session_id`; `Done` carries its token
    /// hash (to close its live connections). With `remote_only` (the caller
    /// is a remote device) sessions on the machine itself are off limits.
    pub fn revoke(&self, session_id: &str, remote_only: bool) -> Result<Revoke, AuthError> {
        let Some((_, hash, remote)) = self.session_hashes()?.into_iter().find(|(id, _, _)| id == session_id) else {
            return Ok(Revoke::NotFound);
        };
        if remote_only && !remote {
            return Ok(Revoke::Forbidden);
        }
        self.db.with(|c| c.execute("DELETE FROM auth_sessions WHERE token_hash = ?1", params![hash]))?;
        Ok(Revoke::Done(hash))
    }

    /// Ends every session bound to an iroh device (the managed policy allows
    /// no remote clients); returns how many.
    pub fn revoke_remote(&self) -> Result<usize, AuthError> {
        Ok(self.db.with(|c| c.execute("DELETE FROM auth_sessions WHERE bound_endpoint IS NOT NULL", []))?)
    }

    /// Ends every session except `keep` (a token hash), sparing local ones
    /// with `remote_only`. Returns the revoked hashes.
    pub fn revoke_others(&self, keep: &str, remote_only: bool) -> Result<Vec<String>, AuthError> {
        let others: Vec<String> =
            self.session_hashes()?.into_iter().filter(|(_, h, remote)| h != keep && (*remote || !remote_only)).map(|(_, h, _)| h).collect();
        self.db.with(|c| {
            for h in &others {
                c.execute("DELETE FROM auth_sessions WHERE token_hash = ?1", params![h])?;
            }
            Ok(())
        })?;
        Ok(others)
    }

    pub fn session_state(&self, session: Option<&Session>) -> AuthSessionState {
        AuthSessionState {
            authenticated: session.is_some(),
            auth: self.descriptor(),
            scopes: session.map(|s| s.scopes.clone()),
            permissions: session.map(|s| s.permissions.clone()),
            session_method: session.map(|s| s.method.clone()),
            expires_at: session.map(|s| s.expires_at.clone()),
        }
    }
}

/// A stable public id for a session; its token hash is never exposed.
pub fn session_id(token_hash: &str) -> String {
    sha256_hex(format!("domovoi-session-id:{token_hash}").as_bytes())[..32].to_owned()
}

/// A session works only on the transport it was created on: bound sessions
/// from their own iroh peer, unbound ones from plain TCP.
fn bound_to(session: &Session, peer: Option<&str>) -> bool {
    session.bound_endpoint.as_deref() == peer
}

/// `Authorization: Bearer <token>`.
pub fn bearer_token(headers: &HeaderMap) -> Option<String> {
    let value = headers.get(axum::http::header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.trim().split_once(' ')?;
    (scheme.eq_ignore_ascii_case("bearer") && !token.trim().is_empty()).then(|| token.trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn auth() -> Auth {
        Auth::new(Db::open_in_memory().unwrap())
    }

    #[test]
    fn pairing_code_is_single_use() {
        let a = auth();
        let code = a.issue_pairing_code(Duration::from_secs(60)).unwrap();
        let (token, session) = a.create_local_session(&code).unwrap();
        assert_eq!(session.scopes, strings(DEFAULT_SCOPES));
        assert!(a.session_for_token(&token).unwrap().is_some());
        assert!(matches!(a.create_local_session(&code), Err(AuthError::InvalidCredential)));
    }

    #[test]
    fn unknown_and_expired_codes_are_rejected() {
        let a = auth();
        assert!(matches!(a.create_local_session("NOPE"), Err(AuthError::InvalidCredential)));
        let code = a.issue_pairing_code(Duration::from_secs(0)).unwrap();
        assert!(matches!(a.create_local_session(&code), Err(AuthError::InvalidCredential)));
    }

    #[test]
    fn limited_grants_carry_their_permissions() {
        let a = auth();
        let code = a.issue_pairing_code_with(Duration::from_secs(60), &Grant::limited(vec!["orchestration:read".into()])).unwrap();
        let (_, session) = a.create_local_session(&code).unwrap();
        assert_eq!(session.permissions, vec!["orchestration:read".to_owned()]);
        assert_eq!(session.scopes, vec!["orchestration:read".to_owned()]);
    }

    #[test]
    fn codes_minted_by_another_handle_work() {
        // `domovoi-server pair` opens the same database while the server runs.
        let db = Db::open_in_memory().unwrap();
        let server = Auth::new(db.clone());
        let cli = Auth::new(db);
        let code = cli.issue_pairing_code(Duration::from_secs(60)).unwrap();
        assert!(server.create_local_session(&code).is_ok());
    }

    /// A session is presented as a bearer token and in no other way: a
    /// cookie that holds the token signs nobody in.
    #[test]
    fn a_session_is_presented_as_a_bearer_token_only() {
        let a = auth();
        let code = a.issue_pairing_code(Duration::from_secs(60)).unwrap();
        let (token, session) = a.create_local_session(&code).unwrap();
        assert_eq!(session.method, METHOD_BEARER);
        assert!(a.authenticate(&bearer(&token), None).unwrap().is_some());
        let mut h = HeaderMap::new();
        h.insert("cookie", format!("t3_session={token}; session={token}").parse().unwrap());
        assert!(a.authenticate(&h, None).unwrap().is_none());
    }

    fn bearer(token: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert("authorization", format!("Bearer {token}").parse().unwrap());
        h
    }

    #[test]
    fn bearer_sessions_are_bound_to_their_peer() {
        let a = auth();
        let code = a.issue_pairing_code(Duration::from_secs(60)).unwrap();
        let new = NewSession { peer: Some("peer-a".into()), ..Default::default() };
        let (token, session) = a.create_session(&code, new).unwrap();
        assert_eq!(session.method, METHOD_BEARER);
        assert!(a.authenticate(&bearer(&token), Some("peer-a")).unwrap().is_some());
        assert!(a.authenticate(&bearer(&token), Some("peer-b")).unwrap().is_none(), "another key");
        assert!(a.authenticate(&bearer(&token), None).unwrap().is_none(), "plain TCP");
        assert!(a.known_peer("peer-a").unwrap());
        assert!(!a.known_peer("peer-b").unwrap());

        // A session made over TCP does not work over iroh.
        let code = a.issue_pairing_code(Duration::from_secs(60)).unwrap();
        let (local, _) = a.create_local_session(&code).unwrap();
        assert!(a.authenticate(&bearer(&local), None).unwrap().is_some());
        assert!(a.authenticate(&bearer(&local), Some("peer-a")).unwrap().is_none());
    }

    #[test]
    fn scope_narrows_and_a_bad_scope_keeps_the_code() {
        let a = auth();
        let code = a.issue_pairing_code(Duration::from_secs(60)).unwrap();
        let mut new = NewSession { scope: Some(vec!["nope".into()]), ..Default::default() };
        assert!(matches!(a.create_session(&code, new.clone()), Err(AuthError::InvalidScope)));
        new.scope = Some(vec!["orchestration:read".into(), "terminal:read".into()]);
        let (_, session) = a.create_session(&code, new).unwrap();
        assert_eq!(session.permissions, vec!["orchestration:read".to_owned(), "terminal:read".to_owned()]);
        assert_eq!(session.scopes, vec!["orchestration:read".to_owned()]);
    }

    #[test]
    fn ws_tickets_are_single_use_and_bound() {
        let a = auth();
        let code = a.issue_pairing_code(Duration::from_secs(60)).unwrap();
        let new = NewSession { peer: Some("peer-a".into()), ..Default::default() };
        let (_, session) = a.create_session(&code, new).unwrap();
        let (ticket, _) = a.mint_ws_ticket(&session);
        assert!(a.redeem_ws_ticket(&ticket, Some("peer-b")).unwrap().is_none());
        // The failed attempt consumed it.
        assert!(a.redeem_ws_ticket(&ticket, Some("peer-a")).unwrap().is_none());
        let (ticket, _) = a.mint_ws_ticket(&session);
        assert!(a.redeem_ws_ticket(&ticket, Some("peer-a")).unwrap().is_some());
        assert!(a.redeem_ws_ticket(&ticket, Some("peer-a")).unwrap().is_none());
    }

    #[test]
    fn revoking_ends_sessions() {
        let a = auth();
        let mut tokens = vec![];
        for _ in 0..3 {
            let code = a.issue_pairing_code(Duration::from_secs(60)).unwrap();
            tokens.push(a.create_local_session(&code).unwrap());
        }
        let (t0, s0) = &tokens[0];
        assert!(matches!(a.revoke(&session_id(&s0.token_hash), false).unwrap(), Revoke::Done(h) if h == s0.token_hash));
        assert!(a.session_for_token(t0).unwrap().is_none());
        assert!(matches!(a.revoke(&session_id(&s0.token_hash), false).unwrap(), Revoke::NotFound));
        let (t1, s1) = &tokens[1];
        assert_eq!(a.revoke_others(&s1.token_hash, false).unwrap().len(), 1);
        assert!(a.session_for_token(t1).unwrap().is_some());
        assert!(a.session_for_token(&tokens[2].0).unwrap().is_none());
    }

    /// Moves a session's expiry to `remaining_ms` from now.
    fn expire_in(a: &Auth, hash: &str, remaining_ms: i64) {
        a.db.with(|c| c.execute("UPDATE auth_sessions SET expires_at_ms = ?2 WHERE token_hash = ?1", params![hash, now_millis() + remaining_ms])).unwrap();
    }

    fn expiry(a: &Auth, hash: &str) -> i64 {
        a.db.with(|c| c.query_row("SELECT expires_at_ms FROM auth_sessions WHERE token_hash = ?1", params![hash], |r| r.get(0))).unwrap()
    }

    #[test]
    fn sessions_renew_while_in_use_and_expire_when_idle() {
        let a = auth();
        let day = 24 * 60 * 60 * 1000;
        let code = a.issue_pairing_code(Duration::from_secs(60)).unwrap();
        let (token, session) = a.create_local_session(&code).unwrap();
        // Fresh: using it changes nothing.
        let before = expiry(&a, &session.token_hash);
        a.session_for_token(&token).unwrap().unwrap();
        assert_eq!(expiry(&a, &session.token_hash), before);
        // Five days left: using it puts the full month back.
        expire_in(&a, &session.token_hash, 5 * day);
        let renewed = a.session_for_token(&token).unwrap().unwrap();
        let left = expiry(&a, &session.token_hash) - now_millis();
        assert!(left > 29 * day, "renewed to a month, got {left} ms");
        assert_eq!(renewed.expires_at, iso_from_millis(expiry(&a, &session.token_hash)), "the reported expiry moves too");
        // Left alone past its expiry: gone, and cleanup reports it once.
        expire_in(&a, &session.token_hash, -1);
        assert!(a.session_for_token(&token).unwrap().is_none());
        assert_eq!(a.purge_expired().unwrap(), vec![session.token_hash.clone()]);
        assert!(a.purge_expired().unwrap().is_empty());
        assert!(a.live_hashes().unwrap().is_empty());
    }

    #[test]
    fn a_local_code_is_refused_over_iroh_and_stays_usable_locally() {
        let a = auth();
        let code = a.issue_local_pairing_code(Duration::from_secs(60)).unwrap();
        let remote = NewSession { peer: Some("peer-key".into()), ..Default::default() };
        assert!(matches!(a.create_session(&code, remote), Err(AuthError::InvalidCredential)));
        assert!(a.create_session(&code, NewSession::default()).is_ok(), "the refused attempt did not use the code up");
    }

    #[test]
    fn failed_pairing_over_tcp_spends_a_budget_that_a_live_code_passes() {
        let a = auth();
        let here: Option<std::net::IpAddr> = Some("127.0.0.1".parse().unwrap());
        let other: Option<std::net::IpAddr> = Some("192.168.1.7".parse().unwrap());

        // Attempts that succeed use nothing up.
        for _ in 0..TCP_FAILURES_PER_ADDRESS * 3 {
            assert_eq!(a.begin_tcp_pairing(here, "whatever").unwrap(), TcpAttempt::Counted);
            a.tcp_pairing_succeeded(here);
        }
        // Failed ones do, for their address.
        for _ in 0..TCP_FAILURES_PER_ADDRESS {
            assert_eq!(a.begin_tcp_pairing(here, "WRONGWRONGWR").unwrap(), TcpAttempt::Counted);
        }
        assert_eq!(a.begin_tcp_pairing(here, "WRONGWRONGWR").unwrap(), TcpAttempt::Refused);
        assert_eq!(a.begin_tcp_pairing(other, "WRONGWRONGWR").unwrap(), TcpAttempt::Counted, "another address has its own");

        // A live code still pairs, as often as codes are minted; a used or expired one does not.
        let code = a.issue_pairing_code(Duration::from_secs(60)).unwrap();
        assert!(a.code_is_live(&code).unwrap() && a.code_is_live(&format!(" {code}\n")).unwrap());
        assert_eq!(a.begin_tcp_pairing(here, &code).unwrap(), TcpAttempt::Live);
        a.create_local_session(&code).unwrap();
        assert!(!a.code_is_live(&code).unwrap());
        assert_eq!(a.begin_tcp_pairing(here, &code).unwrap(), TcpAttempt::Refused, "used");
        let expired = a.issue_pairing_code(Duration::from_secs(0)).unwrap();
        assert_eq!(a.begin_tcp_pairing(here, &expired).unwrap(), TcpAttempt::Refused);

        // Looking at codes with the budget spent is capped too: then even a live one waits.
        let code = a.issue_pairing_code(Duration::from_secs(60)).unwrap();
        for _ in 0..TCP_CHECKS_PER_ADDRESS {
            let _ = a.begin_tcp_pairing(here, "WRONGWRONGWR").unwrap();
        }
        assert_eq!(a.begin_tcp_pairing(here, &code).unwrap(), TcpAttempt::Refused);
        assert!(a.code_is_live(&code).unwrap(), "refused, not used up");

        // The window passing gives the budget back.
        let earlier = std::time::Instant::now().checked_sub(TCP_PAIRING_WINDOW + Duration::from_secs(1)).unwrap();
        {
            let mut pairing = crate::util::locked(&a.tcp_pairing);
            let TcpPairing { failures, checks } = &mut *pairing;
            for window in [failures, checks] {
                window.all.iter_mut().for_each(|t| *t = earlier);
                window.by_address.values_mut().flatten().for_each(|t| *t = earlier);
            }
        }
        assert_eq!(a.begin_tcp_pairing(here, "WRONGWRONGWR").unwrap(), TcpAttempt::Counted);
    }

    #[test]
    fn every_address_together_has_a_budget_of_failed_pairings() {
        let a = auth();
        let address = |i: usize| Some(std::net::IpAddr::V4(std::net::Ipv4Addr::new(10, 0, (i / 250) as u8, (i % 250) as u8 + 1)));
        for i in 0..TCP_FAILURES_TOTAL {
            assert_eq!(a.begin_tcp_pairing(address(i), "WRONGWRONGWR").unwrap(), TcpAttempt::Counted);
        }
        assert_eq!(a.begin_tcp_pairing(address(TCP_FAILURES_TOTAL + 1), "WRONGWRONGWR").unwrap(), TcpAttempt::Refused);
        let code = a.issue_pairing_code(Duration::from_secs(60)).unwrap();
        assert_eq!(a.begin_tcp_pairing(Some("127.0.0.1".parse().unwrap()), &code).unwrap(), TcpAttempt::Live, "this machine still pairs");
    }

    #[test]
    fn expired_tickets_are_refused_and_tickets_work_once() {
        let a = auth();
        let code = a.issue_pairing_code(Duration::from_secs(60)).unwrap();
        let (_, session) = a.create_local_session(&code).unwrap();
        let (ticket, _) = a.mint_ws_ticket(&session);
        assert!(a.redeem_ws_ticket(&ticket, None).unwrap().is_some());
        assert!(a.redeem_ws_ticket(&ticket, None).unwrap().is_none(), "single use");
        let (ticket, _) = a.mint_ws_ticket(&session);
        for (_, expires) in crate::util::locked(&a.tickets).values_mut() {
            *expires = now_millis() - 1;
        }
        assert!(a.redeem_ws_ticket(&ticket, None).unwrap().is_none(), "expired");
        let (ticket, _) = a.mint_ws_ticket(&session);
        assert!(a.redeem_ws_ticket(&ticket, Some("peer-key")).unwrap().is_none(), "a local session's ticket is useless over iroh");
    }
}
