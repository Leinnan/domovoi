//! Text generation (t3code `textGeneration`): one-shot prompts answered
//! as JSON by the local Claude, Codex or Pi CLI, with no tools (an
//! investigation, such as a review guide, may read the checkout). Titles,
//! branch names, commit messages, pull request text and review guides come
//! from here. The model is the matching setting's selection
//! (`textGenerationModelSelection`, `sourceControlWriterModelSelection`,
//! `reviewGuideModelSelection`); prompts are t3code's
//! (`textGenerationPrompts.ts`).

#![expect(clippy::disallowed_types, clippy::disallowed_macros, reason = "the JSON schema given to the model and the model's raw answer")]

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::marker::PhantomData;
use std::time::Duration;
use domovoi_model::typed::{OptionSelection, OptionValue, ServerSettings, TextGenerationModelSelection};

const TIMEOUT: Duration = Duration::from_secs(180);
/// An investigation reads the checkout before answering.
const INVESTIGATION_TIMEOUT: Duration = Duration::from_secs(900);

/// `TextGenerationError`, as t3code reports a failed generation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error, Serialize)]
#[error("{detail}")]
#[serde(tag = "_tag", rename = "TextGenerationError")]
pub struct TextGenerationError {
    /// What was being written, such as `generateCommitMessage`.
    pub operation: String,
    pub detail: String,
}

pub fn error(operation: &str, detail: impl Into<String>) -> TextGenerationError {
    TextGenerationError { operation: operation.to_owned(), detail: detail.into() }
}

/// A prompt with the JSON schema the model's answer must follow, and the
/// record `A` that answer is read as.
pub struct Prompt<A> {
    text: String,
    /// A JSON schema, in the form both CLIs take.
    schema: Value,
    answer: PhantomData<fn() -> A>,
}

impl<A> Prompt<A> {
    fn new(text: String, schema: Value) -> Self {
        Prompt { text, schema, answer: PhantomData }
    }
}

impl Prompt<Value> {
    /// A prompt whose answer the caller reads itself, by the JSON schema it gives.
    pub fn with_schema(text: String, schema: Value) -> Self {
        Prompt::new(text, schema)
    }
}

/// A key of a model's answer, if the model wrote text there.
fn text_if_any<'de, D: serde::Deserializer<'de>>(answered: D) -> Result<Option<String>, D::Error> {
    Ok(Value::deserialize(answered)?.as_str().map(str::to_owned))
}

/// The answer to `commit_message_prompt`; `branch` only when it was asked for.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct CommitMessage {
    #[serde(default, deserialize_with = "text_if_any")]
    pub subject: Option<String>,
    #[serde(default, deserialize_with = "text_if_any")]
    pub body: Option<String>,
    #[serde(default, deserialize_with = "text_if_any")]
    pub branch: Option<String>,
}

/// The answer to `pr_content_prompt`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct PrContent {
    #[serde(default, deserialize_with = "text_if_any")]
    pub title: Option<String>,
    #[serde(default, deserialize_with = "text_if_any")]
    pub body: Option<String>,
}

/// The answer to `branch_name_prompt`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct BranchName {
    #[serde(default, deserialize_with = "text_if_any")]
    pub branch: Option<String>,
}

/// The answer to `thread_title_prompt`. (Its `needsRefinement` is not read.)
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
pub struct ThreadTitle {
    #[serde(default, deserialize_with = "text_if_any")]
    pub title: Option<String>,
}

/// One generation at a time, so a burst of titles doesn't start a crowd of CLIs.
fn gate() -> &'static tokio::sync::Semaphore {
    static G: std::sync::OnceLock<tokio::sync::Semaphore> = std::sync::OnceLock::new();
    G.get_or_init(|| tokio::sync::Semaphore::new(1))
}

/// A strict JSON schema object (every key required, nothing else), which
/// both CLIs accept for structured output.
pub fn object(props: &[(&str, Value)]) -> Value {
    let properties: serde_json::Map<String, Value> = props.iter().map(|(k, v)| ((*k).to_owned(), v.clone())).collect();
    json!({ "type": "object", "properties": properties, "required": props.iter().map(|(k, _)| *k).collect::<Vec<_>>(), "additionalProperties": false })
}

pub fn string() -> Value {
    json!({ "type": "string" })
}

/// What is written: each kind may have a model selection of its own in the settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Writer {
    /// Titles and branch names (`textGenerationModelSelection`).
    Text,
    /// Commit messages and pull request texts (`sourceControlWriterModelSelection`).
    SourceControl,
    /// Review guides (`reviewGuideModelSelection`).
    ReviewGuide,
}

/// Which model writes for `writer`: its own model selection of the settings,
/// or without one the general `textGenerationModelSelection`. The general one
/// may name less than a model (see `TextGenerationModelSelection`); with no
/// selection at all nothing is named.
pub fn model_for(settings: &ServerSettings, writer: Writer) -> TextGenerationModelSelection {
    let specific = match writer {
        Writer::SourceControl => settings.source_control_writer_model_selection.clone().flatten(),
        Writer::ReviewGuide => settings.review_guide_model_selection.clone().flatten(),
        Writer::Text => None,
    };
    match specific {
        Some(own) => TextGenerationModelSelection { instance_id: Some(own.instance_id), model: Some(own.model), options: own.options },
        None => settings.text_generation_model_selection.clone().unwrap_or(TextGenerationModelSelection { instance_id: None, model: None, options: None }),
    }
}

/// What a model selection of the settings says of the writer.
struct Selection {
    instance: Option<String>,
    model: Option<String>,
    /// The value of its first `effort` or `reasoningEffort` option, when that is text.
    effort: Option<String>,
}

impl Selection {
    fn of(selection: &TextGenerationModelSelection) -> Self {
        let effort = |options: &Vec<OptionSelection>| match &options.iter().find(|o| o.id == "effort" || o.id == "reasoningEffort")?.value {
            OptionValue::Choice(effort) => Some(effort.clone()),
            OptionValue::Switch(_) => None,
        };
        Selection {
            instance: selection.instance_id.as_ref().map(|id| id.as_str().to_owned()),
            model: selection.model.clone(),
            effort: selection.options.as_ref().and_then(effort),
        }
    }
}

pub struct Request<'a, A> {
    pub operation: &'a str,
    /// Where the CLI runs (a checkout, for an investigation); `None` runs it
    /// in an empty folder.
    pub cwd: Option<&'a str>,
    pub prompt: Prompt<A>,
    /// The settings' model selection (`model_for`).
    pub model: TextGenerationModelSelection,
    /// Read-only tools for looking around `cwd`.
    pub investigate: bool,
}

/// One run of a CLI: what `generate` was asked, without the answer's type.
struct Run<'a> {
    operation: &'a str,
    prompt: &'a str,
    /// The JSON schema of the answer, as text.
    schema: String,
    selection: Selection,
    model: String,
    investigate: bool,
}

/// Runs the request; the answer is the schema's object, read as `A`.
pub async fn generate<A: DeserializeOwned>(req: Request<'_, A>) -> Result<A, TextGenerationError> {
    let op = req.operation;
    let selection = Selection::of(&req.model);
    let instance = selection.instance.clone().unwrap_or_else(|| "codex".into());
    let model = selection.model.clone().filter(|m| !m.is_empty()).ok_or_else(|| error(op, "No text generation model is selected."))?;
    let _permit = gate().acquire().await.map_err(|_| error(op, "Text generation is shutting down."))?;
    let scratch = std::env::temp_dir().join(format!("domovoi-textgen-{}", crate::util::new_uuid()));
    std::fs::create_dir_all(&scratch).map_err(|e| error(op, format!("Could not create a working folder: {e}")))?;
    let cwd = req.cwd.map(std::path::PathBuf::from).unwrap_or_else(|| scratch.clone());
    let timeout = if req.investigate { INVESTIGATION_TIMEOUT } else { TIMEOUT };
    let run = Run { operation: op, prompt: &req.prompt.text, schema: req.prompt.schema.to_string(), selection, model, investigate: req.investigate };
    let result = match crate::orchestration::commands::driver_for_instance(&instance).as_str() {
        "claudeAgent" => run_claude(&run, &cwd, timeout).await,
        "codex" => run_codex(&run, &cwd, &scratch, timeout).await,
        "pi" => run_pi(&run, &cwd, timeout).await,
        "antigravity" => run_antigravity(&run, &instance, timeout).await,
        other => Err(error(op, format!("Text generation with {other} is not supported by this server."))),
    };
    let _ = std::fs::remove_dir_all(&scratch);
    // The model's answer is an object; each record takes from it the keys it knows.
    serde_json::from_value(result?).map_err(|e| error(op, format!("The model's answer could not be read: {e}")))
}

async fn run(op: &str, mut cmd: tokio::process::Command, stdin: &str, timeout: Duration) -> Result<(i32, String, String), TextGenerationError> {
    use tokio::io::AsyncWriteExt;
    cmd.stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).stderr(std::process::Stdio::piped()).kill_on_drop(true);
    let mut child = cmd.spawn().map_err(|e| error(op, format!("Could not start the CLI: {e}")))?;
    // A `.cmd` CLI's `node` ends with it too (a timeout, a dropped request).
    let _tree = crate::process::Tree::of(child.id());
    if let Some(mut s) = child.stdin.take() {
        let _ = s.write_all(stdin.as_bytes()).await;
    }
    let out = tokio::time::timeout(timeout, child.wait_with_output()).await
        .map_err(|_| error(op, format!("The model did not answer within {} seconds.", timeout.as_secs())))?
        .map_err(|e| error(op, e.to_string()))?;
    Ok((out.status.code().unwrap_or(-1), String::from_utf8_lossy(&out.stdout).into_owned(), String::from_utf8_lossy(&out.stderr).into_owned()))
}

/// The structured answer in `claude -p --output-format json` output: one
/// result object, or (older CLIs) the list of messages.
fn claude_structured(stdout: &str) -> Result<Value, String> {
    let v: Value = serde_json::from_str(stdout.trim()).map_err(|_| "Claude did not answer with JSON.".to_owned())?;
    let messages = match v {
        Value::Array(a) => a,
        other => vec![other],
    };
    if let Some(e) = messages.iter().rev().find(|m| m["is_error"] == true) {
        let detail = e["result"].as_str().map(str::to_owned)
            .or_else(|| e["errors"].as_array().map(|x| x.iter().filter_map(Value::as_str).filter(|s| !s.starts_with("[ede_diagnostic]")).collect::<Vec<_>>().join("\n")))
            .filter(|d| !d.trim().is_empty()).unwrap_or_else(|| "Claude reported an error.".into());
        return Err(detail);
    }
    if let Some(s) = messages.iter().rev().find_map(|m| m.get("structured_output").filter(|s| s.is_object())) {
        return Ok(s.clone());
    }
    // Without structured output, the reply text itself may be the JSON.
    messages.iter().rev().find_map(|m| m["result"].as_str()).and_then(parse_json_text).ok_or_else(|| "Claude's answer had no structured output.".into())
}

/// JSON in a reply, also inside a ```json fence.
fn parse_json_text(text: &str) -> Option<Value> {
    let t = text.trim();
    let t = t.strip_prefix("```json").or_else(|| t.strip_prefix("```")).map(|r| r.trim_end().trim_end_matches("```")).unwrap_or(t);
    serde_json::from_str::<Value>(t.trim()).ok().filter(Value::is_object)
}

/// The model's answer through the Claude CLI: the object its schema asks for.
async fn run_claude(req: &Run<'_>, cwd: &std::path::Path, timeout: Duration) -> Result<Value, TextGenerationError> {
    let op = req.operation;
    let bin = crate::providers::executable(crate::providers::CLAUDE_TOOL).map_err(|e| error(op, e))?;
    let mut cmd = crate::process::command(bin);
    cmd.current_dir(cwd).args(["-p", "--output-format", "json", "--json-schema", &req.schema, "--model", &req.model]);
    if let Some(e) = &req.selection.effort {
        cmd.args(["--effort", e]);
    }
    cmd.args(["--settings", r#"{"disableAllHooks":true}"#]);
    if req.investigate {
        cmd.args(["--tools", "Read,Grep,Glob,Bash", "--allowedTools", "Read,Grep,Glob,Bash(git diff:*),Bash(git log:*),Bash(git show:*)",
            "--disallowedTools", "Bash(git * --output*),Bash(git * -o *)", "--permission-mode", "dontAsk"]);
    } else {
        cmd.args(["--tools", ""]);
    }
    cmd.args(["--disable-slash-commands", "--no-session-persistence", "--strict-mcp-config"]);
    let (code, stdout, stderr) = run(op, cmd, req.prompt, timeout).await?;
    claude_structured(&stdout).map_err(|detail| {
        let detail = if code != 0 && detail.starts_with("Claude did not answer") && !stderr.trim().is_empty() { stderr.trim().to_owned() } else { detail };
        error(op, detail)
    })
}

/// The model's answer through the Codex CLI: the object its schema asks for.
async fn run_codex(req: &Run<'_>, cwd: &std::path::Path, scratch: &std::path::Path, timeout: Duration) -> Result<Value, TextGenerationError> {
    let op = req.operation;
    let bin = crate::providers::executable(crate::providers::CODEX_TOOL).map_err(|e| error(op, e))?;
    let schema_path = scratch.join("schema.json");
    let out_path = scratch.join("answer.json");
    std::fs::write(&schema_path, &req.schema).map_err(|e| error(op, e.to_string()))?;
    let effort = req.selection.effort.as_deref().unwrap_or("low");
    let mut cmd = crate::process::command(bin);
    cmd.current_dir(cwd).args(["exec", "--ephemeral", "--skip-git-repo-check", "-s", "read-only", "--model", &req.model,
        "--config", &format!("model_reasoning_effort=\"{effort}\""), "--output-schema"]).arg(&schema_path).arg("--output-last-message").arg(&out_path).arg("-");
    let (code, _stdout, stderr) = run(op, cmd, req.prompt, timeout).await?;
    let answer = std::fs::read_to_string(&out_path).ok().and_then(|t| parse_json_text(&t));
    answer.ok_or_else(|| {
        let tail: String = stderr.lines().rev().take(5).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n");
        error(op, if code != 0 && !tail.trim().is_empty() { tail } else { "Codex gave no answer.".into() })
    })
}

/// pi's `errorMessage` is the provider's raw answer and can echo
/// credentials: only a leading HTTP status is shown.
fn pi_error_detail(message: Option<&str>) -> String {
    let status = message.and_then(|m| m.get(..3)).filter(|s| s.bytes().all(|b| b.is_ascii_digit()) && matches!(s.as_bytes()[0], b'4' | b'5'));
    let after = message.and_then(|m| m.as_bytes().get(3)).is_none_or(|b| !b.is_ascii_alphanumeric());
    match status.filter(|_| after) {
        Some(s) => format!("Pi could not generate text: the model provider returned HTTP {s}."),
        None => "Pi could not generate text: the model provider reported an error.".into(),
    }
}

/// The model's answer through a short-lived `pi --mode rpc` with no tools and
/// no extensions (a helper must never change the workspace): the reply to
/// the prompt, which asks for the schema's object.
async fn run_pi(req: &Run<'_>, cwd: &std::path::Path, timeout: Duration) -> Result<Value, TextGenerationError> {
    use crate::pi_rpc::{DEFAULT_TIMEOUT, Launch, PiEvent, PiRpc};
    let op = req.operation;
    let exe = crate::providers::executable(crate::pi_rpc::PI_TOOL).map_err(|e| error(op, e))?;
    let user_args = crate::pi_rpc::user_launch_args().map_err(|e| error(op, e))?;
    let launch = Launch { ephemeral: true, short_lived: true, no_extensions: true, no_tools: true, user_args, ..Launch::default() };
    let (rpc, mut events) = PiRpc::spawn(&exe, &launch, &cwd.to_string_lossy(), &crate::pi_rpc::environment("approval-required", None), |_, _| {})
        .map_err(|e| error(op, format!("Could not start pi: {e}")))?;
    let answer = tokio::time::timeout(timeout, async {
        if req.model != crate::providers::PI_DEFAULT_MODEL {
            let models = rpc.request(json!({ "type": "get_available_models" }), DEFAULT_TIMEOUT).await.ok().and_then(|d| d["models"].as_array().cloned()).unwrap_or_default();
            let (provider, id) = crate::pi_rpc::resolve_model_slug(&req.model, &models).ok_or_else(|| format!("Pi model '{}' must use provider/model format.", req.model))?;
            rpc.request(json!({ "type": "set_model", "provider": provider, "modelId": id }), DEFAULT_TIMEOUT).await.map_err(|e| {
                format!("Pi rejected model '{}': {e}. A model from a pi extension or package can't generate text here (pi runs without extensions for it): pick a built-in pi model or one in pi's models.json.", req.model)
            })?;
        }
        let prompt = format!("{}\n\nAnswer with only a JSON object (no prose, no code fence) that matches this JSON schema:\n{}", req.prompt, req.schema);
        rpc.request(json!({ "type": "prompt", "message": prompt }), DEFAULT_TIMEOUT).await?;
        // A failed model call ends on the assistant's `message_end`; pi may retry and still answer, so the latest counts.
        let mut failure = None;
        loop {
            match events.recv().await {
                Some(PiEvent::Record(r)) if r["type"] == "agent_settled" => break,
                Some(PiEvent::Record(r)) if r["type"] == "message_end" && r["message"]["role"] == "assistant" => {
                    failure = (r["message"]["stopReason"] == "error").then(|| pi_error_detail(r["message"]["errorMessage"].as_str()));
                }
                Some(PiEvent::Record(_)) => {}
                Some(PiEvent::Exited(_)) | None => return Err("Pi exited before answering.".to_owned()),
            }
        }
        let text = rpc.request(json!({ "type": "get_last_assistant_text" }), DEFAULT_TIMEOUT).await?["text"].as_str().unwrap_or_default().trim().to_owned();
        if text.is_empty() {
            return Err(failure.unwrap_or_else(|| "Pi returned empty output.".into()));
        }
        parse_json_text(&text).ok_or_else(|| "Pi's answer was not the JSON object asked for.".to_owned())
    })
    .await;
    rpc.terminate().await;
    match answer {
        Ok(result) => result.map_err(|detail| error(op, detail)),
        Err(_) => Err(error(op, format!("The model did not answer within {} seconds.", timeout.as_secs()))),
    }
}

// ---- prompts (t3code textGenerationPrompts.ts) ----

fn limit(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut cut = max;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    format!("{}\n[truncated]", &text[..cut])
}

fn instructions(extra: Option<&str>) -> Vec<String> {
    match extra.map(str::trim).filter(|s| !s.is_empty()) {
        Some(e) => vec![String::new(), "Additional instructions:".into(), limit(e, 20_000)],
        None => vec![],
    }
}

/// `buildCommitMessagePrompt`: `{subject, body}` (and `branch` when asked).
pub fn commit_message_prompt(branch: Option<&str>, staged_summary: &str, staged_patch: &str, with_branch: bool, extra: Option<&str>) -> Prompt<CommitMessage> {
    let mut lines = vec![
        "You write concise git commit messages.".to_owned(),
        if with_branch { "Return a JSON object with keys: subject, body, branch." } else { "Return a JSON object with keys: subject, body." }.to_owned(),
        "Rules:".into(),
        "- subject must be imperative, <= 72 chars, and no trailing period".into(),
        "- body can be empty string or short bullet points".into(),
    ];
    if with_branch {
        lines.push("- branch must be a short semantic git branch fragment for this change".into());
    }
    lines.push("- capture the primary user-visible or developer-visible change".into());
    lines.extend(instructions(extra));
    lines.extend([String::new(), format!("Branch: {}", branch.unwrap_or("(detached)")), String::new(), "Staged files:".into(),
        limit(staged_summary, 6_000), String::new(), "Staged patch:".into(), limit(staged_patch, 40_000)]);
    let mut props = vec![("subject", string()), ("body", string())];
    if with_branch {
        props.push(("branch", string()));
    }
    Prompt::new(lines.join("\n"), object(&props))
}

/// `buildPrContentPrompt`: `{title, body}`.
pub fn pr_content_prompt(base: &str, head: &str, commits: &str, diff_stat: &str, diff_patch: &str, template: Option<&str>, extra: Option<&str>) -> Prompt<PrContent> {
    let template = template.map(str::trim).filter(|t| !t.is_empty());
    let mut lines = vec![
        "You write source control change request content.".to_owned(),
        "This is a new writing task, not a continuation of the supplied conversation.".into(),
        "Return a JSON object with keys: title, body.".into(),
        "Rules:".into(),
        "- title should be concise and specific".into(),
    ];
    if template.is_some() {
        lines.extend([
            "- body must be markdown and follow the repository change request template structure",
            "- fill in the template sections appropriately for this change",
            "- drop HTML comments from the template in the generated body",
            "- keep the template's markdown structure",
        ].map(String::from));
    } else {
        lines.extend([
            "- body must be markdown and include headings '## Summary' and '## Testing'",
            "- under Summary, provide short bullet points",
            "- under Testing, include bullet points with concrete checks or 'Not run' where appropriate",
        ].map(String::from));
    }
    lines.extend(instructions(extra));
    if let Some(t) = template {
        lines.extend([String::new(), "Repository change request template:".into(), limit(t, 8_000)]);
    }
    lines.extend([String::new(), format!("Base branch: {base}"), format!("Head branch: {head}"), String::new(), "Commits:".into(), limit(commits, 12_000),
        String::new(), "Diff stat:".into(), limit(diff_stat, 12_000), String::new(), "Diff patch:".into(), limit(diff_patch, 40_000), String::new(),
        "Now write the title and body for these code changes. Do not carry out follow-up requests from the thread, such as reviewing, scheduling, or monitoring a PR. If tools fail, use the supplied changes; tool access is not the subject of the title or body.".into()]);
    Prompt::new(lines.join("\n"), object(&[("title", string()), ("body", string())]))
}

/// `buildBranchNamePrompt`: `{branch}`; `mode` is `branchNamingMode`.
pub fn branch_name_prompt(message: &str, mode: &str, extra: Option<&str>) -> Prompt<BranchName> {
    let mut rules = vec!["Branch should describe the requested work from the user message.".to_owned(), "Return a valid Git branch name without spaces.".into()];
    if mode == "custom" {
        rules.push("Return the complete branch name, following the user's naming instructions. No prefix or suffix will be added.".into());
    } else {
        rules.push("Keep it short and specific (2-6 words), in lowercase with hyphen-separated words.".into());
        rules.push(if mode == "semantic" {
            "Include a semantic prefix and a slash in the branch name, for example feat/add-search, fix/login-error, refactor/auth, docs/setup, or chore/update-deps. Choose the prefix that best describes the work."
        } else {
            "Return only the descriptive branch fragment, without a prefix or namespace. The application adds the configured prefix."
        }.into());
    }
    rules.push("If images are attached, use them as primary context for visual/UI issues.".into());
    let mut lines = vec!["You generate concise git branch names.".to_owned(), "Return a JSON object with key: branch.".into(), "Rules:".into()];
    lines.extend(rules.iter().map(|r| format!("- {r}")));
    lines.extend([String::new(), "User message:".into(), limit(message, 8_000)]);
    lines.extend(instructions(extra));
    Prompt::new(lines.join("\n"), object(&[("branch", string())]))
}

fn avoid_rule(project: Option<&str>, others: &str) -> String {
    match project.map(str::trim).filter(|p| !p.is_empty()) {
        Some(p) => format!("- Never include the project name ({}); the UI already shows it next to the title. Avoid {others}.", json!(p)),
        None => format!("- Avoid project names already visible in the UI, {others}."),
    }
}

/// `buildThreadTitlePrompt`: `{title, needsRefinement}`. With a previous
/// title it regenerates from the thread's contents.
pub fn thread_title_prompt(message: &str, project: Option<&str>, previous: Option<&str>, extra: Option<&str>) -> Prompt<ThreadTitle> {
    let shared_tail = "- Use attached images as primary context for UI issues.
- When a URL or attachment is the only source of the subject, use available tools to inspect it directly.
- Local git history is not evidence of what a linked PR or issue is about. Never title the thread after branch names, commit messages, or merged commits found in the checkout.
- If a linked PR or issue cannot be read, fall back to the user's stated action plus its number, such as \"Take Over PR 8588\". This is the one case where a PR or issue number belongs in the title.";
    let body = match previous {
        None => format!("Generate a title that will help the user recognize this Domovoi thread weeks later.
Return JSON with keys title and needsRefinement.
Set needsRefinement to true only if the subject is still unknown, such as an unresolved link, \"fix this\", or an unexplained attachment. Otherwise set it to false.

Before answering, silently reduce the request to:
- Subject: What system, feature, or problem is this really about?
- Outcome: What does the user ultimately want to understand or change?
- Incidental instructions: What only describes how the agent should do the work?

Title the subject and outcome. Discard incidental instructions.

Editorial rules:
- 3-8 words, fewer than 40 characters.
- Use a compact noun phrase or clear action phrase.
- Capture the umbrella goal when the request lists several symptoms or steps.
- Name the product change, not the mock, plan, report, branch, or PR used to produce it.
- Models, subagents, tools, output formats, and monitoring instructions do not belong in the title unless they are themselves the topic.
- For reviews, name what is being reviewed and the relevant concern.
- For research, name the question domain rather than the requested research process.
- Do not claim the work is complete.
- Do not copy and truncate the user's message.
{}
{shared_tail}

User message:
{}", avoid_rule(project, "quotes, labels, filler, and trailing punctuation"), limit(message, 8_000)),
        Some(prev) => format!("Regenerate the title for an existing Domovoi thread so the user can recognize it weeks later.
The previous title was {}.
Return JSON with keys title and needsRefinement. Set needsRefinement to false.

Determine the title in this order:
1. Read the USER messages first. Identify the latest explicit durable goal. The original subject remains the subject until the user clearly changes what the thread is about.
2. Use ASSISTANT messages to resolve vague links, unnamed code, and discovered product nouns. Do not promote one assistant finding into the thread subject unless the user adopts it as a new goal.
3. Compare that subject with the previous title. Preserve accurate scope words. Replace the previous title when it is generic, artifact-based, a completion update, or contradicted by the thread.
4. Title the durable subject and desired outcome, not the current workflow state.

Editorial rules:
- 3-8 words, fewer than 40 characters.
- Use a compact noun phrase or clear action phrase.
- Preserve the umbrella subject when later messages focus on one finding, provider, platform, or implementation detail.
- Ignore deliverables and operations such as mocks, plans, HTML, branches, PRs, tests, CI, commits, merging, and monitoring unless they are the actual topic.
- Do not claim the work is complete.
- Do not copy and truncate a thread message.
{}
{shared_tail}
- Keep the previous title unchanged if it is already accurate. Otherwise return a meaningfully improved title, not a cosmetic paraphrase.

Thread contents:
{}", json!(prev), avoid_rule(project, "PR numbers, quotes, labels, filler, and trailing punctuation"), tail(message, 8_000)),
    };
    let mut prompt = body;
    for l in instructions(extra) {
        prompt.push('\n');
        prompt.push_str(&l);
    }
    Prompt::new(prompt, object(&[("title", string()), ("needsRefinement", json!({ "type": "boolean" }))]))
}

/// The end of a long text, marked as cut.
fn tail(text: &str, max: usize) -> String {
    if text.len() <= max {
        return text.to_owned();
    }
    let mut start = text.len() - max;
    while !text.is_char_boundary(start) {
        start += 1;
    }
    format!("[Earlier content truncated]\n\n{}", &text[start..])
}

/// A title as t3code keeps it: one line, no wrapping quotes or trailing
/// period, at most 80 characters.
pub fn clean_title(raw: &str) -> Option<String> {
    let line = raw.lines().map(str::trim).find(|l| !l.is_empty())?;
    let t = line.trim_matches(|c| matches!(c, '"' | '\'' | '`' | '*')).trim().trim_end_matches('.').trim();
    let t: String = t.chars().take(80).collect();
    (!t.is_empty()).then_some(t)
}

/// A branch name from the model, made valid for git (`sanitizeBranchFragment`-like).
pub fn clean_branch(raw: &str) -> Option<String> {
    let lowered = raw.trim().to_lowercase();
    let mut out = String::new();
    for c in lowered.chars() {
        let c = if c.is_ascii_alphanumeric() || matches!(c, '/' | '-' | '_' | '.') { c } else { '-' };
        if (c == '-' || c == '/') && out.ends_with(['-', '/']) {
            continue;
        }
        out.push(c);
    }
    let out = out.trim_matches(|c| matches!(c, '-' | '/' | '.')).replace("..", ".");
    let out: String = out.chars().take(60).collect::<String>().trim_end_matches(['-', '/', '.']).to_owned();
    (!out.is_empty() && !out.ends_with(".lock")).then_some(out)
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_pi_provider_error_shows_only_its_http_status() {
        assert_eq!(super::pi_error_detail(Some("429 {\"error\": \"rate limited for key sk-123\"}")), "Pi could not generate text: the model provider returned HTTP 429.");
        assert!(!super::pi_error_detail(Some("invalid key sk-123")).contains("sk-"));
        assert!(super::pi_error_detail(Some("4291 tokens")).ends_with("reported an error."));
        assert!(super::pi_error_detail(None).ends_with("reported an error."));
    }

    use super::*;

    #[test]
    fn a_failed_generation_is_written_as_the_tagged_error_of_t3code() {
        assert_eq!(
            json!(error("generateCommitMessage", "Codex gave no answer.")),
            json!({ "_tag": "TextGenerationError", "operation": "generateCommitMessage", "detail": "Codex gave no answer." })
        );
    }

    #[test]
    fn answers_keep_the_keys_that_are_text() {
        let answer: CommitMessage = serde_json::from_value(json!({ "subject": "Add x", "body": 7, "extra": true })).unwrap();
        assert_eq!(answer, CommitMessage { subject: Some("Add x".into()), body: None, branch: None });
        let answer: PrContent = serde_json::from_value(json!({ "title": null })).unwrap();
        assert_eq!(answer, PrContent::default());
        let answer: ThreadTitle = serde_json::from_value(json!({ "title": "T", "needsRefinement": false })).unwrap();
        assert_eq!(answer.title.as_deref(), Some("T"));
        let of = |selection: Value| Selection::of(&serde_json::from_value(selection).unwrap());
        let selection = of(json!({ "instanceId": "codex", "model": "m", "options": [{ "id": "effort", "value": true }, { "id": "reasoningEffort", "value": "high" }] }));
        assert_eq!((selection.instance.as_deref(), selection.model.as_deref()), (Some("codex"), Some("m")));
        assert_eq!(selection.effort, None, "the first effort option decides, and its value is no text");
        let selection = of(json!({ "options": [{ "id": "other", "value": "x" }, { "id": "reasoningEffort", "value": "high" }, { "id": "effort", "value": "low" }] }));
        assert_eq!(selection.effort.as_deref(), Some("high"));
        assert_eq!(of(json!({})).instance, None);
    }

    #[test]
    fn claude_answers_are_read_from_structured_output_or_reply_json() {
        assert_eq!(claude_structured(r#"{"type":"result","is_error":false,"structured_output":{"title":"A"}}"#).unwrap()["title"], "A");
        let list = r#"[{"type":"system"},{"type":"result","structured_output":{"title":"B"}}]"#;
        assert_eq!(claude_structured(list).unwrap()["title"], "B");
        assert_eq!(claude_structured(r#"{"type":"result","result":"```json\n{\"title\":\"C\"}\n```"}"#).unwrap()["title"], "C");
        assert_eq!(claude_structured(r#"{"type":"result","is_error":true,"result":"Credit balance is too low"}"#).unwrap_err(), "Credit balance is too low");
        assert!(claude_structured("not json").is_err());
    }

    #[test]
    fn titles_and_branches_are_cleaned_up() {
        assert_eq!(clean_title("\"Fix Login Redirect.\"\n").as_deref(), Some("Fix Login Redirect"));
        assert_eq!(clean_title("  \n"), None);
        assert_eq!(clean_branch("Feat/Add Search Bar!").as_deref(), Some("feat/add-search-bar"));
        assert_eq!(clean_branch("--//x..y.lock"), None);
        assert_eq!(clean_branch("fix: login -- error").as_deref(), Some("fix-login-error"));
    }

    #[test]
    fn prompts_ask_for_their_keys_and_models_follow_the_settings() {
        let prompt = commit_message_prompt(Some("main"), "a.rs | 2", "+x", true, Some("Use emoji"));
        let p = &prompt.text;
        assert!(p.contains("keys: subject, body, branch") && p.contains("Branch: main") && p.contains("Use emoji"));
        assert_eq!(prompt.schema["required"], json!(["subject", "body", "branch"]));
        let prompt = thread_title_prompt("hello", Some("t5"), Some("Old"), None);
        assert!(prompt.text.contains("The previous title was \"Old\"") && prompt.text.contains("(\"t5\")"));
        let prompt = branch_name_prompt("add search", "static", None);
        assert!(prompt.text.contains("without a prefix"));
        let settings: ServerSettings = serde_json::from_value(json!({ "textGenerationModelSelection": { "instanceId": "codex", "model": "m" }, "sourceControlWriterModelSelection": { "instanceId": "claudeAgent", "model": "h" }, "reviewGuideModelSelection": null })).unwrap();
        assert_eq!(model_for(&settings, Writer::SourceControl).model.as_deref(), Some("h"));
        assert_eq!(model_for(&settings, Writer::ReviewGuide).model.as_deref(), Some("m"));
        assert_eq!(model_for(&settings, Writer::Text).model.as_deref(), Some("m"));
        let unset: ServerSettings = serde_json::from_value(json!({})).unwrap();
        assert_eq!(model_for(&unset, Writer::Text).model, None);
    }
}

/// How long Antigravity may take to write an answer.
const ANTIGRAVITY_TIMEOUT: Duration = Duration::from_secs(180);
const ANTIGRAVITY_MAX_OUTPUT: usize = 128_000;

/// Whether Antigravity instance `id` can generate text (t3code
/// `isAntigravityTextGenerationAvailable`): not when its profile has hooks or
/// MCP servers, which could run before a tool request can be refused.
pub fn antigravity_available(id: &str) -> bool {
    let config = crate::antigravity::profile(id).home.join("config");
    ["hooks.json", "mcp_config.json"].iter().all(|name| {
        let path = config.join(name);
        let Ok(meta) = std::fs::metadata(&path) else { return true };
        if !meta.is_file() || meta.len() > 64_000 {
            return false;
        }
        let key = if *name == "hooks.json" { "hooks" } else { "mcpServers" };
        let parsed = std::fs::read_to_string(&path).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok());
        parsed.as_ref().and_then(|v| v.get(key).unwrap_or(v).as_object()).is_some_and(serde_json::Map::is_empty)
    })
}

/// Whether a scratch folder holds only the empty skill folders the agent makes itself.
fn only_runtime_folders(root: &std::path::Path) -> bool {
    let Ok(root) = std::fs::canonicalize(root) else { return false };
    let allowed = [root.join(".agents"), root.join(".agents/skills"), root.join(".gemini"), root.join(".gemini/skills")];
    let mut dirs = vec![root];
    while let Some(dir) = dirs.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else { return false };
        for entry in entries.flatten() {
            let path = entry.path();
            let real_dir = std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_dir());
            if !allowed.contains(&path) || !real_dir {
                return false;
            }
            dirs.push(path);
        }
    }
    true
}

/// Antigravity's writer (t3code `AntigravityTextGeneration`): a fresh agent
/// in an empty folder, told to use only its input; any tool call, request
/// or file it writes fails the answer.
async fn run_antigravity(req: &Run<'_>, instance: &str, timeout: Duration) -> Result<Value, TextGenerationError> {
    use crate::antigravity::{self as ag, launch};
    use crate::jsonrpc::Incoming;
    let op = req.operation;
    if !antigravity_available(instance) {
        return Err(error(op, "Antigravity text generation is unavailable for profiles with global hooks or MCP configuration. Select another system model."));
    }
    let scratch = std::env::temp_dir().join(format!("domovoi-antigravity-text-{}", crate::util::new_uuid()));
    std::fs::create_dir_all(&scratch).map_err(|e| error(op, format!("Could not create a working folder: {e}")))?;
    let cwd = scratch.to_string_lossy().into_owned();
    let mut l = launch::start(instance, &cwd, vec![], |_, _| {}).await.map_err(|e| error(op, e))?;
    let mut session_id = None;
    let answer = tokio::time::timeout(timeout.min(ANTIGRAVITY_TIMEOUT), async {
        l.authenticate().await.map_err(|e| e.message())?;
        let session = l.rpc.request("session/new", json!({ "cwd": cwd, "mcpServers": [] })).await.map_err(|e| if launch::sign_in_required(&e) { ag::SIGN_IN_REQUIRED.to_owned() } else { "Antigravity text generation failed.".to_owned() })?;
        let id = session["sessionId"].as_str().filter(|id| uuid::Uuid::parse_str(id).is_ok_and(|u| u.get_version_num() == 4)).ok_or("Antigravity returned an invalid text helper session ID.")?.to_owned();
        session_id = Some(id.clone());
        let options = session["configOptions"].clone();
        if let Some(mode) = options.as_array().and_then(|o| o.iter().find(|o| o["category"] == "mode" && o["type"] == "select")).filter(|m| m["currentValue"] != "default") {
            let _ = l.rpc.request("session/set_config_option", json!({ "sessionId": id, "configId": mode["id"], "value": "default" })).await;
        } else if session["modes"]["currentModeId"].as_str().is_some_and(|m| m != "default") {
            let _ = l.rpc.request("session/set_mode", json!({ "sessionId": id, "modeId": "default" })).await;
        }
        match ag::acp::choose_model(&options, &req.model, Some(ag::DEFAULT_CHAT_MODEL)) {
            ag::acp::ModelChoice::Keep => {}
            ag::acp::ModelChoice::Set(model) | ag::acp::ModelChoice::SetUnlisted(model) => {
                let config_id = ag::acp::model_option(&options).and_then(|o| o["id"].as_str()).unwrap_or("model").to_owned();
                l.rpc.request("session/set_config_option", json!({ "sessionId": id, "configId": config_id, "value": model })).await.map_err(|_| "Could not select the Antigravity model for text generation.".to_owned())?;
            }
            ag::acp::ModelChoice::Unavailable(_) => return Err("Could not select the Antigravity model for text generation.".to_owned()),
        }
        let prompt = format!(
            "Use only the input below. Do not use tools, read or write files, run commands, or ask questions.\nReturn only the requested JSON object.\n\n{}\n\nAnswer with only a JSON object (no prose, no code fence) that matches this JSON schema:\n{}",
            req.prompt, req.schema
        );
        let rpc = l.rpc.clone();
        let (done_tx, mut done) = tokio::sync::oneshot::channel();
        let prompt_id = id.clone();
        tokio::spawn(async move {
            let _ = done_tx.send(rpc.request("session/prompt", json!({ "sessionId": prompt_id, "prompt": [{ "type": "text", "text": prompt }] })).await);
        });
        let mut text = String::new();
        let mut rejected: Option<String> = None;
        // What the agent sends before its answer; `false` when it exited.
        let rpc = l.rpc.clone();
        let take = |ev: Incoming, text: &mut String, rejected: &mut Option<String>| -> Result<Option<(Value, bool)>, ()> {
            match ev {
                Incoming::Closed => Err(()),
                Incoming::Notification { method, params } if method == "session/update" => {
                    let u = &params["update"];
                    match u["sessionUpdate"].as_str() {
                        Some("tool_call" | "tool_call_update") => *rejected = rejected.take().or(Some("Antigravity attempted tool work during text generation.".into())),
                        Some("agent_message_chunk") if params["sessionId"] == id.as_str() && u["content"]["type"] == "text" => {
                            let chunk = u["content"]["text"].as_str().unwrap_or_default();
                            if text.len() + chunk.len() > ANTIGRAVITY_MAX_OUTPUT {
                                *rejected = rejected.take().or(Some("Antigravity text generation exceeded the output limit.".into()));
                            } else {
                                text.push_str(chunk);
                            }
                        }
                        _ => {}
                    }
                    Ok(None)
                }
                Incoming::Notification { .. } => Ok(None),
                Incoming::Request { id: rid, method, .. } => {
                    *rejected = rejected.take().or(Some("Antigravity text generation requested a tool or user input.".into()));
                    Ok(Some((rid, method == "session/request_permission")))
                }
            }
        };
        let refuse = |(rid, permission): (Value, bool)| {
            let rpc = rpc.clone();
            async move {
                if permission {
                    let _ = rpc.respond(rid, json!({ "outcome": { "outcome": "cancelled" } })).await;
                } else {
                    let _ = rpc.respond_error(rid, -32601, "Tools and user input are disabled for text generation.").await;
                }
            }
        };
        let answer = loop {
            tokio::select! {
                answer = &mut done => {
                    // The reader passed on what came before the answer first: take it in.
                    while let Ok(ev) = l.events.try_recv() {
                        if let Ok(Some(request)) = take(ev, &mut text, &mut rejected) {
                            refuse(request).await;
                        }
                    }
                    break answer;
                }
                ev = l.events.recv() => match ev.map(|ev| take(ev, &mut text, &mut rejected)) {
                    None | Some(Err(())) => return Err("Antigravity exited before answering.".to_owned()),
                    Some(Ok(Some(request))) => refuse(request).await,
                    Some(Ok(None)) => {}
                },
            }
        };
        if let Some(reason) = rejected {
            return Err(reason);
        }
        match answer {
            Ok(Ok(r)) if r["stopReason"] == "cancelled" => Err("Antigravity text generation was cancelled.".to_owned()),
            Ok(Ok(_)) => Ok(text.trim().to_owned()),
            Ok(Err(e)) if launch::sign_in_required(&e) => Err(ag::SIGN_IN_REQUIRED.to_owned()),
            _ => Err("Antigravity text generation failed.".to_owned()),
        }
    })
    .await;
    l.kill().await;
    ag::remove_session_files(&l.profile, session_id.as_deref(), &cwd);
    let clean = only_runtime_folders(&scratch);
    let _ = std::fs::remove_dir_all(&scratch);
    let text = match answer {
        Ok(Ok(text)) => text,
        Ok(Err(detail)) => return Err(error(op, detail)),
        Err(_) => return Err(error(op, "Antigravity text generation timed out.")),
    };
    if !clean {
        return Err(error(op, "Antigravity wrote files during text generation."));
    }
    if text.is_empty() {
        return Err(error(op, "Antigravity returned empty text generation output."));
    }
    parse_json_text(&text).ok_or_else(|| error(op, "Antigravity's answer was not the JSON object asked for."))
}
