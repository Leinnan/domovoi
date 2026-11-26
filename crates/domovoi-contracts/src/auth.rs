use serde::{Deserialize, Serialize};

use domovoi_model::typed::ServerAuthDescriptor;

/// Scopes granted to a session by default (the default pairing grant).
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
    /// Always empty: there are no cookies. An app from before protocol 4
    /// does not read the descriptor without the key, and it must read it to
    /// get as far as being told to update.
    pub session_cookie_name: String,
    pub server_update_scope: String,
}

impl AuthDescriptor {
    /// A pairing code is exchanged for a bearer token.
    pub fn pairing_code() -> Self {
        AuthDescriptor {
            policy: "loopback-browser".into(),
            bootstrap_methods: vec!["one-time-token".into()],
            session_methods: vec!["bearer-access-token".into()],
            session_cookie_name: String::new(),
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
        let auth = AuthDescriptor::pairing_code();
        assert_eq!(serde_json::json!(ServerAuthDescriptor::from(&auth)), serde_json::json!(auth));
    }
}
