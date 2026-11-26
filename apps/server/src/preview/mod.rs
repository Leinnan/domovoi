//! The preview browser and the preview tunnel.
//!
//! - `tunnel`: `/api/preview/tunnel`, raw TCP from a client to this
//!   machine's loopback ports.
//! - `host`: what the rest of the server asks of the headless browser; with
//!   the `preview-browser` feature it is `browser`
//!   over `cdp`, launched by `chrome`, with page scripts in `page`.
//! - `stream`: `/api/preview-stream/*`, a server tab's screencast and input.
//!
//! The tab records themselves are `crate::rpc::preview`.

#[cfg_attr(not(feature = "preview-browser"), expect(dead_code, reason = "without the browser nothing is launched"))]
pub mod chrome;
#[cfg_attr(not(feature = "preview-browser"), expect(dead_code, reason = "without the browser nothing reads what agents ask"))]
pub mod host;
pub mod stream;
pub mod tunnel;

#[cfg(feature = "preview-browser")]
mod browser;
#[cfg(feature = "preview-browser")]
mod cdp;
#[cfg(feature = "preview-browser")]
mod page;

use crate::auth::Session;
use crate::state::SharedState;
use axum::Extension;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use crate::wire::auth::AuthInvalidError;
use domovoi_model::typed::{InternalRejection, RequestRejection, ScopeRequiredRejection};
use domovoi_remote::RemotePeer;

/// The iroh peer a request came through (`None` for TCP).
pub type Peer = Option<Extension<RemotePeer>>;

fn trace_id() -> String {
    crate::util::sha256_hex(&crate::util::random_bytes::<16>())[..32].to_owned()
}

fn unauthorized(reason: &str) -> Response {
    (StatusCode::UNAUTHORIZED, axum::Json(AuthInvalidError::new(reason, trace_id()))).into_response()
}

/// t3code `failEnvironmentScopeRequired` (HTTP 403).
fn scope_required(permission: &str) -> Response {
    let rejection = RequestRejection::ScopeRequired(ScopeRequiredRejection {
        code: "insufficient_scope".into(),
        required_scope: crate::permissions::legacy_parent(permission).into(),
        required_permission: Some(permission.to_owned()),
        trace_id: trace_id(),
    });
    (StatusCode::FORBIDDEN, axum::Json(rejection)).into_response()
}

fn internal(e: impl std::fmt::Display) -> Response {
    tracing::error!("internal error: {e}");
    let rejection = RequestRejection::Internal(InternalRejection { code: "internal_error".into(), reason: None, trace_id: None });
    (StatusCode::INTERNAL_SERVER_ERROR, axum::Json(rejection)).into_response()
}

/// Who a media request is from: a WebSocket ticket in the query
/// (`wsTicket`), else the request's bearer token; either way a session
/// holding `permission`.
pub fn authenticate_media(state: &SharedState, headers: &HeaderMap, peer: &Peer, ticket: Option<&str>, permission: &str) -> Result<Session, Response> {
    let peer = peer.as_ref().map(|Extension(RemotePeer(id))| id.to_string());
    let session = match ticket.filter(|t| !t.trim().is_empty()) {
        Some(ticket) => match state.auth.redeem_ws_ticket(ticket, peer.as_deref()) {
            Ok(Some(s)) => s,
            Ok(None) => return Err(unauthorized("invalid_credential")),
            Err(e) => return Err(internal(e)),
        },
        None => match state.auth.authenticate(headers, peer.as_deref()) {
            Ok(Some(s)) => s,
            Ok(None) => return Err(unauthorized(if crate::auth::bearer_token(headers).is_some() { "invalid_credential" } else { "missing_credential" })),
            Err(e) => return Err(internal(e)),
        },
    };
    if session.permissions.iter().any(|p| p == permission) { Ok(session) } else { Err(scope_required(permission)) }
}

/// How often a long-lived media socket checks that its session still stands.
const SESSION_CHECK: std::time::Duration = std::time::Duration::from_secs(2);

/// Completes once the session of `token_hash` is gone (revoked, signed out,
/// expired) or the server stops, like t3code's `awaitInvalidation`. It does
/// not register a socket, so a tunnel does not make an app count as open.
pub async fn session_ended(state: SharedState, token_hash: String) {
    loop {
        tokio::select! {
            () = state.shutdown.cancelled() => return,
            () = tokio::time::sleep(SESSION_CHECK) => {}
        }
        if !matches!(state.auth.session_for_hash(&token_hash), Ok(Some(_))) {
            return;
        }
    }
}
