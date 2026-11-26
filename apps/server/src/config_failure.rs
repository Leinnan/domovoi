//! Why a call about the server's configuration failed, as the client is
//! told: settings, keybindings, providers and their setup, the access stream,
//! and the permission every call is checked for. Each is a tagged error of
//! t3code's, with the fields that error carries.

use serde::Serialize;

/// A failed call of the configuration group, written as its tagged error.
#[derive(Debug, Clone, PartialEq, thiserror::Error, Serialize)]
#[serde(tag = "_tag")]
pub enum Failure {
    /// The settings file could not be written (`ServerSettingsError`).
    #[error("{operation} of {settings_path}: {cause}")]
    #[serde(rename = "ServerSettingsError", rename_all = "camelCase")]
    Settings { settings_path: String, operation: SettingsOperation, cause: String },

    /// The managed policy refuses a settings write (`ServerSettingsError` with
    /// `write-managed-setting` or `use-disallowed-provider`).
    #[error("{}", managed_message(.operation, .managed_setting.as_deref(), .managed_by.as_deref(), .provider_label.as_deref().or(.provider_instance_id.as_deref())))]
    #[serde(rename = "ServerSettingsError", rename_all = "camelCase")]
    Managed {
        settings_path: String,
        operation: SettingsOperation,
        #[serde(skip_serializing_if = "Option::is_none")]
        managed_setting: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        managed_by: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        provider_instance_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        provider_label: Option<String>,
    },

    /// The keybindings file could not be read or written (`KeybindingsConfigError`).
    #[error("{config_path}: {detail}")]
    #[serde(rename = "KeybindingsConfigParseError", rename_all = "camelCase")]
    Keybindings { config_path: String, detail: String },

    /// `server.updateProvider` (`ServerProviderUpdateError`). `provider` is the driver the caller named.
    #[error("{reason}")]
    #[serde(rename = "ServerProviderUpdateError")]
    ProviderUpdate { provider: Option<String>, reason: String },

    /// A `provider.*` setup call (`ProviderSetupError`).
    #[error("{detail}")]
    #[serde(rename = "ProviderSetupError", rename_all = "camelCase")]
    ProviderSetup { instance_id: String, operation: &'static str, detail: String },

    /// `subscribeAuthAccess` (`AuthAccessStreamError`).
    #[error("{message}")]
    #[serde(rename = "AuthAccessStreamError")]
    AccessStream { message: String },

    /// The session may not call the method (`EnvironmentAuthorizationError`).
    /// `required_permission` is absent for a method that declares none.
    #[error("{message}")]
    #[serde(rename = "EnvironmentAuthorizationError", rename_all = "camelCase")]
    Authorization {
        message: String,
        required_scope: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        required_permission: Option<String>,
    },
}

/// What was being done to the settings file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum SettingsOperation {
    WriteFile,
    /// A write that moves a setting the managed policy locks.
    WriteManagedSetting,
    /// A write that turns on a provider the managed policy does not allow.
    UseDisallowedProvider,
}

impl std::fmt::Display for SettingsOperation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            SettingsOperation::WriteFile => "write-file",
            SettingsOperation::WriteManagedSetting => "write-managed-setting",
            SettingsOperation::UseDisallowedProvider => "use-disallowed-provider",
        })
    }
}

/// `ServerSettingsError.message` for the managed policy's refusals.
fn managed_message(operation: &SettingsOperation, setting: Option<&str>, by: Option<&str>, provider: Option<&str>) -> String {
    match operation {
        SettingsOperation::UseDisallowedProvider => crate::policy::disallowed_provider_message(by, provider.unwrap_or("this")),
        _ => format!("{} is managed by {} and cannot be changed.", setting.unwrap_or("This setting"), by.unwrap_or("your organization")),
    }
}

impl Failure {
    /// The managed policy's refusal of a write to the settings at `settings_path`.
    pub fn managed(settings_path: &std::path::Path, refusal: crate::policy::WriteRefusal) -> Self {
        let managed_by = crate::policy::current().organization().map(str::to_owned);
        let settings_path = settings_path.to_string_lossy().into_owned();
        match refusal {
            crate::policy::WriteRefusal::Locked(path) => Failure::Managed {
                settings_path,
                operation: SettingsOperation::WriteManagedSetting,
                managed_setting: Some(path),
                managed_by,
                provider_instance_id: None,
                provider_label: None,
            },
            crate::policy::WriteRefusal::Disallowed { instance_id, label } => Failure::Managed {
                settings_path,
                operation: SettingsOperation::UseDisallowedProvider,
                managed_setting: None,
                managed_by,
                provider_instance_id: Some(instance_id),
                provider_label: Some(label),
            },
        }
    }
}

impl Failure {
    /// A setup call on `instance_id` that was not carried out.
    pub fn setup(instance_id: &str, operation: &'static str, detail: &str) -> Self {
        Failure::ProviderSetup { instance_id: instance_id.to_owned(), operation, detail: detail.to_owned() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn failures_are_written_as_the_tagged_errors_of_t3code() {
        assert_eq!(
            json!(Failure::Settings { settings_path: "/u/settings.json".into(), operation: SettingsOperation::WriteFile, cause: "Is a directory (os error 21)".into() }),
            json!({ "_tag": "ServerSettingsError", "settingsPath": "/u/settings.json", "operation": "write-file", "cause": "Is a directory (os error 21)" })
        );
        assert_eq!(
            json!(Failure::Managed {
                settings_path: "/u/settings.json".into(),
                operation: SettingsOperation::WriteManagedSetting,
                managed_setting: Some("enableAgentBrowserAccess".into()),
                managed_by: Some("Acme".into()),
                provider_instance_id: None,
                provider_label: None,
            }),
            json!({ "_tag": "ServerSettingsError", "settingsPath": "/u/settings.json", "operation": "write-managed-setting", "managedSetting": "enableAgentBrowserAccess", "managedBy": "Acme" })
        );
        assert_eq!(
            json!(Failure::Keybindings { config_path: "/u/keybindings.json".into(), detail: "keybindings.json must contain an array of rules.".into() }),
            json!({ "_tag": "KeybindingsConfigParseError", "configPath": "/u/keybindings.json", "detail": "keybindings.json must contain an array of rules." })
        );
        assert_eq!(
            json!(Failure::ProviderUpdate { provider: Some("codex".into()), reason: "no".into() }),
            json!({ "_tag": "ServerProviderUpdateError", "provider": "codex", "reason": "no" })
        );
        assert_eq!(json!(Failure::ProviderUpdate { provider: None, reason: "no".into() }), json!({ "_tag": "ServerProviderUpdateError", "provider": null, "reason": "no" }));
        assert_eq!(
            json!(Failure::setup("codex", "remove-install", "There is no managed Codex installation to remove.")),
            json!({ "_tag": "ProviderSetupError", "instanceId": "codex", "operation": "remove-install", "detail": "There is no managed Codex installation to remove." })
        );
        assert_eq!(json!(Failure::AccessStream { message: "no disk".into() }), json!({ "_tag": "AuthAccessStreamError", "message": "no disk" }));
        assert_eq!(
            json!(Failure::Authorization { message: "m".into(), required_scope: "orchestration:read".into(), required_permission: Some("filesystem:read".into()) }),
            json!({ "_tag": "EnvironmentAuthorizationError", "message": "m", "requiredScope": "orchestration:read", "requiredPermission": "filesystem:read" })
        );
        assert_eq!(
            json!(Failure::Authorization { message: "m".into(), required_scope: "orchestration:operate".into(), required_permission: None }),
            json!({ "_tag": "EnvironmentAuthorizationError", "message": "m", "requiredScope": "orchestration:operate" })
        );
    }
}
