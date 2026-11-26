//! Per-RPC authorization: every method requires one permission
//! (`domovoi_api::METHODS`), checked against the session before any handler
//! runs. A few methods need another permission for some payloads: those
//! rules are here.
//!
//! A request is checked as it arrived, before any handler decodes it, so the
//! payload rules read the few fields they need from its JSON.

#![expect(clippy::disallowed_types, reason = "any RPC's raw payload, as effect-rpc hands it to the guard before the handler's own type reads it")]

use crate::config_failure::Failure;
use serde_json::Value;

/// Settings keys that only `providers:manage` may change.
const PROVIDER_SETTINGS: [&str; 5] = ["providers", "providerInstances", "usageLimitSources", "textGenerators", "secretProviders"];

/// `requiredScopesForServerSettingsPatch`.
fn settings_patch_permissions(patch: &Value) -> Vec<&'static str> {
    let entries = patch.as_object().into_iter().flatten().filter(|(_, v)| !v.is_null());
    let (mut providers, mut settings) = (false, false);
    for (k, _) in entries {
        if PROVIDER_SETTINGS.contains(&k.as_str()) { providers = true } else { settings = true }
    }
    // Removing a secret provider also clears settings fields that read from it.
    if patch["secretProviders"].as_object().is_some_and(|m| m.values().any(Value::is_null)) {
        settings = true;
    }
    let mut out = vec![];
    if settings || !providers { out.push("settings:write") }
    if providers { out.push("providers:manage") }
    out
}

/// Every permission `method` requires for `payload`.
pub fn required_permissions(method: &str, payload: &Value) -> Option<Vec<&'static str>> {
    match method {
        "server.retryResourceTelemetry" => return Some(vec!["environment:maintain", "diagnostics:read"]),
        "assets.createUrl" => {
            let r = &payload["resource"];
            let fs = matches!(r["_tag"].as_str(), Some("workspace-file" | "media-file" | "draft-workspace-file" | "artifact-bundle"))
                || (r["_tag"] == "native-app-icon" && r["app"]["_tag"] == "app-path");
            return Some(vec![if fs { "filesystem:read" } else { "orchestration:read" }]);
        }
        "server.updateSettings" => {
            let mut p = settings_patch_permissions(&payload["patch"]);
            if payload.get("providerInstanceMutation").is_some_and(|v| !v.is_null()) {
                let empty = payload["patch"].as_object().is_none_or(|m| m.values().all(Value::is_null));
                if empty { p = vec![] }
                if !p.contains(&"providers:manage") { p.push("providers:manage") }
            }
            return Some(p);
        }
        "projects.mutate" => {
            let mut p = vec!["orchestration:operate"];
            if payload["type"] != "project.delete" && payload.get("scripts").is_some_and(|v| !v.is_null()) {
                // As t3code: the scripts are checked as a patch of `defaultProjectScripts`.
                p.extend(settings_patch_permissions(&Value::Object(serde_json::Map::from_iter([("defaultProjectScripts".to_owned(), payload["scripts"].clone())]))));
            }
            return Some(p);
        }
        "git.preparePullRequestThread" if payload["mode"] == "worktree" && !payload["threadId"].is_null() => {
            return Some(vec!["source-control:write", "orchestration:operate"]);
        }
        "git.runStackedAction"
            if !payload["threadId"].is_null() && matches!(payload["action"].as_str(), Some("create_pr" | "commit_push_pr")) =>
        {
            return Some(vec!["source-control:write", "orchestration:read"]);
        }
        _ => {}
    }
    Some(vec![domovoi_api::method(method)?.permission])
}

/// Older clients only know the original scope names (`authScopeRequiredResponse`).
pub(crate) fn legacy_parent(permission: &str) -> &str {
    match permission {
        "filesystem:read" | "diagnostics:read" => "orchestration:read",
        "settings:write" | "providers:manage" | "environment:maintain" | "preview:operate"
        | "source-control:write" | "filesystem:write" => "orchestration:operate",
        "terminal:read" => "terminal:operate",
        other => other,
    }
}

/// `EnvironmentAuthorizationError` for a missing permission.
pub fn authorization_error(permission: &str) -> Failure {
    Failure::Authorization {
        message: format!("The authenticated token is missing required scope: {permission}."),
        required_scope: legacy_parent(permission).to_owned(),
        required_permission: Some(permission.to_owned()),
    }
}

/// `Ok` if `granted` covers `method` for `payload`; methods without a declared
/// permission are denied. The error names the first missing permission.
pub fn check(method: &str, payload: &Value, granted: &[String]) -> Result<(), Failure> {
    match required_permissions(method, payload) {
        Some(ps) => match ps.into_iter().find(|p| !granted.iter().any(|g| g == p)) {
            Some(missing) => Err(authorization_error(missing)),
            None => Ok(()),
        },
        None => Err(Failure::Authorization {
            message: format!("RPC method {method} has no declared authorization scope."),
            required_scope: "orchestration:operate".to_owned(),
            required_permission: None,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The permission a refusal names.
    fn missing(refused: Failure) -> String {
        match refused {
            Failure::Authorization { required_permission, .. } => required_permission.expect("a permission"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn every_registered_method_declares_a_permission() {
        let router = crate::rpc::router();
        let missing: Vec<&str> = router.tags().filter(|t| required_permissions(t, &Value::Null).is_none()).collect();
        assert!(missing.is_empty(), "methods without a permission: {missing:?}");
    }

    #[test]
    fn denials_match_t3code() {
        let read_only = vec!["orchestration:read".to_owned()];
        let none = Value::Null;
        assert!(check("orchestration.subscribeShell", &none, &read_only).is_ok());
        let e = json!(check("terminal.open", &none, &read_only).unwrap_err());
        assert_eq!(e["requiredPermission"], "terminal:operate");
        assert_eq!(e["message"], "The authenticated token is missing required scope: terminal:operate.");
        let e = json!(check("projects.readFile", &none, &read_only).unwrap_err());
        assert_eq!(e["requiredScope"], "orchestration:read", "legacy parent for older clients");
        assert_eq!(e["requiredPermission"], "filesystem:read");
        // Reading a Git LFS file is not enough to fetch its object.
        let readers = vec!["orchestration:read".to_owned(), "filesystem:read".to_owned()];
        assert!(check("projects.readFile", &none, &readers).is_ok());
        let e = json!(check("projects.fetchLfsObject", &none, &readers).unwrap_err());
        assert_eq!(e["requiredPermission"], "source-control:write");
        assert_eq!(
            json!(check("no.such.method", &none, &read_only).unwrap_err()),
            json!({ "_tag": "EnvironmentAuthorizationError", "message": "RPC method no.such.method has no declared authorization scope.", "requiredScope": "orchestration:operate" })
        );
    }

    #[test]
    fn payload_dependent_rules() {
        let settings_only = vec!["settings:write".to_owned()];
        assert!(check("server.updateSettings", &json!({ "patch": { "enableAssistantStreaming": true } }), &settings_only).is_ok());
        let e = check("server.updateSettings", &json!({ "patch": { "providers": {} } }), &settings_only).unwrap_err();
        assert_eq!(missing(e), "providers:manage");
        let operate = vec!["orchestration:operate".to_owned()];
        assert!(check("projects.mutate", &json!({ "type": "project.update", "title": "x" }), &operate).is_ok());
        let e = check("projects.mutate", &json!({ "type": "project.update", "scripts": [] }), &operate).unwrap_err();
        assert_eq!(missing(e), "settings:write");
        let read = vec!["orchestration:read".to_owned()];
        assert!(check("assets.createUrl", &json!({ "resource": { "_tag": "provider-icon" } }), &read).is_ok());
        let e = check("assets.createUrl", &json!({ "resource": { "_tag": "workspace-file" } }), &read).unwrap_err();
        assert_eq!(missing(e), "filesystem:read");
    }
}
