//! Vercel's fx (t3code `provider-fx`): `fx acp`, its sessions asked to
//! bring the user's `~/.fx/mcp.json` servers.
//!
//! - Probe: `fx status --json` names the sign-in, the default backend and
//!   model (and, from fx 0.0.14, its modes); each backend (the gateway,
//!   Codex, Grok and those `~/.fx/settings.json` configures) is asked
//!   `status --json` and `models --json` with `FX_PROVIDER` set to it, and
//!   its models are listed as `backend:model`. "Default" is what fx starts
//!   on in the thread's folder.
//! - Turns: the session mode follows the runtime mode (`ask`, `auto` or
//!   `full-access`) and is written once a session opens even when fx says it
//!   is already in it (fx 0.0.13 enforces its saved default until a client
//!   writes the mode); the backend, the model and the effort (`medium` when
//!   the thread names none) are session config options; in `auto` fx asks
//!   only about what it refused itself, so every request reaches the user.

#![expect(clippy::disallowed_types, reason = "fx's `status --json`, `models --json` and settings, read as fx writes them")]

use super::{Config, Disposition};
use domovoi_model::typed::{ModelCapabilities, RuntimeMode, ServerProvider, ServerProviderAuthStatus, ServerProviderModel, ServerProviderState};
use serde_json::Value;
use std::path::Path;

/// T3's slug for "Default": what `fx status` reports, never an fx model id.
pub const DEFAULT_MODEL: &str = "default";
pub const RUNTIME_MODES: [RuntimeMode; 2] = [RuntimeMode::ApprovalRequired, RuntimeMode::Auto];
pub const PROVIDER: &str = "provider";
pub const MODEL: &str = "model";
pub const MODE: &str = "mode";
pub const EFFORT: &str = "effort";
pub const DEFAULT_EFFORT: &str = "medium";
pub const FULL_ACCESS: &str = "full-access";
const EFFORTS: [(&str, &str); 6] = [("auto", "Auto"), ("low", "Low"), ("medium", "Medium"), ("high", "High"), ("xhigh", "Extra High"), ("max", "Max")];

/// fx's built-in backends, in its `provider` option's order: (id, name, sign-in command).
const BUILT_IN_BACKENDS: [(&str, &str, &str); 3] = [("gateway", "Vercel AI Gateway", "fx login"), ("codex", "Codex subscription", "fx login codex"), ("grok", "Grok subscription", "fx login grok")];

/// Every fx model's options: its effort.
pub fn model_capabilities() -> ModelCapabilities {
    let choices = EFFORTS.iter().map(|(id, label)| crate::providers::choice(id, label, None, *id == DEFAULT_EFFORT)).collect();
    ModelCapabilities { option_descriptors: Some(vec![crate::providers::select(EFFORT, "Effort", choices, None)]) }
}

/// What a backend reported.
#[derive(Clone, Debug, PartialEq)]
pub struct Backend {
    pub id: String,
    pub name: String,
    /// `None` when its status could not be read.
    pub signed_in: Option<bool>,
    pub models: Option<Vec<String>>,
}

/// "Default" (named after fx's default model), then each backend's models
/// unless it is signed out (t3code `fxModels`).
pub fn models(default_model: Option<&str>, backends: &[Backend]) -> Vec<ServerProviderModel> {
    let caps = Some(model_capabilities());
    let mut default = super::model(DEFAULT_MODEL, &default_model.map_or_else(|| "Default".to_owned(), |m| format!("Default ({m})")), caps.clone());
    default.is_default = Some(true);
    let mut out = vec![default];
    for b in backends.iter().filter(|b| b.signed_in != Some(false)) {
        let mut seen = std::collections::HashSet::new();
        for id in b.models.iter().flatten().filter(|id| seen.insert(id.to_owned())) {
            let mut m = super::model(&format!("{}:{id}", b.id), id, caps.clone());
            m.sub_provider = Some(b.name.clone());
            out.push(m);
        }
    }
    out
}

/// The session mode for a runtime mode (t3code `fxSessionModeForPolicy`).
pub fn session_mode(runtime_mode: &str) -> &'static str {
    match runtime_mode {
        "auto" => "auto",
        "full-access" => FULL_ACCESS,
        _ => "ask",
    }
}

/// fx's id of the mode: fx 0.0.13 calls automatic review `code`.
pub fn mode_id<'a>(requested: &str, advertised: &[&'a str]) -> Option<&'a str> {
    let candidates: &[&str] = match requested {
        "ask" => &["ask"],
        FULL_ACCESS => &[FULL_ACCESS],
        "auto" | "code" => &["auto", "code"],
        _ => &[],
    };
    candidates.iter().find_map(|c| advertised.iter().copied().find(|a| a == c))
}

/// t3code `fxPermissionDisposition`.
pub fn disposition(runtime_mode: &str, tool_kind: &str) -> Disposition {
    match session_mode(runtime_mode) {
        "auto" => Disposition::Ask,
        FULL_ACCESS => super::disposition(runtime_mode, tool_kind),
        _ => super::disposition("approval-required", tool_kind),
    }
}

/// A model slug as (backend, model): a slug without a known backend is a
/// model of the session's current one.
pub fn parse_slug<'a>(slug: &'a str, backends: &[&str]) -> (Option<&'a str>, &'a str) {
    match slug.split_once(':') {
        Some((b, m)) if !b.is_empty() && backends.contains(&b) => (Some(b), m),
        _ => (None, slug),
    }
}

/// `fx status --json`: (auth, auth help, model, model source, modes).
fn status(stdout: &str) -> Option<Value> {
    serde_json::from_str::<Value>(stdout.trim()).ok().filter(Value::is_object)
}

fn text(v: &Value) -> Option<String> {
    v.as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned)
}

/// The backend `model_source` names: a built-in one by its label, a configured endpoint by its id.
fn status_backend(model_source: Option<&str>) -> String {
    match model_source.map(str::trim).filter(|s| !s.is_empty()) {
        None => "gateway".into(),
        Some(s) => BUILT_IN_BACKENDS.iter().find(|b| b.1 == s).map_or_else(|| s.to_owned(), |b| b.0.to_owned()),
    }
}

/// The endpoints `<FX_HOME or ~/.fx>/settings.json` configures.
fn configured_backends(config: &Config) -> Vec<String> {
    let profile = config.var("FX_HOME").map(std::path::PathBuf::from).unwrap_or_else(|| crate::util::home_dir().join(".fx"));
    let settings: Value = std::fs::read_to_string(profile.join("settings.json")).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or(Value::Null);
    settings["providers"]
        .as_object()
        .map(|p| {
            p.keys()
                .filter(|id| id.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'))
                .filter(|id| !BUILT_IN_BACKENDS.iter().any(|b| b.0 == id.to_lowercase()))
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

async fn read(exe: &Path, args: &[&str], env: &[(String, String)], cwd: Option<&Path>) -> Option<String> {
    super::run(exe, args, env, cwd, super::AUTH_TIMEOUT).await.ok_stdout().map(str::to_owned)
}

/// One backend's sign-in and models, with `FX_PROVIDER` naming it.
async fn probe_backend(exe: &Path, env: &[(String, String)], id: &str, name: &str) -> Backend {
    let mut env = env.to_vec();
    env.push(("FX_PROVIDER".into(), id.into()));
    env.push(("FX_MODEL".into(), DEFAULT_MODEL.into()));
    let s = read(exe, &["status", "--json"], &env, None).await.and_then(|o| status(&o));
    let signed_in = s.as_ref().and_then(|s| text(&s["auth"])).map(|a| a != "missing");
    let models = if signed_in == Some(false) {
        None
    } else {
        read(exe, &["models", "--json"], &env, None)
            .await
            .and_then(|o| serde_json::from_str::<Value>(o.trim()).ok())
            .and_then(|v| v["ids"].as_array().map(|ids| ids.iter().filter_map(text).collect()))
    };
    Backend { id: id.into(), name: name.into(), signed_in, models }
}

/// What a probe reads after `--version` (t3code `checkFxProviderStatus`).
pub async fn probe(exe: &Path, config: &Config, v: &mut ServerProvider) {
    let env = super::probe_env(super::Kind::Fx, config);
    let status = read(exe, &["status", "--json"], &env, None).await.and_then(|o| status(&o));
    let mut wanted: Vec<(String, String)> = BUILT_IN_BACKENDS.iter().map(|b| (b.0.to_owned(), b.1.to_owned())).collect();
    wanted.extend(configured_backends(config).into_iter().map(|id| (id.clone(), id)));
    let backends = futures::future::join_all(wanted.iter().map(|(id, name)| probe_backend(exe, &env, id, name))).await;
    let field = |k: &str| status.as_ref().and_then(|s| text(&s[k]));
    let default_model = field("model");
    let default_backend = backends.iter().find(|b| b.id == status_backend(field("model_source").as_deref()));
    let listed: Vec<&Backend> = backends.iter().filter(|b| b.signed_in != Some(false)).collect();
    let listing_failed = listed.is_empty() || listed.iter().any(|b| b.models.is_none());
    let signed_in = backends.iter().find(|b| b.signed_in == Some(true));
    v.auth = match field("auth") {
        Some(a) if a != "missing" => super::auth(ServerProviderAuthStatus::Authenticated, None, Some(&a)),
        _ if signed_in.is_some() => super::auth(ServerProviderAuthStatus::Authenticated, None, signed_in.map(|b| b.name.as_str())),
        Some(_) => super::auth(ServerProviderAuthStatus::Unauthenticated, None, None),
        None => super::auth(ServerProviderAuthStatus::Unknown, None, None),
    };
    let (state, message) = match v.auth.status {
        ServerProviderAuthStatus::Unauthenticated => (ServerProviderState::Error, Some("Run `fx login` to sign in.".to_owned())),
        ServerProviderAuthStatus::Unknown => (ServerProviderState::Warning, Some("fx is installed but login state could not be determined.".to_owned())),
        _ => match default_backend {
            Some(b) if b.signed_in == Some(false) => {
                (ServerProviderState::Warning, Some(format!("fx's default backend, {}, is signed out. Pick a model on another backend or sign in.", b.name)))
            }
            _ if listing_failed => (ServerProviderState::Warning, Some("fx is installed but model listing failed. Model options may be incomplete.".to_owned())),
            _ => (ServerProviderState::Ready, None),
        },
    };
    v.status = state;
    v.message = message;
    v.models = super::with_custom_models(models(default_model.as_deref(), &backends), config, Some(model_capabilities()));
    let full_access = status.as_ref().and_then(|s| s["modes"].as_array()).is_some_and(|m| m.iter().any(|m| m["id"] == FULL_ACCESS));
    if full_access {
        let mut modes = RUNTIME_MODES.to_vec();
        modes.push(RuntimeMode::FullAccess);
        v.supported_runtime_modes = Some(modes.into());
    }
}

/// The backend and model "Default" runs in `cwd` (fx's settings for that folder may name one).
pub async fn default_choice(exe: &Path, config: &Config, cwd: &Path) -> Option<(String, String)> {
    let mut env = config.env.clone();
    env.push(("MISE_OFFLINE".into(), "1".into()));
    let s = read(exe, &["status", "--json"], &env, Some(cwd)).await.and_then(|o| status(&o))?;
    let model = text(&s["model"])?;
    Some((status_backend(s["model_source"].as_str()), model))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modes_and_approvals_follow_the_runtime_mode() {
        assert_eq!((session_mode("approval-required"), session_mode("auto"), session_mode("full-access"), session_mode("auto-accept-edits")), ("ask", "auto", "full-access", "ask"));
        assert_eq!(mode_id("auto", &["ask", "code"]), Some("code"), "fx 0.0.13 calls it code");
        assert_eq!(mode_id("full-access", &["ask", "auto"]), None);
        assert_eq!(disposition("auto", "read"), Disposition::Ask, "auto asks only about what fx refused");
        assert_eq!(disposition("auto-accept-edits", "edit"), Disposition::Ask, "ask mode is Supervised");
        assert_eq!(disposition("full-access", "execute"), Disposition::Allow);
    }

    #[test]
    fn models_carry_their_backend() {
        let backends = vec![
            Backend { id: "gateway".into(), name: "Vercel AI Gateway".into(), signed_in: Some(true), models: Some(vec!["gpt-6".into(), "gpt-6".into()]) },
            Backend { id: "codex".into(), name: "Codex subscription".into(), signed_in: Some(false), models: None },
        ];
        let m = models(Some("gpt-6"), &backends);
        assert_eq!(m.iter().map(|m| (m.slug.as_str(), m.name.as_str())).collect::<Vec<_>>(), [("default", "Default (gpt-6)"), ("gateway:gpt-6", "gpt-6")]);
        assert_eq!(m[1].sub_provider.as_deref(), Some("Vercel AI Gateway"));
        assert_eq!(parse_slug("codex:gpt-5:mini", &["gateway", "codex"]), (Some("codex"), "gpt-5:mini"));
        assert_eq!(parse_slug("other:x", &["gateway"]), (None, "other:x"));
        assert_eq!(status_backend(Some("Grok subscription")), "grok");
        assert_eq!(status_backend(None), "gateway");
    }
}
