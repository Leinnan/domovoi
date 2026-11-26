//! Reaching a server from another device: the Domovoi server's own remote access
//! routes (`/api/domovoi/remote…`, which t3code does not have), and the bodies of
//! the routes a client signs in through (`auth.ts`, `environmentHttp.ts`).

use super::common::Timestamp;
use super::list::ForwardList;
use super::macros::{string_enum, tagged_union_deserialize};
use super::pages::{AuthClientDeviceType, AuthEnvironmentScope, ServerAuthSessionMethod, unknown_record};
use serde::{Deserialize, Serialize};
use serde_json::{Number, Value};

// ---- the Domovoi server's remote access ----

/// The body of `GET /api/domovoi/remote` and of `POST /api/domovoi/remote/configure`:
/// whether devices can reach the server from outside, and which are paired.
///
/// Domovoi only: built by `control::status_json` in the Domovoi server, over `Remote::status`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAccessStatus {
    /// Whether remote access is turned on.
    pub enabled: bool,
    /// Whether its endpoint is up.
    pub running: bool,
    pub state: RemoteAccessState,
    /// Why the endpoint failed to start; in full only for a caller on the server's own machine.
    pub error: Option<String>,
    /// The server's public key, which pairing links name it by.
    pub endpoint_id: String,
    /// The relays set for the server: none for the public ones, `disabled` for the local network only.
    pub relays: Vec<String>,
    /// The relay the endpoint is registered with, once it is.
    pub home_relay: Option<String>,
    pub direct_addresses: Vec<String>,
    pub push: RemotePushStatus,
    /// The signed-in sessions, oldest first.
    pub devices: Vec<RemoteDevice>,
}

string_enum! {
    /// Where a Domovoi server's remote access endpoint is in its life.
    RemoteAccessState {
        Off = "off",
        Starting = "starting",
        /// Up, reachable on the local network only.
        LanOnly = "lanOnly",
        Online = "online",
        Error = "error",
    }
}

/// Whether a Domovoi server can send push notifications, and to how many devices.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemotePushStatus {
    /// Whether the server has the credentials to send pushes with.
    pub configured: bool,
    pub devices: u64,
}

/// A session signed in to a Domovoi server, as its remote access status lists it
/// (`control::Device` in the Domovoi server).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteDevice {
    /// The session's public id, which revoking takes.
    pub id: String,
    pub label: String,
    pub kind: AuthClientDeviceType,
    /// Paired through the remote access endpoint, and bound to the device's key, rather than over plain TCP.
    pub remote: bool,
    pub connected: bool,
    pub last_seen: Option<Timestamp>,
}

/// The body of `POST /api/domovoi/remote/pairing`: a one-time code for a new
/// device, as the link that carries it.
///
/// Domovoi only: built by `control::pairing_link` in the Domovoi server.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemotePairingLink {
    /// `domovoi://connect?ticket=…&code=…&name=…`.
    pub link: String,
    pub code: String,
    /// The address of the server's endpoint, which `link` carries.
    pub ticket: String,
    pub endpoint_id: String,
    pub expires_at: Timestamp,
}

// ---- signing in ----

string_enum! {
    /// Who may reach a server, and how they prove it (`ServerAuthPolicy`).
    ServerAuthPolicy {
        DesktopManagedLocal = "desktop-managed-local",
        LoopbackBrowser = "loopback-browser",
        RemoteReachable = "remote-reachable",
        UnsafeNoAuth = "unsafe-no-auth",
    }
}

string_enum! {
    /// What a client can trade for its first session (`ServerAuthBootstrapMethod`).
    ServerAuthBootstrapMethod {
        DesktopBootstrap = "desktop-bootstrap",
        OneTimeToken = "one-time-token",
    }
}

/// How a server authenticates its clients (`ServerAuthDescriptor`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerAuthDescriptor {
    pub policy: ServerAuthPolicy,
    pub bootstrap_methods: Vec<ServerAuthBootstrapMethod>,
    pub session_methods: Vec<ServerAuthSessionMethod>,
    pub session_cookie_name: String,
    /// Always `environment:maintain`. Older servers leave it out and let
    /// `orchestration:operate` update the server.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_update_scope: Option<AuthEnvironmentScope>,
}

/// The body of `GET /api/auth/session` (`AuthSessionState`): whether the
/// caller is signed in, and as what.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthSessionState {
    pub authenticated: bool,
    pub auth: ServerAuthDescriptor,
    /// The scopes clients older than exact permissions understand; only when signed in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scopes: Option<Vec<AuthEnvironmentScope>>,
    /// Exactly what the session may do; only when signed in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permissions: Option<ForwardList<AuthEnvironmentScope>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_method: Option<ServerAuthSessionMethod>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<Timestamp>,
}

/// The body of `POST /api/auth/browser-session` (`AuthBrowserSessionResult`):
/// the session a pairing code bought, whose cookie the response sets.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthBrowserSessionResult {
    /// Always `true`.
    pub authenticated: bool,
    /// The scopes clients older than exact permissions understand.
    pub scopes: Vec<AuthEnvironmentScope>,
    /// Exactly what the session may do; left out by servers older than permissions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permissions: Option<ForwardList<AuthEnvironmentScope>>,
    pub session_method: ServerAuthSessionMethod,
    pub expires_at: Timestamp,
}

/// The body of `POST /oauth/token` (`AuthAccessTokenResult`): the access token
/// a pairing code was exchanged for. Its fields are spelled as OAuth spells them.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AuthAccessTokenResult {
    pub access_token: String,
    /// Always `urn:ietf:params:oauth:token-type:access_token`.
    pub issued_token_type: String,
    pub token_type: AuthTokenType,
    /// Seconds until the token expires.
    pub expires_in: Number,
    /// The granted scopes, separated by spaces.
    pub scope: String,
}

/// What a client on the server's own machine posts to
/// `POST /api/domovoi/local/session` to sign in without a pairing code: the
/// id of the request file it wrote in the server's home (`domovoi_local`),
/// and what it says of itself, as when pairing.
///
/// Domovoi only.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalSessionRequest {
    pub request: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_device_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_os: Option<String>,
}

/// The body of `POST /api/domovoi/local/session`: a session for a client on
/// the server's own machine.
///
/// Domovoi only: built by `local_session` in the Domovoi server's `http.rs`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalSessionResult {
    pub access_token: String,
    /// The secret in the client's request file: only a program that can read
    /// the server's home knows it, so the client drops an answer without it.
    pub proof: String,
    pub environment_id: String,
}

string_enum! {
    /// How an access token is presented: as it is, or with a proof of the key it is bound to.
    AuthTokenType {
        Bearer = "Bearer",
        Dpop = "DPoP",
    }
}

/// The body of `POST /api/auth/websocket-ticket` and of
/// `POST /api/auth/websocket-ticket/{use}` (`AuthWebSocketTicketResult`): a
/// one-time ticket that opens a socket for the session that asked.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthWebSocketTicketResult {
    pub ticket: String,
    pub expires_at: Timestamp,
}

// ---- a rejected request ----

/// The body of a response that rejects an HTTP request, named by its `_tag`:
/// the errors every t3code route may answer with (`EnvironmentHttpCommonError`)
/// and the three only the Domovoi server sends.
///
/// `Other` is an error this build does not know; a route's own errors, such
/// as t3code's `EnvironmentHttpForbiddenError`, arrive as it.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "_tag")]
pub enum RequestRejection {
    #[serde(rename = "EnvironmentRequestInvalidError")]
    RequestInvalid(RequestInvalidRejection),
    #[serde(rename = "EnvironmentAuthInvalidError")]
    AuthInvalid(AuthInvalidRejection),
    #[serde(rename = "EnvironmentScopeRequiredError")]
    ScopeRequired(ScopeRequiredRejection),
    #[serde(rename = "EnvironmentOperationForbiddenError")]
    OperationForbidden(OperationForbiddenRejection),
    #[serde(rename = "EnvironmentResourceNotFoundError")]
    ResourceNotFound(ResourceNotFoundRejection),
    #[serde(rename = "EnvironmentInternalError")]
    Internal(InternalRejection),
    #[serde(rename = "EnvironmentRateLimitedError")]
    RateLimited(RateLimitedRejection),
    #[serde(rename = "RemoteAccessError")]
    RemoteAccess(RemoteAccessRejection),
    #[serde(rename = "HostNotAllowedError")]
    HostNotAllowed(HostNotAllowedRejection),
    #[serde(untagged)]
    Other(UnknownRejection),
}

tagged_union_deserialize! {
    RequestRejection by "_tag" {
        "EnvironmentRequestInvalidError" => RequestInvalid,
        "EnvironmentAuthInvalidError" => AuthInvalid,
        "EnvironmentScopeRequiredError" => ScopeRequired,
        "EnvironmentOperationForbiddenError" => OperationForbidden,
        "EnvironmentResourceNotFoundError" => ResourceNotFound,
        "EnvironmentInternalError" => Internal,
        "EnvironmentRateLimitedError" => RateLimited,
        "RemoteAccessError" => RemoteAccess,
        "HostNotAllowedError" => HostNotAllowed,
        _ => Other,
    }
}

impl RequestRejection {
    /// The sentence the server wrote for the user, when the error carries one.
    pub fn message(&self) -> Option<&str> {
        match self {
            Self::RemoteAccess(rejection) => Some(&rejection.message),
            Self::HostNotAllowed(rejection) => Some(&rejection.message),
            Self::Other(unknown) => unknown.record().get("message").and_then(Value::as_str),
            _ => None,
        }
    }

    /// The code that says why, when the error carries one.
    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::RequestInvalid(rejection) => Some(rejection.reason.as_str()),
            Self::AuthInvalid(rejection) => Some(rejection.reason.as_str()),
            Self::OperationForbidden(rejection) => Some(rejection.reason.as_str()),
            Self::ResourceNotFound(rejection) => Some(rejection.reason.as_str()),
            Self::Internal(rejection) => rejection.reason.as_ref().map(InternalErrorReason::as_str),
            Self::Other(unknown) => unknown.record().get("reason").and_then(Value::as_str),
            Self::ScopeRequired(_) | Self::RateLimited(_) | Self::RemoteAccess(_) | Self::HostNotAllowed(_) => None,
        }
    }
}

/// A request the server could not make sense of, answered with 400 (`EnvironmentRequestInvalidError`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestInvalidRejection {
    /// Always `invalid_request`.
    pub code: String,
    pub reason: RequestInvalidReason,
    pub trace_id: String,
}

string_enum! {
    /// What was wrong with a request (`EnvironmentRequestInvalidReason`).
    RequestInvalidReason {
        InvalidScope = "invalid_scope",
        ScopeNotGranted = "scope_not_granted",
        InvalidCommand = "invalid_command",
        InvalidHistoryCursor = "invalid_history_cursor",
        ThreadTranscriptTooLarge = "thread_transcript_too_large",
        InvalidAudio = "invalid_audio",
        SpeechUnavailable = "speech_unavailable",
        SpeechBusy = "speech_busy",
    }
}

/// A request without a session the server accepts, answered with 401 (`EnvironmentAuthInvalidError`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthInvalidRejection {
    /// Always `auth_invalid`.
    pub code: String,
    pub reason: AuthInvalidReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dpop_failure_reason: Option<DpopFailureReason>,
    pub trace_id: String,
}

string_enum! {
    /// Why the server did not accept a request's credentials (`EnvironmentAuthInvalidReason`).
    AuthInvalidReason {
        MissingCredential = "missing_credential",
        InvalidCredential = "invalid_credential",
    }
}

string_enum! {
    /// Why the proof sent with a key-bound token did not hold (`DpopFailureReason`).
    DpopFailureReason {
        TimeWindow = "time_window",
        KeyMismatch = "key_mismatch",
        RequestMismatch = "request_mismatch",
        TokenMismatch = "token_mismatch",
        Replay = "replay",
        InvalidProof = "invalid_proof",
    }
}

/// A request whose session lacks a permission, answered with 403 (`EnvironmentScopeRequiredError`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScopeRequiredRejection {
    /// Always `insufficient_scope`.
    pub code: String,
    /// The scope clients older than exact permissions know the permission by.
    /// Domovoi writes the permission's area with `:write` here, which is not always a scope.
    pub required_scope: AuthEnvironmentScope,
    /// The permission the session lacks; a name, so one this build does not know still reads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required_permission: Option<String>,
    pub trace_id: String,
}

/// A request the server refuses whoever asks, answered with 403 (`EnvironmentOperationForbiddenError`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OperationForbiddenRejection {
    /// `operation_forbidden` from t3code, `forbidden` from domovoi.
    pub code: String,
    pub reason: OperationForbiddenReason,
    pub trace_id: String,
}

string_enum! {
    /// Why an operation is refused (`EnvironmentOperationForbiddenReason`, and Domovoi's own).
    OperationForbiddenReason {
        CurrentSessionRevokeNotAllowed = "current_session_revoke_not_allowed",
        /// Domovoi only: a remote device may not sign out a session on the server's own machine.
        LocalSessionRevokeNotAllowed = "local_session_revoke_not_allowed",
        /// Domovoi only: a remote device may not turn remote access off or change its relays.
        RemoteAccessIsChangedLocally = "remote_access_is_changed_locally",
        /// Domovoi only: signing in without a code is for a program on the server's own machine.
        LocalSignInIsForThisMachine = "local_sign_in_is_for_this_machine",
        /// Domovoi only: this server offers no sign-in without a code.
        LocalSignInIsOff = "local_sign_in_is_off",
    }
}

/// A request for something the server does not have, answered with 404 (`EnvironmentResourceNotFoundError`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResourceNotFoundRejection {
    /// Always `not_found`.
    pub code: String,
    pub reason: ResourceNotFoundReason,
    pub trace_id: String,
}

string_enum! {
    /// What a request named that the server does not have (`EnvironmentResourceNotFoundReason`).
    ResourceNotFoundReason {
        ThreadNotFound = "thread_not_found",
    }
}

/// A request the server failed to answer, answered with 500 (`EnvironmentInternalError`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InternalRejection {
    /// Always `internal_error`.
    pub code: String,
    /// Required by the schema; Domovoi leaves it out and keeps the cause in its log.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<InternalErrorReason>,
    /// Required by the schema; Domovoi leaves it out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
}

string_enum! {
    /// What the server was doing when it failed (`EnvironmentInternalErrorReason`).
    InternalErrorReason {
        BootstrapValidationFailed = "bootstrap_validation_failed",
        BrowserSessionIssuanceFailed = "browser_session_issuance_failed",
        BrowserSessionCookieFailed = "browser_session_cookie_failed",
        AccessTokenIssuanceFailed = "access_token_issuance_failed",
        WebsocketTicketIssuanceFailed = "websocket_ticket_issuance_failed",
        PairingCredentialIssuanceFailed = "pairing_credential_issuance_failed",
        PairingLinksLoadFailed = "pairing_links_load_failed",
        PairingLinkRevokeFailed = "pairing_link_revoke_failed",
        ClientSessionsLoadFailed = "client_sessions_load_failed",
        ClientSessionRevokeFailed = "client_session_revoke_failed",
        ProjectSnapshotFailed = "project_snapshot_failed",
        ProjectMutationFailed = "project_mutation_failed",
        OrchestrationSnapshotFailed = "orchestration_snapshot_failed",
        OrchestrationThreadSnapshotFailed = "orchestration_thread_snapshot_failed",
        OrchestrationThreadBoundedSnapshotFailed = "orchestration_thread_bounded_snapshot_failed",
        OrchestrationThreadHistoryFailed = "orchestration_thread_history_failed",
        InternalError = "internal_error",
    }
}

/// Too many pairing attempts from a device with no session, answered with 429.
///
/// Domovoi only: built by `rate_limited` in the Domovoi server's `http.rs`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RateLimitedRejection {
    /// Always `rate_limited`.
    pub code: String,
    pub retry_after_seconds: u64,
    pub trace_id: String,
}

/// Remote access that could not be set as asked, answered with 400.
///
/// Domovoi only: built by `remote_configure` in the Domovoi server's `http.rs`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteAccessRejection {
    pub message: String,
}

/// A request over plain TCP that named the server by a host it does not
/// answer to, or came from a web page of another origin, answered with 403.
/// It is the server's answer to a page that reaches it through a DNS name of
/// its own (DNS rebinding), and to a reverse proxy the server was not told of.
///
/// Domovoi only: built by `guard_host` in the Domovoi server's `http.rs`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostNotAllowedRejection {
    /// The host (or origin) the request named.
    pub host: String,
    /// What to do about it, for the user.
    pub message: String,
}

unknown_record! {
    /// A rejection whose `_tag` this build does not know, as it was sent.
    UnknownRejection tagged "_tag"
}
