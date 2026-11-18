use serde::{Deserialize, Serialize};

use domovoi_model::typed::ServerAuthDescriptor;

/// Scopes granted to a one-time-token browser session (the default pairing grant).
pub const DEFAULT_SCOPES: &[&str] = &[
    "orchestration:read", "orchestration:operate", "terminal:operate", "relay:read",
    "access:read", "access:write", "relay:write",
];

/// Exact permissions behind [`DEFAULT_SCOPES`].
pub const DEFAULT_PERMISSIONS: &[&str] = &[
    "orchestration:read", "orchestration:operate", "settings:write", "providers:manage",
    "environment:maintain", "preview:operate", "diagnostics:read", "terminal:read",
    "terminal:operate", "source-control:write", "filesystem:read", "filesystem:write",
    "relay:read", "access:read", "access:write", "relay:write",
];

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthDescriptor {
    pub policy: String,
    pub bootstrap_methods: Vec<String>,
    pub session_methods: Vec<String>,
    pub session_cookie_name: String,
    pub server_update_scope: String,
}

impl AuthDescriptor {
    pub fn loopback_browser(cookie_name: &str) -> Self {
        AuthDescriptor {
            policy: "loopback-browser".into(),
            bootstrap_methods: vec!["one-time-token".into()],
            session_methods: vec!["browser-session-cookie".into()],
            session_cookie_name: cookie_name.into(),
            server_update_scope: "environment:maintain".into(),
        }
    }
}

impl From<&AuthDescriptor> for ServerAuthDescriptor {
    /// The descriptor as the record clients read, which the server's config
    /// carries: the same JSON.
    fn from(auth: &AuthDescriptor) -> Self {
        ServerAuthDescriptor {
            policy: auth.policy.as_str().into(),
            bootstrap_methods: auth.bootstrap_methods.iter().map(|method| method.as_str().into()).collect(),
            session_methods: auth.session_methods.iter().map(|method| method.as_str().into()).collect(),
            session_cookie_name: auth.session_cookie_name.clone(),
            server_update_scope: Some(auth.server_update_scope.as_str().into()),
        }
    }
}

/// `GET /api/auth/session` response.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AuthSessionState {
    pub authenticated: bool,
    pub auth: AuthDescriptor,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scopes: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub permissions: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub session_method: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<String>,
}

/// `POST /api/auth/browser-session` request.
#[derive(Debug, Clone, Deserialize)]
pub struct BrowserSessionRequest {
    pub credential: String,
}

/// `POST /api/auth/browser-session` response.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BrowserSessionResponse {
    pub authenticated: bool,
    pub scopes: Vec<String>,
    pub permissions: Vec<String>,
    pub session_method: String,
    pub expires_at: String,
}

/// `EnvironmentAuthInvalidError` body (HTTP 401).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthInvalidError {
    #[serde(rename = "_tag")]
    pub tag: String,
    pub code: String,
    pub reason: String,
    #[serde(rename = "traceId")]
    pub trace_id: String,
}

impl AuthInvalidError {
    pub fn new(reason: &str, trace_id: String) -> Self {
        AuthInvalidError {
            tag: "EnvironmentAuthInvalidError".into(),
            code: "auth_invalid".into(),
            reason: reason.into(),
            trace_id,
        }
    }
}

pub fn strings(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_descriptor_and_the_record_clients_read_are_the_same_json() {
        let mut auth = AuthDescriptor::loopback_browser("t3_session_1");
        auth.session_methods.push("bearer-access-token".into());
        assert_eq!(serde_json::json!(ServerAuthDescriptor::from(&auth)), serde_json::json!(auth));
    }
}
