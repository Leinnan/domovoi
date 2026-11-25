//! HTTP routes (t3code `EnvironmentHttpApi` subset) and the RPC WebSocket.
//!
//! Domovoi's own, not t3code's: `/health` (the process serves HTTP) and
//! `/ready` (the database answers and startup recovery has run), both
//! unauthenticated, for systemd and monitoring. HTTP responses are gzip or
//! brotli compressed when the client asks (`Accept-Encoding`), except the
//! RPC socket, webhook URLs and media that is compressed already.
//!
//! The RPC socket refuses a client of another orchestration protocol with
//! HTTP 426 before any authentication, as t3code's `/ws` route does.

#![expect(clippy::disallowed_types, clippy::disallowed_macros, reason = "the MCP route: a JSON-RPC body and its parse-error answer")]

use crate::auth::{AuthError, ClientMeta, METHOD_BEARER, NewSession, Session, origin_matches_host};
use crate::control;
use crate::rpc;
use crate::state::{ConnCtx, SharedState};
use axum::extract::{Extension, Form, Query, Request, State, WebSocketUpgrade};
use axum::middleware::Next;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use effect_rpc::{ConnectionOptions, Router as RpcRouter, axum_ws::serve_websocket_until};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use domovoi_contracts::auth::{AuthInvalidError, BrowserSessionRequest, BrowserSessionResponse};
use domovoi_model::typed::{
    AuthAccessTokenResult, AuthTokenType, AuthWebSocketTicketResult, InternalRejection, OperationForbiddenRejection, RateLimitedRejection,
    RemoteAccessRejection, RequestInvalidRejection, RequestRejection, ScopeRequiredRejection,
};
use domovoi_remote::RemotePeer;
use domovoi_contracts::environment::ORCHESTRATION_PROTOCOL_VERSION;

/// The iroh peer a request came through, as its endpoint id (`None` for TCP).
type Peer = Option<Extension<RemotePeer>>;

fn peer_id(peer: &Peer) -> Option<String> {
    peer.as_ref().map(|Extension(RemotePeer(id))| id.to_string())
}

/// Routes an iroh peer without a session may use: discovery and pairing.
const PAIRING_ROUTES: &[&str] = &["/.well-known/t3/environment", "/api/auth/session", "/api/auth/browser-session", "/oauth/token"];

fn rpc_router() -> Arc<RpcRouter<ConnCtx>> {
    static ROUTER: OnceLock<Arc<RpcRouter<ConnCtx>>> = OnceLock::new();
    ROUTER.get_or_init(|| Arc::new(rpc::router())).clone()
}

pub fn router(state: SharedState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/ready", get(ready))
        .route("/.well-known/t3/environment", get(environment))
        .route("/api/auth/session", get(auth_session))
        .route("/api/auth/browser-session", post(browser_session))
        .route("/oauth/token", post(token_exchange))
        .route("/api/auth/websocket-ticket", post(ws_ticket))
        .route("/api/auth/websocket-ticket/{use}", post(ws_ticket))
        .route("/api/auth/clients/revoke", post(revoke_client))
        .route("/api/auth/clients/revoke-others", post(revoke_other_clients))
        .route("/api/domovoi/remote", get(remote_status))
        .route("/api/domovoi/remote/pairing", post(remote_pairing))
        .route("/api/domovoi/remote/configure", post(remote_configure))
        .route("/api/domovoi/server/stop", post(server_stop))
        .route("/api/domovoi/session/end", post(end_session))
        .route("/api/domovoi/push/devices", post(push_register).delete(push_unregister))
        .route("/api/domovoi/push/activities", post(activity_register).delete(activity_unregister))
        .merge(crate::push_web::routes())
        .route("/api/orchestration/shell", get(shell_snapshot))
        .route("/mcp", post(mcp_post).get(|| async { StatusCode::METHOD_NOT_ALLOWED }).delete(|| async { StatusCode::METHOD_NOT_ALLOWED }))
        .route("/api/observability/v1/traces", post(|| async { StatusCode::NO_CONTENT }))
        .route("/api/assets/attachment/{token}", get(attachment_asset))
        .route(&format!("{}/{{token}}", crate::file_images::ROUTE_PREFIX), get(project_image))
        .route(&format!("{}/{{token}}", crate::diagrams::ROUTE_PREFIX), get(diagram_image))
        .route(&format!("{}/{{token}}", crate::diagrams::ROUTE_PREFIX_SVG), get(diagram_svg))
        .route(&format!("{}/{{token}}/{{*path}}", crate::artifacts::ROUTE_PREFIX_BUNDLE), get(artifact_bundle))
        .route(&format!("{}/{{token}}", crate::artifacts::ROUTE_PREFIX_REACT), get(artifact_react))
        .route(&format!("{}/{{file}}", crate::artifacts::ROUTE_PREFIX_RUNTIME), get(artifact_runtime))
        .route(&format!("{}/{{token}}", crate::tool_images::ROUTE_PREFIX), get(tool_image))
        .route("/api/assets/{*rest}", get(|| async { StatusCode::NOT_FOUND }))
        // PROVIDER_SEND_TURN_MAX_FILE_BYTES (50 MiB); axum's default cap is 2 MiB.
        .route(
            "/api/attachments/upload/{token}",
            post(attachment_upload).layer(axum::extract::DefaultBodyLimit::max(50 * 1024 * 1024)),
        )
        // Only the routes above are compressed.
        .layer(compression())
        .route("/ws", get(ws))
        .route(crate::preview::tunnel::ROUTE, get(crate::preview::tunnel::handler))
        .route(crate::preview::stream::ROUTE, get(crate::preview::stream::handler).post(crate::preview::stream::upload))
        .route("/api/hooks/{task}/{token}", hooks_route())
        .layer(axum::middleware::from_fn_with_state(state.clone(), gate_unknown_peers))
        .with_state(state)
}

/// Media types whose bodies are compressed already (or are opaque bytes),
/// by prefix: compressing them again costs time and saves nothing.
const PRECOMPRESSED: &[&str] = &[
    "audio/",
    "video/",
    "font/woff",
    "application/zip",
    "application/gzip",
    "application/x-gzip",
    "application/zstd",
    "application/x-7z-compressed",
    "application/x-bzip2",
    "application/x-xz",
    "application/x-rar",
    "application/pdf",
    "application/octet-stream",
];

/// gzip and brotli for responses of 32 bytes or more, when the client
/// accepts them; never images (but SVG), event streams or `PRECOMPRESSED`.
fn compression() -> tower_http::compression::CompressionLayer<impl tower_http::compression::Predicate> {
    use tower_http::compression::{CompressionLayer, DefaultPredicate, Predicate};
    let not_precompressed = |_: StatusCode, _: axum::http::Version, headers: &HeaderMap, _: &axum::http::Extensions| {
        let media = headers.get(header::CONTENT_TYPE).and_then(|v| v.to_str().ok()).unwrap_or_default().to_ascii_lowercase();
        !PRECOMPRESSED.iter().any(|prefix| media.starts_with(prefix))
    };
    CompressionLayer::new().no_deflate().no_zstd().compress_when(DefaultPredicate::new().and(not_precompressed))
}

/// `/health`: the process is up and serves HTTP.
async fn health() -> Response {
    ([(header::CACHE_CONTROL, "no-store")], Json(Health { status: "ok" })).into_response()
}

/// `/ready`: 200 once the database answers and startup recovery has run, 503 before.
async fn ready(State(state): State<SharedState>) -> Response {
    let store = state.store.clone();
    let database = tokio::task::spawn_blocking(move || store.db().read(|c| c.query_row("SELECT 1", [], |row| row.get::<_, i64>(0))).is_ok())
        .await
        .unwrap_or(false);
    let recovered = crate::orchestration::recovery::recovered();
    let (code, status) = match (database, recovered) {
        (true, true) => (StatusCode::OK, "ready"),
        (false, _) => (StatusCode::SERVICE_UNAVAILABLE, "database_unavailable"),
        (true, false) => (StatusCode::SERVICE_UNAVAILABLE, "recovering"),
    };
    (code, [(header::CACHE_CONTROL, "no-store")], Json(Health { status })).into_response()
}

/// The body of `/health` and `/ready`.
#[derive(Serialize)]
struct Health {
    status: &'static str,
}

/// Webhook task URLs and nothing else, for `--hooks-listen`.
pub fn hooks_router(state: SharedState) -> Router {
    Router::new().route("/api/hooks/{task}/{token}", hooks_route()).with_state(state)
}

fn hooks_route() -> axum::routing::MethodRouter<SharedState> {
    axum::routing::any(webhook).layer(axum::extract::DefaultBodyLimit::max(crate::webhooks::MAX_BODY_BYTES))
}

/// A webhook task's URL (t3code `webhookRoute`): the body is read raw, so the
/// signature is checked over the exact bytes.
async fn webhook(
    State(state): State<SharedState>,
    axum::extract::Path((task, token)): axum::extract::Path<(String, String)>,
    method: axum::http::Method,
    uri: axum::http::Uri,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    use axum::http::{HeaderValue, Method};
    let respond = |status: u16, body: String, outcome: &'static str| -> Response {
        let mut res = Response::new(axum::body::Body::from(body));
        *res.status_mut() = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        res.headers_mut().insert(axum::http::header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
        res.headers_mut().insert("x-t3-hook-outcome", HeaderValue::from_static(outcome));
        res
    };
    if !matches!(method, Method::GET | Method::POST | Method::PUT | Method::PATCH) {
        // Link checkers probe with HEAD.
        let mut res = respond(405, "{\"error\":\"method_not_allowed\"}".into(), "method_not_allowed");
        res.headers_mut().insert(axum::http::header::ALLOW, HeaderValue::from_static("GET, POST, PUT, PATCH"));
        return res;
    }
    let headers = headers.iter().filter_map(|(k, v)| v.to_str().ok().map(|v| (k.as_str().to_ascii_lowercase(), v.to_owned()))).collect();
    let request = crate::webhooks::WebhookRequest {
        method: method.as_str().to_owned(),
        path: format!("/api/hooks/{}", crate::util::encode_uri_component(&task)),
        query: uri.query().unwrap_or_default().to_owned(),
        headers,
        body_text: String::from_utf8_lossy(&body).into_owned(),
        body: body.to_vec(),
    };
    let service = crate::scheduled_tasks::service(&state);
    let (status, json, outcome) = crate::webhooks::trigger(service, &task, &token, request).await.http();
    respond(status, json, outcome)
}

/// iroh peers without a live session reach only the pairing routes, and
/// failed pairing attempts over iroh are rate-limited.
async fn gate_unknown_peers(State(state): State<SharedState>, request: Request, next: Next) -> Response {
    let Some(RemotePeer(peer)) = request.extensions().get::<RemotePeer>().copied() else {
        return next.run(request).await;
    };
    let peer = peer.to_string();
    let known = match state.auth.known_peer(&peer) {
        Ok(known) => known,
        Err(e) => return internal(e),
    };
    if known {
        return next.run(request).await;
    }
    if !PAIRING_ROUTES.contains(&request.uri().path()) {
        return unauthorized("missing_credential");
    }
    // A stranger exchanging a code: counted before it runs, so parallel
    // requests cannot slip past the limit.
    if request.method() == axum::http::Method::POST && !state.remote.allow_attempt(&peer) {
        return rate_limited();
    }
    next.run(request).await
}

/// HTTP 429 in the shape of t3code's environment errors.
fn rate_limited() -> Response {
    let rejection = RequestRejection::RateLimited(RateLimitedRejection { code: "rate_limited".into(), retry_after_seconds: 60, trace_id: trace_id() });
    let mut res = (StatusCode::TOO_MANY_REQUESTS, Json(rejection)).into_response();
    res.headers_mut().insert(header::RETRY_AFTER, HeaderValue::from_static("60"));
    res
}

/// `EnvironmentOperationForbiddenError` (HTTP 403).
fn forbidden(reason: &str) -> Response {
    let rejection = RequestRejection::OperationForbidden(OperationForbiddenRejection { code: "forbidden".into(), reason: reason.into(), trace_id: trace_id() });
    (StatusCode::FORBIDDEN, Json(rejection)).into_response()
}

fn unauthorized(reason: &str) -> Response {
    let trace_id = crate::util::sha256_hex(&crate::util::random_bytes::<16>())[..32].to_owned();
    (StatusCode::UNAUTHORIZED, Json(AuthInvalidError::new(reason, trace_id))).into_response()
}

fn internal(e: impl std::fmt::Display) -> Response {
    tracing::error!("internal error: {e}");
    let rejection = RequestRejection::Internal(InternalRejection { code: "internal_error".into(), reason: None, trace_id: None });
    (StatusCode::INTERNAL_SERVER_ERROR, Json(rejection)).into_response()
}

fn trace_id() -> String {
    crate::util::sha256_hex(&crate::util::random_bytes::<16>())[..32].to_owned()
}

/// `EnvironmentRequestInvalidError` (HTTP 400).
fn invalid_request(reason: &str) -> Response {
    let rejection = RequestRejection::RequestInvalid(RequestInvalidRejection { code: "invalid_request".into(), reason: reason.into(), trace_id: trace_id() });
    (StatusCode::BAD_REQUEST, Json(rejection)).into_response()
}

/// `EnvironmentScopeRequiredError` (HTTP 403).
fn scope_required(permission: &str) -> Response {
    let scope = permission.split(':').next().map(|s| format!("{s}:write")).unwrap_or_default();
    let rejection = RequestRejection::ScopeRequired(ScopeRequiredRejection {
        code: "insufficient_scope".into(),
        required_scope: scope.as_str().into(),
        required_permission: Some(permission.to_owned()),
        trace_id: trace_id(),
    });
    (StatusCode::FORBIDDEN, Json(rejection)).into_response()
}

fn require_permission(state: &SharedState, headers: &HeaderMap, peer: &Peer, permission: &str) -> Result<Session, Response> {
    let session = require_session(state, headers, peer)?;
    if session.permissions.iter().any(|p| p == permission) { Ok(session) } else { Err(scope_required(permission)) }
}

fn require_session(state: &SharedState, headers: &HeaderMap, peer: &Peer) -> Result<Session, Response> {
    match state.auth.authenticate(headers, peer_id(peer).as_deref()) {
        Ok(Some(s)) => Ok(s),
        Ok(None) => Err(unauthorized(if crate::auth::bearer_token(headers).is_some() || crate::auth::cookie_value(headers, state.auth.cookie_name()).is_some() {
            "invalid_credential"
        } else {
            "missing_credential"
        })),
        Err(e) => Err(internal(e)),
    }
}

/// Signed one-time attachment uploads (the token is the credential).
/// An attachment through the signed URL `assets.createUrl` handed out.
async fn attachment_asset(State(state): State<SharedState>, axum::extract::Path(token): axum::extract::Path<String>) -> Response {
    let Some((file, mime)) = crate::attachments::read_asset(&state.paths.userdata, &token) else {
        return (StatusCode::NOT_FOUND, "Not Found").into_response();
    };
    match tokio::fs::read(&file).await {
        Ok(bytes) => {
            let disposition = if mime.starts_with("image/") && mime != "image/svg+xml" { "inline" } else { "attachment" };
            (
                [
                    (axum::http::header::CONTENT_TYPE, mime),
                    (axum::http::header::CONTENT_DISPOSITION, disposition),
                    (axum::http::header::CACHE_CONTROL, "private, max-age=86400"),
                    (axum::http::header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
                ],
                bytes,
            )
                .into_response()
        }
        Err(_) => (StatusCode::NOT_FOUND, "Not Found").into_response(),
    }
}

async fn project_image(State(state): State<SharedState>, axum::extract::Path(token): axum::extract::Path<String>) -> Response {
    let Some((mime, bytes)) = crate::file_images::read(&state.paths.userdata, &token).await else {
        return StatusCode::NOT_FOUND.into_response();
    };
    (
        [
            (axum::http::header::CONTENT_TYPE, mime),
            (axum::http::header::CONTENT_DISPOSITION, "inline"),
            (axum::http::header::CACHE_CONTROL, "private, no-store"),
            (axum::http::header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        bytes,
    ).into_response()
}

async fn tool_image(State(state): State<SharedState>, axum::extract::Path(token): axum::extract::Path<String>) -> Response {
    let Some((mime, bytes)) = crate::tool_images::read(&state.paths.userdata, &state.store, &token) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    (
        [
            (axum::http::header::CONTENT_TYPE, mime),
            (axum::http::header::CONTENT_DISPOSITION, "inline"),
            (axum::http::header::CACHE_CONTROL, "private, max-age=86400"),
            (axum::http::header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        bytes,
    ).into_response()
}

async fn diagram_image(State(state): State<SharedState>, axum::extract::Path(token): axum::extract::Path<String>) -> Response {
    let Some(png) = crate::diagrams::read(&state.paths.userdata, &token) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    (
        [
            (axum::http::header::CONTENT_TYPE, "image/png"),
            (axum::http::header::CONTENT_DISPOSITION, "inline"),
            (axum::http::header::CACHE_CONTROL, "private, max-age=300"),
            (axum::http::header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        png,
    ).into_response()
}

async fn diagram_svg(State(state): State<SharedState>, axum::extract::Path(token): axum::extract::Path<String>) -> Response {
    let Some(svg) = crate::diagrams::read_svg(&state.paths.userdata, &token) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    (
        [
            (axum::http::header::CONTENT_TYPE, "image/svg+xml; charset=utf-8"),
            (axum::http::header::CONTENT_DISPOSITION, "inline"),
            (axum::http::header::CACHE_CONTROL, "private, max-age=300"),
            (axum::http::header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (
                axum::http::header::CONTENT_SECURITY_POLICY,
                "default-src 'none'; style-src 'unsafe-inline'; sandbox",
            ),
        ],
        svg,
    )
        .into_response()
}

async fn artifact_bundle(
    State(state): State<SharedState>,
    axum::extract::Path((token, path)): axum::extract::Path<(String, String)>,
) -> Response {
    let Some((mime, bytes)) = crate::artifacts::read_bundle(&state.paths.userdata, &token, &path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    (
        [
            (axum::http::header::CONTENT_TYPE, mime),
            (axum::http::header::CONTENT_DISPOSITION, "inline"),
            (axum::http::header::CACHE_CONTROL, "private, no-store"),
            (axum::http::header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            (
                axum::http::header::CONTENT_SECURITY_POLICY,
                "default-src 'self' data: blob:; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; font-src 'self' data:; media-src 'self' data: blob:; connect-src 'self'; frame-src 'none'; worker-src 'none'; form-action 'none'; base-uri 'self'",
            ),
        ],
        bytes,
    )
        .into_response()
}

async fn artifact_react(State(state): State<SharedState>, axum::extract::Path(token): axum::extract::Path<String>) -> Response {
    let Some(html) = crate::artifacts::read_react_preview(&state.paths.userdata, &token) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    (
        [
            (axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8"),
            (axum::http::header::CONTENT_DISPOSITION, "inline"),
            (axum::http::header::CACHE_CONTROL, "private, max-age=300"),
            (axum::http::header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        html,
    )
        .into_response()
}

async fn artifact_runtime(axum::extract::Path(file): axum::extract::Path<String>) -> Response {
    let Some((mime, bytes)) = crate::artifacts::read_runtime_asset(&file) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    (
        [
            (axum::http::header::CONTENT_TYPE, mime),
            (axum::http::header::CACHE_CONTROL, "public, max-age=31536000, immutable"),
            (axum::http::header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        bytes,
    )
        .into_response()
}

async fn attachment_upload(
    State(state): State<SharedState>,
    axum::extract::Path(token): axum::extract::Path<String>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let userdata = &state.paths.userdata;
    let Some(claims) = crate::attachments::validate_token(userdata, &token) else {
        return (StatusCode::NOT_FOUND, "Not Found").into_response();
    };
    if let Some(len) = headers.get(axum::http::header::CONTENT_LENGTH).and_then(|v| v.to_str().ok()) {
        if len.parse::<u64>().ok() != Some(claims.size_bytes) {
            return (StatusCode::BAD_REQUEST, "Content-Length must match the upload size.").into_response();
        }
    }
    match crate::attachments::store_upload(userdata, &claims, &body) {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err((status, detail)) => (StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR), detail).into_response(),
    }
}

async fn environment(State(state): State<SharedState>) -> impl IntoResponse {
    Json(state.environment.clone())
}

async fn auth_session(State(state): State<SharedState>, headers: HeaderMap, peer: Peer) -> Response {
    match state.auth.authenticate(&headers, peer_id(&peer).as_deref()) {
        Ok(session) => {
            let mut res = Json(state.auth.session_state(session.as_ref())).into_response();
            // Sessions renew while in use; a browser's cookie follows along.
            if let Some(session) = &session
                && crate::auth::bearer_token(&headers).is_none()
                && let Some(token) = crate::auth::cookie_value(&headers, state.auth.cookie_name())
                && let Ok(cookie) = HeaderValue::from_str(&state.auth.session_cookie(&token, session))
            {
                res.headers_mut().insert(header::SET_COOKIE, cookie);
            }
            res
        }
        Err(e) => internal(e),
    }
}

async fn browser_session(State(state): State<SharedState>, peer: Peer, Json(body): Json<BrowserSessionRequest>) -> Response {
    match state.auth.create_session(&body.credential, NewSession::cookie(peer_id(&peer))) {
        Ok((token, session)) => {
            state.pulse.notify_one();
            let cookie = state.auth.session_cookie(&token, &session);
            let mut res = Json(BrowserSessionResponse {
                authenticated: true,
                scopes: session.scopes,
                permissions: session.permissions,
                session_method: "browser-session-cookie".into(),
                expires_at: session.expires_at,
            })
            .into_response();
            res.headers_mut().insert(header::SET_COOKIE, HeaderValue::from_str(&cookie).expect("the cookie is a token and ASCII attributes"));
            res.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            res
        }
        Err(AuthError::InvalidCredential | AuthError::InvalidScope) => unauthorized("invalid_credential"),
        Err(e) => internal(e),
    }
}

const GRANT_TOKEN_EXCHANGE: &str = "urn:ietf:params:oauth:grant-type:token-exchange";
const TOKEN_TYPE_BOOTSTRAP: &str = "urn:t3:params:oauth:token-type:environment-bootstrap";
const TOKEN_TYPE_ACCESS: &str = "urn:ietf:params:oauth:token-type:access_token";

/// t3code `AuthTokenExchangeRequest` (form-encoded).
#[derive(Deserialize)]
struct TokenRequest {
    grant_type: String,
    subject_token: String,
    subject_token_type: String,
    requested_token_type: String,
    scope: Option<String>,
    client_label: Option<String>,
    client_device_type: Option<String>,
    client_os: Option<String>,
}

/// RFC 8693 token exchange: a pairing code for a bearer access token. Over
/// iroh the token is bound to the peer's key.
async fn token_exchange(State(state): State<SharedState>, peer: Peer, Form(body): Form<TokenRequest>) -> Response {
    if body.grant_type != GRANT_TOKEN_EXCHANGE || body.subject_token_type != TOKEN_TYPE_BOOTSTRAP || body.requested_token_type != TOKEN_TYPE_ACCESS {
        return invalid_request("invalid_command");
    }
    let scope = match body.scope.as_deref().map(str::trim) {
        Some("") => return invalid_request("invalid_scope"),
        Some(s) => Some(s.split_whitespace().map(str::to_owned).collect::<Vec<_>>()),
        None => None,
    };
    let trimmed = |v: Option<String>| v.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty());
    let new = NewSession {
        method: METHOD_BEARER,
        client: ClientMeta { label: trimmed(body.client_label), device_type: trimmed(body.client_device_type), os: trimmed(body.client_os) },
        peer: peer_id(&peer),
        scope,
    };
    match state.auth.create_session(&body.subject_token, new) {
        Ok((token, session)) => {
            state.pulse.notify_one();
            let expires_in = crate::util::seconds_until(&session.expires_at);
            let mut res = Json(AuthAccessTokenResult {
                access_token: token,
                issued_token_type: TOKEN_TYPE_ACCESS.to_owned(),
                token_type: AuthTokenType::Bearer,
                expires_in: expires_in.into(),
                scope: session.scopes.join(" "),
            })
            .into_response();
            res.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
            res
        }
        Err(AuthError::InvalidCredential) => unauthorized("invalid_credential"),
        Err(AuthError::InvalidScope) => invalid_request("scope_not_granted"),
        Err(e) => internal(e),
    }
}

/// A one-time ticket for the RPC WebSocket (`/ws?wsTicket=…`), for clients
/// that authenticate with a bearer token.
async fn ws_ticket(State(state): State<SharedState>, headers: HeaderMap, peer: Peer) -> Response {
    let session = match require_session(&state, &headers, &peer) {
        Ok(s) => s,
        Err(r) => return r,
    };
    let (ticket, expires_at) = state.auth.mint_ws_ticket(&session);
    let mut res = Json(AuthWebSocketTicketResult { ticket, expires_at: expires_at.into() }).into_response();
    res.headers_mut().insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res
}

#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RevokeRequest {
    pub session_id: String,
}

/// The body of `POST /api/auth/clients/revoke` (`AuthRevokeClientSessionResult`): whether a session was signed out.
#[derive(Serialize)]
struct Revoked {
    revoked: bool,
}

/// The body of `POST /api/auth/clients/revoke-others` (`AuthRevokeOtherClientSessionsResult`).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RevokedOthers {
    revoked_count: usize,
}

/// The body of `POST /api/domovoi/session/end` (Domovoi only).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SignedOut {
    signed_out: bool,
}

/// The body of `POST /api/domovoi/server/stop` (Domovoi only).
#[derive(Serialize)]
struct Stopping {
    stopping: bool,
}

/// The body of the push and Live Activity registration routes (Domovoi only):
/// whether the device is registered now, and after registering whether this
/// server can send pushes at all.
#[derive(Serialize)]
struct PushRegistration {
    registered: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    configured: Option<bool>,
}

/// The body of `GET /api/orchestration/shell` for a client that is up to date (`{ resumable: true }`).
#[derive(Serialize)]
struct Resumable {
    resumable: bool,
}

async fn revoke_client(State(state): State<SharedState>, headers: HeaderMap, peer: Peer, Json(body): Json<RevokeRequest>) -> Response {
    let session = match require_permission(&state, &headers, &peer, "access:write") {
        Ok(s) => s,
        Err(r) => return r,
    };
    if crate::auth::session_id(&session.token_hash) == body.session_id {
        return forbidden("current_session_revoke_not_allowed");
    }
    // A remote device may sign out other remote devices, not the machine's own sessions.
    match control::revoke(&state, &body.session_id, peer.is_some()) {
        Ok(control::Revoked::Done) => Json(Revoked { revoked: true }).into_response(),
        Ok(control::Revoked::NotFound) => Json(Revoked { revoked: false }).into_response(),
        Ok(control::Revoked::Forbidden) => forbidden("local_session_revoke_not_allowed"),
        Err(e) => internal(e),
    }
}

async fn revoke_other_clients(State(state): State<SharedState>, headers: HeaderMap, peer: Peer) -> Response {
    let session = match require_permission(&state, &headers, &peer, "access:write") {
        Ok(s) => s,
        Err(r) => return r,
    };
    match control::revoke_others(&state, &session.token_hash, peer.is_some()) {
        Ok(revoked_count) => Json(RevokedOthers { revoked_count }).into_response(),
        Err(e) => internal(e),
    }
}

/// Domovoi only: remote access status.
async fn remote_status(State(state): State<SharedState>, headers: HeaderMap, peer: Peer) -> Response {
    if let Err(r) = require_permission(&state, &headers, &peer, "access:read") {
        return r;
    }
    Json(control::remote_status(&state, peer.is_none())).into_response()
}

#[derive(Deserialize)]
struct PushDevice {
    token: String,
    /// `sandbox` (debug builds) or `production`.
    #[serde(default)]
    environment: Option<String>,
}

/// Domovoi only: an iPhone registers its APNs token for this session.
async fn push_register(State(state): State<SharedState>, headers: HeaderMap, peer: Peer, Json(body): Json<PushDevice>) -> Response {
    let session = match require_permission(&state, &headers, &peer, "orchestration:read") {
        Ok(s) => s,
        Err(r) => return r,
    };
    let environment = body.environment.as_deref().unwrap_or("production");
    if !crate::push::valid_token(&body.token) || !matches!(environment, "sandbox" | "production") {
        return invalid_request("invalid_command");
    }
    match crate::push::register(&state.store.db(), &body.token.to_ascii_lowercase(), environment, &session.token_hash) {
        Ok(()) => {
            state.pulse.notify_one();
            Json(PushRegistration { registered: true, configured: Some(state.push_configured) }).into_response()
        }
        Err(e) => internal(e),
    }
}

async fn push_unregister(State(state): State<SharedState>, headers: HeaderMap, peer: Peer, Json(body): Json<PushDevice>) -> Response {
    let session = match require_session(&state, &headers, &peer) {
        Ok(s) => s,
        Err(r) => return r,
    };
    // A session removes its own registration, nobody else's.
    match crate::push::unregister(&state.store.db(), &body.token.to_ascii_lowercase(), &session.token_hash) {
        Ok(()) => Json(PushRegistration { registered: false, configured: None }).into_response(),
        Err(e) => internal(e),
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ActivityRegistration {
    token: String,
    #[serde(default)]
    thread_id: String,
    #[serde(default)]
    environment: Option<String>,
}

/// Domovoi only: a Live Activity the app started for a thread; status changes
/// are pushed to it until the thread's turn ends.
async fn activity_register(State(state): State<SharedState>, headers: HeaderMap, peer: Peer, Json(body): Json<ActivityRegistration>) -> Response {
    if let Err(r) = require_permission(&state, &headers, &peer, "orchestration:read") {
        return r;
    }
    let environment = body.environment.as_deref().unwrap_or("production");
    if !crate::push::valid_token(&body.token) || body.thread_id.is_empty() || !matches!(environment, "sandbox" | "production") {
        return invalid_request("invalid_command");
    }
    crate::push::register_activity(&body.thread_id, &body.token.to_ascii_lowercase(), environment == "sandbox");
    Json(PushRegistration { registered: true, configured: Some(state.push_configured) }).into_response()
}

async fn activity_unregister(State(state): State<SharedState>, headers: HeaderMap, peer: Peer, Json(body): Json<ActivityRegistration>) -> Response {
    if let Err(r) = require_session(&state, &headers, &peer) {
        return r;
    }
    crate::push::unregister_activity(&body.token.to_ascii_lowercase());
    Json(PushRegistration { registered: false, configured: None }).into_response()
}

#[derive(Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct PairingRequest {
    /// Minutes the code stays valid (default 10).
    ttl_minutes: Option<u64>,
    /// Grant only these permissions; they must be among the caller's own.
    permissions: Option<Vec<String>>,
}

/// Domovoi only: mints a pairing code and returns the `domovoi://connect` link for it.
async fn remote_pairing(State(state): State<SharedState>, headers: HeaderMap, peer: Peer, body: Option<Json<PairingRequest>>) -> Response {
    let session = match require_permission(&state, &headers, &peer, "access:write") {
        Ok(s) => s,
        Err(r) => return r,
    };
    // No link for another device where the managed policy allows none (`crate::policy`).
    let policy = crate::policy::current();
    if !policy.allows_remote_clients() {
        let message = policy.refuse_remote_clients(Some("so no link for another device is made".into())).to_string();
        return (StatusCode::FORBIDDEN, Json(RequestRejection::RemoteAccess(RemoteAccessRejection { message }))).into_response();
    }
    let body = body.map(|b| b.0).unwrap_or_default();
    let ttl = std::time::Duration::from_secs(body.ttl_minutes.unwrap_or(10).clamp(1, 24 * 60) * 60);
    // A new device gets at most what the session minting the code holds.
    let grant = match body.permissions {
        Some(wanted) if wanted.is_empty() || wanted.iter().any(|p| !session.permissions.contains(p)) => return invalid_request("scope_not_granted"),
        Some(wanted) => crate::auth::Grant::limited(wanted),
        None => crate::auth::Grant { scopes: session.scopes.clone(), permissions: session.permissions.clone() },
    };
    match control::pairing_link(&state, ttl, &grant).await {
        Ok(link) => Json(link).into_response(),
        Err(e) => internal(e),
    }
}

/// What `POST /api/domovoi/remote/configure` is asked: remote access on or off,
/// and optionally the relays to use.
#[derive(Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigureRequest {
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relays: Option<Vec<String>>,
}

/// Domovoi only: turns remote access on or off (saved in `<home>/remote.json`).
async fn remote_configure(State(state): State<SharedState>, headers: HeaderMap, peer: Peer, Json(body): Json<ConfigureRequest>) -> Response {
    if let Err(r) = require_permission(&state, &headers, &peer, "access:write") {
        return r;
    }
    if peer.is_some() {
        // A remote device cannot turn remote access off, change its relays or
        // restart it: each would cut off that device and every other one.
        // Asking for what is already so is answered with the status.
        return if body.enabled && body.relays.is_none() { Json(control::remote_status(&state, false)).into_response() } else { forbidden("remote_access_is_changed_locally") };
    }
    match control::set_remote(&state, body.enabled, body.relays).await {
        Ok(()) => Json(control::remote_status(&state, true)).into_response(),
        Err(e) => {
            // What the caller typed is echoed back; why the endpoint failed stays in the log.
            let message = match &e {
                crate::remote::RemoteError::InvalidRelay(m) | crate::remote::RemoteError::Refused(m) => m.clone(),
                _ => "Remote access could not start. See the server log.".to_owned(),
            };
            (StatusCode::BAD_REQUEST, Json(RequestRejection::RemoteAccess(RemoteAccessRejection { message }))).into_response()
        }
    }
}

/// Domovoi only: the caller signs itself out (a device forgetting this server):
/// its session, sockets and push registrations end.
async fn end_session(State(state): State<SharedState>, headers: HeaderMap, peer: Peer) -> Response {
    let session = match require_session(&state, &headers, &peer) {
        Ok(s) => s,
        Err(r) => return r,
    };
    match state.auth.revoke(&crate::auth::session_id(&session.token_hash), false) {
        Ok(_) => {
            control::forget(&state, std::slice::from_ref(&session.token_hash));
            Json(SignedOut { signed_out: true }).into_response()
        }
        Err(e) => internal(e),
    }
}

/// Domovoi only: stops the server (`domovoi-server stop`); never from a remote device.
/// The agent MCP server (`crate::mcp`): JSON-RPC over POST, answered as
/// JSON; local connections with a session's bearer token only.
async fn mcp_post(State(state): State<SharedState>, headers: HeaderMap, peer: Peer, body: axum::body::Bytes) -> Response {
    if peer.is_some() {
        return StatusCode::FORBIDDEN.into_response();
    }
    let token = headers.get("authorization").and_then(|v| v.to_str().ok()).and_then(|v| v.strip_prefix("Bearer ")).unwrap_or_default();
    if !crate::mcp::known_token(token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Ok(msg) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return (StatusCode::BAD_REQUEST, Json(json!({ "jsonrpc": "2.0", "id": null, "error": { "code": -32700, "message": "Parse error" } }))).into_response();
    };
    match msg {
        serde_json::Value::Array(batch) => {
            let mut out = vec![];
            for m in &batch {
                if let Some(r) = crate::mcp::handle(&state, token, m).await {
                    out.push(r);
                }
            }
            if out.is_empty() { StatusCode::ACCEPTED.into_response() } else { Json(serde_json::Value::Array(out)).into_response() }
        }
        m => match crate::mcp::handle(&state, token, &m).await {
            Some(r) => Json(r).into_response(),
            None => StatusCode::ACCEPTED.into_response(),
        },
    }
}

async fn server_stop(State(state): State<SharedState>, headers: HeaderMap, peer: Peer) -> Response {
    if let Err(r) = require_permission(&state, &headers, &peer, "access:write") {
        return r;
    }
    if peer.is_some() {
        return forbidden("server_is_stopped_locally");
    }
    tracing::info!("Stop requested");
    state.shutdown.cancel();
    Json(Stopping { stopping: true }).into_response()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ShellQuery {
    resume_from: Option<u64>,
}

async fn shell_snapshot(State(state): State<SharedState>, headers: HeaderMap, peer: Peer, Query(q): Query<ShellQuery>) -> Response {
    if let Err(r) = require_session(&state, &headers, &peer) {
        return r;
    }
    // A read that fails is not "resumable": the client gets a snapshot.
    if q.resume_from.is_some_and(|seq| state.store.latest_sequence().is_ok_and(|latest| seq <= latest)) {
        return Json(Resumable { resumable: true }).into_response();
    }
    match rpc::shell_snapshot(state.clone()).await {
        Ok(snap) => Json(snap).into_response(),
        Err(e) => internal(e),
    }
}

/// How often sockets are pinged (`DOMOVOI_WS_HEARTBEAT_MS` shortens it in tests).
fn heartbeat_interval() -> std::time::Duration {
    let ms = std::env::var("DOMOVOI_WS_HEARTBEAT_MS").ok().and_then(|v| v.parse::<u64>().ok()).unwrap_or(15_000);
    std::time::Duration::from_millis(ms.max(50))
}

async fn ws(
    State(state): State<SharedState>,
    headers: HeaderMap,
    peer: Peer,
    Query(q): Query<HashMap<String, String>>,
    upgrade: Result<WebSocketUpgrade, axum::extract::ws::rejection::WebSocketUpgradeRejection>,
) -> Response {
    // t3code checks the protocol first, before the upgrade and any credential.
    if !compatible_protocol(&q) {
        return protocol_incompatible();
    }
    let upgrade = match upgrade {
        Ok(upgrade) => upgrade,
        Err(rejection) => return rejection.into_response(),
    };
    let session = match q.get("wsTicket") {
        // A ticket was minted over authenticated HTTP; no cookie is involved,
        // so there is no cross-site risk to guard against.
        Some(ticket) => match state.auth.redeem_ws_ticket(ticket, peer_id(&peer).as_deref()) {
            Ok(Some(s)) => s,
            Ok(None) => return unauthorized("invalid_credential"),
            Err(e) => return internal(e),
        },
        None => {
            let session = match require_session(&state, &headers, &peer) {
                Ok(s) => s,
                Err(r) => return r,
            };
            if !origin_matches_host(&headers) {
                return unauthorized("origin_mismatch");
            }
            session
        }
    };
    let options = ConnectionOptions {
        frame_parts: q.contains_key(effect_rpc::frame_parts::SOCKET_FRAME_PARTS_QUERY_PARAM),
        // A client that vanished (no Close frame) is closed within about 40 s,
        // so it stops counting as an open app (pushes go to it again).
        heartbeat: Some(heartbeat_interval()),
        ..Default::default()
    };
    let live = state.connections.open(&session.token_hash);
    // A revoke between the check above and `open` found no socket to close.
    match state.auth.session_for_hash(&session.token_hash) {
        Ok(Some(_)) => {}
        Ok(None) => return unauthorized("invalid_credential"),
        Err(e) => return internal(e),
    }
    let ctx = Arc::new(ConnCtx { state, session });
    upgrade.on_upgrade(move |socket| async move {
        // Revoking the session (or stopping the server) cancels `live`: the
        // client gets a Close frame. Handlers that do not stop within a few
        // seconds are cut off with the socket.
        let cancel = live.cancel.clone();
        tokio::select! {
            () = serve_websocket_until(socket, rpc_router(), ctx, options, live.cancel.clone().cancelled_owned()) => {}
            () = async move {
                cancel.cancelled().await;
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            } => {}
        }
        drop(live);
    })
}

/// t3code `ORCHESTRATION_PROTOCOL_QUERY_PARAM`.
const ORCHESTRATION_PROTOCOL_QUERY_PARAM: &str = "orchestrationProtocol";

/// t3code `hasCompatibleOrchestrationProtocol`: the socket URL names this
/// server's orchestration protocol.
fn compatible_protocol(query: &HashMap<String, String>) -> bool {
    query.get(ORCHESTRATION_PROTOCOL_QUERY_PARAM).map(String::as_str) == Some(ORCHESTRATION_PROTOCOL_VERSION.to_string().as_str())
}

/// What t3code's `/ws` answers a client of another protocol (HTTP 426).
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ProtocolIncompatible {
    code: &'static str,
    message: String,
    orchestration_protocol_version: u32,
}

fn protocol_incompatible() -> Response {
    let body = ProtocolIncompatible {
        code: "orchestration_protocol_incompatible",
        message: format!("Update this client to one that supports orchestration protocol {ORCHESTRATION_PROTOCOL_VERSION}."),
        orchestration_protocol_version: ORCHESTRATION_PROTOCOL_VERSION,
    };
    (StatusCode::UPGRADE_REQUIRED, Json(body)).into_response()
}
