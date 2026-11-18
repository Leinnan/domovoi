//! `subscribeAuthAccess`: active pairing links and client sessions, after
//! t3code's handler in `ws.ts` (`loadAuthAccessSnapshot` plus the
//! `PairingGrantStore` and `SessionStore` change streams).
//!
//! A snapshot (revision 1) is followed by `pairingLinkUpserted`/`Removed` and
//! `clientUpserted`/`Removed` events. Domovoi's auth store has no change feed, so
//! changes are found by re-reading it every two seconds and diffing; a link
//! disappears once it is used or expires, a session once it expires.
//!
//! Limits, from what Domovoi's store records:
//! - pairing links are identified by their row id and have no label; t3code
//!   hides the startup link (administrative subject), Domovoi cannot tell it apart
//!   and lists it too;
//! - client metadata is what native clients send when pairing (label, device
//!   type, OS); browsers send none, so theirs is `{ deviceType: "unknown" }`.

use crate::auth::ClientMeta;
use crate::config_failure::Failure;
use crate::state::{AppState, ConnCtx};
use effect_rpc::Router;
use futures::StreamExt;
use futures::stream::BoxStream;
use rusqlite::params;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;
use domovoi_contracts::time_fmt::{iso_from_millis, now_millis};
use domovoi_model::typed::{
    AuthAccessEvent, AuthAccessSnapshot, AuthAccessStreamEvent, AuthClientDeviceType, AuthClientMetadata, AuthClientRemoved, AuthClientSession,
    AuthEnvironmentScope, AuthPairingLink, AuthPairingLinkRemoved,
};

const POLL_INTERVAL: Duration = Duration::from_secs(2);

pub use crate::auth::session_id;

/// The scopes or permissions a row holds, as the JSON list of names it was stored as.
fn scopes(raw: &str) -> Vec<AuthEnvironmentScope> {
    serde_json::from_str(raw).unwrap_or_default()
}

struct Access {
    links: BTreeMap<String, AuthPairingLink>,
    clients: BTreeMap<String, AuthClientSession>,
}

fn read(state: &AppState, current_token_hash: &str) -> Result<Access, String> {
    let now = now_millis();
    let db = state.store.db();
    let link_rows: Vec<(i64, String, String, String, i64)> = db
        .with(|c| {
            let mut st = c.prepare(
                "SELECT rowid, scopes, permissions, created_at, expires_at_ms FROM pairing_grants
                 WHERE consumed_at IS NULL AND expires_at_ms > ?1 ORDER BY created_at DESC",
            )?;
            let rows = st.query_map(params![now], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)))?;
            rows.collect()
        })
        .map_err(|e| e.to_string())?;
    type Row = (String, String, String, String, String, String, String, String);
    let session_rows: Vec<Row> = db
        .with(|c| {
            let mut st = c.prepare(
                "SELECT token_hash, subject, scopes, permissions, created_at, expires_at, method, client FROM auth_sessions
                 WHERE expires_at_ms > ?1 ORDER BY created_at",
            )?;
            let rows = st.query_map(params![now], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?)))?;
            rows.collect()
        })
        .map_err(|e| e.to_string())?;
    let links = link_rows
        .into_iter()
        .map(|(rowid, link_scopes, permissions, created_at, expires_ms)| {
            let id = format!("pairing-link-{rowid}");
            let link = AuthPairingLink {
                id: id.clone(),
                scopes: scopes(&link_scopes),
                permissions: Some(scopes(&permissions).into()),
                subject: "one-time-token".to_owned(),
                label: None,
                created_at: created_at.into(),
                expires_at: iso_from_millis(expires_ms).into(),
            };
            (id, link)
        })
        .collect();
    let clients = session_rows
        .into_iter()
        .map(|(hash, subject, session_scopes, permissions, issued_at, expires_at, method, client)| {
            let current = hash == current_token_hash;
            let id = session_id(&hash);
            // What the client said of itself when it paired; a browser says nothing.
            let meta: ClientMeta = serde_json::from_str(&client).unwrap_or_default();
            let client = AuthClientSession {
                session_id: id.clone(),
                subject,
                scopes: scopes(&session_scopes),
                permissions: Some(scopes(&permissions).into()),
                method: method.as_str().into(),
                client: AuthClientMetadata {
                    label: meta.label,
                    ip_address: None,
                    user_agent: None,
                    device_type: meta.device_type.as_deref().map_or(AuthClientDeviceType::Unknown, AuthClientDeviceType::from),
                    os: meta.os,
                    browser: None,
                },
                issued_at: issued_at.into(),
                expires_at: expires_at.into(),
                last_connected_at: state.connections.last_seen(&hash).map(|seen| iso_from_millis(seen).into()),
                connected: state.connections.is_live(&hash),
                current,
            };
            (id, client)
        })
        .collect();
    Ok(Access { links, clients })
}

fn sorted_links(links: &BTreeMap<String, AuthPairingLink>) -> Vec<AuthPairingLink> {
    let mut out: Vec<AuthPairingLink> = links.values().cloned().collect();
    out.sort_by(|a, b| b.created_at.cmp(&a.created_at));
    out
}

/// A change to the access list: an event of the stream without its place in it.
#[derive(Debug, PartialEq)]
enum Change {
    LinkUpserted(AuthPairingLink),
    LinkRemoved(String),
    ClientUpserted(AuthClientSession),
    ClientRemoved(String),
}

impl Change {
    /// The change as the `revision`th event of a subscription.
    fn event(self, revision: u64) -> AuthAccessStreamEvent {
        fn at<P>(revision: u64, payload: P) -> AuthAccessEvent<P> {
            AuthAccessEvent { version: 1, revision: revision.into(), payload }
        }
        match self {
            Change::LinkUpserted(link) => AuthAccessStreamEvent::PairingLinkUpserted(at(revision, link)),
            Change::LinkRemoved(id) => AuthAccessStreamEvent::PairingLinkRemoved(at(revision, AuthPairingLinkRemoved { id })),
            Change::ClientUpserted(client) => AuthAccessStreamEvent::ClientUpserted(at(revision, client)),
            Change::ClientRemoved(session_id) => AuthAccessStreamEvent::ClientRemoved(at(revision, AuthClientRemoved { session_id })),
        }
    }
}

/// The changes turning `before` into `after`. `lastConnectedAt` of the current
/// session moves on every read, so it does not count as a change.
fn diff(before: &Access, after: &Access) -> Vec<Change> {
    let mut out = vec![];
    for (id, link) in &after.links {
        if before.links.get(id) != Some(link) {
            out.push(Change::LinkUpserted(link.clone()));
        }
    }
    for id in before.links.keys().filter(|id| !after.links.contains_key(*id)) {
        out.push(Change::LinkRemoved(id.clone()));
    }
    let comparable = |client: &AuthClientSession| AuthClientSession { last_connected_at: None, ..client.clone() };
    for (id, client) in &after.clients {
        if before.clients.get(id).map(comparable) != Some(comparable(client)) {
            out.push(Change::ClientUpserted(client.clone()));
        }
    }
    for id in before.clients.keys().filter(|id| !after.clients.contains_key(*id)) {
        out.push(Change::ClientRemoved(id.clone()));
    }
    out
}

fn access_stream(ctx: Arc<ConnCtx>) -> BoxStream<'static, Result<AuthAccessStreamEvent, Failure>> {
    async_stream::stream! {
        let token_hash = ctx.session.token_hash.clone();
        let mut last = match read(&ctx.state, &token_hash) {
            Ok(a) => a,
            Err(message) => { yield Err(Failure::AccessStream { message }); return; }
        };
        let mut revision = 1u64;
        yield Ok(AuthAccessStreamEvent::Snapshot(AuthAccessEvent {
            version: 1,
            revision: revision.into(),
            payload: AuthAccessSnapshot { pairing_links: sorted_links(&last.links), client_sessions: last.clients.values().cloned().collect() },
        }));
        loop {
            tokio::time::sleep(POLL_INTERVAL).await;
            let Ok(next) = read(&ctx.state, &token_hash) else { continue };
            for change in diff(&last, &next) {
                revision += 1;
                yield Ok(change.event(revision));
            }
            last = next;
        }
    }
    .boxed()
}

pub fn register(r: Router<ConnCtx>) -> Router<ConnCtx> {
    r.stream("subscribeAuthAccess", |ctx, _: serde::de::IgnoredAny| access_stream(ctx))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn link(id: &str, created: &str) -> AuthPairingLink {
        AuthPairingLink { id: id.into(), scopes: vec![], permissions: Some(vec![].into()), subject: "one-time-token".into(), label: None, created_at: created.into(), expires_at: "9".into() }
    }

    fn client(id: &str, current: bool, at: &str) -> AuthClientSession {
        AuthClientSession {
            session_id: id.into(),
            subject: "one-time-token".into(),
            scopes: scopes(r#"["orchestration:read"]"#),
            permissions: Some(scopes(r#"["orchestration:read", "terminal:read"]"#).into()),
            method: "browser-session-cookie".into(),
            client: AuthClientMetadata { label: None, ip_address: None, user_agent: None, device_type: AuthClientDeviceType::Unknown, os: None, browser: None },
            issued_at: "1".into(),
            expires_at: "9".into(),
            last_connected_at: (!at.is_empty()).then(|| at.into()),
            connected: false,
            current,
        }
    }

    fn access(links: &[(&str, &str)], clients: &[(&str, bool, &str)]) -> Access {
        Access {
            links: links.iter().map(|(id, created)| (id.to_string(), link(id, created))).collect(),
            clients: clients.iter().map(|(id, current, at)| (id.to_string(), client(id, *current, at))).collect(),
        }
    }

    #[test]
    fn diffs_into_t3code_change_events() {
        let before = access(&[("a", "1"), ("b", "2")], &[("s1", true, "t1")]);
        let after = access(&[("b", "2"), ("c", "3")], &[("s1", true, "t2"), ("s2", false, "")]);
        let changes = diff(&before, &after);
        assert_eq!(changes, vec![Change::LinkUpserted(link("c", "3")), Change::LinkRemoved("a".into()), Change::ClientUpserted(client("s2", false, ""))]);
        assert_eq!(sorted_links(&after.links)[0].id, "c");

        let events: Vec<_> = changes.into_iter().zip(2..).map(|(change, revision)| json!(change.event(revision))).collect();
        assert_eq!(
            events[0],
            json!({ "version": 1, "revision": 2, "type": "pairingLinkUpserted", "payload": { "id": "c", "scopes": [], "permissions": [], "subject": "one-time-token", "createdAt": "3", "expiresAt": "9" } })
        );
        assert_eq!(events[1], json!({ "version": 1, "revision": 3, "type": "pairingLinkRemoved", "payload": { "id": "a" } }));
        assert_eq!(
            events[2],
            json!({ "version": 1, "revision": 4, "type": "clientUpserted", "payload": {
                "sessionId": "s2", "subject": "one-time-token", "scopes": ["orchestration:read"], "permissions": ["orchestration:read", "terminal:read"],
                "method": "browser-session-cookie", "client": { "deviceType": "unknown" }, "issuedAt": "1", "expiresAt": "9",
                "lastConnectedAt": null, "connected": false, "current": false } })
        );
        assert_eq!(json!(Change::ClientRemoved("s1".into()).event(5)), json!({ "version": 1, "revision": 5, "type": "clientRemoved", "payload": { "sessionId": "s1" } }));
    }

    #[test]
    fn session_ids_hide_token_hashes() {
        let id = session_id("abc");
        assert_eq!(id.len(), 32);
        assert!(!id.contains("abc"));
        assert_eq!(id, session_id("abc"));
    }
}
