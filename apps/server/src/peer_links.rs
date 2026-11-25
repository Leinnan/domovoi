//! Linked environments over iroh (t3code `peer/PeerLinks.ts` and
//! `contracts/peerLink.ts`): links from this server to other Domovoi servers
//! (peers), so the user and this server's agents can work there.
//!
//! t3code signs in to a peer's `/mcp` over HTTPS or T3 Connect. Domovoi reaches
//! a peer the way its phones do: over iroh, by the peer's key. A link is made
//! from the peer's pairing link or ticket (`domovoi-server pair --remote`
//! there: `domovoi://connect?ticket=…`, a bare `endpoint…` ticket or endpoint
//! id) and a pairing code from it. The QUIC handshake proves the peer holds
//! the key the ticket names, so the code and then the link's token only ever
//! reach that key: no address can answer as a linked machine.
//!
//! - `peerLinks.link` (`PeerLinkCreateInput`): dials the peer with this
//!   server's own peer-link key (`<home>/remote/peer.key`, apart from the key
//!   phones dial), reads its descriptor (`/.well-known/t3/environment`), and
//!   exchanges the code (`/oauth/token`) for a session bound to that key,
//!   narrowed to the access asked for (`mcpClientScopes`): `read-only` is
//!   `orchestration:read`; `full-access` adds `orchestration:operate`,
//!   `settings:write` and `source-control:write`. Nothing else (terminals,
//!   files, access) is ever granted. The peer lists the link as a paired
//!   client named `Domovoi · <this server>` and revokes it there;
//! - `peerLinks.list`: each link with its status (`expired` past its end, or
//!   once the peer signed it out; `reachable` when the peer answers as itself
//!   and still takes the token; else `unreachable`), its label following the
//!   peer's machine name;
//! - `peerLinks.unlink`: forgets the link and its token here and signs the
//!   session out at the peer (`/api/domovoi/session/end`) when it answers,
//!   so either side can end a link;
//! - agents: `t3_environment_links`, `t3_environment_unlink`, and
//!   `t3_thread_list` with `environmentId`, read through the link from the
//!   peer's `/api/orchestration/shell` (`crate::mcp::environments`).
//!
//! Links are kept in `<userdata>/peer-links.json` (owner-only) and each token
//! in the server secret store (`<userdata>/server-secrets/peer-link-<hex>`),
//! as t3code keeps the row and the token apart.
//!
//! Limits: the access levels between read-only and full access
//! (`approval-required`, `auto-accept-edits`, `auto`) are refused as
//! `incompatible`, since a Domovoi peer cannot hold a linked agent to a
//! runtime-mode ceiling (t3code's `mcpModeLimitHeader` and `linkFence`); no
//! link request cards (`t3_environment_link`, so `peerLinks.answerRequest`
//! only checks the card); no other forwarded tool (t3code forwards every
//! thread tool to the peer's `/mcp`, which serves only its own agents here)
//! and no remote delegation (`delegated_task.remote.*`); http(s)
//! addresses are refused (Domovoi has no T3 Connect or TLS listener to sign
//! in to).

use crate::rpc::admin_failure::{Failure, PeerLinkReason};
use crate::state::AppState;
use domovoi_contracts::time_fmt::{iso_from_millis, now_millis};
use domovoi_remote::{Bridge, EndpointTicket, SecretKey};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

pub const LINKED_ENVIRONMENTS_OFF: &str = "Linked environments is off on this machine. Turn it on in Settings → Labs.";
/// How long dialling a peer and reading its descriptor may take before it counts as unreachable.
const PROBE_TIMEOUT: Duration = Duration::from_secs(20);
/// How long a listing waits on each link.
const LIST_PROBE_TIMEOUT: Duration = Duration::from_secs(5);
/// How long unlinking waits for the peer to sign the session out.
const SIGN_OUT_TIMEOUT: Duration = Duration::from_secs(5);

/// The most a linked environment lets this one's agents do there (`AuthMcpClientAccess`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Access {
    ReadOnly,
    ApprovalRequired,
    AutoAcceptEdits,
    Auto,
    FullAccess,
}

impl Access {
    /// The scopes a session gets for the access (`mcpClientScopes`), for the levels Domovoi can hold a peer to.
    fn scope(self) -> Option<&'static str> {
        match self {
            Access::ReadOnly => Some("orchestration:read"),
            Access::FullAccess => Some("orchestration:read orchestration:operate settings:write source-control:write"),
            _ => None,
        }
    }
}

/// A link to another environment (`PeerLink`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PeerLink {
    pub environment_id: String,
    pub label: String,
    /// The peer's links without a code (`domovoi://connect?ticket=…`), tried in order.
    pub urls: Vec<String>,
    pub access: Access,
    pub linked_at: String,
    pub expires_at: String,
    pub last_reached_at: Option<String>,
    pub last_error: Option<String>,
}

impl PeerLink {
    fn expired(&self) -> bool {
        crate::util::seconds_until(&self.expires_at) <= 0
    }
}

/// Whether a link can be used now (`PeerLinkStatus`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Reachable,
    Unreachable,
    Expired,
}

/// `PeerLinkSummary`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Summary {
    #[serde(flatten)]
    pub link: PeerLink,
    pub status: Status,
}

/// `PeerLinkCreateInput`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateInput {
    pub url: String,
    #[serde(default)]
    pub alternate_urls: Vec<String>,
    pub pairing_code: String,
    pub access: Access,
    #[serde(default)]
    pub expected_environment_id: Option<String>,
    #[serde(default)]
    pub expected_label: Option<String>,
}

/// A link as stored: the link and the name of its token in the secret store.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Stored {
    #[serde(flatten)]
    link: PeerLink,
    secret_name: String,
}

/// What a peer says it is (`ExecutionEnvironmentDescriptor`, the part read here).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Descriptor {
    environment_id: String,
    label: String,
}

/// `AuthSessionState`, the part read here.
#[derive(Deserialize)]
struct SessionState {
    authenticated: bool,
}

/// `AuthAccessTokenResult`, the part read here.
#[derive(Deserialize)]
struct TokenAnswer {
    access_token: String,
    expires_in: i64,
}

fn error(reason: PeerLinkReason, message: impl Into<String>) -> Failure {
    Failure::PeerLink { reason, message: message.into(), answered_as: None }
}

fn storage(cause: impl std::fmt::Display) -> Failure {
    tracing::warn!(error = %cause, "Peer link storage failed");
    error(PeerLinkReason::Storage, "Linked environments could not be read or saved here. The server log has details.")
}

/// One address of a peer: its ticket, and the address as stored.
struct Address {
    ticket: EndpointTicket,
    /// The endpoint id the ticket names.
    key: String,
    url: String,
}

/// The ticket an address names: a pairing link (its code is not used), a
/// ticket, or an endpoint id. A web address is none: Domovoi reaches peers over iroh.
fn address(url: &str) -> Result<Address, Failure> {
    let (ticket, _) = crate::remote::parse_link(url);
    let parsed = domovoi_remote::parse_ticket(&ticket).map_err(|_| {
        // Never the address as given: it may hold a code.
        error(
            PeerLinkReason::Unreachable,
            "That is not a Domovoi link. Domovoi links environments over iroh: use the domovoi://connect link or ticket that `domovoi-server pair --remote` prints on the other machine.",
        )
    })?;
    let key = parsed.endpoint_addr().id.to_string();
    let url = format!("domovoi://connect?{}", url::form_urlencoded::Serializer::new(String::new()).append_pair("ticket", ticket.trim()).finish());
    Ok(Address { ticket: parsed, key, url })
}

/// The links of this server, and the iroh connections to their peers.
pub struct PeerLinks {
    userdata: PathBuf,
    file: PathBuf,
    key: SecretKey,
    /// Serializes read-modify-write of the file.
    write: Mutex<()>,
    /// An open connection to each peer, by its endpoint id.
    bridges: tokio::sync::Mutex<HashMap<String, Arc<Bridge>>>,
}

/// The server's links, opened on first use.
pub fn links(state: &AppState) -> Result<&'static PeerLinks, Failure> {
    static LINKS: OnceLock<PeerLinks> = OnceLock::new();
    if let Some(links) = LINKS.get() {
        return Ok(links);
    }
    let key = crate::remote::load_or_create_key(&state.paths.home.join("remote").join("peer.key")).map_err(storage)?;
    Ok(LINKS.get_or_init(|| PeerLinks {
        userdata: state.paths.userdata.clone(),
        file: state.paths.userdata.join("peer-links.json"),
        key,
        write: Mutex::new(()),
        bridges: Default::default(),
    }))
}

/// Whether the Linked environments beta (Settings → Labs) is on here.
pub async fn enabled(state: &AppState) -> bool {
    state.settings.read().await.linked_environments_enabled == Some(true)
}

impl PeerLinks {
    fn read(&self) -> Result<Vec<Stored>, Failure> {
        match std::fs::read(&self.file) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(storage),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
            Err(e) => Err(storage(e)),
        }
    }

    fn write_all(&self, rows: &[Stored]) -> Result<(), Failure> {
        let tmp = self.file.with_extension("json.tmp");
        crate::remote::write_private(&tmp, &serde_json::to_vec_pretty(rows).map_err(storage)?).map_err(storage)?;
        std::fs::rename(&tmp, &self.file).map_err(storage)
    }

    /// Changes the stored link of `environment_id`, if there is one.
    fn update(&self, environment_id: &str, change: impl FnOnce(&mut PeerLink)) {
        let _guard = crate::util::locked(&self.write);
        let Ok(mut rows) = self.read() else { return };
        if let Some(row) = rows.iter_mut().find(|r| r.link.environment_id == environment_id) {
            change(&mut row.link);
            let _ = self.write_all(&rows);
        }
    }

    fn stored(&self, environment_id: &str) -> Result<Option<Stored>, Failure> {
        Ok(self.read()?.into_iter().find(|r| r.link.environment_id == environment_id))
    }

    /// The loopback origin of an open connection to `address`'s peer, dialled
    /// within `timeout` when there is none. The map is not held while dialling,
    /// so one silent peer never holds up the others.
    async fn origin(&self, state: &AppState, address: &Address, timeout: Duration) -> Result<String, Failure> {
        if let Some(bridge) = self.bridges.lock().await.get(&address.key).filter(|b| b.status().listening) {
            return Ok(format!("http://127.0.0.1:{}", bridge.port()));
        }
        let network = crate::remote::network(&state.remote.settings().relays).map_err(|e| error(PeerLinkReason::Unreachable, e))?;
        let started = tokio::time::timeout(timeout, Bridge::start(self.key.clone(), network, address.ticket.clone(), 0)).await;
        let bridge = match started {
            Ok(Ok(bridge)) => Arc::new(bridge),
            Ok(Err(e)) => return Err(error(PeerLinkReason::Unreachable, format!("{} did not answer ({e}).", short(&address.key)))),
            Err(_) => return Err(error(PeerLinkReason::Unreachable, format!("{} did not answer within {} seconds.", short(&address.key), timeout.as_secs()))),
        };
        // A dial that raced this one may have won: its connection is kept, this one dropped.
        let mut bridges = self.bridges.lock().await;
        let kept = match bridges.get(&address.key).filter(|b| b.status().listening) {
            Some(other) => other.clone(),
            None => {
                bridges.insert(address.key.clone(), bridge.clone());
                bridge
            }
        };
        Ok(format!("http://127.0.0.1:{}", kept.port()))
    }

    /// The environment answering at `address` (`probe`).
    async fn probe(&self, state: &AppState, address: &Address, timeout: Duration) -> Result<(String, Descriptor), Failure> {
        let origin = self.origin(state, address, timeout).await?;
        let answer = tokio::time::timeout(timeout, async {
            http().get(format!("{origin}/.well-known/t3/environment")).send().await?.error_for_status()?.json::<Descriptor>().await
        })
        .await;
        match answer {
            Ok(Ok(descriptor)) => Ok((origin, descriptor)),
            Ok(Err(e)) if e.is_decode() || e.is_status() => {
                Err(error(PeerLinkReason::NotAT3Environment, format!("{} answered, but not as an environment this version understands.", short(&address.key))))
            }
            Ok(Err(_)) => Err(error(PeerLinkReason::Unreachable, format!("{} did not answer.", short(&address.key)))),
            Err(_) => Err(error(PeerLinkReason::Unreachable, format!("{} did not answer within {} seconds.", short(&address.key), timeout.as_secs()))),
        }
    }

    /// `PeerLinks.link`; with `keep_linked`, a peer whose link has not expired
    /// is left as it is, its code unused (`linkUnlessLinked`).
    pub async fn link(&self, state: &AppState, input: CreateInput, keep_linked: bool) -> Result<(PeerLink, bool), Failure> {
        if !enabled(state).await {
            return Err(error(PeerLinkReason::Disabled, LINKED_ENVIRONMENTS_OFF));
        }
        let Some(scope) = input.access.scope() else {
            return Err(error(
                PeerLinkReason::Incompatible,
                "A Domovoi environment cannot hold a linked agent to a runtime mode. Link it read-only or with full access.",
            ));
        };
        let code = input.pairing_code.trim();
        let addresses = std::iter::once(&input.url).chain(&input.alternate_urls).map(|u| address(u)).collect::<Result<Vec<_>, _>>()?;
        let own = state.remote.endpoint_id().to_string();
        if addresses.iter().any(|a| a.key == own) {
            return Err(error(PeerLinkReason::OwnEnvironment, "That address is this environment."));
        }
        // The user picked a machine: link through the addresses that answer as
        // it, so one that answers as another never receives the code.
        let (origin, peer, urls) = match &input.expected_environment_id {
            None => {
                let primary = &addresses[0];
                let (origin, peer) = self.probe(state, primary, PROBE_TIMEOUT).await?;
                (origin, peer, addresses.iter().map(|a| a.url.clone()).collect::<Vec<_>>())
            }
            Some(expected) => {
                let name = input.expected_label.clone().unwrap_or_else(|| expected.clone());
                let mut found: Option<(String, Descriptor)> = None;
                let (mut urls, mut failures, mut answered_as) = (Vec::new(), Vec::new(), None);
                for a in &addresses {
                    match self.probe(state, a, PROBE_TIMEOUT).await {
                        Ok((origin, d)) if d.environment_id == *expected => {
                            urls.push(a.url.clone());
                            found.get_or_insert((origin, d));
                        }
                        Ok((_, d)) => {
                            failures.push(format!("{} answered as another environment, {}.", short(&a.key), d.label));
                            answered_as.get_or_insert(d.label);
                        }
                        Err(Failure::PeerLink { message, .. }) => failures.push(message),
                        Err(other) => return Err(other),
                    }
                }
                let Some((origin, peer)) = found else {
                    return Err(Failure::PeerLink {
                        reason: PeerLinkReason::Unreachable,
                        message: format!("This environment could not reach {name}: {}", failures.join(" ")),
                        answered_as,
                    });
                };
                (origin, peer, urls)
            }
        };
        if peer.environment_id == state.environment.environment_id {
            return Err(error(PeerLinkReason::OwnEnvironment, "That address is this environment."));
        }
        let previous = self.stored(&peer.environment_id)?;
        let current = previous.as_ref().map(|r| r.link.clone()).filter(|l| !l.expired());
        if let (true, Some(current)) = (keep_linked, &current) {
            return Ok((current.clone(), true));
        }
        // An address alone never moves a link to another key.
        if let Some(current) = &current {
            let keys: Vec<String> = current.urls.iter().filter_map(|u| address(u).ok()).map(|a| a.key).collect();
            if !addresses.iter().any(|a| keys.contains(&a.key)) {
                return Err(error(
                    PeerLinkReason::PairingRejected,
                    format!("That address answers as {}, which is already linked by another key. If {} moved, forget that link first.", current.label, current.label),
                ));
            }
        }
        let issued = self.sign_in(&origin, code, scope, &state.environment.label, &peer.label, input.access).await?;
        let now = now_millis();
        let link = PeerLink {
            environment_id: peer.environment_id.clone(),
            label: peer.label.clone(),
            urls,
            access: input.access,
            linked_at: iso_from_millis(now),
            expires_at: iso_from_millis(now + issued.expires_in.max(0) * 1000),
            last_reached_at: Some(iso_from_millis(now)),
            last_error: None,
        };
        // The token goes under a name of its own before the row points at it.
        let secret_name = format!("peer-link-{}", crate::util::random_bytes::<16>().iter().map(|b| format!("{b:02x}")).collect::<String>());
        crate::secrets::set_server_secret(&self.userdata, &secret_name, &issued.access_token).map_err(storage)?;
        let replaced = {
            let _guard = crate::util::locked(&self.write);
            let saved = self.read().and_then(|mut rows| {
                let replaced = rows.iter().position(|r| r.link.environment_id == link.environment_id).map(|i| rows.remove(i));
                rows.push(Stored { link: link.clone(), secret_name: secret_name.clone() });
                rows.sort_by(|a, b| (&a.link.label, &a.link.environment_id).cmp(&(&b.link.label, &b.link.environment_id)));
                self.write_all(&rows).map(|()| replaced)
            });
            match saved {
                Ok(replaced) => replaced,
                Err(e) => {
                    let _ = crate::secrets::remove_server_secret(&self.userdata, &secret_name);
                    return Err(e);
                }
            }
        };
        // Linking again replaces the old token.
        if let Some(old) = replaced.filter(|old| old.secret_name != secret_name) {
            let _ = crate::secrets::remove_server_secret(&self.userdata, &old.secret_name);
        }
        Ok((link, false))
    }

    /// Exchanges the peer's pairing code for a session narrowed to `scope`.
    async fn sign_in(&self, origin: &str, code: &str, scope: &str, own_label: &str, peer_label: &str, access: Access) -> Result<TokenAnswer, Failure> {
        let form = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("grant_type", "urn:ietf:params:oauth:grant-type:token-exchange")
            .append_pair("subject_token", code)
            .append_pair("subject_token_type", "urn:t3:params:oauth:token-type:environment-bootstrap")
            .append_pair("requested_token_type", "urn:ietf:params:oauth:token-type:access_token")
            .append_pair("scope", scope)
            .append_pair("client_label", &format!("Domovoi · {own_label}"))
            .finish();
        let stopped = |_| error(PeerLinkReason::Unreachable, format!("{peer_label} stopped answering while linking."));
        let res = tokio::time::timeout(PROBE_TIMEOUT, http().post(format!("{origin}/oauth/token")).header("content-type", "application/x-www-form-urlencoded").body(form).send())
            .await
            .map_err(|_| error(PeerLinkReason::Unreachable, format!("{peer_label} stopped answering while linking.")))?
            .map_err(stopped)?;
        let status = res.status();
        if status.is_success() {
            return res.json::<TokenAnswer>().await.map_err(|_| {
                error(PeerLinkReason::Incompatible, format!("{peer_label} answered the sign-in in a shape this version does not understand. Update both environments."))
            });
        }
        let body = res.text().await.unwrap_or_default();
        if status.as_u16() == 400 && body.contains("scope_not_granted") {
            let level = serde_json::to_string(&access).unwrap_or_default();
            return Err(error(PeerLinkReason::PairingRejected, format!("That pairing code from {peer_label} does not grant {} access.", level.trim_matches('"'))));
        }
        if status.as_u16() == 401 {
            return Err(error(PeerLinkReason::PairingRejected, "The peer did not approve the link. Check the pairing code and try again."));
        }
        if status.as_u16() == 429 {
            return Err(error(PeerLinkReason::PairingRejected, format!("{peer_label} refused more pairing attempts for now. Try again in a minute.")));
        }
        Err(error(PeerLinkReason::Incompatible, format!("{peer_label} answered the sign-in with HTTP {}. Update both environments.", status.as_u16())))
    }

    /// A stored link's status (`statusOf`), its label following the peer's.
    async fn status_of(&self, state: &AppState, stored: Stored) -> Summary {
        let mut link = stored.link;
        if link.expired() {
            return Summary { link, status: Status::Expired };
        }
        let token = crate::secrets::server_secret(&self.userdata, &stored.secret_name).ok().flatten();
        let Some(token) = token else {
            link.last_error = Some(format!("The link to {} lost its credential. Link it again.", link.label));
            return Summary { link, status: Status::Expired };
        };
        for a in link.urls.iter().filter_map(|u| address(u).ok()) {
            let Ok((origin, peer)) = self.probe(state, &a, LIST_PROBE_TIMEOUT).await else { continue };
            if peer.environment_id != link.environment_id {
                continue;
            }
            let session = tokio::time::timeout(LIST_PROBE_TIMEOUT, async {
                http().get(format!("{origin}/api/auth/session")).bearer_auth(&token).send().await?.json::<SessionState>().await
            })
            .await;
            let now = iso_from_millis(now_millis());
            let (label, environment_id) = (peer.label.clone(), link.environment_id.clone());
            return match session {
                Ok(Ok(s)) if s.authenticated => {
                    link.label = label.clone();
                    link.last_reached_at = Some(now.clone());
                    link.last_error = None;
                    self.update(&environment_id, |l| {
                        l.label = label;
                        l.last_reached_at = Some(now);
                        l.last_error = None;
                    });
                    Summary { link, status: Status::Reachable }
                }
                // The peer signed the link out: only a new code brings it back.
                Ok(Ok(_)) => {
                    let message = format!("{label} signed this environment out. Link it again with a new pairing code from {label}.");
                    link.label = label.clone();
                    link.last_error = Some(message.clone());
                    link.expires_at = now.clone();
                    self.update(&environment_id, |l| {
                        l.label = label;
                        l.last_error = Some(message);
                        l.expires_at = now;
                    });
                    Summary { link, status: Status::Expired }
                }
                _ => Summary { link, status: Status::Unreachable },
            };
        }
        Summary { link, status: Status::Unreachable }
    }

    /// `PeerLinks.list`: every link with whether it can be used now.
    pub async fn list(&self, state: &AppState) -> Result<Vec<Summary>, Failure> {
        let rows = self.read()?;
        Ok(futures::future::join_all(rows.into_iter().map(|row| self.status_of(state, row))).await)
    }

    /// An address that answers as the linked peer, with its token (`resolve`).
    pub async fn resolve(&self, state: &AppState, environment_id: &str) -> Result<(PeerLink, String, String), Failure> {
        let not_linked = || error(PeerLinkReason::UnknownLink, format!("This environment is not linked to {environment_id}. t3_environment_links lists the linked ones."));
        let stored = self.stored(environment_id)?.ok_or_else(not_linked)?;
        let link = stored.link;
        if link.expired() {
            return Err(error(PeerLinkReason::Expired, format!("The link to {0} expired. Link it again with a new pairing code from {0}.", link.label)));
        }
        let Some(token) = crate::secrets::server_secret(&self.userdata, &stored.secret_name).map_err(storage)? else {
            return Err(error(PeerLinkReason::UnknownLink, format!("The link to {} lost its credential. Link it again.", link.label)));
        };
        for a in link.urls.iter().filter_map(|u| address(u).ok()) {
            if let Ok((origin, peer)) = self.probe(state, &a, PROBE_TIMEOUT).await
                && peer.environment_id == link.environment_id
            {
                return Ok((link, origin, token));
            }
        }
        Err(error(PeerLinkReason::Unreachable, format!("{} did not answer.", link.label)))
    }

    /// Reads `path` from the linked environment with the link's token, and
    /// records whether it answered (`recordOutcome`). The peer holds the call
    /// to the link's access.
    pub async fn fetch<T: serde::de::DeserializeOwned>(&self, state: &AppState, environment_id: &str, path: &str) -> Result<T, Failure> {
        let (link, origin, token) = self.resolve(state, environment_id).await?;
        let answer = tokio::time::timeout(PROBE_TIMEOUT, async { http().get(format!("{origin}{path}")).bearer_auth(&token).send().await }).await;
        let outcome = match answer {
            Ok(Ok(res)) if res.status().is_success() => res.json::<T>().await.map_err(|_| format!("{} answered in a shape this version does not understand.", link.label)),
            Ok(Ok(res)) if matches!(res.status().as_u16(), 401 | 403) => Err(format!("{} refused the call: the link does not allow it.", link.label)),
            Ok(Ok(res)) => Err(format!("{} answered with HTTP {}.", link.label, res.status().as_u16())),
            _ => Err(format!("{} did not answer.", link.label)),
        };
        let now = iso_from_millis(now_millis());
        let recorded = outcome.as_ref().err().cloned();
        self.update(environment_id, |l| match recorded {
            None => {
                l.last_reached_at = Some(now);
                l.last_error = None;
            }
            Some(e) => l.last_error = Some(e),
        });
        outcome.map_err(|message| error(PeerLinkReason::Unreachable, message))
    }

    /// `PeerLinks.unlink`: forgets the link here, and signs its session out at the peer when the peer answers.
    pub async fn unlink(&self, state: &AppState, environment_id: &str) -> Result<bool, Failure> {
        let Some(stored) = self.stored(environment_id)? else { return Ok(false) };
        let token = crate::secrets::server_secret(&self.userdata, &stored.secret_name).ok().flatten();
        if let Some(token) = token {
            for a in stored.link.urls.iter().filter_map(|u| address(u).ok()) {
                let signed_out = tokio::time::timeout(SIGN_OUT_TIMEOUT, async {
                    let origin = self.origin(state, &a, SIGN_OUT_TIMEOUT).await.ok()?;
                    http().post(format!("{origin}/api/domovoi/session/end")).bearer_auth(&token).send().await.ok()?.status().is_success().then_some(())
                })
                .await;
                if matches!(signed_out, Ok(Some(()))) {
                    break;
                }
            }
        }
        let removed = {
            let _guard = crate::util::locked(&self.write);
            let mut rows = self.read()?;
            let before = rows.len();
            rows.retain(|r| r.link.environment_id != environment_id);
            let removed = rows.len() != before;
            if removed {
                self.write_all(&rows)?;
            }
            removed
        };
        crate::secrets::remove_server_secret(&self.userdata, &stored.secret_name).map_err(storage)?;
        let keys: Vec<String> = stored.link.urls.iter().filter_map(|u| address(u).ok()).map(|a| a.key).collect();
        self.bridges.lock().await.retain(|key, _| !keys.contains(key));
        Ok(removed)
    }
}

/// A client that follows no redirect: only the peer the ticket names may receive a code or token.
fn http() -> reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().unwrap_or_default()).clone()
}

/// An endpoint id short enough for a message.
fn short(key: &str) -> String {
    format!("The environment {}", key.get(..10).unwrap_or(key))
}

/// Where the links of a home directory are kept (for tests and tools).
#[cfg(test)]
pub fn file_in(userdata: &std::path::Path) -> PathBuf {
    userdata.join("peer-links.json")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn links_are_written_as_the_client_reads_them() {
        let link = PeerLink {
            environment_id: "env-b".into(),
            label: "Box".into(),
            urls: vec!["domovoi://connect?ticket=abc".into()],
            access: Access::ReadOnly,
            linked_at: "2026-11-24T10:00:00.000Z".into(),
            expires_at: "2026-12-24T10:00:00.000Z".into(),
            last_reached_at: None,
            last_error: None,
        };
        assert_eq!(
            json!(Summary { link: link.clone(), status: Status::Reachable }),
            json!({ "environmentId": "env-b", "label": "Box", "urls": ["domovoi://connect?ticket=abc"], "access": "read-only", "linkedAt": "2026-11-24T10:00:00.000Z",
                "expiresAt": "2026-12-24T10:00:00.000Z", "lastReachedAt": null, "lastError": null, "status": "reachable" })
        );
        let stored: Stored = serde_json::from_value(json!({ "secretName": "peer-link-1", "environmentId": "env-b", "label": "Box", "urls": [], "access": "full-access",
            "linkedAt": "x", "expiresAt": "y", "lastReachedAt": null, "lastError": "gone" }))
        .unwrap();
        assert_eq!((stored.secret_name.as_str(), stored.link.access, stored.link.last_error.as_deref()), ("peer-link-1", Access::FullAccess, Some("gone")));
        assert!(file_in(std::path::Path::new("/u")).ends_with("peer-links.json"));
    }

    #[test]
    fn only_read_only_and_full_access_are_held() {
        assert_eq!(Access::ReadOnly.scope(), Some("orchestration:read"));
        assert!(Access::FullAccess.scope().is_some_and(|s| s.contains("orchestration:operate") && !s.contains("terminal") && !s.contains("filesystem") && !s.contains("access:")));
        for level in [Access::ApprovalRequired, Access::AutoAcceptEdits, Access::Auto] {
            assert_eq!(level.scope(), None);
        }
    }

    #[test]
    fn addresses_are_iroh_tickets_never_web_addresses() {
        let id = SecretKey::generate().public().to_string();
        let link = crate::remote::pairing_link(&id, "SECRETCODE12", "Box");
        let a = address(&link).unwrap();
        assert_eq!(a.key, id);
        assert!(!a.url.contains("SECRETCODE12"), "the code is never stored: {}", a.url);
        assert_eq!(address(&a.url).unwrap().key, id, "a stored address reads back");
        assert_eq!(address(&format!(" {id} ")).unwrap().key, id);
        for web in ["https://box.example.com", "http://192.0.2.10:3773", "not an address"] {
            let Err(Failure::PeerLink { reason, message, .. }) = address(web) else { panic!("{web} accepted") };
            assert_eq!(reason, PeerLinkReason::Unreachable);
            assert!(!message.contains(web));
        }
    }
}
