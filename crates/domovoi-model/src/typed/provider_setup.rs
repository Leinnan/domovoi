//! A provider's sign-in and managed installation as the server reports them
//! (`providerSetup.ts`: `ProviderAuthState`, `ProviderInstallState`), from
//! `provider.auth.subscribe` and `provider.install.subscribe`.

use serde::{Deserialize, Serialize};

/// Where a provider's sign-in stands (`ProviderAuthState`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderAuthState {
    pub instance_id: String,
    /// `idle` | `starting` | `waiting` | `verifying` | `succeeded` | `failed` | `cancelled`.
    pub phase: String,
    #[serde(default)]
    pub flow_id: Option<String>,
    /// The page to sign in on, while it waits for it.
    #[serde(default)]
    pub authorization_url: Option<String>,
    #[serde(default)]
    pub expires_at: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    /// What the user is to do (`subscribe` only): a browser page, and whether
    /// its redirect can be pasted back.
    #[serde(default)]
    pub interaction: Option<ProviderAuthInteraction>,
    #[serde(default)]
    pub credential_owner: Option<String>,
}

impl ProviderAuthState {
    /// The sign-in is under way.
    pub fn busy(&self) -> bool {
        matches!(self.phase.as_str(), "starting" | "waiting" | "verifying")
    }
}

/// A step of a sign-in the user takes (`ProviderAuthInteraction`); only the
/// browser kind's fields are read.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderAuthInteraction {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub accepts_callback: Option<bool>,
}

/// Where a provider's managed installation stands (`ProviderInstallState`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderInstallState {
    pub driver: String,
    #[serde(default)]
    pub operation_id: Option<String>,
    /// `idle` | `downloading` | `extracting` | `verifying` | `succeeded` | `failed` | `cancelled`.
    pub phase: String,
    #[serde(default)]
    pub downloaded_bytes: u64,
    #[serde(default)]
    pub total_bytes: Option<u64>,
    /// The release offered (or being installed).
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub installed_version: Option<String>,
    #[serde(default)]
    pub executable_path: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
    #[serde(default)]
    pub can_remove: bool,
    #[serde(default)]
    pub message: Option<String>,
}

impl ProviderInstallState {
    /// An installation is under way.
    pub fn busy(&self) -> bool {
        matches!(self.phase.as_str(), "downloading" | "extracting" | "verifying")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_servers_states_are_read() {
        let auth: ProviderAuthState = serde_json::from_str(r#"{"instanceId":"antigravity","phase":"waiting","flowId":"f","authorizationUrl":"https://accounts.google.com/x","expiresAt":"t","message":"m","interaction":{"type":"browser","id":"f","url":"https://accounts.google.com/x","requiresConsent":false,"acceptsCallback":true},"credentialOwner":"provider"}"#).unwrap();
        assert!(auth.busy() && auth.interaction.as_ref().and_then(|i| i.accepts_callback) == Some(true));
        let idle: ProviderAuthState = serde_json::from_str(r#"{"instanceId":"a","phase":"idle","flowId":null,"authorizationUrl":null,"expiresAt":null,"message":null}"#).unwrap();
        assert!(!idle.busy());
        let install: ProviderInstallState = serde_json::from_str(r#"{"driver":"antigravity","operationId":"o","phase":"downloading","downloadedBytes":10,"totalBytes":100,"version":"1.5.14","installedVersion":null,"executablePath":null,"source":null,"canRemove":true,"message":"Downloading"}"#).unwrap();
        assert!(install.busy() && install.total_bytes == Some(100));
    }
}
