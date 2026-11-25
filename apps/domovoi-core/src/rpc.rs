//! Client side of Effect RPC (JSON serialization) as t3code's client-runtime
//! speaks it: numbered requests, chunk acknowledgements, interrupts, and
//! frameParts reassembly.

#![expect(clippy::disallowed_types, clippy::disallowed_macros, reason = "the RPC framing: a frame is read as JSON and taken apart, and a failure's cause is kept for its message")]

use effect_rpc::message::{FromServer, RequestId};
use serde::Serialize;
use serde_json::{Value, json};
use std::collections::HashMap;

/// The most a frame sent in parts may hold (a long thread's history is a
/// few megabytes).
const MAX_FRAME_BYTES: usize = 256 * 1024 * 1024;

/// Where a request's replies go.
#[derive(Debug, Clone, PartialEq)]
pub enum Route {
    // Subscriptions
    Config,
    Lifecycle,
    Shell,
    Thread(String),
    VcsStatus(String),
    Terminal { thread_id: String, terminal_id: String },
    TerminalMetadata,
    ScheduledTasks,
    /// A provider instance's sign-in (`provider.auth.subscribe`) and managed
    /// install (`provider.install.subscribe`), by instance id.
    ProviderAuth(String),
    ProviderInstall(String),
    AuthAccess,
    DatabaseStorage,
    GitAction(String),
    // Unary calls
    /// Fire-and-forget: a failure shows `label` with the server's message.
    Command(String),
    Launch(String),
    /// `message.dispatch` from the composer, by command id.
    Send(String),
    ProjectCreated,
    ProjectIconChanged(String),
    ProjectIconAsset { project_id: String, request: String },
    ProjectIconFiles(String),
    ProjectIconThumbnail { path: String, request: String },
    /// `assets.createUrl` for a Mermaid diagram, by the shell's key.
    Diagram { key: String, request: String },
    AgentScan,
    /// `project.create` for an import, then its sessions.
    ImportProject { path: String, project_id: String },
    ImportSessions(String),
    Browse(u32),
    SearchThreads(u32),
    FindInThread(u32),
    Mention(u32),
    FileSearch(u32),
    /// A palette search (`files` or `content`), by its revision.
    PaletteSearch(u32),
    ListEntries(String),
    ReadFile { path: String, request: String },
    /// `projects.fetchLfsObject` for the open preview (its `request`).
    FetchLfs { path: String, request: String },
    ReadArtifactFile(String),
    /// Active artifact workspace file read (for `panel.artifacts.file_read`).
    ReadPanelArtifactFile(String),
    /// `assets.createUrl` for an active artifact preview (multi-file bundle, React runtime, Mermaid SVG).
    ArtifactPreviewUrl(String),
    /// A diff for the panel, by its source, and whose it is (`diff_owner`):
    /// an answer for a thread or checkout since left is not shown.
    Diff { source: String, of: String },
    ListRefs(String),
    Attachments(Vec<String>),
    /// `assets.createUrl` for an attachment, by attachment id.
    AssetUrl(String),
    Settings,
    Keybindings,
    Diagnostics,
    HostResources,
    CleanupReport,
    PullRequests,
    PullRequestDetail(String),
    PullRequestChecks(String),
    /// Merge, close, review, comment…: `done` is the toast once it worked.
    PullRequestAction { key: String, done: String },
    /// A pull request checked out in a worktree for a new thread.
    PullRequestThread(String),
    Usage,
    UsageLimits,
    Archived,
    /// A webhook task's deliveries, by task id.
    WebhookDeliveries(String),
    /// Scheduled tasks' runs: one task's, or (`None`) every task's latest.
    ScheduledRuns(Option<String>),
    /// A scheduled task run by hand, then the run it started (to open its thread).
    ScheduledRunNow(String),
    ScheduledRunStarted(String),
    /// A thread's allowed tools forgotten (`orchestration.forgetThreadPermissions`).
    ThreadPermissionsForgotten,
    /// `secrets.answerRequest` for a `secret_request` item (`saved`: with a
    /// value, else declined).
    SecretAnswer { item_id: String, saved: bool },
}

#[derive(Debug, Default)]
pub struct RpcClient {
    next_id: u64,
    pending: HashMap<String, Route>,
    parts: String,
}

/// A decoded server message with its request's purpose.
#[derive(Debug)]
pub enum Reply {
    Chunk { id: String, route: Route, values: Vec<Value> },
    Exit { route: Route, ok: bool, value: Value },
    Defect(Value),
}

fn id_string(id: &RequestId) -> String {
    id.to_string()
}

/// A `Request` frame: the payload is written straight into it.
#[derive(Serialize)]
struct RequestFrame<'a, P: ?Sized> {
    #[serde(rename = "_tag")]
    frame: &'static str,
    id: &'a str,
    tag: &'a str,
    payload: &'a P,
    headers: [(); 0],
}

impl RpcClient {
    /// A `Request` frame; returns (request id, frame text). This is where a
    /// request's payload is serialized, once.
    pub fn request<P: Serialize + ?Sized>(&mut self, tag: &str, payload: &P, route: Route) -> (String, String) {
        self.next_id += 1;
        let id = self.next_id.to_string();
        self.pending.insert(id.clone(), route);
        let frame = RequestFrame { frame: "Request", id: &id, tag, payload, headers: [] };
        let text = serde_json::to_string(&frame).expect("a request is plain data");
        (id, text)
    }

    pub fn ack(id: &str) -> String {
        json!({ "_tag": "Ack", "requestId": id }).to_string()
    }

    /// Routes of every open request.
    pub fn routes(&self) -> Vec<Route> {
        self.pending.values().cloned().collect()
    }

    /// Whether a request with this route is still open.
    pub fn has(&self, route: &Route) -> bool {
        self.pending.values().any(|r| r == route)
    }

    /// The open request id for `route`, if any.
    pub fn id_of(&self, route: &Route) -> Option<String> {
        self.pending.iter().find(|(_, r)| *r == route).map(|(id, _)| id.clone())
    }

    /// Interrupts a subscription and forgets it.
    pub fn interrupt(&mut self, id: &str) -> String {
        self.pending.remove(id);
        json!({ "_tag": "Interrupt", "requestId": id }).to_string()
    }

    /// Stops waiting for a request's answer (a later one is ignored).
    pub fn forget(&mut self, id: &str) {
        self.pending.remove(id);
    }

    pub fn reset(&mut self) {
        self.pending.clear();
        self.parts.clear();
    }

    /// Decodes one WebSocket text frame (joining `frameParts` if present).
    pub fn receive(&mut self, text: &str) -> Vec<Reply> {
        let joined;
        let whole = if let Some(part) = text.strip_prefix('\u{1e}') {
            // A frame that never ends must not grow without bound: past the
            // limit its parts are dropped (the request it answers times out).
            if self.parts.len() + part.len() > MAX_FRAME_BYTES {
                self.parts = String::new();
                return vec![];
            }
            self.parts.push_str(part);
            return vec![];
        } else if self.parts.is_empty() {
            text
        } else {
            joined = std::mem::take(&mut self.parts) + text;
            &joined
        };
        let Ok(value) = serde_json::from_str::<Value>(whole) else { return vec![] };
        let messages: Vec<Value> = match value {
            Value::Array(a) => a,
            v => vec![v],
        };
        messages
            .into_iter()
            .filter_map(from_server)
            .filter_map(|m| match m {
                FromServer::Chunk { request_id, values } => {
                    let id = id_string(&request_id);
                    let route = self.pending.get(&id)?.clone();
                    Some(Reply::Chunk { id, route, values })
                }
                FromServer::Exit { request_id, exit } => {
                    let route = self.pending.remove(&id_string(&request_id))?;
                    // Interrupted by the server because the connection is
                    // closing (signed out, server stopping): nothing to report.
                    if let effect_rpc::ExitEncoded::Failure { cause } = &exit
                        && let Ok(cause) = serde_json::to_value(cause)
                        && cause.as_array().is_some_and(|c| !c.is_empty() && c.iter().all(|e| e["_tag"] == "Interrupt"))
                    {
                        return None;
                    }
                    let (ok, value) = match exit {
                        effect_rpc::ExitEncoded::Success { value } => (true, value),
                        effect_rpc::ExitEncoded::Failure { cause } => (false, serde_json::to_value(cause).unwrap_or(Value::Null)),
                    };
                    Some(Reply::Exit { route, ok, value })
                }
                FromServer::Defect { defect } => Some(Reply::Defect(defect)),
                _ => None,
            })
            .collect()
    }
}

/// The server message that parsed JSON holds, or `None` if it is not one.
///
/// A stream's chunk, which nearly every frame is, is taken apart where it
/// is, so that the values it carries (a thread's records, with their text)
/// are handed on as they were parsed and not built a second time.
fn from_server(message: Value) -> Option<FromServer> {
    match message {
        Value::Object(mut fields) if fields.get("_tag").and_then(Value::as_str) == Some("Chunk") => {
            let request_id = serde_json::from_value(fields.remove("requestId")?).ok()?;
            match fields.remove("values")? {
                Value::Array(values) => Some(FromServer::Chunk { request_id, values }),
                _ => None,
            }
        }
        other => serde_json::from_value(other).ok(),
    }
}

/// A readable message from a failure cause (`[{_tag: Fail, error: {message}}]`).
pub fn failure_message(cause: &Value) -> String {
    cause.as_array().and_then(|c| c.first()).and_then(|c| {
        c["error"]["message"].as_str().or(c["error"]["cause"].as_str()).or(c["defect"].as_str()).map(str::to_owned)
    }).unwrap_or_else(|| "The server rejected the request.".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_chunks_and_exits_and_joins_parts() {
        let mut c = RpcClient::default();
        let (id, frame) = c.request("orchestration.subscribeShell", &json!({}), Route::Shell);
        assert_eq!(serde_json::from_str::<Value>(&frame).unwrap(), json!({ "_tag": "Request", "id": id, "tag": "orchestration.subscribeShell", "payload": {}, "headers": [] }));
        let chunk = json!({ "_tag": "Chunk", "requestId": id, "values": [{ "kind": "synchronized" }] }).to_string();
        let (a, b) = chunk.split_at(10);
        assert!(c.receive(&format!("\u{1e}{a}")).is_empty());
        match &c.receive(b)[..] {
            [Reply::Chunk { route: Route::Shell, values, .. }] => assert_eq!(values[0]["kind"], "synchronized"),
            other => panic!("{other:?}"),
        }
        let (cid, _) = c.request("orchestration.dispatchCommand", &json!({}), Route::Command("send".into()));
        let exit = json!({ "_tag": "Exit", "requestId": cid, "exit": { "_tag": "Failure", "cause": [{ "_tag": "Fail", "error": { "message": "nope" } }] } });
        match &c.receive(&exit.to_string())[..] {
            [Reply::Exit { ok: false, value, .. }] => assert_eq!(failure_message(value), "nope"),
            other => panic!("{other:?}"),
        }
        assert!(c.receive(&exit.to_string()).is_empty(), "exits are delivered once");
        // A request the server interrupted because the socket is closing.
        let (iid, _) = c.request("orchestration.subscribeShell", &json!({}), Route::Shell);
        let interrupted = json!({ "_tag": "Exit", "requestId": iid, "exit": { "_tag": "Failure", "cause": [{ "_tag": "Interrupt", "fiberId": 1 }] } });
        assert!(c.receive(&interrupted.to_string()).is_empty(), "no error for an interruption");
    }

    /// A chunk taken apart by hand is the chunk `FromServer` decodes, and what
    /// is no chunk to the one is none to the other.
    #[test]
    fn chunks_are_read_as_from_server_reads_them() {
        let messages = [
            json!({ "_tag": "Chunk", "requestId": "7", "values": [{ "kind": "synchronized" }, 3, null] }),
            json!({ "_tag": "Chunk", "requestId": 7, "values": [], "more": true }),
            json!({ "_tag": "Chunk", "requestId": 7 }),
            json!({ "_tag": "Chunk", "requestId": 7, "values": null }),
            json!({ "_tag": "Chunk", "requestId": 7, "values": { "kind": "synchronized" } }),
            json!({ "_tag": "Chunk", "requestId": 7.5, "values": [] }),
            json!({ "_tag": "Chunk", "requestId": null, "values": [] }),
            json!({ "_tag": "Chunk", "values": [] }),
            json!({ "_tag": "Pong" }),
            json!({ "_tag": "Defect", "defect": "boom" }),
            json!({ "_tag": 4, "requestId": 7, "values": [] }),
            json!(["Chunk"]),
        ];
        for message in messages {
            assert_eq!(from_server(message.clone()), serde_json::from_value::<FromServer>(message.clone()).ok(), "{message}");
        }
    }
}
