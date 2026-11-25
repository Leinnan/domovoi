//! Kiro's CLI (t3code `provider-kiro`): `kiro-cli acp --agent-engine=v3
//! --auth-method=cli`, whose access tokens stay inside Kiro.
//!
//! - Probe: `kiro-cli whoami --format json` says who is signed in
//!   (`{"account":null}`, exit 1, when no one is); signed in,
//!   `chat --list-models --format json` lists the account's models (its
//!   default as "Kiro default") and `initialize` with
//!   `_kiro/config/template` its agents, offered as every model's `mode`
//!   option.
//! - Turns: the agent (`mode`), the model (`model`) and its reasoning level
//!   (`effortLevel`) are session config options, set in that order;
//!   `autopilot` is on only for full access; a policy approval answers only
//!   with `allow_once` (Kiro's `allow_always` saves a rule for the whole
//!   workspace); a message sent during a turn is `_session/steer`, text only,
//!   taken once Kiro reports it `steering_injected`; the note Kiro's model
//!   writes about a steer is left out of the reply; Kiro's own cloud-config
//!   tool call is not the turn's.
//!
//! Limits: no fork (`session/fork` at a prompt's `userMessageId`), no
//! `_kiro/session/compact` (`/compact` goes to Kiro as a prompt), and Kiro's
//! sub-agents show as tool calls, not child threads.

#![expect(clippy::disallowed_types, clippy::disallowed_macros, reason = "Kiro's CLI JSON, its `_kiro/*` answers and `_meta.kiro` session updates, read as Kiro sends them")]

use super::{Config, Ran};
use domovoi_model::typed::{
    ApprovalDecision, ApprovalOption, ModelCapabilities, ProviderOptionDescriptor, RuntimeMode, ServerProvider, ServerProviderAuthStatus,
    ServerProviderModel, ServerProviderState,
};
use serde_json::{Value, json};
use std::path::Path;

/// T3's slug for "Kiro default".
pub const DEFAULT_MODEL: &str = "default";
/// Kiro's documented default model id.
pub const FALLBACK_DEFAULT_MODEL: &str = "auto";
pub const RUNTIME_MODES: [RuntimeMode; 2] = [RuntimeMode::ApprovalRequired, RuntimeMode::FullAccess];
pub const ACP_ARGS: [&str; 3] = ["acp", "--agent-engine=v3", "--auth-method=cli"];
pub const AUTOPILOT: &str = "autopilot";
pub const MODEL: &str = "model";
pub const EFFORT: &str = "effortLevel";
/// Kiro's agents are the choices of its `mode` select.
pub const AGENT: &str = "mode";
pub const STEER: &str = "_session/steer";

/// `autopilot`: on only for full access (t3code `kiroAutopilotValue`).
pub fn autopilot_value(runtime_mode: &str) -> &'static str {
    if runtime_mode == "full-access" { "on" } else { "off" }
}

pub fn built_in_models() -> Vec<ServerProviderModel> {
    vec![super::model(DEFAULT_MODEL, "Kiro default", None)]
}

/// The sign-in `whoami --format json` reports (t3code `kiroAuthFromWhoami`).
pub fn auth_from_whoami(code: Option<i32>, stdout: &str, api_key: bool) -> Option<domovoi_model::typed::ServerProviderAuth> {
    if code != Some(0) {
        return stdout.contains("\"account\":null").then(|| super::auth(ServerProviderAuthStatus::Unauthenticated, None, None));
    }
    let first: Value = serde_json::from_str(stdout.trim_start().lines().next().unwrap_or_default().trim()).unwrap_or(Value::Null);
    if first["accountType"] == "ApiKey" || api_key {
        return Some(super::auth(ServerProviderAuthStatus::Authenticated, Some("api_key"), Some("Kiro API key")));
    }
    let mut auth = super::auth(ServerProviderAuthStatus::Authenticated, None, Some("Kiro account"));
    auth.email = first["email"].as_str().map(str::to_owned);
    Some(auth)
}

/// Reasoning levels by Kiro model id and their default, as Kiro 2.32.0
/// offers them (`--list-models` names none).
fn effort_levels(model: &str) -> Option<(&'static [&'static str], &'static str)> {
    const CLAUDE: &[&str] = &["low", "medium", "high", "xhigh", "max"];
    const CLAUDE_4_6: &[&str] = &["low", "medium", "high", "max"];
    const GPT_5_6: &[&str] = &["none", "low", "medium", "high", "xhigh", "max"];
    const KIMI_K3: &[&str] = &["low", "high", "max"];
    Some(match model {
        "claude-fable-5.1" | "claude-opus-5" | "claude-opus-4.8" | "claude-sonnet-5.5" | "claude-sonnet-5" => (CLAUDE, "high"),
        "claude-opus-5.5" => (CLAUDE, "medium"),
        "claude-opus-4.7" => (CLAUDE, "xhigh"),
        "claude-opus-4.6" | "claude-sonnet-4.6" => (CLAUDE_4_6, "high"),
        "gpt-5.6-sol" | "gpt-5.6-terra" | "gpt-5.6-luna" => (GPT_5_6, "high"),
        "kimi-k3" => (KIMI_K3, "max"),
        _ => return None,
    })
}

fn effort_label(level: &str) -> &str {
    match level {
        "none" => "None",
        "low" => "Low",
        "medium" => "Medium",
        "high" => "High",
        "xhigh" => "Extra High",
        "max" => "Max",
        other => other,
    }
}

fn model_capabilities(model: &str) -> ModelCapabilities {
    let Some((levels, default)) = effort_levels(model) else { return ModelCapabilities { option_descriptors: Some(vec![]) } };
    let choices = levels.iter().map(|l| crate::providers::choice(l, effort_label(l), None, *l == default)).collect();
    ModelCapabilities { option_descriptors: Some(vec![crate::providers::select("reasoningEffort", "Reasoning", choices, None)]) }
}

/// The account's models from `chat --list-models --format json`: Kiro's
/// default listed as "Kiro default" with its id as its alias (t3code `kiroModelsFromList`).
pub fn models_from_list(stdout: &str) -> Option<Vec<ServerProviderModel>> {
    let list: Value = serde_json::from_str(stdout.trim()).ok()?;
    let models = list["models"].as_array().filter(|m| !m.is_empty())?;
    let default_id = list["default_model"].as_str().unwrap_or(FALLBACK_DEFAULT_MODEL);
    let listed_default = models.iter().find(|m| m["model_id"] == default_id);
    let mut default = super::model(
        DEFAULT_MODEL,
        listed_default.map_or("Kiro default", |m| m["model_name"].as_str().or(m["model_id"].as_str()).unwrap_or("Kiro default")),
        None,
    );
    default.aliases = Some(vec![default_id.to_owned()]);
    default.is_default = Some(true);
    let mut out = vec![];
    let mut placed = false;
    for m in models {
        let Some(id) = m["model_id"].as_str() else { continue };
        if listed_default.is_some_and(|d| d["model_id"] == id) {
            out.push(default.clone());
            placed = true;
        } else {
            out.push(super::model(id, m["model_name"].as_str().unwrap_or(id), Some(model_capabilities(id))));
        }
    }
    if !placed {
        out.insert(0, default);
    }
    Some(out)
}

/// Kiro's id for "Kiro default" on instance `id`: the alias its probe found, else `auto`.
pub fn default_model_id(id: &str) -> String {
    super::last_probe(id)
        .and_then(|p| p.models.into_iter().find(|m| m.slug == DEFAULT_MODEL)?.aliases?.into_iter().next())
        .unwrap_or_else(|| FALLBACK_DEFAULT_MODEL.to_owned())
}

/// The agent picker from `_kiro/config/template` (t3code `kiroAgentDescriptor`): none for one agent.
pub fn agent_descriptor(template: &Value) -> Option<ProviderOptionDescriptor> {
    let mode = template["configOptions"].as_array()?.iter().find(|o| o["id"] == AGENT && o["type"] == "select")?;
    let default = mode["currentValue"].as_str();
    let choices: Vec<_> = mode["options"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| {
            let id = c["value"].as_str()?.trim();
            let label = c["name"].as_str().map(str::trim).filter(|n| !n.is_empty()).unwrap_or(id);
            (!id.is_empty()).then(|| crate::providers::choice(id, label, c["description"].as_str().map(str::trim).filter(|d| !d.is_empty()), Some(id) == default))
        })
        .collect();
    if choices.len() < 2 {
        return None;
    }
    let current = default.filter(|d| choices.iter().any(|c| c.id == *d)).map(str::to_owned);
    Some(crate::providers::select(AGENT, "Agent", choices, current))
}

/// Every model with the agent picker (the composer reads options per model).
fn with_agents(models: Vec<ServerProviderModel>, agents: Option<&ProviderOptionDescriptor>) -> Vec<ServerProviderModel> {
    let Some(agents) = agents else { return models };
    models
        .into_iter()
        .map(|mut m| {
            let descriptors = m.capabilities.get_or_insert_with(|| ModelCapabilities { option_descriptors: Some(vec![]) }).option_descriptors.get_or_insert_with(Vec::new);
            if !descriptors.iter().any(|d| d.id() == Some(AGENT)) {
                descriptors.push(agents.clone());
            }
            m
        })
        .collect()
}

/// What a probe reads after `--version` (t3code `checkKiroProviderStatus`).
pub async fn probe(exe: &Path, config: &Config, v: &mut ServerProvider) {
    let env = super::probe_env(super::Kind::Kiro, config);
    let whoami = super::run(exe, &["whoami", "--format", "json"], &env, None, super::AUTH_TIMEOUT).await;
    let api_key = config.var("KIRO_API_KEY").is_some();
    let auth = match &whoami {
        Ran::Done { code, stdout, .. } => auth_from_whoami(*code, stdout, api_key),
        _ => None,
    };
    v.auth = auth.unwrap_or_else(|| super::auth(ServerProviderAuthStatus::Unknown, None, None));
    match v.auth.status {
        ServerProviderAuthStatus::Unauthenticated => {
            v.status = ServerProviderState::Error;
            v.message = Some("Kiro CLI is installed but not signed in. Run `kiro-cli login`.".into());
        }
        ServerProviderAuthStatus::Authenticated => {
            // Signed out, `--list-models` would start a browser login: only now are both asked.
            let (listed, template) = tokio::join!(
                super::run(exe, &["chat", "--list-models", "--format", "json"], &env, None, super::AUTH_TIMEOUT),
                super::acp_initialize(super::Kind::Kiro, exe, config, Some(("_kiro/config/template", json!({}))), super::AUTH_TIMEOUT),
            );
            let agents = template.ok().and_then(|(_, t)| t).and_then(|t| agent_descriptor(&t));
            let models = listed.ok_stdout().and_then(models_from_list).unwrap_or_else(built_in_models);
            v.models = with_agents(super::with_custom_models(models, config, None), agents.as_ref());
            v.status = ServerProviderState::Ready;
            v.message = None;
        }
        _ => {
            v.status = ServerProviderState::Warning;
            v.message = Some("Could not confirm the Kiro sign-in. Run `kiro-cli whoami` to check.".into());
        }
    }
}

/// The choices a Kiro approval offers (t3code `kiroApprovalOptions`):
/// "Always allow" only where Kiro offers no one-time choice.
pub fn approval_options(params: &Value) -> Vec<ApprovalOption> {
    let option = |decision, label: &str| ApprovalOption { decision, label: label.into(), warning: None };
    let mut out = vec![];
    if super::offers(&params["options"], "allow_once") {
        out.push(option(ApprovalDecision::Accept, "Allow once"));
    } else if super::offers(&params["options"], "allow_always") {
        out.push(option(ApprovalDecision::AcceptForSession, "Always allow"));
    }
    if super::offers(&params["options"], "reject_once") {
        out.push(option(ApprovalDecision::Decline, "Deny"));
    }
    out.push(option(ApprovalDecision::Cancel, "Cancel"));
    out
}

/// The id `_session/steer` queued the message under.
pub fn steer_queued(answer: &Value) -> Option<String> {
    (answer["queued"] == true).then(|| answer["messageId"].as_str().map(str::to_owned)).flatten()
}

/// What a `session_info_update` says of a steer: (kind, message id).
pub fn steering_info(update: &Value) -> Option<(&str, Option<&str>)> {
    if update["sessionUpdate"] != "session_info_update" {
        return None;
    }
    let kiro = &update["_meta"]["kiro"];
    Some((kiro["kind"].as_str()?, kiro["messageId"].as_str()))
}

const STEERING_NOTE: &str = "[STEERING steer-";

/// Leaves out the `[STEERING steer-…: …]` note Kiro's model ends a reply
/// with after a steer, holding back a chunk's end that may begin one
/// (t3code `makeKiroSteeringNoteFilter`).
#[derive(Default)]
pub struct SteeringNotes {
    /// After `steering_injected`, until `steering_cleared`.
    active: bool,
    held: String,
    in_note: bool,
}

impl SteeringNotes {
    /// Takes an update in; a message chunk comes back without the note.
    pub fn filter(&mut self, update: &mut Value) {
        if let Some((kind, _)) = steering_info(update) {
            match kind {
                "steering_injected" => *self = SteeringNotes { active: true, ..SteeringNotes::default() },
                "steering_cleared" => *self = SteeringNotes::default(),
                _ => {}
            }
            return;
        }
        if !self.active || update["sessionUpdate"] != "agent_message_chunk" || update["content"]["type"] != "text" {
            return;
        }
        let mut pending = std::mem::take(&mut self.held) + update["content"]["text"].as_str().unwrap_or_default();
        let mut visible = String::new();
        while !pending.is_empty() {
            if self.in_note {
                let Some(end) = pending.find(']') else {
                    pending.clear();
                    break;
                };
                pending = pending[end + 1..].to_owned();
                self.in_note = false;
                continue;
            }
            if let Some(start) = pending.find(STEERING_NOTE) {
                visible.push_str(pending[..start].trim_end());
                pending = pending[start + STEERING_NOTE.len()..].to_owned();
                self.in_note = true;
                continue;
            }
            let held = (1..STEERING_NOTE.len().min(pending.len() + 1)).rev().find(|&n| pending.is_char_boundary(pending.len() - n) && STEERING_NOTE.starts_with(&pending[pending.len() - n..])).unwrap_or(0);
            visible.push_str(&pending[..pending.len() - held]);
            self.held = pending[pending.len() - held..].to_owned();
            break;
        }
        update["content"]["text"] = json!(visible);
    }
}

/// Kiro's cloud-config fetch, a tool call of its own around session start
/// that can land in the user's turn: it and its updates are dropped.
#[derive(Default)]
pub struct CloudConfigCalls(std::collections::HashSet<String>);

impl CloudConfigCalls {
    /// Whether `update` is the fetch's (and so left out).
    pub fn ignore(&mut self, update: &Value) -> bool {
        if !matches!(update["sessionUpdate"].as_str(), Some("tool_call" | "tool_call_update" | "tool_call_content_chunk")) {
            return false;
        }
        let Some(id) = update["toolCallId"].as_str() else { return false };
        if update["_meta"]["kiro"]["toolId"] == "fetch_cloud_config" {
            self.0.insert(id.to_owned());
        } else if !self.0.contains(id) {
            return false;
        }
        if matches!(update["status"].as_str(), Some("completed" | "failed")) {
            self.0.remove(id);
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn whoami_says_who_is_signed_in() {
        assert_eq!(auth_from_whoami(Some(1), "{\"account\":null}", false).map(|a| a.status), Some(ServerProviderAuthStatus::Unauthenticated));
        assert!(auth_from_whoami(Some(1), "boom", false).is_none());
        let a = auth_from_whoami(Some(0), "{\"accountType\":\"Social\",\"email\":\"a@b.c\"}\nProfile: x", false).unwrap();
        assert_eq!((a.label.as_deref(), a.email.as_deref()), (Some("Kiro account"), Some("a@b.c")));
        assert_eq!(auth_from_whoami(Some(0), "{\"accountType\":\"ApiKey\"}", false).unwrap().r#type.as_deref(), Some("api_key"));
    }

    #[test]
    fn the_model_list_names_kiros_default() {
        let models = models_from_list(r#"{"models":[{"model_id":"auto","model_name":"Auto"},{"model_id":"claude-opus-4.6"}],"default_model":"auto"}"#).unwrap();
        assert_eq!(models.iter().map(|m| (m.slug.as_str(), m.name.as_str())).collect::<Vec<_>>(), [("default", "Auto"), ("claude-opus-4.6", "claude-opus-4.6")]);
        assert_eq!(models[0].aliases.as_deref(), Some(&["auto".to_owned()][..]));
        let effort = serde_json::to_value(&models[1].capabilities).unwrap();
        assert_eq!(effort["optionDescriptors"][0]["options"].as_array().unwrap().len(), 4);
        let unlisted = models_from_list(r#"{"models":[{"model_id":"x"}],"default_model":"auto"}"#).unwrap();
        assert_eq!(unlisted[0].slug, "default", "listed even when Kiro's list leaves it out");
        assert!(models_from_list("not json").is_none());
    }

    #[test]
    fn agents_are_offered_when_there_is_a_choice() {
        let template = json!({ "configOptions": [{ "id": "mode", "type": "select", "currentValue": "kiro_default", "options": [
            { "value": "kiro_default", "name": "Default" }, { "value": "reviewer", "name": "Reviewer", "description": "Reviews" }] }] });
        let d = serde_json::to_value(agent_descriptor(&template).unwrap()).unwrap();
        assert_eq!((d["id"].as_str(), d["currentValue"].as_str()), (Some("mode"), Some("kiro_default")));
        let one = json!({ "configOptions": [{ "id": "mode", "type": "select", "options": [{ "value": "a", "name": "A" }] }] });
        assert!(agent_descriptor(&one).is_none());
    }

    #[test]
    fn the_steering_note_is_left_out_across_chunks() {
        let mut notes = SteeringNotes::default();
        let chunk = |t: &str| json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": t } });
        let mut before = chunk("untouched [STEERING steer-1: x]");
        notes.filter(&mut before);
        assert_eq!(before["content"]["text"], "untouched [STEERING steer-1: x]", "only after an injected steer");
        notes.filter(&mut json!({ "sessionUpdate": "session_info_update", "_meta": { "kiro": { "kind": "steering_injected", "messageId": "m" } } }));
        let mut out = String::new();
        for part in ["Done. [STEE", "RING steer-9: use tabs", "] Bye", " [ok"] {
            let mut c = chunk(part);
            notes.filter(&mut c);
            out.push_str(c["content"]["text"].as_str().unwrap());
        }
        // The text before the note was sent already; the note and its trailing space go.
        assert_eq!(out, "Done.  Bye [ok");
    }

    #[test]
    fn the_cloud_config_call_is_not_the_turns() {
        let mut calls = CloudConfigCalls::default();
        assert!(calls.ignore(&json!({ "sessionUpdate": "tool_call", "toolCallId": "c", "_meta": { "kiro": { "toolId": "fetch_cloud_config" } } })));
        assert!(calls.ignore(&json!({ "sessionUpdate": "tool_call_update", "toolCallId": "c", "status": "completed" })));
        assert!(!calls.ignore(&json!({ "sessionUpdate": "tool_call_update", "toolCallId": "c", "status": "completed" })), "done with it");
        assert!(!calls.ignore(&json!({ "sessionUpdate": "tool_call", "toolCallId": "d" })));
    }

    #[test]
    fn approvals_never_offer_a_workspace_rule_beside_a_one_time_choice() {
        let both = json!({ "options": [{ "optionId": "o", "kind": "allow_once" }, { "optionId": "a", "kind": "allow_always" }, { "optionId": "r", "kind": "reject_once" }] });
        assert_eq!(approval_options(&both).into_iter().map(|o| o.decision).collect::<Vec<_>>(), [ApprovalDecision::Accept, ApprovalDecision::Decline, ApprovalDecision::Cancel]);
        let always = json!({ "options": [{ "optionId": "a", "kind": "allow_always" }] });
        assert_eq!(approval_options(&always)[0].label, "Always allow");
        assert_eq!(steer_queued(&json!({ "queued": true, "messageId": "m1" })).as_deref(), Some("m1"));
        assert_eq!(steer_queued(&json!({ "queued": false })), None);
    }
}
