//! What Antigravity's agent says and wants over ACP beyond the generic
//! adapter (t3code `acp/AntigravityAcpSupport.ts`, `acp/AntigravityProtocol.ts`,
//! `acp/AntigravityClientFiles.ts`, `AntigravityProvider.ts`).

#![expect(clippy::disallowed_types, clippy::disallowed_macros, reason = "Antigravity's ACP messages, read and written as its agent sends them")]

use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use domovoi_model::typed::{ApprovalDecision, ApprovalOption, ModelCapabilities, QuestionOption, ServerProviderModel, UserInputQuestion};

const TEXT_LIMIT: usize = 8_000;
const QUESTION_LABEL_LIMIT: usize = 512;
const WARNING_LIMIT: usize = 512;
const CLIENT_FILE_MAX_BYTES: u64 = 8 * 1024 * 1024;

/// `1.5.14` as numbers (missing parts are 0).
fn version(v: Option<&str>) -> Option<(u64, u64, u64)> {
    super::version_tuple(v?)
}

/// The client the agent is told it talks to. Before 1.5.1 the agent offered
/// non-Gemini models only to allowlisted editors, so older runtimes are told
/// it is Zed.
pub fn client_info(agent_version: Option<&str>) -> Value {
    match version(agent_version) {
        Some(v) if v >= (1, 5, 1) => json!({ "name": "t3-code", "title": "T3 Code", "version": domovoi_contracts::environment::SERVER_VERSION }),
        _ => json!({ "name": "zed", "version": "0.0.0" }),
    }
}

/// `initialize`: shaped for both ACP v1 and v2 (t3code's negotiating
/// request). File reads are offered; writes never are (advertising them
/// swaps the agent's diff edits for whole-file `client_edit_file`).
pub fn initialize_params(agent_version: Option<&str>) -> Value {
    let info = client_info(agent_version);
    json!({
        "protocolVersion": 2,
        "info": info,
        "capabilities": {},
        "clientCapabilities": {
            "fs": { "readTextFile": true, "writeTextFile": false },
            "terminal": false,
            "session": { "notices": {}, "compaction": {} },
        },
        "clientInfo": info,
    })
}

/// Whether the agent appends `_meta.antigravity.systemPrompt.append` to its
/// system instructions (it says so in `initialize`).
pub fn appends_system_prompt(init: &Value) -> bool {
    init["agentCapabilities"]["_meta"]["antigravity"]["systemPrompt"]["append"] == true
}

/// `_meta` for `session/new` and `session/resume`: the instructions about
/// Domovoi's tools as a system prompt addition, when the agent takes one.
/// The agent keeps none of it between processes, so every activation sends it.
pub fn session_meta(init: &Value, instructions: Option<&str>) -> Option<Value> {
    let append = instructions.filter(|_| appends_system_prompt(init))?;
    Some(json!({ "antigravity": { "systemPrompt": { "append": append } } }))
}

/// A select option's choices, groups flattened.
pub fn flatten_choices(options: &Value) -> Vec<Value> {
    options.as_array().into_iter().flatten().flat_map(|c| if c.get("value").is_some() { vec![c.clone()] } else { c["options"].as_array().cloned().unwrap_or_default() }).collect()
}

/// The model select among the session's config options.
pub fn model_option(options: &Value) -> Option<&Value> {
    options.as_array()?.iter().find(|o| (o["id"] == "model" || o["category"] == "model") && o["type"] == "select")
}

/// The thinking level select (runtime 1.4 and later report it as its own option).
pub fn thinking_option(options: &Value) -> Option<&Value> {
    options.as_array()?.iter().find(|o| o["category"] == "thought_level" && o["type"] == "select")
}

/// Records the thinking option the selected model shows (`null`: it has none).
pub fn observe_thinking(thinking: &mut BTreeMap<String, Value>, options: &Value) {
    if let Some(model) = model_option(options).and_then(|m| m["currentValue"].as_str()).filter(|m| !m.is_empty()) {
        thinking.insert(model.to_owned(), thinking_option(options).cloned().unwrap_or(Value::Null));
    }
}

fn capabilities(option: Option<&Value>, observed: bool) -> ModelCapabilities {
    // Another model's selected level is not this model's default.
    let descriptor = option.and_then(|o| super::select_descriptor(o, observed));
    ModelCapabilities { option_descriptors: Some(descriptor.into_iter().collect()) }
}

/// The models a session lists (t3code `buildAntigravityModelsFromSession`):
/// its model select, else its `models` list. The current one is the
/// account's default (and answers to [`super::DEFAULT_MODEL`]); each has the
/// thinking levels it showed when selected, else the current model's, unselected.
pub fn models_from_session(session: &Value, thinking: &BTreeMap<String, Value>) -> Vec<ServerProviderModel> {
    let options = &session["configOptions"];
    let config = options.as_array().and_then(|o| o.iter().find(|o| o["id"] == "model" || o["category"] == "model"));
    let current = match config {
        Some(c) if c["type"] == "select" => c["currentValue"].as_str(),
        _ => session["models"]["currentModelId"].as_str(),
    };
    let entries: Vec<(String, String)> = match config {
        Some(c) if c["type"] == "select" => flatten_choices(&c["options"]).iter().filter_map(|e| Some((e["value"].as_str()?.to_owned(), e["name"].as_str().unwrap_or_default().to_owned()))).collect(),
        Some(_) => vec![],
        None => session["models"]["availableModels"].as_array().into_iter().flatten().filter_map(|m| Some((m["modelId"].as_str()?.to_owned(), m["name"].as_str().unwrap_or_default().to_owned()))).collect(),
    };
    let current_thinking = thinking_option(options);
    let mut seen = std::collections::HashSet::new();
    let mut names = std::collections::HashSet::new();
    let mut out = vec![];
    for (value, name) in entries {
        if value.trim().is_empty() || !seen.insert(value.clone()) {
            continue;
        }
        let base = if name.trim().is_empty() { value.clone() } else { name };
        // The agent can list two ids under one name: the later ones say which.
        let name = if names.contains(&base) { format!("{base} ({value})") } else { base.clone() };
        names.insert(base);
        let is_current = Some(value.as_str()) == current;
        let caps = if is_current {
            capabilities(current_thinking, true)
        } else {
            match thinking.get(&value) {
                Some(Value::Null) => capabilities(None, true),
                Some(observed) => capabilities(Some(observed), true),
                None => capabilities(current_thinking, false),
            }
        };
        out.push(ServerProviderModel {
            slug: value,
            name,
            short_name: None,
            sub_provider: None,
            aliases: is_current.then(|| vec![super::DEFAULT_MODEL.to_owned()]),
            badge: None,
            is_custom: false,
            is_default: is_current.then_some(true),
            is_legacy: None,
            supports_text_generation: None,
            capabilities: Some(caps),
        });
    }
    out
}

/// What setting the model takes (t3code `applyAntigravityAcpModelSelection`).
#[derive(Debug, PartialEq, Eq)]
pub enum ModelChoice {
    /// Leave the session's model.
    Keep,
    /// Set a listed model.
    Set(String),
    /// Set an older id of a listed family (`gemini-3.8-flash-high` where
    /// `gemini-3.8-flash` is listed): the agent still takes it, unlisted.
    SetUnlisted(String),
    /// The account has no such model.
    Unavailable(String),
}

/// The listed family an older suffixed id belongs to (the longest).
fn legacy_family(listed: &[String], model: &str) -> Option<String> {
    listed.iter().filter(|f| model.starts_with(&format!("{f}-"))).max_by_key(|f| f.len()).cloned()
}

/// The model to set for `wanted` (empty or [`super::DEFAULT_MODEL`]: the
/// catalog default if the account has it, else the session's own). A saved
/// selection is never replaced by the default a cold resume reports.
pub fn choose_model(options: &Value, wanted: &str, default: Option<&str>) -> ModelChoice {
    let current = model_option(options).and_then(|m| m["currentValue"].as_str());
    let listed: Vec<String> = model_option(options).map(|m| flatten_choices(&m["options"]).iter().filter_map(|e| e["value"].as_str().map(str::to_owned)).collect()).unwrap_or_default();
    let explicit = !wanted.is_empty() && wanted != super::DEFAULT_MODEL && wanted != "default";
    let resolved = if explicit {
        Some(wanted.to_owned())
    } else {
        default.filter(|d| listed.iter().any(|l| l == d) || legacy_family(&listed, d).is_some()).map(str::to_owned).or_else(|| current.map(str::to_owned))
    };
    let Some(resolved) = resolved else { return ModelChoice::Keep };
    if !explicit && Some(resolved.as_str()) == current {
        return ModelChoice::Keep;
    }
    if listed.iter().any(|l| *l == resolved) {
        return if Some(resolved.as_str()) == current { ModelChoice::Keep } else { ModelChoice::Set(resolved) };
    }
    if legacy_family(&listed, &resolved).is_some() {
        return ModelChoice::SetUnlisted(resolved);
    }
    if listed.is_empty() {
        // A session that lists no models (an older runtime): its model is set as asked.
        return ModelChoice::Set(resolved);
    }
    ModelChoice::Unavailable(format!("Antigravity model '{resolved}' is unavailable for this Google account. Select an available model."))
}

/// The value a model option selection sets on the session, when the
/// session has that option and offers that value (others are skipped,
/// as an option of another model can be).
pub fn option_value<'a>(options: &'a Value, id: &str, value: &'a str) -> Option<&'a str> {
    let option = options.as_array()?.iter().find(|o| o["id"] == id)?;
    let value = if value == "_t3/empty" { "" } else { value };
    if option["type"] == "select" && !flatten_choices(&option["options"]).iter().any(|c| c["value"] == value) {
        return None;
    }
    (option["currentValue"] != value).then_some(value)
}

/// The session mode for a turn: `"plan"` when planning first and the agent
/// offers it, else `"default"`. The CLI rejects the managed runtime's `yolo`
/// and `auto_edit`, so non-plan turns stay in `default`, where it asks first
/// and Domovoi answers from the thread's runtime mode.
pub fn session_mode(_runtime_mode: &str, interaction_mode: &str) -> &'static str {
    if interaction_mode == "plan" { "plan" } else { "default" }
}

/// The mode value to set on `mode_option` (`configOptions` select with
/// `category: "mode"`): `"plan"` when planning first and offered, else `"default"`.
pub fn resolved_mode_for_option(mode_option: &Value, interaction_mode: &str) -> &'static str {
    let wanted = session_mode("", interaction_mode);
    if wanted == "plan" && flatten_choices(&mode_option["options"]).iter().any(|c| c["value"] == "plan") {
        "plan"
    } else {
        "default"
    }
}

/// The mode id to set via `session/set_mode`: `"plan"` when planning first and
/// listed in `availableModes`, else `"default"`.
pub fn resolved_mode_for_session(session: &Value, interaction_mode: &str) -> &'static str {
    let wanted = session_mode("", interaction_mode);
    if wanted == "plan" && session["modes"]["availableModes"].as_array().is_some_and(|m| m.iter().any(|m| m["id"] == "plan")) {
        "plan"
    } else {
        "default"
    }
}

/// agy can end a prompt with a pending question tool and consume its answer
/// in the next prompt. The notification has no JSON-RPC request to answer.
pub fn message_questions(update: &Value) -> Option<Vec<UserInputQuestion>> {
    if update["name"] != "ask_question" || update["status"] != "pending" {
        return None;
    }
    let fields = update["args"]["questions"].as_array()?;
    if fields.is_empty() || fields.len() > 16 {
        return None;
    }
    fields.iter().enumerate().map(|(index, field)| {
        let question = field["question"].as_str()?.trim();
        let options = field["options"].as_array()?;
        if question.is_empty() || question.chars().count() > TEXT_LIMIT || options.is_empty() || options.len() > 16 {
            return None;
        }
        let options = options.iter().enumerate().map(|(index, option)| {
            let label = option.as_str()?.trim();
            if label.is_empty() || label.chars().count() > QUESTION_LABEL_LIMIT {
                return None;
            }
            Some(QuestionOption { label: label.into(), description: label.into(), value: Some(format!("option-{}", index + 1)), preview: None })
        }).collect::<Option<Vec<_>>>()?;
        Some(UserInputQuestion {
            id: format!("question-{}", index + 1),
            header: format!("Question {}", index + 1),
            question: question.into(),
            options,
            multi_select: Some(field["is_multi_select"] == true),
            allow_custom_answer: Some(false),
            initial_answer: None,
            required: Some(true),
        })
    }).collect()
}

/// Native questions come as permission requests too.
pub fn is_question(params: &Value) -> bool {
    params["toolCall"]["toolCallId"].as_str().is_some_and(|id| id.starts_with("interaction_"))
}

fn bounded(text: &str, limit: usize) -> String {
    if text.chars().count() <= limit { text.to_owned() } else { format!("{}...", text.chars().take(limit - 3).collect::<String>()) }
}

fn question_label(option: &Value) -> String {
    let name = option["name"].as_str().map(str::trim).filter(|n| !n.is_empty());
    bounded(name.or(option["optionId"].as_str()).unwrap_or_default(), QUESTION_LABEL_LIMIT)
}

/// A native question as a question form: its options are the choices
/// (`None` when they are not distinct and named).
pub fn question(params: &Value) -> Option<UserInputQuestion> {
    if !is_question(params) {
        return None;
    }
    let options = params["options"].as_array().filter(|o| !o.is_empty())?;
    let mut ids = std::collections::HashSet::new();
    for o in options {
        let id = o["optionId"].as_str().filter(|id| !id.trim().is_empty())?;
        if !ids.insert(id) {
            return None;
        }
    }
    let call = &params["toolCall"];
    let text = call["title"].as_str().map(str::trim).filter(|t| !t.is_empty()).unwrap_or("Choose an option.");
    Some(UserInputQuestion {
        id: call["toolCallId"].as_str().unwrap_or_default().to_owned(),
        header: "Question".into(),
        question: bounded(text, TEXT_LIMIT),
        options: options
            .iter()
            .map(|o| {
                let label = question_label(o);
                QuestionOption { description: label.clone(), label, value: o["optionId"].as_str().map(str::to_owned), preview: None }
            })
            .collect(),
        multi_select: Some(false),
        allow_custom_answer: Some(false),
        initial_answer: None,
        required: None,
    })
}

/// The answer to a native question: the option the answer names by id, or
/// by its label when only one has it.
pub fn question_answer(params: &Value, answers: Option<&serde_json::Map<String, Value>>) -> Option<Value> {
    question(params)?;
    let id = params["toolCall"]["toolCallId"].as_str()?;
    let answer = match answers?.get(id)? {
        Value::String(s) => s.clone(),
        Value::Array(a) if a.len() == 1 => a[0].as_str()?.to_owned(),
        _ => return None,
    };
    let options = params["options"].as_array()?;
    let chosen = options.iter().find(|o| o["optionId"] == answer.as_str()).or_else(|| {
        let by_label: Vec<&Value> = options.iter().filter(|o| question_label(o) == answer).collect();
        (by_label.len() == 1).then(|| by_label[0])
    })?;
    Some(json!({ "outcome": { "outcome": "selected", "optionId": chosen["optionId"] } }))
}

/// The answers an approval offers, as the client shows them; "Allow for
/// this thread" carries the agent's security warning.
pub fn approval_options(params: &Value) -> Vec<ApprovalOption> {
    if is_question(params) {
        return vec![];
    }
    let options = params["options"].as_array().cloned().unwrap_or_default();
    let with = |kind: &str| options.iter().find(|o| o["kind"] == kind && o["optionId"].as_str().is_some_and(|id| !id.trim().is_empty()));
    let mut out = vec![];
    if with("allow_once").is_some() {
        out.push(ApprovalOption { decision: ApprovalDecision::Accept, label: "Allow once".into(), warning: None });
    }
    if let Some(always) = with("allow_always") {
        let warning = &always["_meta"]["agy.security.warning"];
        let text = [warning["message"].as_str(), warning["title"].as_str()].into_iter().flatten().map(str::trim).find(|t| !t.is_empty());
        out.push(ApprovalOption { decision: ApprovalDecision::AcceptForSession, label: "Allow for this thread".into(), warning: text.map(|t| bounded(t, WARNING_LIMIT)) });
    }
    if with("reject_once").is_some() {
        out.push(ApprovalOption { decision: ApprovalDecision::Decline, label: "Deny".into(), warning: None });
    }
    out.push(ApprovalOption { decision: ApprovalDecision::Cancel, label: "Cancel".into(), warning: None });
    out
}

/// A turn's token use from `session/prompt`'s answer, with Gemini's
/// counting: cached reads add to input, thoughts to output.
/// (input, cached input, output, reasoning).
pub fn prompt_usage(usage: &Value) -> Option<(u64, u64, u64, u64)> {
    let input = usage["inputTokens"].as_u64()?;
    let output = usage["outputTokens"].as_u64().unwrap_or(0);
    let cached = usage["cachedReadTokens"].as_u64().unwrap_or(0);
    let thought = usage["thoughtTokens"].as_u64().unwrap_or(0);
    let (input, output) = (input + cached, output + thought);
    Some((input, cached.min(input), output, thought.min(output)))
}

/// The real path of `requested` when it is inside one of `roots` (every
/// link resolved, the deepest existing ancestor's too, so a link above
/// missing folders can't lead out).
fn contained(requested: &Path, roots: &[PathBuf]) -> Option<PathBuf> {
    let mut candidate = requested.to_path_buf();
    let mut missing = vec![];
    let real = loop {
        if let Ok(real) = std::fs::canonicalize(&candidate) {
            break missing.iter().rev().fold(real, |p: PathBuf, part: &std::ffi::OsString| p.join(part));
        }
        // Only missing entries are walked past, never a dangling link.
        if std::fs::symlink_metadata(&candidate).is_ok() {
            return None;
        }
        missing.push(candidate.file_name()?.to_owned());
        candidate = candidate.parent()?.to_path_buf();
    };
    roots.iter().map(|r| std::fs::canonicalize(r).unwrap_or_else(|_| r.clone())).any(|root| real.starts_with(&root)).then_some(real)
}

/// `fs/read_text_file` for the agent: UTF-8 text inside the thread's folder
/// or the attachments folder, up to 8 MiB; `line` (from 1) and `limit` (a
/// line count) take part of it. An error is (code, message).
pub fn read_text_file(params: &Value, roots: &[PathBuf]) -> Result<Value, (i64, String)> {
    let requested = params["path"].as_str().ok_or((-32602, "No path to read.".to_owned()))?;
    let path = std::path::absolute(requested).unwrap_or_else(|_| PathBuf::from(requested));
    let outside = || (-32602, format!("Path '{requested}' is outside the session workspace."));
    let real = contained(&path, roots).ok_or_else(outside)?;
    let meta = std::fs::metadata(&real).map_err(|_| (-32002, format!("File '{requested}' not found.")))?;
    if !meta.is_file() || meta.len() > CLIENT_FILE_MAX_BYTES {
        return Err((-32602, format!("File '{requested}' is not a readable text file under {CLIENT_FILE_MAX_BYTES} bytes.")));
    }
    let bytes = std::fs::read(&real).map_err(|_| (-32603, format!("Could not read '{requested}'.")))?;
    let text = String::from_utf8(bytes).map_err(|_| (-32602, format!("File '{requested}' is not a UTF-8 text file.")))?;
    if text.contains('\0') {
        return Err((-32602, format!("File '{requested}' contains binary data.")));
    }
    let (line, limit) = (params["line"].as_u64(), params["limit"].as_u64());
    if line.is_none() && limit.is_none() {
        return Ok(json!({ "content": text }));
    }
    let lines: Vec<&str> = text.split('\n').collect();
    let start = (line.unwrap_or(1).max(1) - 1) as usize;
    let start = start.min(lines.len());
    let end = limit.map_or(lines.len(), |l| (start + l as usize).min(lines.len()));
    let part = lines[start..end].join("\n");
    // A CRLF file's last line taken ends in the `\r` of its ending.
    Ok(json!({ "content": part.strip_suffix('\r').unwrap_or(&part) }))
}

/// A Google sign-in link the agent printed: after its stdout prefix, or the
/// browser helper's marker (a JSON string). `None` for any other line.
pub fn auth_url_in(line: &str) -> Option<String> {
    let line = line.strip_suffix('\r').unwrap_or(line);
    if line.len() > 16_384 + 128 {
        return None;
    }
    if let Some(url) = line.strip_prefix(super::AUTH_STDOUT_PREFIX) {
        return Some(url.to_owned());
    }
    serde_json::from_str::<String>(line.strip_prefix(super::AUTH_BROWSER_MARKER)?).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What `agy acp` 1.5.14's `session/new` lists (trimmed).
    fn session() -> Value {
        json!({
            "sessionId": "s1",
            "models": { "availableModels": [{ "modelId": "gemini-4-neon-high", "name": "Gemini 4 Neon (High)" }], "currentModelId": "gemini-4-neon-high" },
            "configOptions": [
                { "id": "model", "name": "Model", "category": "model", "type": "select", "currentValue": "gemini-4-neon",
                  "options": [{ "value": "gemini-4-neon", "name": "Gemini 4 Neon" }, { "value": "gemini-3.8-flash", "name": "Gemini 3.8 Flash" }, { "value": "gpt-oss-120b-medium", "name": "GPT-OSS 120B (Medium)" }] },
                { "id": "thought_level", "name": "Thinking Level", "category": "thought_level", "type": "select", "currentValue": "high",
                  "options": [{ "value": "low", "name": "Low" }, { "value": "high", "name": "High" }] },
                { "id": "mode", "name": "Mode", "category": "mode", "type": "select", "currentValue": "default", "options": [{ "value": "default", "name": "Default" }] },
            ],
        })
    }

    #[test]
    fn the_model_list_comes_from_the_session_with_thinking_levels() {
        let mut thinking = BTreeMap::new();
        observe_thinking(&mut thinking, &session()["configOptions"]);
        thinking.insert("gpt-oss-120b-medium".into(), Value::Null);
        let models = models_from_session(&session(), &thinking);
        let slugs: Vec<&str> = models.iter().map(|m| m.slug.as_str()).collect();
        assert_eq!(slugs, ["gemini-4-neon", "gemini-3.8-flash", "gpt-oss-120b-medium"]);
        assert_eq!(models[0].is_default, Some(true));
        assert_eq!(models[0].aliases.as_deref(), Some(&["antigravity-default".to_owned()][..]));
        let descriptors = |m: &ServerProviderModel| serde_json::to_value(m.capabilities.as_ref().unwrap().option_descriptors.as_ref().unwrap()).unwrap();
        let current = descriptors(&models[0]);
        assert_eq!((current[0]["id"].as_str(), current[0]["currentValue"].as_str(), current[0]["role"].as_str()), (Some("thought_level"), Some("high"), Some("reasoning")));
        // Not yet seen selected: the current model's levels, none chosen.
        let unseen = descriptors(&models[1]);
        assert_eq!(unseen[0]["currentValue"], Value::Null);
        assert_eq!(descriptors(&models[2]), json!([]), "seen without thinking levels");
    }

    #[test]
    fn duplicate_model_names_say_which_id_they_are() {
        let s = json!({ "models": { "availableModels": [{ "modelId": "a", "name": "Same" }, { "modelId": "b", "name": "Same" }], "currentModelId": "a" } });
        let names: Vec<String> = models_from_session(&s, &BTreeMap::new()).into_iter().map(|m| m.name).collect();
        assert_eq!(names, ["Same", "Same (b)"]);
    }

    #[test]
    fn the_model_is_chosen_as_t3code_does() {
        let options = &session()["configOptions"];
        assert_eq!(choose_model(options, "gemini-3.8-flash", None), ModelChoice::Set("gemini-3.8-flash".into()));
        assert_eq!(choose_model(options, "gemini-4-neon", None), ModelChoice::Keep);
        assert_eq!(choose_model(options, "", Some("gemini-3.8-flash-high")), ModelChoice::SetUnlisted("gemini-3.8-flash-high".into()));
        assert_eq!(choose_model(options, "antigravity-default", Some("gemini-9")), ModelChoice::Keep, "a default the account lacks keeps the session's");
        assert!(matches!(choose_model(options, "claude-opus-9", None), ModelChoice::Unavailable(m) if m.contains("claude-opus-9")));
        assert_eq!(option_value(options, "thought_level", "low"), Some("low"));
        assert_eq!(option_value(options, "thought_level", "high"), None, "already set");
        assert_eq!(option_value(options, "thought_level", "max"), None, "not offered");
        assert_eq!(option_value(options, "nope", "x"), None);
    }

    #[test]
    fn questions_and_approvals_are_told_apart() {
        let ask = json!({ "toolCall": { "toolCallId": "interaction_7", "title": "Which file?" },
            "options": [{ "optionId": "o1", "name": "a.txt", "kind": "allow_once" }, { "optionId": "o2", "name": "b.txt", "kind": "allow_once" }] });
        let q = question(&ask).unwrap();
        assert_eq!((q.id.as_str(), q.question.as_str(), q.options.len()), ("interaction_7", "Which file?", 2));
        assert_eq!(q.options[1].value.as_deref(), Some("o2"));
        let by_label = serde_json::Map::from_iter([("interaction_7".to_owned(), json!(["b.txt"]))]);
        assert_eq!(question_answer(&ask, Some(&by_label)).unwrap(), json!({ "outcome": { "outcome": "selected", "optionId": "o2" } }));
        let by_id = serde_json::Map::from_iter([("interaction_7".to_owned(), json!("o1"))]);
        assert_eq!(question_answer(&ask, Some(&by_id)).unwrap()["outcome"]["optionId"], "o1");
        assert!(approval_options(&ask).is_empty());

        let approve = json!({ "toolCall": { "toolCallId": "call-1", "title": "rm" }, "options": [
            { "optionId": "y", "name": "Allow", "kind": "allow_once" },
            { "optionId": "a", "name": "Always", "kind": "allow_always", "_meta": { "agy.security.warning": { "message": " Runs any command " } } },
            { "optionId": "n", "name": "No", "kind": "reject_once" }] });
        assert!(question(&approve).is_none());
        let options = serde_json::to_value(approval_options(&approve)).unwrap();
        assert_eq!(options, json!([{ "decision": "accept", "label": "Allow once" }, { "decision": "acceptForSession", "label": "Allow for this thread", "warning": "Runs any command" },
            { "decision": "decline", "label": "Deny" }, { "decision": "cancel", "label": "Cancel" }]));
    }

    #[test]
    fn initialize_and_session_meta_follow_the_agent_version() {
        assert_eq!(client_info(Some("1.5.14"))["name"], "t3-code");
        assert_eq!(client_info(Some("1.5.0"))["name"], "zed");
        assert_eq!(client_info(None)["name"], "zed");
        let params = initialize_params(Some("1.5.14"));
        assert_eq!((params["protocolVersion"].as_u64(), params["clientCapabilities"]["fs"]["writeTextFile"].as_bool()), (Some(2), Some(false)));
        assert_eq!(params["info"], params["clientInfo"]);
        let init = json!({ "agentCapabilities": { "_meta": { "antigravity": { "systemPrompt": { "append": true } } } } });
        assert_eq!(session_meta(&init, Some("tools")).unwrap(), json!({ "antigravity": { "systemPrompt": { "append": "tools" } } }));
        assert_eq!(session_meta(&json!({}), Some("tools")), None);
        assert_eq!(session_meta(&init, None), None);
    }

    #[test]
    fn prompt_usage_counts_as_gemini_does() {
        assert_eq!(prompt_usage(&json!({ "inputTokens": 100, "outputTokens": 20, "cachedReadTokens": 50, "thoughtTokens": 5 })), Some((150, 50, 25, 5)));
        assert_eq!(prompt_usage(&json!({})), None);
    }

    #[test]
    fn file_reads_stay_in_the_workspace() {
        let dir = std::env::temp_dir().join(format!("domovoi-ag-read-{}", crate::util::new_uuid()));
        let (work, other) = (dir.join("w"), dir.join("o"));
        std::fs::create_dir_all(&work).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        std::fs::write(work.join("a.txt"), "one\ntwo\nthree").unwrap();
        std::fs::write(other.join("secret.txt"), "no").unwrap();
        std::fs::write(work.join("bin"), b"a\0b").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&other, work.join("out")).unwrap();
        let roots = [work.clone()];
        let read = |path: PathBuf, extra: Value| {
            let mut p = json!({ "path": path.to_string_lossy() });
            if let (Some(p), Some(e)) = (p.as_object_mut(), extra.as_object()) {
                p.extend(e.clone());
            }
            read_text_file(&p, &roots)
        };
        assert_eq!(read(work.join("a.txt"), json!({})).unwrap()["content"], "one\ntwo\nthree");
        assert_eq!(read(work.join("a.txt"), json!({ "line": 2, "limit": 1 })).unwrap()["content"], "two");
        // A CRLF file (written on Windows): lines without their `\r`, the
        // endings between them as they are.
        std::fs::write(work.join("crlf.txt"), "one\r\ntwo\r\nthree\r\n").unwrap();
        assert_eq!(read(work.join("crlf.txt"), json!({ "line": 3, "limit": 1 })).unwrap()["content"], "three");
        assert_eq!(read(work.join("crlf.txt"), json!({ "line": 1, "limit": 2 })).unwrap()["content"], "one\r\ntwo");
        assert!(read(other.join("secret.txt"), json!({})).unwrap_err().1.contains("outside"));
        #[cfg(unix)]
        assert!(read(work.join("out/secret.txt"), json!({})).unwrap_err().1.contains("outside"), "a link leading out");
        assert!(read(work.join("out/missing/x"), json!({})).is_err());
        assert!(read(work.join("bin"), json!({})).unwrap_err().1.contains("binary"));
        assert_eq!(read(work.join("nope.txt"), json!({})).unwrap_err().0, -32002);
        // Paths that only look as if they were inside.
        assert!(read(work.join("../o/secret.txt"), json!({})).unwrap_err().1.contains("outside"), "a `..` out");
        assert!(read(work.join("missing/../../o/secret.txt"), json!({})).unwrap_err().1.contains("outside"), "a `..` past a missing folder");
        std::fs::create_dir_all(dir.join("w2")).unwrap();
        std::fs::write(dir.join("w2/a.txt"), "no").unwrap();
        assert!(read(dir.join("w2/a.txt"), json!({})).unwrap_err().1.contains("outside"), "a sibling whose name starts with the root's");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(dir.join("gone"), work.join("dangling")).unwrap();
            assert!(read(work.join("dangling/x"), json!({})).unwrap_err().1.contains("outside"), "a dangling link is not walked past");
            std::os::unix::fs::symlink(work.join("a.txt"), work.join("inner")).unwrap();
            assert_eq!(read(work.join("inner"), json!({})).unwrap()["content"], "one\ntwo\nthree", "a link that stays inside");
        }
        assert!(read(PathBuf::from("relative/a.txt"), json!({})).unwrap_err().1.contains("outside"), "relative to the server's folder, not the thread's");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sign_in_links_are_found_in_either_form() {
        assert_eq!(auth_url_in("Open the following link to authenticate the ACP server: https://accounts.google.com/x\r").as_deref(), Some("https://accounts.google.com/x"));
        assert_eq!(auth_url_in("__T3_ANTIGRAVITY_AUTH_URL__\"https://accounts.google.com/y\"").as_deref(), Some("https://accounts.google.com/y"));
        assert_eq!(auth_url_in("{\"jsonrpc\":\"2.0\"}"), None);
    }

    #[test]
    fn plan_mode_is_used_when_requested_and_offered() {
        let with_plan = json!({ "id": "mode", "category": "mode", "type": "select", "currentValue": "default",
            "options": [{ "value": "default", "name": "Default" }, { "value": "plan", "name": "Plan" }] });
        let without_plan = json!({ "id": "mode", "category": "mode", "type": "select", "currentValue": "default",
            "options": [{ "value": "default", "name": "Default" }] });
        assert_eq!(resolved_mode_for_option(&with_plan, "plan"), "plan");
        assert_eq!(resolved_mode_for_option(&with_plan, "default"), "default");
        assert_eq!(resolved_mode_for_option(&without_plan, "plan"), "default");

        let s_with_plan = json!({ "modes": { "availableModes": [{ "id": "default" }, { "id": "plan" }] } });
        let s_without_plan = json!({ "modes": { "availableModes": [{ "id": "default" }] } });
        assert_eq!(resolved_mode_for_session(&s_with_plan, "plan"), "plan");
        assert_eq!(resolved_mode_for_session(&s_with_plan, "default"), "default");
        assert_eq!(resolved_mode_for_session(&s_without_plan, "plan"), "default");
    }
}
