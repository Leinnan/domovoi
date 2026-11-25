//! The `claude` CLI's stream-json protocol, spoken directly (t3code reaches it
//! through `@anthropic-ai/claude-agent-sdk`; there is no Rust SDK).
//!
//! stdin:  `control_request` (initialize, interrupt, set_permission_mode, ...),
//!         `user` messages, and `control_response`s answering the CLI's requests.
//! stdout: `system`/`assistant`/`user`/`stream_event`/`result` messages, plus
//!         `control_request` (`can_use_tool`) and `control_response`s.
//! Wire shapes were taken from a recorded session (fixtures/golden/ts-session1/claude-cli.*).

#![expect(clippy::disallowed_types, clippy::disallowed_macros, reason = "the Claude CLI's stream-json and control messages")]

use serde_json::{Value, json};
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin};
use tokio::sync::{Mutex, mpsc, oneshot};

#[derive(Debug, Clone)]
pub struct LaunchOptions {
    pub binary: String,
    pub cwd: String,
    pub model: String,
    pub session_id: String,
    /// Resume an existing CLI session instead of starting `session_id` fresh.
    pub resume: bool,
    /// When resuming, continue from this assistant message (drops later turns:
    /// the Agent SDK's `resumeSessionAt`, used for conversation rollback).
    pub resume_at: Option<String>,
    /// `default` (ask), `acceptEdits`, `bypassPermissions`, or `plan`.
    pub permission_mode: String,
    pub effort: Option<String>,
    pub extra_dirs: Vec<String>,
    /// Start `session_id` as a copy of this session (`--fork-session`),
    /// up to `resume_at` when set.
    pub fork_from: Option<String>,
    /// `--mcp-config` JSON, and tools allowed without asking.
    pub mcp_config: Option<String>,
    pub allowed_tools: Vec<String>,
    /// Added to Claude Code's own system prompt (`--append-system-prompt`).
    pub append_system_prompt: Option<String>,
    /// Sets `CLAUDE_CODE_ARTIFACTS=1` so Claude Code exposes its native
    /// `Artifact` / `ArtifactComments` / `ArtifactData` tools in stream-json mode.
    pub enable_artifacts: bool,
}

impl LaunchOptions {
    pub fn args(&self) -> Vec<String> {
        let mut a: Vec<String> = [
            "--output-format", "stream-json", "--verbose", "--input-format", "stream-json",
            "--permission-prompt-tool=stdio", "--include-partial-messages",
            // Each user message comes back (`isReplay`, same `uuid`) when Claude
            // takes it: what came before belongs to an earlier turn.
            "--replay-user-messages",
            // Lets Domovoi switch to `bypassPermissions` later (full access after
            // planning, or a runtime mode change); the mode itself stays what
            // `--permission-mode` and `set_permission_mode` say.
            "--allow-dangerously-skip-permissions",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        a.push(format!("--model={}", self.model));
        a.push(format!("--permission-mode={}", self.permission_mode));
        if let Some(e) = &self.effort {
            a.push(format!("--effort={e}"));
        }
        a.push(format!("--add-dir={}", self.cwd));
        for d in &self.extra_dirs {
            a.push(format!("--add-dir={d}"));
        }
        if let Some(c) = &self.mcp_config {
            a.push(format!("--mcp-config={c}"));
        }
        if !self.allowed_tools.is_empty() {
            a.push(format!("--allowedTools={}", self.allowed_tools.join(",")));
        }
        if let Some(p) = &self.append_system_prompt {
            a.push(format!("--append-system-prompt={p}"));
        }
        if let Some(source) = &self.fork_from {
            a.push(format!("--resume={source}"));
            a.push("--fork-session".into());
            a.push(format!("--session-id={}", self.session_id));
            if let Some(at) = &self.resume_at {
                a.push(format!("--resume-session-at={at}"));
            }
        } else if self.resume {
            a.push(format!("--resume={}", self.session_id));
            if let Some(at) = &self.resume_at {
                a.push(format!("--resume-session-at={at}"));
            }
        } else {
            a.push(format!("--session-id={}", self.session_id));
        }
        a
    }
}

/// Messages from the CLI, minus control responses (resolved internally).
#[derive(Debug, Clone)]
pub enum CliEvent {
    /// Any `system` / `assistant` / `user` / `stream_event` / `result` / other message.
    Message(Value),
    /// `can_use_tool`: answer with [`ClaudeCli::answer_permission`].
    PermissionRequest { request_id: String, request: Value },
    Exited,
}

/// Writes one protocol log line: (`direction`, message).
type ProtocolLog = Arc<dyn Fn(&str, &Value) + Send + Sync>;

pub struct ClaudeCli {
    stdin: Mutex<ChildStdin>,
    child: Mutex<Child>,
    /// What the CLI starts ends with it (Windows: a `.cmd` shim's `node`).
    _tree: crate::process::Tree,
    pending: Arc<Mutex<HashMap<String, oneshot::Sender<Value>>>>,
    /// The session's protocol log (`direction`, message).
    log: ProtocolLog,
}

fn request_id() -> String {
    crate::util::new_uuid().replace('-', "")[..12].to_owned()
}

impl ClaudeCli {
    pub fn spawn(
        opts: &LaunchOptions,
        log: impl Fn(&str, &Value) + Send + Sync + 'static,
    ) -> std::io::Result<(Arc<Self>, mpsc::UnboundedReceiver<CliEvent>)> {
        let mut cmd = crate::process::command(&opts.binary);
        cmd.args(opts.args())
            .current_dir(&opts.cwd)
            .env("CLAUDE_CODE_ENTRYPOINT", "sdk-ts")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        if opts.enable_artifacts {
            cmd.env("CLAUDE_CODE_ARTIFACTS", "1");
        }
        let mut child = cmd.spawn()?;
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        #[expect(clippy::disallowed_methods, reason = "the reader also delivers the answers the turn waits for: made to wait for room here, it could never read the answer that frees the turn")]
        let (tx, rx) = mpsc::unbounded_channel();
        let pending: Arc<Mutex<HashMap<String, oneshot::Sender<Value>>>> = Arc::default();
        let log: ProtocolLog = Arc::new(log);
        let tree = crate::process::Tree::of(child.id());
        let cli = Arc::new(ClaudeCli { stdin: Mutex::new(stdin), child: Mutex::new(child), _tree: tree, pending: pending.clone(), log: log.clone() });
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
                log("incoming", &msg);
                match msg["type"].as_str() {
                    Some("control_response") => {
                        let id = msg["response"]["request_id"].as_str().unwrap_or_default().to_owned();
                        if let Some(w) = pending.lock().await.remove(&id) {
                            let _ = w.send(msg["response"].clone());
                        }
                    }
                    Some("control_request") if msg["request"]["subtype"] == "can_use_tool" => {
                        let _ = tx.send(CliEvent::PermissionRequest {
                            request_id: msg["request_id"].as_str().unwrap_or_default().to_owned(),
                            request: msg["request"].clone(),
                        });
                    }
                    _ => {
                        let _ = tx.send(CliEvent::Message(msg));
                    }
                }
            }
            // Fail control requests still waiting: their answers will never come.
            pending.lock().await.clear();
            let _ = tx.send(CliEvent::Exited);
        });
        Ok((cli, rx))
    }

    async fn write(&self, msg: &Value) -> std::io::Result<()> {
        // What was asked and answered (permission answers, mode switches);
        // user messages stay out, they can carry whole images.
        if matches!(msg["type"].as_str(), Some("control_request" | "control_response")) {
            (self.log)("outgoing", msg);
        }
        let mut line = serde_json::to_vec(msg).expect("serializable");
        line.push(b'\n');
        let mut s = self.stdin.lock().await;
        s.write_all(&line).await?;
        s.flush().await
    }

    /// Sends a control request and waits for its response.
    pub async fn control(&self, request: Value) -> std::io::Result<Value> {
        let id = request_id();
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id.clone(), tx);
        self.write(&json!({ "request_id": id, "type": "control_request", "request": request })).await?;
        rx.await.map_err(|_| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "claude exited"))
    }

    pub async fn initialize(&self) -> std::io::Result<Value> {
        self.control(json!({ "subtype": "initialize" })).await
    }

    /// Sends a user message; returns its `uuid` (which the replay echoes).
    pub async fn send_user_message(&self, text: &str) -> std::io::Result<String> {
        self.send_user_content(json!(text)).await
    }

    /// A user message with images: content blocks, the images first (as
    /// the Messages API recommends), then the text.
    pub async fn send_user_message_with_images(&self, text: &str, images: &[(String, std::path::PathBuf)]) -> std::io::Result<String> {
        use base64::Engine as _;
        let mut content = vec![];
        for (mime, path) in images {
            let bytes = std::fs::read(path)?;
            content.push(json!({ "type": "image", "source": { "type": "base64", "media_type": mime, "data": base64::engine::general_purpose::STANDARD.encode(bytes) } }));
        }
        if !text.is_empty() {
            content.push(json!({ "type": "text", "text": text }));
        }
        self.send_user_content(Value::Array(content)).await
    }

    /// A user message of several text blocks (a skill's command last), the images first.
    pub async fn send_user_blocks(&self, texts: &[String], images: &[(String, std::path::PathBuf)]) -> std::io::Result<String> {
        use base64::Engine as _;
        let mut content = vec![];
        for (mime, path) in images {
            let bytes = std::fs::read(path)?;
            content.push(json!({ "type": "image", "source": { "type": "base64", "media_type": mime, "data": base64::engine::general_purpose::STANDARD.encode(bytes) } }));
        }
        content.extend(texts.iter().map(|t| json!({ "type": "text", "text": t })));
        self.send_user_content(Value::Array(content)).await
    }

    async fn send_user_content(&self, content: Value) -> std::io::Result<String> {
        let uuid = crate::util::new_uuid();
        self.write(&json!({
            "type": "user",
            "message": { "role": "user", "content": content },
            "parent_tool_use_id": null,
            "uuid": uuid,
        }))
        .await?;
        Ok(uuid)
    }

    /// A message for the turn in progress (`priority: now`): Claude reads it
    /// at its next step rather than after the turn (t3code steerTurn).
    /// Returns the message's uuid, which Claude's `command_lifecycle` events name.
    pub async fn send_steer(&self, text: &str, images: &[(String, std::path::PathBuf)]) -> std::io::Result<String> {
        use base64::Engine as _;
        let mut content = vec![];
        for (mime, path) in images {
            let bytes = std::fs::read(path)?;
            content.push(json!({ "type": "image", "source": { "type": "base64", "media_type": mime, "data": base64::engine::general_purpose::STANDARD.encode(bytes) } }));
        }
        content.push(json!({ "type": "text", "text": text }));
        let uuid = crate::util::new_uuid();
        self.write(&json!({
            "type": "user",
            "message": { "role": "user", "content": content },
            "parent_tool_use_id": null,
            "priority": "now",
            "uuid": uuid,
        }))
        .await?;
        Ok(uuid)
    }

    pub async fn interrupt(&self) -> std::io::Result<Value> {
        self.control(json!({ "subtype": "interrupt" })).await
    }

    /// Switches the permission mode. Claude can refuse (`bypassPermissions`
    /// for a session launched without the flag that allows it): that is an
    /// error here too, with Claude's message.
    pub async fn set_permission_mode(&self, mode: &str) -> std::io::Result<()> {
        let response = self.control(json!({ "subtype": "set_permission_mode", "mode": mode })).await?;
        if response["subtype"] == "error" {
            let message = response["error"].as_str().unwrap_or("Claude refused the permission mode");
            return Err(std::io::Error::other(message.to_owned()));
        }
        Ok(())
    }

    /// Answers a `can_use_tool` request. Allow echoes the (unchanged) input, as
    /// the Agent SDK does; deny carries a message the model sees.
    pub async fn answer_permission(&self, request_id: &str, request: &Value, allow: bool, message: Option<&str>) -> std::io::Result<()> {
        let response = if allow {
            allow_response(request, false)
        } else {
            json!({
                "behavior": "deny",
                "message": message.unwrap_or("The user declined this tool call."),
                "toolUseID": request["tool_use_id"],
            })
        };
        self.write(&json!({
            "type": "control_response",
            "response": { "subtype": "success", "request_id": request_id, "response": response },
        }))
        .await
    }

    /// Allows a tool call and what it asked to be allowed for the rest of the
    /// session ("Allow for Session"): Claude asks no more for it.
    pub async fn answer_permission_for_session(&self, request_id: &str, request: &Value) -> std::io::Result<()> {
        self.write(&json!({
            "type": "control_response",
            "response": { "subtype": "success", "request_id": request_id, "response": allow_response(request, true) },
        }))
        .await
    }

    /// Allows a tool call with input of its own (a question's answers).
    pub async fn answer_permission_with_input(&self, request_id: &str, request: &Value, input: Value) -> std::io::Result<()> {
        let response = json!({
            "behavior": "allow",
            "updatedInput": input,
            "toolUseID": request["tool_use_id"],
            "decisionClassification": "user_temporary",
        });
        self.write(&json!({
            "type": "control_response",
            "response": { "subtype": "success", "request_id": request_id, "response": response },
        }))
        .await
    }

    pub async fn kill(&self) {
        let _ = self.child.lock().await.kill().await;
    }
}

/// What "Allow for Session" grants (t3code `toSessionPermissionUpdates`):
/// the request's own suggestions, kept for the session, else an allow rule
/// for the tool.
pub fn session_permission_updates(request: &Value) -> Vec<Value> {
    let suggested: Vec<Value> = request["permission_suggestions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|suggestion| suggestion.is_object())
        .map(|suggestion| {
            let mut suggestion = suggestion.clone();
            suggestion["destination"] = json!("session");
            suggestion
        })
        .collect();
    if !suggested.is_empty() {
        return suggested;
    }
    vec![json!({ "type": "addRules", "rules": [{ "toolName": request["tool_name"] }], "behavior": "allow", "destination": "session" })]
}

/// The answer that allows a `can_use_tool` request: once, or (`for_session`)
/// with what it suggested allowed for the rest of the session.
fn allow_response(request: &Value, for_session: bool) -> Value {
    let mut response = json!({
        "behavior": "allow",
        "updatedInput": request["input"],
        "toolUseID": request["tool_use_id"],
        "decisionClassification": if for_session { "user_permanent" } else { "user_temporary" },
    });
    if for_session {
        response["updatedPermissions"] = json!(session_permission_updates(request));
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allow_for_session_keeps_what_the_request_suggested_for_the_session() {
        let fetch = json!({ "tool_name": "WebFetch", "tool_use_id": "toolu_1", "input": { "url": "https://allegro.pl/x" },
            "permission_suggestions": [{ "type": "addRules", "rules": [{ "toolName": "WebFetch", "ruleContent": "domain:allegro.pl" }], "behavior": "allow", "destination": "localSettings" }] });
        let once = allow_response(&fetch, false);
        assert_eq!((once["decisionClassification"].as_str(), once.get("updatedPermissions")), (Some("user_temporary"), None));
        let session = allow_response(&fetch, true);
        assert_eq!(session["decisionClassification"], "user_permanent");
        assert_eq!(session["updatedInput"], fetch["input"]);
        assert_eq!(session["updatedPermissions"], json!([{ "type": "addRules", "rules": [{ "toolName": "WebFetch", "ruleContent": "domain:allegro.pl" }], "behavior": "allow", "destination": "session" }]));
        // Without suggestions: the tool, for the session.
        let search = json!({ "tool_name": "WebSearch", "tool_use_id": "toolu_2", "input": { "query": "ceny" } });
        assert_eq!(allow_response(&search, true)["updatedPermissions"], json!([{ "type": "addRules", "rules": [{ "toolName": "WebSearch" }], "behavior": "allow", "destination": "session" }]));
    }

    #[test]
    fn launch_args_match_the_recorded_sdk_invocation() {
        let opts = LaunchOptions {
            binary: "claude".into(), cwd: "/w".into(), model: "claude-haiku-5-5".into(),
            session_id: "s1".into(), resume: false, resume_at: None, permission_mode: "default".into(),
            effort: Some("medium".into()), extra_dirs: vec![], fork_from: None, mcp_config: None, allowed_tools: vec![], append_system_prompt: None, enable_artifacts: false,
        };
        let a = opts.args();
        let recorded = include_str!("../../../fixtures/golden/ts-session1/claude-cli.args.txt");
        for flag in ["--output-format", "stream-json", "--input-format", "--permission-prompt-tool=stdio",
                     "--include-partial-messages", "--model=claude-haiku-5-5", "--permission-mode=default", "--effort=medium"] {
            assert!(recorded.lines().any(|l| l == flag), "{flag} not in recorded args");
            assert!(a.iter().any(|x| x == flag), "{flag} missing");
        }
        assert!(a.contains(&"--session-id=s1".to_owned()));
        // Full access stays reachable after planning (t3code: allowDangerouslySkipPermissions).
        assert!(a.contains(&"--allow-dangerously-skip-permissions".to_owned()));
        let forked = LaunchOptions { fork_from: Some("src".into()), resume_at: Some("m-1".into()), ..opts.clone() }.args();
        assert!(forked.windows(4).any(|w| w == ["--resume=src", "--fork-session", &format!("--session-id={}", opts.session_id), "--resume-session-at=m-1"]), "{forked:?}");
        let resumed = LaunchOptions { resume: true, resume_at: Some("m-uuid".into()), ..opts }.args();
        assert!(resumed.contains(&"--resume=s1".to_owned()) && resumed.contains(&"--resume-session-at=m-uuid".to_owned()));
        assert!(!resumed.iter().any(|a| a.starts_with("--session-id")));
    }

    #[test]
    fn permission_answer_matches_recorded_shape() {
        let recorded: Value = include_str!("../../../fixtures/golden/ts-session1/claude-cli.stdin.ndjson")
            .lines()
            .map(|l| serde_json::from_str::<Value>(l).unwrap())
            .find(|m| m["type"] == "control_response")
            .unwrap();
        let r = &recorded["response"]["response"];
        assert_eq!(r["behavior"], "allow");
        for key in ["updatedInput", "toolUseID", "decisionClassification"] {
            assert!(r.get(key).is_some(), "{key}");
        }
    }
}
