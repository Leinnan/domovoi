//! OpenCode adapter over `opencode serve` (the v2 HTTP API, t3code
//! `provider-opencode` v2). One server, started on loopback with a
//! generated password, serves every thread; its `/api/event` stream is
//! routed by session. A thread is an OpenCode session in the thread's
//! folder; a turn is a prompt, ended by `session.execution.*`.
//!
//! Permissions follow the runtime mode as t3code sets them (full access
//! allows everything; otherwise `shell` and `external_directory` ask, and
//! `edit` asks unless edits are auto-accepted); asked permissions become
//! approvals. Models are `providerID/modelID` slugs.

#![expect(clippy::disallowed_types, clippy::disallowed_macros, reason = "OpenCode's HTTP bodies and SSE events")]

use super::turn::{Turn, TurnEnd, WindowUse, Work, approval_kind};
use super::{Control, TurnInput};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use domovoi_model::typed::{FileChangeDetail, ModelCapabilities, ProviderSessionStatus, ProviderThreadStatus, ServerProviderModel, TurnItemStatus};
use tokio::sync::{Mutex, mpsc};

pub const OPENCODE_TOOL: crate::providers::Tool = crate::providers::Tool { program: "opencode", label: "OpenCode", env: "DOMOVOI_OPENCODE_BINARY" };

pub struct OpenCodeServer {
    base: String,
    auth: String,
    http: reqwest::Client,
    routes: Arc<Mutex<HashMap<String, mpsc::UnboundedSender<Value>>>>,
    child: Mutex<tokio::process::Child>,
}

/// The session's permission rules for a runtime mode.
pub fn rules(runtime_mode: &str) -> Value {
    if runtime_mode == "full-access" {
        return json!([{ "action": "*", "resource": "*", "effect": "allow" }]);
    }
    let edit = if matches!(runtime_mode, "auto-accept-edits" | "auto") { "allow" } else { "ask" };
    json!([
        { "action": "shell", "resource": "*", "effect": "ask" },
        { "action": "edit", "resource": "*", "effect": edit },
        { "action": "external_directory", "resource": "*", "effect": "ask" },
    ])
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
        let server = Arc::new(OpenCodeServer { base, auth, http, routes, child: Mutex::new(child) });
        let s = server.clone();
        tokio::spawn(async move { s.pump_events().await });
        Ok(server)
    }

    /// Reads `/api/event` for good (reconnecting), sending each session's
    /// events to its turn; a lost server tells every turn.
    async fn pump_events(self: Arc<Self>) {
        loop {
            match self.http.get(format!("{}/api/event", self.base)).header("authorization", &self.auth).header("accept", "text/event-stream").send().await {
                Ok(mut res) if res.status().is_success() => {
                    let mut buf = String::new();
                    while let Ok(Some(chunk)) = res.chunk().await {
                        buf.push_str(&String::from_utf8_lossy(&chunk));
                        for ev in take_events(&mut buf) {
                            let sid = ev["data"]["sessionID"].as_str().map(str::to_owned);
                            let Some(sid) = sid else { continue };
                            let tx = self.routes.lock().await.get(&sid).cloned();
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
        let mut req = self.http.request(method, format!("{}{path}", self.base)).header("authorization", &self.auth);
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

    /// Runs one turn to completion, recording it on `turn`.
    pub async fn run_turn(&self, mut turn: Turn, input: TurnInput, mut controls: mpsc::UnboundedReceiver<Control>) -> Turn {
        let slug = input.model_selection.model.clone();
        turn.session(ProviderSessionStatus::Ready, &input.cwd, &slug, None);
        let model = model_ref(&slug);
        let session = match turn.native_thread_id() {
            Some(id) => {
                // Settings may have changed since the last turn.
                let _ = self.call(reqwest::Method::PATCH, &format!("/api/session/{id}"), Some(json!({ "permissions": rules(&input.runtime_mode) }))).await;
                if let Some(m) = &model {
                    let _ = self.call(reqwest::Method::POST, &format!("/api/session/{id}/model"), Some(json!({ "model": m }))).await;
                }
                id
            }
            None => {
                let mut body = json!({ "location": { "directory": input.cwd }, "permissions": rules(&input.runtime_mode) });
                if let Some(m) = &model {
                    body["model"] = m.clone();
                }
                match self.call(reqwest::Method::POST, "/api/session", Some(body)).await {
                    Ok(r) => r["data"]["id"].as_str().unwrap_or_default().to_owned(),
                    Err(e) => {
                        turn.complete(TurnEnd::Failed, Some(&format!("Could not start an OpenCode session: {e}")));
                        return turn;
                    }
                }
            }
        };
        #[expect(clippy::disallowed_methods, reason = "filled by the event pump every session shares: one slow turn must not hold up the others")]
        let (tx, mut events) = mpsc::unbounded_channel();
        self.routes.lock().await.insert(session.clone(), tx);
        turn.thread_status(ProviderThreadStatus::Active, Some(&session));
        let files: Vec<Value> = input.images.iter().map(|(_, p)| json!({ "uri": format!("file://{}", p.display()), "name": p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default() })).collect();
        let mut prompt = json!({ "text": input.text });
        if !files.is_empty() {
            prompt["files"] = Value::Array(files);
        }
        match self.call(reqwest::Method::POST, &format!("/api/session/{session}/prompt"), Some(prompt)).await {
            Ok(r) => turn.turn_started(r["data"]["id"].as_str().unwrap_or(&session)),
            Err(e) => {
                self.routes.lock().await.remove(&session);
                turn.complete(TurnEnd::Failed, Some(&format!("OpenCode refused the message: {e}")));
                return turn;
            }
        }

        let mut texts: HashMap<String, String> = HashMap::new();
        let mut tools: HashMap<String, (String, Value)> = HashMap::new();
        // t3code request id -> OpenCode permission id.
        let mut asked: HashMap<String, String> = HashMap::new();
        loop {
            tokio::select! {
                Some(c) = controls.recv() => match c {
                    Control::Respond { request_id, decision, .. } => {
                        if let Some(pid) = asked.remove(&request_id) {
                            let d = match decision.as_str() { "acceptForSession" | "acceptAlways" => "always", d if super::turn::decision_allows(d) => "once", _ => "reject" };
                            let _ = self.call(reqwest::Method::POST, &format!("/api/session/{session}/permission/{pid}/reply"), Some(json!({ "decision": d }))).await;
                            turn.resolve_request(&request_id, &decision, None);
                        }
                    }
                    Control::Interrupt => {
                        let _ = self.call(reqwest::Method::POST, &format!("/api/session/{session}/interrupt"), None).await;
                    }
                    Control::Steer { text, reply, .. } => {
                        // A prompt while it works is delivered into the running execution.
                        let r = self.call(reqwest::Method::POST, &format!("/api/session/{session}/prompt"), Some(json!({ "text": text }))).await;
                        let _ = reply.send(r.map(|_| ()));
                    }
                    Control::StopSubagent { reply, .. } => {
                        let _ = reply.send(Err("OpenCode subagents can't be stopped one by one.".into()));
                    }
                },
                Some(ev) = events.recv() => {
                    let d = &ev["data"];
                    let ty = ev["type"].as_str().unwrap_or_default();
                    let native = |kind: &str| format!("{}:{kind}:{}", d["assistantMessageID"].as_str().unwrap_or(""), d["ordinal"].as_u64().unwrap_or(0));
                    match ty {
                        "session.text.started" => { texts.insert(native("text"), String::new()); }
                        "session.text.delta" => {
                            let id = native("text");
                            let t = texts.entry(id.clone()).or_default();
                            t.push_str(d["delta"].as_str().unwrap_or_default());
                            let t = t.clone();
                            turn.assistant(&id, &t, true, None);
                        }
                        "session.text.ended" => {
                            let id = native("text");
                            let t = d["text"].as_str().map(str::to_owned).or_else(|| texts.remove(&id)).unwrap_or_default();
                            turn.assistant(&id, &t, false, None);
                        }
                        "session.reasoning.delta" => {
                            let id = native("reasoning");
                            let t = texts.entry(id.clone()).or_default();
                            t.push_str(d["delta"].as_str().unwrap_or_default());
                            let t = t.clone();
                            let len = t.len();
                            turn.item_streaming(&id, Work::Reasoning { text: t, streaming: true }, len);
                        }
                        "session.reasoning.ended" => {
                            let id = native("reasoning");
                            let t = d["text"].as_str().map(str::to_owned).or_else(|| texts.remove(&id)).unwrap_or_default();
                            turn.item(&id, Work::Reasoning { text: t, streaming: false }, TurnItemStatus::Completed);
                        }
                        "session.tool.input.started" => {
                            if let Some(id) = d["id"].as_str() {
                                tools.insert(id.to_owned(), (d["name"].as_str().unwrap_or("tool").to_owned(), Value::Null));
                            }
                        }
                        "session.tool.called" => {
                            let id = d["id"].as_str().unwrap_or_default().to_owned();
                            let name = tools.get(&id).map(|t| t.0.clone()).or_else(|| d["name"].as_str().map(str::to_owned)).unwrap_or_else(|| "tool".into());
                            tools.insert(id.clone(), (name.clone(), d["input"].clone()));
                            record_tool(&mut turn, &id, &name, &d["input"], None, TurnItemStatus::Running);
                        }
                        "session.tool.success" | "session.tool.failed" => {
                            let id = d["id"].as_str().unwrap_or_default().to_owned();
                            let (name, input) = tools.get(&id).cloned().unwrap_or_else(|| ("tool".into(), Value::Null));
                            let output = if ty == "session.tool.failed" {
                                d["error"]["message"].as_str().unwrap_or("The tool failed.").to_owned()
                            } else {
                                d["content"].as_array().into_iter().flatten().filter_map(|c| c["text"].as_str()).collect::<Vec<_>>().join("\n")
                            };
                            let failed = ty == "session.tool.failed" || d["metadata"]["exit"].as_i64().is_some_and(|c| c != 0);
                            record_tool(&mut turn, &id, &name, &input, Some((&output, d["metadata"]["exit"].as_i64(), failed)), if failed { TurnItemStatus::Failed } else { TurnItemStatus::Completed });
                        }
                        "permission.asked" => {
                            let pid = d["id"].as_str().unwrap_or_default().to_owned();
                            let action = d["action"].as_str().unwrap_or("permission");
                            let resources: Vec<&str> = d["resources"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
                            let prompt = if resources.is_empty() { format!("Allow {action}") } else { resources.join("\n") };
                            let item = d["source"]["id"].as_str().unwrap_or(&pid).to_owned();
                            let rid = turn.open_request(&pid, &item, approval_kind(request_kind(action)), &prompt);
                            asked.insert(rid, pid);
                        }
                        "form.created" => {
                            // Questions are not shown yet: cancel, so OpenCode doesn't wait for ever.
                            if let Some(fid) = d["id"].as_str().or(d["form"]["id"].as_str()) {
                                let _ = self.call(reqwest::Method::DELETE, &format!("/api/session/{session}/form/{fid}"), None).await;
                            }
                        }
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
                        "session.execution.succeeded" => {
                            turn.complete(TurnEnd::Completed, None);
                            break;
                        }
                        "session.execution.interrupted" => {
                            turn.complete(TurnEnd::Interrupted, None);
                            break;
                        }
                        "session.execution.failed" => {
                            let e = &d["error"];
                            let msg = e["message"].as_str().or(e["data"]["message"].as_str()).or(e.as_str()).unwrap_or("OpenCode failed.").to_owned();
                            turn.complete(TurnEnd::Failed, Some(&msg));
                            break;
                        }
                        "domovoi.server.exited" => {
                            turn.complete(TurnEnd::Failed, Some("The OpenCode server exited."));
                            break;
                        }
                        _ => {}
                    }
                }
                else => break,
            }
        }
        self.routes.lock().await.remove(&session);
        turn
    }
}

/// An OpenCode tool call as a turn item: shell commands, file edits, or a
/// generic tool. `result`: (output, exit code, failed).
fn record_tool(turn: &mut Turn, id: &str, name: &str, input: &Value, result: Option<(&str, Option<i64>, bool)>, status: TurnItemStatus) {
    let work = match name {
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
            Work::FileChange { file_name, changes, texts, patch: None }
        }
        _ => Work::Tool { name: name.to_owned(), input: input.clone(), output: result.map(|(out, _, _)| Value::String(out.to_owned())), viewed_image_path: None },
    };
    turn.item(id, work, status);
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
        assert_eq!(rules("full-access"), json!([{ "action": "*", "resource": "*", "effect": "allow" }]));
        assert_eq!(rules("auto-accept-edits")[1]["effect"], "allow");
        assert_eq!(rules("approval-required")[1]["effect"], "ask");
        assert_eq!(model_ref("ollama/qwen3.5:4b"), Some(json!({ "providerID": "ollama", "id": "qwen3.5:4b" })));
        assert_eq!(model_ref("nope"), None);
        assert_eq!(request_kind("shell"), "command");
    }
}
