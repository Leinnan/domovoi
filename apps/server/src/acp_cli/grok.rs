//! xAI's Grok CLI: `grok agent stdio`, launched in
//! the permission mode the thread's runtime mode asks for (Grok has no
//! auto-accept-edits mode), signing in with `XAI_API_KEY` when set, else
//! the CLI's cached `grok login` token.
//!
//! - Probe: `grok models` says whether the CLI is logged in (it exits 0
//!   either way; a logged-out answer is asked once more, since a token the
//!   CLI refreshes prints logged-out first) and lists models; `initialize`
//!   of `grok agent stdio` lists them with their reasoning levels in
//!   `_meta.modelState` and its slash commands in `_meta.availableCommands`.
//! - Turns: the model is `session/set_model`, its reasoning level that
//!   call's `_meta.reasoningEffort` (protocol 1) or the `thought_level`
//!   option; a message sent during a turn is `_x.ai/interject`; Grok's
//!   `ask_user_question` is a question form and `exit_plan_mode` a proposed
//!   plan.

#![expect(clippy::disallowed_types, clippy::disallowed_macros, reason = "Grok's `initialize` answer, `grok models` output and xAI extension requests, read as Grok sends them")]

use super::{Config, Ran};
use domovoi_model::typed::{
    ApprovalDecision, ApprovalOption, ModelCapabilities, QuestionOption, RuntimeMode, ServerProvider, ServerProviderAuthStatus, ServerProviderModel,
    ServerProviderSlashCommand, ServerProviderSlashCommandInput, ServerProviderState, UserInputQuestion,
};
use serde_json::{Map, Value, json};
use std::path::Path;
use std::time::Duration;

/// A product name, not an ACP model id: it keeps the session's model.
pub const DEFAULT_MODEL: &str = "grok-build";
pub const RUNTIME_MODES: [RuntimeMode; 3] = [RuntimeMode::ApprovalRequired, RuntimeMode::Auto, RuntimeMode::FullAccess];
/// `grok agent stdio` answers `initialize` after its startup work, 9-14 s
/// from a home folder.
const INITIALIZE_TIMEOUT: Duration = Duration::from_secs(20);
/// The interjection method (`_x.ai/interject`).
pub const INTERJECT: &str = "_x.ai/interject";
/// The rate-limited error code of a failed prompt.
pub const RATE_LIMITED: i64 = -32003;

/// The arguments for a runtime mode: argv overrides Grok's config, so
/// Supervised cannot inherit always-approve.
pub fn spawn_args(runtime_mode: Option<&str>) -> &'static [&'static str] {
    match runtime_mode {
        None => &["agent", "stdio"],
        Some("auto") => &["--permission-mode", "auto", "agent", "stdio"],
        Some("full-access") => &["agent", "--always-approve", "stdio"],
        Some(_) => &["--permission-mode", "default", "agent", "stdio"],
    }
}

/// The mode Grok is launched in for the thread's runtime mode.
pub fn launch_mode(runtime_mode: &str) -> &str {
    runtime_mode
}

/// `xai.api_key` with an API key, else the cached `grok login` token.
pub fn auth_method(config: &Config) -> &'static str {
    if config.var("XAI_API_KEY").is_some() { "xai.api_key" } else { "cached_token" }
}

pub fn built_in_models() -> Vec<ServerProviderModel> {
    vec![super::model(DEFAULT_MODEL, "Grok Build", None)]
}

/// The model id sent to Grok for a thread's model.
pub fn model_id(model: &str) -> &str {
    let model = model.trim();
    if model.is_empty() { DEFAULT_MODEL } else { model }
}

const EFFORT_RANK: [&str; 7] = ["none", "minimal", "low", "medium", "high", "xhigh", "max"];

fn valid_effort(v: &str) -> bool {
    let mut chars = v.chars();
    v.len() <= 32 && chars.next().is_some_and(|c| c.is_ascii_alphanumeric()) && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
}

/// A model's reasoning levels from its `_meta` (lowest first) and the
/// current one.
pub fn reasoning_levels(model: &Value) -> (Vec<(String, String, Option<String>, bool)>, Option<String>) {
    let meta = &model["_meta"];
    if !meta.is_object() || meta["supportsReasoningEffort"] == false {
        return (vec![], None);
    }
    let text = |v: &Value| v.as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned);
    let mut seen = std::collections::HashSet::new();
    let mut options: Vec<(String, String, Option<String>, bool)> = vec![];
    for entry in meta["reasoningEfforts"].as_array().into_iter().flatten().filter(|e| e.is_object()) {
        let value = text(&entry["value"]).filter(|v| valid_effort(v)).or_else(|| text(&entry["id"]).filter(|v| valid_effort(v)));
        let Some(value) = value.filter(|v| seen.insert(v.clone())) else { continue };
        let label = text(&entry["label"]).unwrap_or_else(|| value.clone());
        options.push((value, label, text(&entry["description"]), entry["default"] == true || entry["isDefault"] == true));
    }
    let current = text(&meta["reasoningEffort"]).filter(|c| options.iter().any(|o| &o.0 == c));
    let default = options.iter().find(|o| o.3 && Some(&o.0) == current.as_ref()).or_else(|| options.iter().find(|o| o.3)).map(|o| o.0.clone());
    let rank = |v: &str| EFFORT_RANK.iter().position(|r| *r == v);
    if options.iter().all(|o| rank(&o.0).is_some()) {
        options.sort_by_key(|o| rank(&o.0));
    }
    let options = options.into_iter().map(|(v, l, d, _)| { let is_default = Some(&v) == default.as_ref(); (v, l, d, is_default) }).collect();
    (options, current.or(default))
}

fn capabilities(model: &Value) -> ModelCapabilities {
    let (levels, current) = reasoning_levels(model);
    if levels.is_empty() {
        return ModelCapabilities { option_descriptors: Some(vec![]) };
    }
    let choices = levels.iter().map(|(v, l, d, is_default)| crate::providers::choice(v, l, d.as_deref(), *is_default)).collect();
    ModelCapabilities { option_descriptors: Some(vec![crate::providers::select("reasoningEffort", "Reasoning", choices, current)]) }
}

/// The models of a session model state (`initialize`'s `_meta.modelState`).
pub fn models_of_state(state: &Value) -> Vec<ServerProviderModel> {
    let current = state["currentModelId"].as_str().unwrap_or_default().trim();
    let mut seen = std::collections::HashSet::new();
    state["availableModels"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| {
            let id = m["modelId"].as_str()?.trim();
            let slug = model_id(id).to_owned();
            if !seen.insert(slug.clone()) {
                return None;
            }
            let name = m["name"].as_str().map(str::trim).filter(|n| !n.is_empty()).unwrap_or(&slug).to_owned();
            let mut model = super::model(&slug, &name, Some(capabilities(m)));
            model.is_default = (id == current).then_some(true);
            Some(model)
        })
        .collect()
}

/// `grok models`: whether it is logged in, and its model bullets.
pub fn parse_models_output(output: &str) -> (Option<bool>, Vec<ServerProviderModel>) {
    let lower = output.to_lowercase();
    let authenticated = if lower.contains("you are logged in") {
        Some(true)
    } else if lower.contains("not authenticated") || lower.contains("not logged in") {
        Some(false)
    } else {
        None
    };
    let mut seen = std::collections::HashSet::new();
    let mut models = vec![];
    for line in output.lines() {
        let line = line.trim_start();
        let Some(rest) = line.strip_prefix("* ").or_else(|| line.strip_prefix("- ")) else { continue };
        let rest = rest.trim_start();
        let (id, after) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
        if id.is_empty() {
            continue;
        }
        let slug = model_id(id).to_owned();
        if !seen.insert(slug.clone()) {
            continue;
        }
        let name = slug.split(['-', '_']).map(|p| if p.eq_ignore_ascii_case("grok") { "Grok" } else { p }).collect::<Vec<_>>().join(" ");
        let mut model = super::model(&slug, &name, None);
        model.is_default = after.to_lowercase().contains("(default)").then_some(true);
        models.push(model);
    }
    (authenticated, models)
}

/// The slash commands `initialize` lists, after `/compact` (t3code
/// `grokSlashCommandsFromInitialize`): not `always-approve`, nor `context`
/// (its ACP handler prints nothing).
pub fn slash_commands(init: &Value) -> Vec<ServerProviderSlashCommand> {
    let mut out = vec![super::compact_command()];
    for c in init["_meta"]["availableCommands"].as_array().into_iter().flatten() {
        let (Some(name), Some(description)) = (c["name"].as_str().map(str::trim), c["description"].as_str()) else { continue };
        if name.is_empty() || name.eq_ignore_ascii_case("always-approve") || name.eq_ignore_ascii_case("context") {
            continue;
        }
        let description = Some(description.trim().to_owned()).filter(|d| !d.is_empty());
        let input = c["input"]["hint"].as_str().map(str::trim).filter(|h| !h.is_empty()).map(|hint| ServerProviderSlashCommandInput { hint: hint.to_owned() });
        let command = ServerProviderSlashCommand { name: name.to_owned(), description, input };
        match out.iter_mut().find(|o| o.name == name) {
            Some(slot) => *slot = command,
            None => out.push(command),
        }
    }
    out
}

/// What a probe reads after `--version`.
pub async fn probe(exe: &Path, config: &Config, v: &mut ServerProvider) {
    let env = super::probe_env(super::Kind::Grok, config);
    let api_key = config.var("XAI_API_KEY").is_some();
    let list = || async {
        match super::run(exe, &["models"], &env, None, super::AUTH_TIMEOUT).await {
            Ran::Done { code: Some(0), stdout, stderr } => parse_models_output(&format!("{stdout}\n{stderr}")),
            _ => (None, vec![]),
        }
    };
    let mut cli = list().await;
    if cli.0 == Some(false) && !api_key {
        let again = list().await;
        if again.0.is_some() {
            cli = again;
        }
    }
    v.auth = if api_key {
        super::auth(ServerProviderAuthStatus::Authenticated, Some("api_key"), Some("xAI API key"))
    } else {
        match cli.0 {
            Some(true) => super::auth(ServerProviderAuthStatus::Authenticated, Some("cached_token"), Some("Grok account")),
            Some(false) => super::auth(ServerProviderAuthStatus::Unauthenticated, None, None),
            None => super::auth(ServerProviderAuthStatus::Unknown, None, None),
        }
    };
    let init = super::acp_initialize(super::Kind::Grok, exe, config, None, INITIALIZE_TIMEOUT).await.ok().map(|(init, _)| init);
    let acp_models = init.as_ref().map(|i| models_of_state(&i["_meta"]["modelState"])).unwrap_or_default();
    let discovered = if acp_models.is_empty() { cli.1 } else { acp_models };
    if !discovered.is_empty() {
        v.models = super::with_custom_models(discovered.clone(), config, None);
    }
    if v.auth.status == ServerProviderAuthStatus::Unauthenticated {
        v.status = ServerProviderState::Error;
        v.message = Some("Grok CLI is installed but not logged in. Run `grok login`.".into());
        return;
    }
    v.slash_commands = Some(init.as_ref().map_or_else(|| vec![super::compact_command()], slash_commands));
    let incomplete = if init.is_none() {
        Some("Grok CLI is installed but ACP initialize failed. Model options may be incomplete.")
    } else if discovered.is_empty() {
        Some("Grok CLI did not list any models. Model options may be incomplete.")
    } else {
        None
    };
    v.status = if incomplete.is_some() { ServerProviderState::Warning } else { ServerProviderState::Ready };
    v.message = incomplete.map(str::to_owned);
}

/// The choices a Grok approval offers: only
/// `allow-edits-session` is for the session; Grok's other allow-always
/// choices save grants for the project, so they are not offered.
pub fn approval_options(params: &Value) -> Vec<ApprovalOption> {
    let has = |kind: &str, id: Option<&str>| {
        params["options"].as_array().is_some_and(|o| o.iter().any(|o| o["kind"] == kind && id.is_none_or(|id| o["optionId"].as_str().map(str::trim) == Some(id))))
    };
    let option = |decision, label: &str| ApprovalOption { decision, label: label.into(), warning: None };
    let mut out = vec![option(ApprovalDecision::Cancel, "Cancel")];
    if has("reject_once", None) {
        out.push(option(ApprovalDecision::Decline, "Decline"));
    }
    if has("allow_always", Some("allow-edits-session")) {
        out.push(option(ApprovalDecision::AcceptForSession, "Allow all edits this session"));
    }
    if has("allow_once", None) {
        out.push(option(ApprovalDecision::Accept, "Approve"));
    }
    out
}

/// The parameters of an xAI extension request, whether wrapped or not.
fn unwrapped(params: &Value) -> &Value {
    if params["params"].is_object() && params["method"].is_string() { &params["params"] } else { params }
}

/// Whether `method` is Grok's question (`x.ai/ask_user_question`).
pub fn is_question(method: &str) -> bool {
    matches!(method, "x.ai/ask_user_question" | "_x.ai/ask_user_question")
}

/// Whether `method` is Grok's plan exit (`x.ai/exit_plan_mode`).
pub fn is_plan_exit(method: &str) -> bool {
    matches!(method, "x.ai/exit_plan_mode" | "_x.ai/exit_plan_mode")
}

/// Whether `method` says a prompt is done (`x.ai/session/prompt_complete`).
pub fn is_prompt_complete(method: &str) -> bool {
    matches!(method, "x.ai/session/prompt_complete" | "_x.ai/session/prompt_complete")
}

/// Grok's question as a form, and the tool call it belongs to.
pub fn questions(params: &Value) -> Option<(String, Vec<UserInputQuestion>)> {
    let p = unwrapped(params);
    let call = p["toolCallId"].as_str()?.to_owned();
    let questions = p["questions"]
        .as_array()?
        .iter()
        .filter_map(|q| {
            let question = q["question"].as_str()?.to_owned();
            let mut options: Vec<QuestionOption> = q["options"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|o| {
                    let label = o["label"].as_str()?.to_owned();
                    Some(QuestionOption { description: o["description"].as_str().map_or_else(|| label.clone(), str::to_owned), label, value: None, preview: None })
                })
                .collect();
            if options.is_empty() {
                options.push(QuestionOption { label: "OK".into(), description: "Continue".into(), value: None, preview: None });
            }
            Some(UserInputQuestion {
                id: q["id"].as_str().map_or_else(|| question.clone(), str::to_owned),
                header: "Question".into(),
                question,
                options,
                multi_select: Some(q["multiSelect"] == true),
                allow_custom_answer: None,
                initial_answer: None,
                required: None,
            })
        })
        .collect::<Vec<_>>();
    (!questions.is_empty()).then_some((call, questions))
}

/// The answer Grok is sent: the
/// labels chosen by question text, free text as notes; none, cancelled.
pub fn question_answer(params: &Value, answers: Option<&Map<String, Value>>) -> Value {
    let Some(answers) = answers else { return json!({ "outcome": "cancelled" }) };
    let mut chosen = Map::new();
    let mut notes = Map::new();
    for q in unwrapped(params)["questions"].as_array().into_iter().flatten() {
        let Some(text) = q["question"].as_str() else { continue };
        let key = q["id"].as_str().unwrap_or(text);
        let given = answers.get(key).or_else(|| answers.get(text));
        let values: Vec<String> = match given {
            Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned).collect(),
            Some(Value::String(s)) if !s.trim().is_empty() => vec![s.trim().to_owned()],
            _ => continue,
        };
        let options: Vec<&Value> = q["options"].as_array().into_iter().flatten().collect();
        let option = |v: &str| options.iter().find(|o| o["label"] == v).copied();
        let labels: Vec<String> = values.iter().filter(|v| option(v).is_some()).cloned().collect();
        let free: Vec<&str> = values.iter().filter(|v| option(v).is_none()).map(String::as_str).collect();
        let preview = (q["multiSelect"] != true).then(|| values.iter().find_map(|v| option(v)?["preview"].as_str().map(str::trim).filter(|p| !p.is_empty()))).flatten();
        let mut note = Map::new();
        if let Some(p) = preview {
            note.insert("preview".into(), json!(p));
        }
        if !free.is_empty() {
            note.insert("notes".into(), json!(free.join("\n")));
        }
        if !note.is_empty() {
            notes.insert(text.to_owned(), Value::Object(note));
        }
        chosen.insert(text.to_owned(), json!(if labels.is_empty() { vec!["Other".to_owned()] } else { labels }));
    }
    let mut answer = json!({ "outcome": "accepted", "answers": chosen });
    if !notes.is_empty() {
        answer["annotations"] = Value::Object(notes);
    }
    answer
}

/// The plan of `exit_plan_mode`.
pub fn plan_markdown(params: &Value, fallback: Option<&str>) -> String {
    let from = |s: Option<&str>| s.map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned);
    from(unwrapped(params)["planContent"].as_str())
        .or_else(|| from(fallback))
        .unwrap_or_else(|| "# No plan written yet\n\n(The agent exited plan mode without writing a plan.)".to_owned())
}

/// Grok is told the plan was captured, and stops there (the user implements it later).
pub fn plan_captured() -> Value {
    json!({ "outcome": "abandoned", "feedback": "The client captured your proposed plan. Stop here and wait for the user's feedback or implementation request in a later turn." })
}

/// The current model and reasoning level of a session (`models`).
pub fn current(session: &Value) -> (Option<String>, Option<String>) {
    let state = &session["models"];
    let id = state["currentModelId"].as_str().map(str::trim).filter(|s| !s.is_empty()).map(str::to_owned);
    let effort = id.as_ref().and_then(|id| {
        let model = state["availableModels"].as_array()?.iter().find(|m| m["modelId"].as_str().map(str::trim) == Some(id))?;
        model["_meta"]["reasoningEffort"].as_str().map(str::trim).filter(|e| valid_effort(e)).map(str::to_owned)
    });
    (id, effort)
}

/// The levels the session offers for `model`.
pub fn offered_efforts(session: &Value, model: &str) -> Vec<String> {
    session["models"]["availableModels"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|m| m["modelId"].as_str().map(str::trim) == Some(model))
        .map(|m| reasoning_levels(m).0.into_iter().map(|o| o.0).collect())
        .unwrap_or_default()
}

/// What `session/set_model` is sent for a selection, or nothing when the
/// session already runs it.
pub fn set_model_params(session_id: &str, session: &Value, model: &str, effort: Option<&str>) -> Option<Value> {
    let (current, current_effort) = current(session);
    let requested = Some(model_id(model)).filter(|m| *m != DEFAULT_MODEL);
    let target = requested.map(str::to_owned).or(current.clone())?;
    let offered = offered_efforts(session, &target);
    let effort = effort.map(str::trim).filter(|e| valid_effort(e) && (offered.is_empty() || offered.iter().any(|o| o == e)));
    let model_changed = requested.is_some_and(|r| Some(r) != current.as_deref());
    let effort_changed = effort.is_some_and(|e| Some(e) != current_effort.as_deref());
    if !model_changed && !effort_changed {
        return None;
    }
    let mut params = json!({ "sessionId": session_id, "modelId": target });
    if let Some(e) = effort {
        params["_meta"] = json!({ "reasoningEffort": e });
    }
    Some(params)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> Value {
        json!({ "models": { "currentModelId": "grok-4", "availableModels": [
            { "modelId": "grok-4", "name": "Grok 4", "_meta": { "reasoningEffort": "high", "reasoningEfforts": [{ "value": "high", "default": true }, { "value": "low" }] } },
            { "modelId": "grok-code", "name": "Grok Code", "_meta": { "supportsReasoningEffort": false } },
        ] } })
    }

    #[test]
    fn reasoning_levels_read_lowest_first_with_the_default() {
        let (levels, current) = reasoning_levels(&session()["models"]["availableModels"][0]);
        assert_eq!(levels.iter().map(|l| (l.0.as_str(), l.3)).collect::<Vec<_>>(), [("low", false), ("high", true)]);
        assert_eq!(current.as_deref(), Some("high"));
        assert!(reasoning_levels(&session()["models"]["availableModels"][1]).0.is_empty());
        let models = models_of_state(&session()["models"]);
        assert_eq!((models[0].slug.as_str(), models[0].is_default), ("grok-4", Some(true)));
        let caps = serde_json::to_value(&models[0].capabilities).unwrap();
        assert_eq!(caps["optionDescriptors"][0]["id"], "reasoningEffort");
    }

    #[test]
    fn the_model_is_set_only_when_it_or_its_level_changes() {
        let s = session();
        assert_eq!(set_model_params("s", &s, "grok-build", None), None, "the product name keeps the session's model");
        assert_eq!(set_model_params("s", &s, "grok-4", Some("high")), None);
        assert_eq!(set_model_params("s", &s, "grok-4", Some("low")).unwrap(), json!({ "sessionId": "s", "modelId": "grok-4", "_meta": { "reasoningEffort": "low" } }));
        assert_eq!(set_model_params("s", &s, "grok-4", Some("max")), None, "a level the model doesn't offer is skipped");
        assert_eq!(set_model_params("s", &s, "grok-code", None).unwrap(), json!({ "sessionId": "s", "modelId": "grok-code" }));
    }

    #[test]
    fn grok_models_output_says_who_is_logged_in() {
        let (auth, models) = parse_models_output("You are logged in as x.\n\nModels:\n  * grok-4 (default)\n  - grok-code-fast\n");
        assert_eq!(auth, Some(true));
        assert_eq!(models.iter().map(|m| (m.slug.as_str(), m.name.as_str(), m.is_default)).collect::<Vec<_>>(), [("grok-4", "Grok 4", Some(true)), ("grok-code-fast", "Grok code fast", None)]);
        assert_eq!(parse_models_output("Not authenticated. Run grok login").0, Some(false));
    }

    #[test]
    fn slash_commands_skip_the_ones_grok_cannot_run_here() {
        let init = json!({ "_meta": { "availableCommands": [{ "name": "review", "description": "Review" }, { "name": "context", "description": "x" }, { "name": "always-approve", "description": "x" }, { "name": "compact", "description": "Compact", "input": { "hint": "focus" } }] } });
        let commands = slash_commands(&init);
        assert_eq!(commands.iter().map(|c| c.name.as_str()).collect::<Vec<_>>(), ["compact", "review"]);
        assert_eq!(commands[0].input.as_ref().map(|i| i.hint.as_str()), Some("focus"));
    }

    #[test]
    fn questions_and_answers_follow_xai() {
        let params = json!({ "sessionId": "s", "toolCallId": "t", "mode": "default", "questions": [
            { "question": "Which?", "options": [{ "label": "A", "preview": "pa" }, { "label": "B" }] },
            { "id": "q2", "question": "Free?", "options": [] },
        ] });
        let (call, qs) = questions(&json!({ "method": "_x.ai/ask_user_question", "params": params })).unwrap();
        assert_eq!((call.as_str(), qs[0].id.as_str(), qs[1].options[0].label.as_str()), ("t", "Which?", "OK"));
        let answers: Map<String, Value> = serde_json::from_value(json!({ "Which?": "A", "q2": "my words" })).unwrap();
        assert_eq!(question_answer(&params, Some(&answers)), json!({ "outcome": "accepted", "answers": { "Which?": ["A"], "Free?": ["Other"] },
            "annotations": { "Which?": { "preview": "pa" }, "Free?": { "notes": "my words" } } }));
        assert_eq!(question_answer(&params, None), json!({ "outcome": "cancelled" }));
        assert_eq!(plan_markdown(&json!({ "planContent": "  # P " }), None), "# P");
        assert!(plan_markdown(&json!({}), None).starts_with("# No plan"));
    }

    #[test]
    fn approvals_offer_only_session_scoped_grants() {
        let params = json!({ "options": [{ "optionId": "once", "kind": "allow_once" }, { "optionId": "allow-project", "kind": "allow_always" }, { "optionId": "no", "kind": "reject_once" }] });
        let offered: Vec<ApprovalDecision> = approval_options(&params).into_iter().map(|o| o.decision).collect();
        assert_eq!(offered, [ApprovalDecision::Cancel, ApprovalDecision::Decline, ApprovalDecision::Accept]);
    }
}
