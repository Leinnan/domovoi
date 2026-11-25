//! OpenCode adapter over `opencode serve` (the v2 HTTP API, t3code
//! `provider-opencode` v2). One server, started on loopback with a
//! generated password, serves every thread; its `/api/event` stream is
//! routed by session (a session's child sessions go to its turn too). A
//! thread is an OpenCode session in the thread's folder; a turn is a prompt
//! (whose message id Domovoi picks, `msg_t3_turn_…`, so it marks where a fork
//! or rollback cuts), ended by `session.execution.*`.
//!
//! Permissions follow the runtime mode as t3code sets them (full access
//! allows everything; otherwise `shell` and `external_directory` ask, and
//! `edit` asks unless edits are auto-accepted); asked permissions become
//! approvals (declined with a note, so the agent goes on without it; cancel
//! stops it). Forms (the `question` tool) become `user_input` requests;
//! dismissing one cancels the form, and a form Domovoi can't show is
//! cancelled with a note to ask in chat. Models are `providerID/modelID` slugs.
//!
//! Domovoi's MCP tools: each thread's MCP server (`t3-code-<thread>`, the
//! session's bearer token) is added to OpenCode's location for the thread's
//! folder, and the session's rules allow only that thread's server; the
//! `agent_instructions` sections are the session's `t3-code` instruction
//! entry. A forked thread's first turn forks the source session before the
//! source's next turn (`/fork`), and a rollback reverts the session to before
//! the first undone turn (`/revert/stage`, then `/revert/commit`; files are
//! left to checkpoints). A `subagent` tool call is child work
//! (`subagents.rs`): its child session's text and tools go to the child
//! thread, its questions and approvals to the parent's turn, and stopping it
//! interrupts the child session. Workspace usage limits (OpenCode Console Go
//! and spend limits) are read from `/api/experimental/usage/limits`.
//!
//! Limits: a background subagent keeps the turn open until its report comes
//! back (or the turn is stopped); a report that arrives after the turn ended
//! goes unrecorded. Sessions a subagent starts in turn show in the same
//! child thread. The MCP server added to a location stays there until
//! OpenCode restarts (a thread whose folder changes gets it added in the new
//! one, and removed from the old). Steered prompts are not marked, so a fork
//! or rollback cuts only at turn boundaries.

#![expect(clippy::disallowed_types, clippy::disallowed_macros, reason = "OpenCode's HTTP bodies and SSE events")]

use super::subagents::Subagents;
use super::turn::{Turn, TurnEnd, WindowUse, Work, approval_kind};
use super::{Control, TurnInput};
use domovoi_model::typed::{
    FileChangeDetail, ModelCapabilities, ProviderSessionStatus, ProviderThreadStatus, QuestionOption, ServerProviderExternalUsage, ServerProviderModel,
    ServerProviderUsageLimits, ServerProviderUsageUnavailable, ServerProviderUsageUnavailableReason, ServerProviderUsageWindow, ServerProviderUsageWindowKind,
    Timestamp, TurnItemStatus, UserInputQuestion,
};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, mpsc};

pub const OPENCODE_TOOL: crate::providers::Tool = crate::providers::Tool { program: "opencode", label: "OpenCode", env: "DOMOVOI_OPENCODE_BINARY" };

/// A permission declined with this message lets the agent go on; without
/// one OpenCode stops the execution (Cancel).
const DECLINED: &str = "The user declined this request.";
/// A dismissed subagent question: without a message OpenCode aborts the
/// step, and a background subagent never reports.
const DISMISSED: &str = "The user dismissed this question. Continue without an answer.";
/// The session's instruction entry Domovoi writes.
const INSTRUCTIONS_KEY: &str = "t3-code";
/// OpenCode before 2.2.6 loads a new MCP server's tools 100 ms after it
/// connects: a prompt sent sooner lacks them.
const MCP_TOOL_SETTLE: Duration = Duration::from_millis(250);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a turn whose background subagents all reported waits for
/// OpenCode to wake the session with their reports.
const WAKE_GRACE: Duration = Duration::from_secs(5);
/// Events of a child session kept until its subagent call names it.
const STRAY_EVENTS_MAX: usize = 256;

pub struct OpenCodeServer {
    base: String,
    auth: String,
    http: reqwest::Client,
    routes: Arc<Mutex<HashMap<String, mpsc::UnboundedSender<Value>>>>,
    child: Mutex<tokio::process::Child>,
    /// The folder each thread's MCP server was added for, by thread.
    mcp: Mutex<HashMap<String, String>>,
    /// The instruction entry each session holds, by session.
    instructed: Mutex<HashMap<String, String>>,
}

/// The session's permission rules for a runtime mode; with `mcp` (the
/// thread's MCP server), every thread's T3 tools denied but its own (the
/// last rule that matches wins).
pub fn rules(runtime_mode: &str, mcp: Option<&str>) -> Value {
    let mut rules = if runtime_mode == "full-access" {
        vec![json!({ "action": "*", "resource": "*", "effect": "allow" })]
    } else {
        let edit = if matches!(runtime_mode, "auto-accept-edits" | "auto") { "allow" } else { "ask" };
        vec![
            json!({ "action": "shell", "resource": "*", "effect": "ask" }),
            json!({ "action": "edit", "resource": "*", "effect": edit }),
            json!({ "action": "external_directory", "resource": "*", "effect": "ask" }),
        ]
    };
    if let Some(name) = mcp {
        rules.push(json!({ "action": "t3-code-*", "resource": "*", "effect": "deny" }));
        rules.push(json!({ "action": format!("{name}_*"), "resource": "*", "effect": "allow" }));
    }
    Value::Array(rules)
}

/// The thread's MCP server name (t3code `t3McpServerName`). OpenCode hides
/// tools whose names pass 64 characters, so long thread ids are hashed.
pub fn mcp_server_name(thread_id: &str) -> String {
    let safe: String = thread_id.encode_utf16().map(|unit| match char::from_u32(u32::from(unit)) {
        Some(c) if c.is_ascii_alphanumeric() || c == '_' || c == '-' => c,
        _ => '_',
    }).collect();
    let name = format!("t3-code-{safe}");
    if name.len() <= 64 {
        return name;
    }
    use sha2::Digest as _;
    let digest = hex::encode(sha2::Sha256::digest(thread_id.as_bytes()));
    format!("t3-code-{}", &digest[..16])
}

/// `providerID/modelID` as OpenCode's model ref.
pub fn model_ref(slug: &str) -> Option<Value> {
    let (provider, model) = slug.split_once('/')?;
    (!provider.is_empty() && !model.is_empty()).then(|| json!({ "providerID": provider, "id": model }))
}

/// The approval kind of an asked permission.
pub fn request_kind(action: &str) -> &'static str {
    match action {
        "shell" => "command",
        "edit" | "write" | "patch" => "file-change",
        "read" | "external_directory" => "file-read",
        _ => "permission",
    }
}

/// Whether an execution's error is a usage or rate limit (t3code `isUsageLimit`;
/// a 402 quota is a billing problem, not a limit that resets).
pub fn is_usage_limit(error: &Value) -> bool {
    (error["type"] == "provider.quota" && error["status"].as_u64() != Some(402)) || error["type"] == "provider.rate-limit"
}

/// A subagent's answer without the `<subagent …>` envelope it comes in.
pub fn subagent_output(text: &str) -> String {
    let t = text.trim();
    let inner = t.strip_prefix("<subagent").filter(|rest| rest.starts_with(|c: char| c == '>' || c.is_whitespace())).and_then(|rest| {
        let open = rest.find('>')?;
        rest[open + 1..].strip_suffix("</subagent>")
    });
    match inner {
        Some(inner) => {
            let inner = inner.strip_prefix('\n').unwrap_or(inner);
            inner.strip_suffix('\n').unwrap_or(inner).to_owned()
        }
        None => text.to_owned(),
    }
}

/// `opencode serve` under a small watchdog shell (Unix): it stops the
/// server when Domovoi is gone however Domovoi ended (`opencode serve` doesn't watch
/// its stdin), and when stopped itself (SIGTERM).
fn serve_command(exe: &std::path::Path, port: u16) -> tokio::process::Command {
    if cfg!(unix) {
        let mut c = crate::process::command("/bin/sh");
        c.args(["-c", WATCHDOG, "opencode-watchdog"]).arg(exe).arg(port.to_string()).arg(std::process::id().to_string());
        c
    } else {
        let mut c = crate::process::command(exe);
        c.args(["serve", "--hostname", "127.0.0.1", "--port", &port.to_string()]);
        c
    }
}

/// $1 opencode, $2 port, $3 Domovoi's pid.
const WATCHDOG: &str = r#""$1" serve --hostname 127.0.0.1 --port "$2" & c=$!
trap 'kill $c 2>/dev/null; exit 0' TERM INT HUP
while kill -0 "$3" 2>/dev/null && kill -0 $c 2>/dev/null; do sleep 1; done
kill $c 2>/dev/null
wait $c"#;

fn basic(password: &str) -> String {
    use base64::Engine as _;
    format!("Basic {}", base64::engine::general_purpose::STANDARD.encode(format!("opencode:{password}")))
}

/// Complete SSE `data:` payloads in `buf` (consumed), as JSON.
pub fn take_events(buf: &mut String) -> Vec<Value> {
    let mut out = vec![];
    while let Some(end) = buf.find("\n\n") {
        let frame: String = buf.drain(..end + 2).collect();
        let data: String = frame.lines().filter_map(|l| l.strip_prefix("data:")).map(str::trim_start).collect::<Vec<_>>().join("\n");
        if let Ok(v) = serde_json::from_str::<Value>(&data) {
            out.push(v);
        }
    }
    out
}

/// A form OpenCode asks, as questions; `session` asked it (a subagent's
/// child session, or the turn's own).
struct Form {
    id: String,
    session: String,
    questions: Vec<UserInputQuestion>,
}

impl Form {
    fn read(id: &str, session: &str, data: &Value) -> Option<Self> {
        let fields = data["fields"].as_array().filter(|fields| !fields.is_empty())?;
        let mut keys = HashSet::new();
        let questions = fields.iter().enumerate().map(|(index, field)| {
            let kind = field["type"].as_str()?;
            let key = field["key"].as_str().filter(|key| !key.trim().is_empty())?;
            if !matches!(kind, "string" | "multiselect") || !keys.insert(key) || field["hidden"] == true
                || field["when"].as_array().is_some_and(|conditions| !conditions.is_empty())
            {
                return None;
            }
            let header = field["title"].as_str().filter(|text| !text.trim().is_empty())
                .map_or_else(|| format!("Question {}", index + 1), str::to_owned);
            let options = field["options"].as_array().into_iter().flatten().map(|option| {
                let value = option["value"].as_str().filter(|value| !value.trim().is_empty())?;
                let label = option["label"].as_str().filter(|label| !label.trim().is_empty()).unwrap_or(value);
                Some(QuestionOption {
                    label: label.into(), description: option["description"].as_str().filter(|description| !description.trim().is_empty()).unwrap_or(label).into(),
                    value: Some(value.into()), preview: None,
                })
            }).collect::<Option<Vec<_>>>()?;
            Some(UserInputQuestion {
                id: key.into(),
                question: field["description"].as_str().filter(|text| !text.trim().is_empty()).unwrap_or(&header).into(),
                header,
                multi_select: Some(kind == "multiselect"),
                allow_custom_answer: Some(field["custom"] == true || options.is_empty()),
                options,
                initial_answer: None,
                required: Some(field["required"] == true),
            })
        }).collect::<Option<Vec<_>>>()?;
        Some(Self { id: id.into(), session: session.into(), questions })
    }

    /// The answer as OpenCode takes it (t3code `formAnswer`): a multiselect's
    /// values, any other field's joined; blank values and fields left out.
    fn reply(&self, answers: &super::Answers) -> Value {
        let answer = self.questions.iter().filter_map(|question| {
            let raw = answers.get(&question.id)?;
            let values: Vec<&str> = match raw {
                Value::Array(values) => values.iter().filter_map(Value::as_str).collect(),
                other => other.as_str().into_iter().collect(),
            };
            let values: Vec<&str> = values.into_iter().filter(|value| !value.trim().is_empty()).collect();
            if values.is_empty() {
                return None;
            }
            let value = if question.multi_select == Some(true) { json!(values) } else { Value::String(values.join(", ")) };
            Some((question.id.clone(), value))
        }).collect::<super::Answers>();
        json!({ "answer": answer })
    }
}

fn session_of_event(event: &Value) -> Option<&str> {
    if event["type"] == "form.created" {
        return event["data"]["form"]["sessionID"].as_str().or_else(|| event["data"]["sessionID"].as_str());
    }
    event["data"]["sessionID"].as_str()
}

/// The prompt message id of a turn: OpenCode takes it as given, so it is
/// known before the prompt is (t3code `turnPromptId`). Kept to characters a
/// URL path takes as they are.
fn prompt_id(session: &str, attempt: &str) -> String {
    let attempt: String = attempt.chars().map(|c| if c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | ':' | '.') { c } else { '_' }).collect();
    format!("msg_t3_turn_{session}:{attempt}")
}

/// Whether a recorded native turn is a prompt's message id, which a fork
/// or a rollback can cut before (OpenCode message ids start `msg_`).
pub fn is_prompt_id(native_turn: &str) -> bool {
    native_turn.starts_with("msg_")
}

/// `location[directory]=…`, the location an endpoint acts in.
fn located(directory: &str) -> [(&'static str, &str); 1] {
    [("location[directory]", directory)]
}

fn clamp_percent(value: f64) -> f64 {
    if value.is_finite() { value.clamp(0.0, 100.0) } else { 0.0 }
}

fn percent(value: f64) -> serde_json::Number {
    serde_json::Number::from_f64(clamp_percent(value)).unwrap_or_else(|| serde_json::Number::from(0))
}

fn reset_time(ms: &Value) -> Option<Timestamp> {
    let ms = ms.as_f64().filter(|ms| ms.is_finite())?;
    Some(Timestamp::from(domovoi_contracts::time_fmt::iso_from_millis(ms as i64)))
}

fn unavailable(checked_at: &Timestamp, reason: ServerProviderUsageUnavailableReason, message: Option<&str>) -> ServerProviderUsageLimits {
    ServerProviderUsageLimits {
        checked_at: checked_at.clone(),
        windows: Default::default(),
        credential_fingerprint: None,
        reset_credits: None,
        external_usage: None,
        unavailable: Some(ServerProviderUsageUnavailable { reason, message: message.map(str::to_owned) }),
    }
}

/// Whether a usage limits answer has a spend limit on all of a set's spend,
/// which applies to the Console's providers (read from `/api/provider`).
fn needs_console_providers(body: &Value) -> bool {
    body["data"]["budgets"].as_array().into_iter().flatten().any(|budget| budget["coverage"] == "all" && budget["per"] == "set")
}

/// A workspace's usage limits as `/api/experimental/usage/limits` answers
/// (t3code `readServerUsageLimits`): Go's rolling, weekly and monthly meters
/// and each spend limit. `console_providers`: the providers an all-spend
/// limit covers. `None` for an answer of another shape.
pub fn usage_limits_from(body: &Value, console_providers: &[String], checked_at: Timestamp) -> Option<ServerProviderUsageLimits> {
    let data = body.get("data")?;
    if data.is_null() {
        return Some(unavailable(&checked_at, ServerProviderUsageUnavailableReason::Unsupported, Some("No OpenCode Console workspace is signed in.")));
    }
    let workspace = &data["workspace"];
    let workspace_id = workspace["id"].as_str()?;
    let budgets = data["budgets"].as_array()?;
    let external_usage = workspace["usageURL"].as_str().and_then(|url| reqwest::Url::parse(url).ok()).filter(|url| matches!(url.scheme(), "http" | "https")).map(|url| {
        let label = workspace["name"].as_str().map(str::trim).filter(|name| !name.is_empty()).unwrap_or(workspace_id);
        ServerProviderExternalUsage { label: label.to_owned(), url: url.to_string() }
    });
    let go = data["go"].as_object();
    // A Zen-only workspace has neither a Go subscription nor spend limits.
    if go.is_none() && budgets.is_empty() {
        let mut limits = unavailable(&checked_at, ServerProviderUsageUnavailableReason::Unsupported, None);
        limits.external_usage = external_usage;
        return Some(limits);
    }
    let window = |id: String, kind, label: String, used: f64, resets_at: &Value, mins: Option<u64>| ServerProviderUsageWindow {
        id, kind, label, model_ids: None, provider_ids: None, used_percent: percent(used), resets_at: reset_time(resets_at), window_duration_mins: mins,
    };
    let mut windows = vec![];
    if let Some(go) = go {
        for (key, kind, label, mins) in [
            ("rolling", ServerProviderUsageWindowKind::Session, "Session", Some(300)),
            ("weekly", ServerProviderUsageWindowKind::Weekly, "Weekly", Some(10080)),
            ("monthly", ServerProviderUsageWindowKind::Monthly, "Monthly", None),
        ] {
            let meter = go.get(key)?;
            let used = if meter["limited"] == true { 100.0 } else { meter["percent"].as_f64()? };
            windows.push(window(format!("go_{key}"), kind, format!("Go \u{b7} {label}"), used, &meter["resetsAt"], mins));
        }
    }
    let spent_share = |spent: f64, limit: f64| if limit <= 0.0 { 100.0 } else { clamp_percent(spent / limit * 100.0) };
    for budget in budgets {
        let id = budget["id"].as_str()?;
        let (limit, spent) = (budget["limit"].as_f64()?, budget["spent"].as_f64()?);
        let scope = if budget["shared"] == true || budget["scope"] == "combined" { "Shared" } else { "Individual" };
        let models = budget["models"].as_array().cloned().unwrap_or_default();
        let model_id = |model: &Value| format!("{}/{}", model["providerID"].as_str().unwrap_or_default(), model["modelID"].as_str().unwrap_or_default());
        let per_model = budget["per"] == "model";
        if per_model && budget["coverage"] == "models" && !models.is_empty() && models.iter().all(|model| model["spent"].is_number()) {
            for model in &models {
                let name = model["name"].as_str().unwrap_or_default();
                let mut w = window(
                    format!("budget_{id}_{}_{}", model["providerID"].as_str().unwrap_or_default(), model["modelID"].as_str().unwrap_or_default()),
                    ServerProviderUsageWindowKind::Unnamed, format!("Console \u{b7} {scope} \u{b7} {name}"),
                    spent_share(model["spent"].as_f64().unwrap_or(spent), limit), &budget["resetsAt"], None,
                );
                w.model_ids = Some(vec![model_id(model)]);
                windows.push(w);
            }
            continue;
        }
        let covered = if budget["coverage"] == "all" {
            "All spend".to_owned()
        } else {
            Some(models.iter().filter_map(|model| model["name"].as_str()).collect::<Vec<_>>().join(", ")).filter(|names| !names.is_empty()).unwrap_or_else(|| "Models".to_owned())
        };
        let most_used = if per_model { " \u{b7} Most used model" } else { "" };
        let mut w = window(format!("budget_{id}"), ServerProviderUsageWindowKind::Unnamed, format!("Console \u{b7} {scope} \u{b7} {covered}{most_used}"), spent_share(spent, limit), &budget["resetsAt"], None);
        // Aggregate per-model spend cannot name the exhausted model.
        if per_model {
            w.model_ids = Some(vec![]);
        } else if budget["coverage"] == "models" {
            w.model_ids = Some(models.iter().map(model_id).collect());
        } else {
            w.provider_ids = Some(console_providers.to_vec());
        }
        windows.push(w);
    }
    Some(ServerProviderUsageLimits { checked_at, windows: windows.into(), credential_fingerprint: None, reset_credits: None, external_usage, unavailable: None })
}

impl OpenCodeServer {
    pub async fn start() -> Result<Arc<Self>, String> {
        let exe = crate::providers::executable(OPENCODE_TOOL)?;
        let port = std::net::TcpListener::bind("127.0.0.1:0").and_then(|l| l.local_addr()).map_err(|e| e.to_string())?.port();
        let password = crate::util::new_uuid().replace('-', "");
        let mut child = serve_command(&exe, port)
            .env("OPENCODE_SERVER_PASSWORD", &password)
            .stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null())
            .kill_on_drop(true).spawn().map_err(|e| format!("could not start opencode serve: {e}"))?;
        let base = format!("http://127.0.0.1:{port}");
        let http = reqwest::Client::builder().build().map_err(|e| e.to_string())?;
        let auth = basic(&password);
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            if let Ok(Some(status)) = child.try_wait() {
                return Err(format!("opencode serve exited ({status})."));
            }
            if http.get(format!("{base}/global/health")).header("authorization", &auth).send().await.is_ok_and(|r| r.status().is_success()) {
                break;
            }
            if tokio::time::Instant::now() > deadline {
                return Err("opencode serve did not come up.".into());
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        let routes: Arc<Mutex<HashMap<String, mpsc::UnboundedSender<Value>>>> = Arc::default();
        let server = Arc::new(OpenCodeServer { base, auth, http, routes, child: Mutex::new(child), mcp: Mutex::default(), instructed: Mutex::default() });
        let s = server.clone();
        tokio::spawn(async move { s.pump_events().await });
        Ok(server)
    }

    /// Reads `/api/event` for good (reconnecting), sending each session's
    /// events to its turn (a child session's to its parent's); a lost
    /// server tells every turn.
    async fn pump_events(self: Arc<Self>) {
        loop {
            match self.http.get(format!("{}/api/event", self.base)).header("authorization", &self.auth).header("accept", "text/event-stream").send().await {
                Ok(mut res) if res.status().is_success() => {
                    let mut buf = String::new();
                    while let Ok(Some(chunk)) = res.chunk().await {
                        buf.push_str(&String::from_utf8_lossy(&chunk));
                        for ev in take_events(&mut buf) {
                            let sid = session_of_event(&ev).map(str::to_owned);
                            let Some(sid) = sid else { continue };
                            let tx = {
                                let mut routes = self.routes.lock().await;
                                // A subagent's session: its events go to the turn that started it.
                                if ev["type"] == "session.created"
                                    && let Some(parent) = ev["data"]["parentID"].as_str().and_then(|parent| routes.get(parent)).cloned()
                                {
                                    routes.entry(sid.clone()).or_insert(parent);
                                }
                                routes.get(&sid).cloned()
                            };
                            if let Some(tx) = tx {
                                let _ = tx.send(ev);
                            }
                        }
                    }
                }
                _ => {}
            }
            if self.child.lock().await.try_wait().ok().flatten().is_some() {
                for tx in self.routes.lock().await.values() {
                    let _ = tx.send(json!({ "type": "domovoi.server.exited" }));
                }
                return;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    pub async fn alive(&self) -> bool {
        self.child.lock().await.try_wait().ok().flatten().is_none()
    }

    async fn call(&self, method: reqwest::Method, path: &str, body: Option<Value>) -> Result<Value, String> {
        self.call_at(method, path, &[], body).await
    }

    /// A request with query parameters, bounded by [`REQUEST_TIMEOUT`].
    async fn call_at(&self, method: reqwest::Method, path: &str, query: &[(&str, &str)], body: Option<Value>) -> Result<Value, String> {
        let mut url = reqwest::Url::parse(&format!("{}{path}", self.base)).map_err(|e| e.to_string())?;
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query);
        }
        let mut req = self.http.request(method, url).header("authorization", &self.auth).timeout(REQUEST_TIMEOUT);
        if let Some(b) = body {
            req = req.json(&b);
        }
        let res = req.send().await.map_err(|e| e.to_string())?;
        let status = res.status();
        let text = res.text().await.unwrap_or_default();
        if !status.is_success() {
            let msg = serde_json::from_str::<Value>(&text).ok().and_then(|v| v["message"].as_str().or(v["error"]["message"].as_str()).map(str::to_owned));
            return Err(msg.unwrap_or_else(|| format!("OpenCode answered {status}: {}", text.chars().take(300).collect::<String>())));
        }
        Ok(serde_json::from_str(&text).unwrap_or(Value::Null))
    }

    /// Form routes return no content. HTTP conflicts and missing forms mean
    /// the live request can already have settled in a different client.
    async fn deliver_form(&self, method: reqwest::Method, path: &str, body: Option<Value>, message: Option<&str>) -> Result<bool, String> {
        let mut url = reqwest::Url::parse(&format!("{}{path}", self.base)).map_err(|e| e.to_string())?;
        if let Some(message) = message {
            url.query_pairs_mut().append_pair("message", message);
        }
        let mut req = self.http.request(method, url).header("authorization", &self.auth).timeout(REQUEST_TIMEOUT);
        if let Some(body) = body {
            req = req.json(&body);
        }
        let status = req.send().await.map_err(|e| e.to_string())?.status();
        match status.as_u16() {
            404 | 409 => Ok(false),
            _ if status.is_success() => Ok(true),
            _ => Err(format!("OpenCode answered {status}.")),
        }
    }

    /// The models OpenCode can run now (its enabled, connected models) as
    /// provider models, and its default slug.
    pub async fn models(&self) -> Result<(Vec<ServerProviderModel>, Option<String>), String> {
        let list = self.call(reqwest::Method::GET, "/api/model", None).await?;
        let default = self.call(reqwest::Method::GET, "/api/model/default", None).await.ok()
            .and_then(|d| Some(format!("{}/{}", d["data"]["providerID"].as_str()?, d["data"]["id"].as_str()?)));
        let models = list["data"].as_array().into_iter().flatten().filter(|m| m["enabled"] != false).filter_map(|m| {
            let slug = format!("{}/{}", m["providerID"].as_str()?, m["id"].as_str()?);
            Some(ServerProviderModel {
                name: m["name"].as_str().unwrap_or(&slug).to_owned(),
                short_name: None,
                sub_provider: None,
                aliases: None,
                badge: None,
                is_custom: false,
                is_default: (default.as_deref() == Some(slug.as_str())).then_some(true),
                is_legacy: None,
                supports_text_generation: None,
                capabilities: Some(ModelCapabilities { option_descriptors: Some(vec![]) }),
                slug,
            })
        }).collect();
        Ok((models, default))
    }

    /// Reads the workspace's usage limits for `instance` (in the location of
    /// `directory`). An OpenCode without the endpoint leaves them as they were.
    pub async fn read_usage_limits(&self, instance: &str, directory: &str) {
        let checked_at = Timestamp::from(domovoi_contracts::iso_now());
        let read = async {
            let mut url = reqwest::Url::parse(&format!("{}/api/experimental/usage/limits", self.base)).map_err(|e| e.to_string())?;
            url.query_pairs_mut().extend_pairs(located(directory));
            let res = self.http.get(url).header("authorization", &self.auth).send().await.map_err(|e| e.to_string())?;
            let html = res.headers().get("content-type").and_then(|v| v.to_str().ok()).and_then(|v| v.split(';').next()).is_some_and(|v| v.trim() == "text/html");
            let status = res.status().as_u16();
            if matches!(status, 404 | 405) || (status == 200 && html) {
                return Ok(None);
            }
            if !res.status().is_success() {
                return Err(format!("OpenCode answered {status}"));
            }
            let body: Value = res.json().await.map_err(|e| e.to_string())?;
            let providers: Vec<String> = if needs_console_providers(&body) {
                self.call_at(reqwest::Method::GET, "/api/provider", &located(directory), None).await.ok().map(|page| {
                    page["data"].as_array().into_iter().flatten()
                        .filter(|p| p["integrationID"] == "opencode" && p["id"] != "opencode-go")
                        .filter_map(|p| p["id"].as_str().map(str::to_owned)).collect()
                }).unwrap_or_default()
            } else {
                vec![]
            };
            usage_limits_from(&body, &providers, checked_at.clone()).map(Some).ok_or_else(|| "an answer of another shape".to_owned())
        };
        match tokio::time::timeout(Duration::from_secs(5), read).await {
            Ok(Ok(Some(limits))) => crate::usage_limits::set(instance, limits),
            Ok(Ok(None)) => {}
            failed => {
                tracing::info!(?failed, "could not read OpenCode's usage limits");
                crate::usage_limits::set(instance, unavailable(&checked_at, ServerProviderUsageUnavailableReason::ProbeFailed, Some("OpenCode could not read workspace usage.")));
            }
        }
    }

    /// Stops the server: SIGTERM lets the watchdog stop `opencode serve`.
    pub async fn kill(&self) {
        let mut child = self.child.lock().await;
        #[cfg(unix)]
        if let Some(pid) = child.id() {
            // SAFETY: signalling a child process this server started.
            unsafe { libc::kill(pid as i32, libc::SIGTERM) };
            if tokio::time::timeout(Duration::from_secs(3), child.wait()).await.is_ok() {
                return;
            }
        }
        let _ = child.kill().await;
    }

    /// Adds the thread's MCP server to the location of `directory` (again
    /// each turn: OpenCode drops runtime servers of a location it evicts, and
    /// keeps the connection of an unchanged one). Its name, once OpenCode took
    /// it; `None` with the tools off or refused.
    async fn add_mcp(&self, thread_id: &str, directory: &str) -> Option<String> {
        let (url, token) = crate::mcp::session_for(thread_id)?;
        let name = mcp_server_name(thread_id);
        let path = format!("/api/experimental/mcp/{name}");
        let before = self.mcp.lock().await.get(thread_id).cloned();
        if let Some(old) = before.as_deref().filter(|old| *old != directory) {
            if let Err(e) = self.call_at(reqwest::Method::DELETE, &path, &located(old), None).await {
                tracing::info!(%thread_id, "could not take the MCP server out of OpenCode's old location: {e}");
            }
            self.mcp.lock().await.remove(thread_id);
        }
        let config = json!({ "config": { "type": "remote", "url": url, "headers": { "Authorization": format!("Bearer {token}") }, "oauth": false } });
        match self.call_at(reqwest::Method::PUT, &path, &located(directory), Some(config)).await {
            Ok(_) => {
                let fresh = self.mcp.lock().await.insert(thread_id.to_owned(), directory.to_owned()).is_none();
                if fresh {
                    tokio::time::sleep(MCP_TOOL_SETTLE).await;
                }
                Some(name)
            }
            Err(e) => {
                tracing::warn!(%thread_id, "could not add Domovoi's MCP server to OpenCode: {e}");
                self.mcp.lock().await.remove(thread_id);
                None
            }
        }
    }

    /// The session's `t3-code` instruction entry: `text`, or none.
    async fn instruct(&self, session: &str, text: Option<String>) {
        let held = self.instructed.lock().await.get(session).cloned();
        if held == text {
            return;
        }
        let path = format!("/api/experimental/session/{session}/instructions/entries/{INSTRUCTIONS_KEY}");
        let sent = match &text {
            Some(text) => self.call(reqwest::Method::PUT, &path, Some(json!({ "value": text }))).await,
            None => self.call(reqwest::Method::DELETE, &path, None).await,
        };
        match sent {
            Ok(_) => {
                let mut instructed = self.instructed.lock().await;
                match text {
                    Some(text) => instructed.insert(session.to_owned(), text),
                    None => instructed.remove(session),
                };
            }
            Err(e) => tracing::warn!(%session, "could not set the OpenCode session's instructions: {e}"),
        }
    }

    /// The session a turn runs in: the thread's, a fork of the source's for a
    /// forked thread's first turn, or a new one.
    async fn open_session(&self, turn: &Turn, input: &TurnInput, rules: &Value) -> Result<String, String> {
        let model = model_ref(&input.model_selection.model);
        let model = model.as_ref();
        let set_model = |id: String| async move {
            if let Some(m) = model {
                let _ = self.call(reqwest::Method::POST, &format!("/api/session/{id}/model"), Some(json!({ "model": m }))).await;
            }
            id
        };
        if let Some(id) = turn.native_thread_id() {
            // Settings may have changed since the last turn.
            let _ = self.call(reqwest::Method::PATCH, &format!("/api/session/{id}"), Some(json!({ "permissions": rules }))).await;
            return Ok(set_model(id).await);
        }
        if let Some((source, _)) = &input.fork {
            // A copy of the source's history before its next turn after the fork point.
            let body = match &input.fork_before {
                Some(before) => json!({ "before": before }),
                None => json!({}),
            };
            let forked = self.call(reqwest::Method::POST, &format!("/api/session/{source}/fork"), Some(body)).await.map_err(|e| format!("Could not fork the OpenCode session: {e}"))?;
            let id = forked["data"]["id"].as_str().filter(|id| !id.is_empty()).ok_or("Could not fork the OpenCode session: it named no session.")?.to_owned();
            self.call(reqwest::Method::PATCH, &format!("/api/session/{id}"), Some(json!({ "permissions": rules }))).await.map_err(|e| format!("Could not set the forked session's permissions: {e}"))?;
            // A fork stays in its source's folder: move it to this thread's.
            if forked["data"]["location"]["directory"].as_str() != Some(input.cwd.as_str()) {
                self.call(reqwest::Method::POST, &format!("/api/session/{id}/move"), Some(json!({ "directory": input.cwd }))).await.map_err(|e| format!("Could not move the forked session: {e}"))?;
            }
            return Ok(set_model(id).await);
        }
        let mut body = json!({ "location": { "directory": input.cwd }, "permissions": rules });
        if let Some(m) = model {
            body["model"] = m.clone();
        }
        let r = self.call(reqwest::Method::POST, "/api/session", Some(body)).await.map_err(|e| format!("Could not start an OpenCode session: {e}"))?;
        r["data"]["id"].as_str().filter(|id| !id.is_empty()).map(str::to_owned).ok_or_else(|| "Could not start an OpenCode session: it named no session.".to_owned())
    }

    /// Rolls the session back to before message `before` (t3code
    /// `rollBackTo`): history only, files are the checkpoint's. A revert that
    /// fails half way is cleared, or the next prompt would commit it.
    pub async fn rollback(&self, session: &str, before: &str) -> Result<(), String> {
        let staged = self.call(reqwest::Method::POST, &format!("/api/session/{session}/revert/stage"), Some(json!({ "messageID": before, "files": false }))).await;
        let committed = match staged {
            Ok(_) => self.call(reqwest::Method::POST, &format!("/api/session/{session}/revert/commit"), None).await,
            Err(e) => Err(e),
        };
        if let Err(e) = committed {
            let _ = self.call(reqwest::Method::DELETE, &format!("/api/session/{session}/revert"), None).await;
            return Err(format!("Could not roll back the OpenCode session: {e}"));
        }
        Ok(())
    }

    /// Runs one turn to completion, recording it on `turn`.
    pub async fn run_turn(&self, mut turn: Turn, input: TurnInput, mut controls: mpsc::UnboundedReceiver<Control>) -> Turn {
        let slug = input.model_selection.model.clone();
        turn.session(ProviderSessionStatus::Ready, &input.cwd, &slug, None);
        let mcp = self.add_mcp(&input.thread_id, &input.cwd).await;
        let session = match self.open_session(&turn, &input, &rules(&input.runtime_mode, mcp.as_deref())).await {
            Ok(session) => session,
            Err(e) => {
                turn.complete(TurnEnd::Failed, Some(&e));
                return turn;
            }
        };
        // What Domovoi's tools are for, while the session has them.
        self.instruct(&session, mcp.is_some().then(crate::agent_instructions::system_prompt)).await;
        #[expect(clippy::disallowed_methods, reason = "filled by the event pump every session shares: one slow turn must not hold up the others")]
        let (tx, mut events) = mpsc::unbounded_channel();
        let mine = tx.clone();
        self.routes.lock().await.insert(session.clone(), tx);
        turn.thread_status(ProviderThreadStatus::Active, Some(&session));
        let files: Vec<Value> = input.images.iter().map(|(_, p)| json!({ "uri": format!("file://{}", p.display()), "name": p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default() })).collect();
        let text = if input.text.trim_start().starts_with('/') {
            input.text.clone()
        } else {
            let instructions = crate::agent_instructions::question_guidance("question", Some(&input.interaction_mode));
            format!("<domovoi_instructions>\n{instructions}\n</domovoi_instructions>\n\n<user_request>\n{}\n</user_request>", input.text)
        };
        let attempt = turn.run.active_attempt_id.as_ref().map_or_else(|| turn.run.id.as_str().to_owned(), |a| a.as_str().to_owned());
        let prompt_id = prompt_id(&session, &attempt);
        let mut prompt = json!({ "id": prompt_id, "text": text });
        if !files.is_empty() {
            prompt["files"] = Value::Array(files);
        }
        if let Err(e) = self.call(reqwest::Method::POST, &format!("/api/session/{session}/prompt"), Some(prompt)).await {
            self.routes.lock().await.retain(|_, tx| !tx.same_channel(&mine));
            turn.complete(TurnEnd::Failed, Some(&format!("OpenCode refused the message: {e}")));
            return turn;
        }
        turn.turn_started(&prompt_id);

        let mut live = Live::new(self, session, prompt_id);
        let end = loop {
            let wake = live.wake_by;
            tokio::select! {
                Some(c) = controls.recv() => {
                    if let Some(end) = live.control(&mut turn, c).await {
                        break end;
                    }
                }
                Some(ev) = events.recv() => {
                    if let Some(end) = live.event(&mut turn, ev).await {
                        break end;
                    }
                }
                // The background subagents all reported, and OpenCode did not wake the session with them.
                () = async { if let Some(at) = wake { tokio::time::sleep_until(at).await } }, if wake.is_some() => break (TurnEnd::Completed, None),
                else => break (TurnEnd::Failed, Some("The OpenCode turn lost its events.".to_owned())),
            }
        };
        self.routes.lock().await.retain(|_, tx| !tx.same_channel(&mine));
        let (end, error) = end;
        live.subs.end(&turn, end == TurnEnd::Completed);
        for request in live.forms.keys() {
            turn.cancel_request(request);
        }
        if live.limited {
            self.read_usage_limits(&turn.instance, &input.cwd).await;
            turn.usage_limited(crate::usage_limits::exhausted_reset_at(&turn.instance));
        }
        turn.complete(end, error.as_deref());
        turn
    }
}

/// An asked permission: who asked, and what for.
struct Asked {
    session: String,
    id: String,
    action: String,
    resources: Vec<String>,
}

/// A `subagent` tool call of the turn's session.
#[derive(Default)]
struct Call {
    /// Its subagent's native id, once the call says what it asks.
    native: Option<String>,
}

/// A running turn's state.
struct Live<'a> {
    server: &'a OpenCodeServer,
    session: String,
    prompt_id: String,
    texts: HashMap<String, String>,
    tools: HashMap<String, (String, Value)>,
    /// Tool calls that are not items (the `question` tool: its form is the request).
    hidden: HashSet<String>,
    /// t3code request id -> the OpenCode permission.
    asked: HashMap<String, Asked>,
    forms: HashMap<String, Form>,
    seen_forms: HashSet<String>,
    subs: Subagents,
    /// `subagent` tool calls by tool id.
    calls: HashMap<String, Call>,
    /// Child sessions (and theirs) by session: the subagent they work for.
    children: HashMap<String, String>,
    /// Every session's parent, as OpenCode announced them.
    parents: HashMap<String, String>,
    /// Child session events from before their subagent call named them.
    strays: HashMap<String, Vec<Value>>,
    /// Subagents in the background: the turn waits for their reports.
    background: HashSet<String>,
    /// Subagents the user stopped.
    stopped: HashSet<String>,
    /// The session's execution ended with background subagents owed.
    idle: bool,
    wake_by: Option<tokio::time::Instant>,
    /// A usage limit stopped the turn.
    limited: bool,
}

type End = (TurnEnd, Option<String>);

impl<'a> Live<'a> {
    fn new(server: &'a OpenCodeServer, session: String, prompt_id: String) -> Self {
        Live {
            server, session, prompt_id, texts: HashMap::new(), tools: HashMap::new(), hidden: HashSet::new(), asked: HashMap::new(), forms: HashMap::new(),
            seen_forms: HashSet::new(), subs: Subagents::default(), calls: HashMap::new(), children: HashMap::new(), parents: HashMap::new(),
            strays: HashMap::new(), background: HashSet::new(), stopped: HashSet::new(), idle: false, wake_by: None, limited: false,
        }
    }

    async fn post(&self, path: &str, body: Option<Value>) -> Result<Value, String> {
        self.server.call(reqwest::Method::POST, path, body).await
    }

    async fn control(&mut self, turn: &mut Turn, control: Control) -> Option<End> {
        match control {
            Control::Respond { request_id, decision, answers } => {
                if let Some(form) = self.forms.get(&request_id) {
                    let path = format!("/api/session/{}/form/{}", form.session, form.id);
                    let answer = answers.as_ref().filter(|_| !matches!(decision.as_str(), "cancel" | "decline" | "stop"));
                    // A subagent's question needs a message, or its step aborts.
                    let dismissed = (form.session != self.session).then_some(DISMISSED);
                    let result = match answer {
                        Some(answer) => self.server.deliver_form(reqwest::Method::POST, &format!("{path}/reply"), Some(form.reply(answer)), None).await,
                        None => self.server.deliver_form(reqwest::Method::DELETE, &path, None, dismissed).await,
                    };
                    match result {
                        Ok(delivered) => {
                            self.forms.remove(&request_id);
                            if delivered && answer.is_some() {
                                turn.resolve_request(&request_id, &decision, answer);
                            } else {
                                turn.cancel_request(&request_id);
                            }
                        }
                        Err(error) => {
                            turn.item(&format!("question-error:{request_id}"), Work::Notice {
                                message: format!("Could not send the question response. Try again. {error}"),
                            }, TurnItemStatus::Completed);
                        }
                    }
                } else if let Some(asked) = self.asked.remove(&request_id) {
                    let reply = match decision.as_str() {
                        "acceptForSession" | "acceptAlways" => json!({ "decision": "always" }),
                        d if super::turn::decision_allows(d) => json!({ "decision": "once" }),
                        // Declined, the agent hears why and goes on; cancelled, it stops.
                        "decline" => {
                            let on = if asked.resources.is_empty() { String::new() } else { format!(" ({})", asked.resources.join(", ")) };
                            let note = format!("The user declined the {} request{on}. Do not retry it; continue without it or ask the user how to proceed.", asked.action);
                            let _ = self.post(&format!("/api/session/{}/synthetic", asked.session), Some(json!({ "text": note, "delivery": "steer", "resume": false }))).await;
                            json!({ "decision": "reject", "message": DECLINED })
                        }
                        _ => json!({ "decision": "reject" }),
                    };
                    let _ = self.post(&format!("/api/session/{}/permission/{}/reply", asked.session, asked.id), Some(reply)).await;
                    turn.resolve_request(&request_id, &decision, None);
                }
            }
            Control::Interrupt => {
                if self.idle {
                    // Only background subagents still work: stop them, and the turn with them.
                    for session in self.sessions_of(|native| self.background.contains(native)) {
                        let _ = self.post(&format!("/api/session/{session}/interrupt"), None).await;
                    }
                    return Some((TurnEnd::Interrupted, None));
                }
                let _ = self.post(&format!("/api/session/{}/interrupt", self.session), None).await;
                // Its subagents' sessions stop with it (t3code `stopStrays`).
                for session in self.sessions_of(|_| true) {
                    let _ = self.post(&format!("/api/session/{session}/interrupt"), None).await;
                }
            }
            Control::Steer { text, reply, .. } => {
                // A prompt while it works is delivered into the running execution.
                let r = self.post(&format!("/api/session/{}/prompt", self.session), Some(json!({ "text": text }))).await;
                let _ = reply.send(r.map(|_| ()));
            }
            Control::StopSubagent { native_id, reply } => {
                let sessions = self.sessions_of(|native| *native == native_id);
                if sessions.is_empty() {
                    let _ = reply.send(Err("OpenCode has not reported this subagent's session yet.".into()));
                } else {
                    self.stopped.insert(native_id);
                    let mut result = Ok(());
                    for session in sessions {
                        if let Err(e) = self.post(&format!("/api/session/{session}/interrupt"), None).await {
                            result = Err(format!("Could not stop the OpenCode subagent: {e}"));
                        }
                    }
                    let _ = reply.send(result);
                }
            }
        }
        None
    }

    /// The child sessions of the subagents `which` picks.
    fn sessions_of(&self, which: impl Fn(&String) -> bool) -> Vec<String> {
        self.children.iter().filter(|(_, native)| which(native)).map(|(session, _)| session.clone()).collect()
    }

    async fn event(&mut self, turn: &mut Turn, ev: Value) -> Option<End> {
        let ty = ev["type"].as_str().unwrap_or_default().to_owned();
        let sid = session_of_event(&ev).unwrap_or_default().to_owned();
        let d = &ev["data"];
        match ty.as_str() {
            "domovoi.server.exited" => return Some((TurnEnd::Failed, Some("The OpenCode server exited.".to_owned()))),
            // A subagent's questions and approvals are the turn's too.
            "permission.asked" => {
                let id = d["id"].as_str().unwrap_or_default().to_owned();
                let action = d["action"].as_str().unwrap_or("permission").to_owned();
                let resources: Vec<String> = d["resources"].as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_owned).collect();
                let prompt = if resources.is_empty() { format!("Allow {action}") } else { resources.join("\n") };
                let item = if sid == self.session { d["source"]["id"].as_str().unwrap_or(&id).to_owned() } else { id.clone() };
                let rid = turn.open_request(&id, &item, approval_kind(request_kind(&action)), &prompt);
                self.asked.insert(rid, Asked { session: sid, id, action, resources });
                return None;
            }
            "form.created" => {
                let form = &d["form"];
                let Some(id) = form["id"].as_str().or(d["id"].as_str()) else { return None };
                if !self.seen_forms.insert(id.to_owned()) {
                    return None;
                }
                if let Some(form) = Form::read(id, &sid, form) {
                    let request = turn.open_user_input(id, id, form.questions.clone());
                    self.forms.insert(request, form);
                } else {
                    let path = format!("/api/session/{sid}/form/{id}");
                    if let Err(error) = self.server.deliver_form(reqwest::Method::DELETE, &path, None, Some("This form cannot be displayed. Ask the user in chat instead.")).await {
                        let _ = self.post(&format!("/api/session/{}/interrupt", self.session), None).await;
                        return Some((TurnEnd::Failed, Some(format!("Could not cancel the unsupported form: {error}"))));
                    }
                }
                return None;
            }
            "form.replied" | "form.cancelled" => {
                let Some(id) = d["id"].as_str() else { return None };
                if let Some(request) = self.forms.iter().find(|(_, form)| form.id == id).map(|(request, _)| request.clone()) {
                    self.forms.remove(&request);
                    if ty == "form.replied" {
                        turn.resolve_request(&request, "accept", d["answer"].as_object());
                    } else {
                        turn.cancel_request(&request);
                    }
                }
                return None;
            }
            _ => {}
        }
        if sid != self.session {
            self.child_event(turn, &sid, &ev);
            return None;
        }
        let native = |kind: &str| format!("{}:{kind}:{}", d["assistantMessageID"].as_str().unwrap_or(""), d["ordinal"].as_u64().unwrap_or(0));
        let tool_id = d["id"].as_str().unwrap_or_default().to_owned();
        match ty.as_str() {
            "session.text.started" => {
                self.texts.insert(native("text"), String::new());
            }
            "session.text.delta" => {
                let id = native("text");
                let t = self.texts.entry(id.clone()).or_default();
                t.push_str(d["delta"].as_str().unwrap_or_default());
                let t = t.clone();
                turn.assistant(&id, &t, true, None);
            }
            "session.text.ended" => {
                let id = native("text");
                let t = d["text"].as_str().map(str::to_owned).or_else(|| self.texts.remove(&id)).unwrap_or_default();
                turn.assistant(&id, &t, false, None);
            }
            "session.reasoning.delta" => {
                let id = native("reasoning");
                let t = self.texts.entry(id.clone()).or_default();
                t.push_str(d["delta"].as_str().unwrap_or_default());
                let t = t.clone();
                let len = t.len();
                turn.item_streaming(&id, Work::Reasoning { text: t, streaming: true }, len);
            }
            "session.reasoning.ended" => {
                let id = native("reasoning");
                let t = d["text"].as_str().map(str::to_owned).or_else(|| self.texts.remove(&id)).unwrap_or_default();
                turn.item(&id, Work::Reasoning { text: t, streaming: false }, TurnItemStatus::Completed);
            }
            "session.tool.input.started" => match d["name"].as_str().unwrap_or("tool") {
                "question" => {
                    self.hidden.insert(tool_id);
                }
                "subagent" => {
                    self.calls.entry(tool_id).or_default();
                }
                name => {
                    self.tools.insert(tool_id, (name.to_owned(), Value::Null));
                }
            },
            "session.tool.called" => {
                if self.hidden.contains(&tool_id) || d["name"] == "question" {
                    self.hidden.insert(tool_id);
                    return None;
                }
                if self.calls.contains_key(&tool_id) || d["name"] == "subagent" {
                    self.start_subagent(turn, &tool_id, &d["input"]);
                    return None;
                }
                let name = self.tools.get(&tool_id).map(|t| t.0.clone()).or_else(|| d["name"].as_str().map(str::to_owned)).unwrap_or_else(|| "tool".into());
                self.tools.insert(tool_id.clone(), (name.clone(), d["input"].clone()));
                turn.item(&tool_id, work_of(&name, &d["input"], None), TurnItemStatus::Running);
            }
            "session.tool.progress" => {
                if let (Some(native), Some(child)) = (self.call_native(&tool_id), d["metadata"]["sessionID"].as_str()) {
                    self.attach(turn, child, &native);
                }
            }
            "session.tool.success" | "session.tool.failed" => {
                if self.hidden.contains(&tool_id) {
                    return None;
                }
                if self.calls.contains_key(&tool_id) {
                    self.settle_call(turn, &tool_id, &ev);
                    return None;
                }
                let (name, input) = self.tools.get(&tool_id).cloned().unwrap_or_else(|| ("tool".into(), Value::Null));
                let (output, exit, failed) = tool_result(&ty, d);
                let status = if failed { TurnItemStatus::Failed } else { TurnItemStatus::Completed };
                turn.item(&tool_id, work_of(&name, &input, Some((&output, exit, failed))), status);
            }
            "session.inbox.enqueued" => self.report(turn, &d["item"]),
            "session.step.ended" => {
                let t = &d["tokens"];
                if let (Some(i), Some(o)) = (t["input"].as_u64(), t["output"].as_u64()) {
                    turn.token_usage(WindowUse {
                        used_tokens: i + o,
                        max_tokens: None,
                        input_tokens: i,
                        cached_input_tokens: t["cache"]["read"].as_u64().unwrap_or(0),
                        output_tokens: o,
                        reasoning_output_tokens: t["reasoning"].as_u64().unwrap_or(0),
                    });
                }
            }
            "session.compaction.ended" => {
                turn.item(&format!("compaction:{}", ev["id"].as_str().unwrap_or("")), Work::Compaction { before_token_count: None }, TurnItemStatus::Completed);
            }
            "session.execution.started" => {
                // OpenCode woke the session with background subagents' reports.
                self.idle = false;
                self.wake_by = None;
            }
            "session.execution.succeeded" => {
                if !self.background.is_empty() {
                    self.idle = true;
                    return None;
                }
                return Some((TurnEnd::Completed, None));
            }
            "session.execution.interrupted" => return Some((TurnEnd::Interrupted, None)),
            "session.execution.failed" => {
                let e = &d["error"];
                self.limited = is_usage_limit(e);
                let msg = e["message"].as_str().or(e["data"]["message"].as_str()).or(e.as_str()).unwrap_or("OpenCode failed.").to_owned();
                return Some((TurnEnd::Failed, Some(msg)));
            }
            _ => {}
        }
        None
    }

    fn call_native(&self, tool_id: &str) -> Option<String> {
        self.calls.get(tool_id).and_then(|call| call.native.clone())
    }

    /// A `subagent` call says what it asks: its subagent starts.
    fn start_subagent(&mut self, turn: &mut Turn, tool_id: &str, input: &Value) {
        if self.call_native(tool_id).is_some() {
            return;
        }
        // Model-made tool ids can repeat across turns; the turn's prompt is unique.
        let native = format!("{}/{tool_id}", self.prompt_id);
        let prompt = input["prompt"].as_str().unwrap_or_default();
        self.subs.start(turn, &native, prompt, input["description"].as_str(), input["model"].as_str());
        self.calls.entry(tool_id.to_owned()).or_default().native = Some(native);
    }

    /// The `subagent` call returned: the subagent's answer, or (background)
    /// that it runs on until its report.
    fn settle_call(&mut self, turn: &mut Turn, tool_id: &str, ev: &Value) {
        let d = &ev["data"];
        if self.call_native(tool_id).is_none() {
            self.start_subagent(turn, tool_id, &Value::Null);
        }
        let Some(native) = self.call_native(tool_id) else { return };
        if let Some(child) = d["metadata"]["sessionID"].as_str() {
            self.attach(turn, child, &native);
        }
        if ev["type"] == "session.tool.failed" {
            let status = if d["error"]["type"] == "aborted" || self.stopped.contains(&native) { "interrupted" } else { "failed" };
            self.subs.finish(turn, &native, status, d["error"]["message"].as_str());
        } else if d["metadata"]["status"] == "running" {
            self.background.insert(native);
        } else {
            let status = if self.stopped.contains(&native) { "interrupted" } else { "completed" };
            let text = d["content"].as_array().into_iter().flatten().filter_map(|c| c["text"].as_str()).collect::<Vec<_>>().join("\n");
            self.subs.finish(turn, &native, status, Some(&subagent_output(&text)));
        }
    }

    /// A background subagent's report, delivered to the session's inbox.
    fn report(&mut self, turn: &Turn, item: &Value) {
        let metadata = &item["payload"]["metadata"];
        if item["type"] != "synthetic" || metadata["source"] != "subagent" {
            return;
        }
        let Some(native) = metadata["childID"].as_str().and_then(|child| self.children.get(child)).cloned() else { return };
        let status = if self.stopped.contains(&native) {
            "interrupted"
        } else {
            match metadata["state"].as_str() {
                Some("error") => "failed",
                Some("cancelled") => "cancelled",
                _ => "completed",
            }
        };
        let text = subagent_output(item["payload"]["text"].as_str().unwrap_or_default());
        self.subs.finish(turn, &native, status, Some(&text));
        self.background.remove(&native);
        if self.idle && self.background.is_empty() {
            self.wake_by = Some(tokio::time::Instant::now() + WAKE_GRACE);
        }
    }

    /// `child` (and the sessions it started) work for subagent `native`;
    /// what they did so far goes to its thread.
    fn attach(&mut self, turn: &mut Turn, child: &str, native: &str) {
        let mut queue = vec![child.to_owned()];
        while let Some(session) = queue.pop() {
            if session == self.session || self.children.contains_key(&session) {
                continue;
            }
            self.children.insert(session.clone(), native.to_owned());
            for ev in self.strays.remove(&session).unwrap_or_default() {
                self.child_event(turn, &session, &ev);
            }
            queue.extend(self.parents.iter().filter(|(_, parent)| **parent == session).map(|(child, _)| child.clone()));
        }
    }

    /// An event of a subagent's session: its words and tools, in its thread.
    fn child_event(&mut self, turn: &mut Turn, sid: &str, ev: &Value) {
        let d = &ev["data"];
        let ty = ev["type"].as_str().unwrap_or_default();
        if ty == "session.created" {
            if let Some(parent) = d["parentID"].as_str() {
                self.parents.insert(sid.to_owned(), parent.to_owned());
                if let Some(native) = self.children.get(parent).cloned() {
                    self.attach(turn, sid, &native);
                }
            }
            return;
        }
        let Some(owner) = self.children.get(sid).cloned() else {
            let held = self.strays.entry(sid.to_owned()).or_default();
            if held.len() < STRAY_EVENTS_MAX {
                held.push(ev.clone());
            }
            return;
        };
        let part = format!("{sid}:{}:{}", d["assistantMessageID"].as_str().unwrap_or(""), d["ordinal"].as_u64().unwrap_or(0));
        let tool = format!("{sid}:{}", d["id"].as_str().unwrap_or_default());
        match ty {
            "session.text.delta" | "session.reasoning.delta" => {
                self.texts.entry(format!("{ty}:{part}")).or_default().push_str(d["delta"].as_str().unwrap_or_default());
            }
            "session.text.ended" => {
                let held = self.texts.remove(&format!("session.text.delta:{part}"));
                let text = d["text"].as_str().map(str::to_owned).or(held).unwrap_or_default();
                self.subs.text(turn, &owner, &part, &text);
            }
            "session.reasoning.ended" => {
                let held = self.texts.remove(&format!("session.reasoning.delta:{part}"));
                let text = d["text"].as_str().map(str::to_owned).or(held).unwrap_or_default();
                self.subs.record_work(turn, &owner, &format!("{part}:reasoning"), Work::Reasoning { text, streaming: false }, TurnItemStatus::Completed);
            }
            "session.tool.input.started" => {
                self.tools.insert(tool, (d["name"].as_str().unwrap_or("tool").to_owned(), Value::Null));
            }
            "session.tool.called" => {
                let name = self.tools.get(&tool).map(|t| t.0.clone()).or_else(|| d["name"].as_str().map(str::to_owned)).unwrap_or_else(|| "tool".into());
                self.tools.insert(tool.clone(), (name.clone(), d["input"].clone()));
                self.subs.record_work(turn, &owner, &tool, work_of(&name, &d["input"], None), TurnItemStatus::Running);
            }
            "session.tool.success" | "session.tool.failed" => {
                let (name, input) = self.tools.get(&tool).cloned().unwrap_or_else(|| ("tool".into(), Value::Null));
                let (output, exit, failed) = tool_result(ty, d);
                let status = if failed { TurnItemStatus::Failed } else { TurnItemStatus::Completed };
                self.subs.record_work(turn, &owner, &tool, work_of(&name, &input, Some((&output, exit, failed))), status);
            }
            _ => {}
        }
    }
}

/// A finished tool call's (output, exit code, failed).
fn tool_result(ty: &str, d: &Value) -> (String, Option<i64>, bool) {
    let output = if ty == "session.tool.failed" {
        d["error"]["message"].as_str().unwrap_or("The tool failed.").to_owned()
    } else {
        d["content"].as_array().into_iter().flatten().filter_map(|c| c["text"].as_str()).collect::<Vec<_>>().join("\n")
    };
    let exit = d["metadata"]["exit"].as_i64();
    (output, exit, ty == "session.tool.failed" || exit.is_some_and(|c| c != 0))
}

/// An OpenCode tool call as work: shell commands, file edits, or a generic
/// tool. `result`: (output, exit code, failed).
fn work_of(name: &str, input: &Value, result: Option<(&str, Option<i64>, bool)>) -> Work {
    match name {
        "shell" | "bash" => Work::Command {
            input: input["command"].as_str().unwrap_or_default().to_owned(),
            output: result.map(|(out, _, _)| out.to_owned()),
            exit_code: result.map(|(_, code, failed)| code.unwrap_or(i64::from(failed))),
        },
        "edit" | "write" | "patch" => {
            let path = input["filePath"].as_str().or(input["path"].as_str()).unwrap_or("file");
            let file_name = std::path::Path::new(path).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or(path.to_owned());
            let operation = if name == "write" { "add" } else { "update" };
            let changes = vec![FileChangeDetail { operation: operation.into(), path: path.to_owned(), old_path: None, file_type: None, mime_type: None }];
            let texts = input["oldString"].as_str().zip(input["newString"].as_str()).map(|(old, new)| (old.to_owned(), new.to_owned()));
            Work::FileChange { file_name, changes, texts, patch: None, error: None }
        }
        _ => Work::Tool { name: name.to_owned(), input: input.clone(), output: result.map(|(out, _, _)| Value::String(out.to_owned())), viewed_image_path: None },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn events_are_read_from_sse_frames_and_rules_follow_the_mode() {
        let mut buf = "data: {\"type\":\"a\",\"data\":{\"sessionID\":\"s\"}}\n\ndata: {\"type\":\"b\"".to_owned();
        let evs = take_events(&mut buf);
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0]["type"], "a");
        buf.push_str("}\n\n");
        assert_eq!(take_events(&mut buf)[0]["type"], "b");
        assert_eq!(rules("full-access", None), json!([{ "action": "*", "resource": "*", "effect": "allow" }]));
        assert_eq!(rules("auto-accept-edits", None)[1]["effect"], "allow");
        assert_eq!(rules("approval-required", None)[1]["effect"], "ask");
        assert_eq!(model_ref("ollama/qwen3.5:4b"), Some(json!({ "providerID": "ollama", "id": "qwen3.5:4b" })));
        assert_eq!(model_ref("nope"), None);
        assert_eq!(request_kind("shell"), "command");
    }

    #[test]
    fn each_thread_allows_only_its_own_mcp_server() {
        assert_eq!(mcp_server_name("thread:1/é"), "t3-code-thread_1__");
        let long = mcp_server_name(&"x".repeat(80));
        assert_eq!(long.len(), "t3-code-".len() + 16);
        assert!(long.starts_with("t3-code-") && long[8..].chars().all(|c| c.is_ascii_hexdigit()));
        let r = rules("full-access", Some("t3-code-t1"));
        assert_eq!(r[1], json!({ "action": "t3-code-*", "resource": "*", "effect": "deny" }));
        assert_eq!(r[2], json!({ "action": "t3-code-t1_*", "resource": "*", "effect": "allow" }));
    }

    #[test]
    fn forms_are_answered_as_opencode_takes_them() {
        let form = Form::read("f", "s", &json!({ "fields": [
            { "type": "string", "key": "scope", "options": [{ "value": "all" }] },
            { "type": "multiselect", "key": "tags", "options": [{ "value": "a" }, { "value": "b" }] },
            { "type": "string", "key": "notes", "custom": true },
        ] })).unwrap();
        let answers: super::super::Answers = serde_json::from_value(json!({ "scope": ["all"], "tags": ["a", " ", "b"], "notes": ["one", "two"] })).unwrap();
        assert_eq!(form.reply(&answers), json!({ "answer": { "scope": "all", "tags": ["a", "b"], "notes": "one, two" } }));
        let blank: super::super::Answers = serde_json::from_value(json!({ "notes": [""] })).unwrap();
        assert_eq!(form.reply(&blank), json!({ "answer": {} }));
        assert!(Form::read("f", "s", &json!({ "fields": [{ "type": "external", "key": "url" }] })).is_none());
    }

    #[test]
    fn subagent_answers_lose_their_envelope_and_limits_are_classified() {
        assert_eq!(subagent_output("<subagent id=\"ses_2\">\nFound it.\n</subagent>"), "Found it.");
        assert_eq!(subagent_output("<subagent>x</subagent>"), "x");
        assert_eq!(subagent_output("<subagents>x</subagents>"), "<subagents>x</subagents>");
        assert_eq!(subagent_output("plain"), "plain");
        assert!(is_usage_limit(&json!({ "type": "provider.rate-limit", "message": "slow down" })));
        assert!(is_usage_limit(&json!({ "type": "provider.quota", "status": 429, "message": "" })));
        assert!(!is_usage_limit(&json!({ "type": "provider.quota", "status": 402, "message": "pay" })));
        assert!(!is_usage_limit(&json!({ "type": "provider.overloaded", "message": "" })));
    }

    #[test]
    fn workspace_limits_become_windows() {
        let at = Timestamp::from("2026-11-24T00:00:00.000Z");
        let meter = |percent: f64, limited: bool| json!({ "percent": percent, "limited": limited, "resetsAt": 1_800_000_000_000_i64 });
        let body = json!({ "location": { "directory": "/w" }, "data": {
            "workspace": { "id": "wrk_1", "name": " Team ", "usageURL": "https://opencode.ai/workspace/wrk_1/usage" },
            "go": { "rolling": meter(12.5, false), "weekly": meter(40.0, true), "monthly": meter(140.0, false) },
            "budgets": [
                { "id": "b1", "limit": 100, "spent": 25, "scope": "each", "per": "set", "coverage": "all", "models": [], "shared": false, "resetsAt": 1_800_000_000_000_i64 },
                { "id": "b2", "limit": 10, "spent": 5, "scope": "combined", "per": "model", "coverage": "models", "shared": true, "resetsAt": 1_800_000_000_000_i64,
                  "models": [{ "providerID": "opencode", "modelID": "m1", "name": "M1", "spent": 9 }, { "providerID": "opencode", "modelID": "m2", "name": "M2", "spent": 1 }] },
                { "id": "b3", "limit": 0, "spent": 0, "scope": "each", "per": "set", "coverage": "models", "shared": false, "resetsAt": 1_800_000_000_000_i64,
                  "models": [{ "providerID": "opencode", "modelID": "m3", "name": "M3" }] },
            ],
        } });
        let limits = usage_limits_from(&body, &["opencode".to_owned()], at.clone()).unwrap();
        let windows: Vec<_> = limits.windows.iter().map(|w| (w.id.as_str(), w.label.as_str(), w.used_percent.as_f64().unwrap())).collect();
        assert_eq!(windows, [
            ("go_rolling", "Go \u{b7} Session", 12.5),
            ("go_weekly", "Go \u{b7} Weekly", 100.0),
            ("go_monthly", "Go \u{b7} Monthly", 100.0),
            ("budget_b1", "Console \u{b7} Individual \u{b7} All spend", 25.0),
            ("budget_b2_opencode_m1", "Console \u{b7} Shared \u{b7} M1", 90.0),
            ("budget_b2_opencode_m2", "Console \u{b7} Shared \u{b7} M2", 10.0),
            ("budget_b3", "Console \u{b7} Individual \u{b7} M3", 100.0),
        ]);
        assert_eq!(limits.windows.iter().next().unwrap().window_duration_mins, Some(300));
        assert_eq!(limits.windows.iter().nth(3).unwrap().provider_ids.as_deref(), Some(&["opencode".to_owned()][..]));
        assert_eq!(limits.external_usage.as_ref().map(|u| u.label.as_str()), Some("Team"));
        let none = usage_limits_from(&json!({ "data": null }), &[], at.clone()).unwrap();
        assert_eq!(none.unavailable.unwrap().reason, ServerProviderUsageUnavailableReason::Unsupported);
        let zen = usage_limits_from(&json!({ "data": { "workspace": { "id": "w", "usageURL": "javascript:x" }, "budgets": [] } }), &[], at.clone()).unwrap();
        assert!(zen.unavailable.is_some() && zen.external_usage.is_none());
        assert!(usage_limits_from(&json!({ "nope": 1 }), &[], at).is_none());
    }
}
