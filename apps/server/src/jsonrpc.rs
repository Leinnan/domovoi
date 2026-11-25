//! Newline-delimited JSON-RPC over a child process's stdio, as spoken by
//! `codex app-server` (no `jsonrpc` field; ids are integers). Requests are
//! correlated by id; notifications and server-to-client requests are delivered
//! on a channel, and server requests are answered with [`RpcClient::respond`].

#![expect(clippy::disallowed_types, clippy::disallowed_macros, reason = "newline-delimited JSON-RPC of `codex app-server`: a result or an error is whatever Codex sent")]

use serde_json::{Value, json};
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin};
use tokio::sync::{Mutex, mpsc, oneshot};

#[derive(Debug, Clone)]
pub enum Incoming {
    Notification { method: String, params: Value },
    Request { id: Value, method: String, params: Value },
    /// The process exited or its stdout closed.
    Closed,
}

#[derive(Debug, thiserror::Error)]
pub enum RpcError {
    #[error("process I/O: {0}")]
    Io(#[from] std::io::Error),
    #[error("remote error: {0}")]
    Remote(Value),
    #[error("connection closed")]
    Closed,
}

type Pending = Arc<Mutex<HashMap<i64, oneshot::Sender<Result<Value, Value>>>>>;

/// A callback on one line of the child's output.
pub type LineHook = Box<dyn Fn(&str) + Send + Sync>;

/// How [`RpcClient::spawn_opts`] starts the child.
#[derive(Default)]
pub struct SpawnOptions {
    /// Variables set for the child, over those it inherits.
    pub env: Vec<(String, String)>,
    /// Inherited variables the child must not see.
    pub remove_env: Vec<String>,
    /// JSON-RPC 2.0 framing (`"jsonrpc": "2.0"` on every message).
    pub jsonrpc2: bool,
    /// Each line the child writes to stderr (else stderr is dropped).
    pub stderr: Option<LineHook>,
    /// Sees each stdout line first; `false` drops it (a line that is not the protocol's).
    pub stdout_line: Option<Box<dyn Fn(&str) -> bool + Send + Sync>>,
}

pub struct RpcClient {
    stdin: Mutex<ChildStdin>,
    /// JSON-RPC 2.0 framing (`"jsonrpc": "2.0"` on every message), as ACP agents speak it.
    jsonrpc2: bool,
    next_id: AtomicI64,
    pending: Pending,
    #[cfg_attr(not(test), allow(dead_code))]
    child: Mutex<Child>,
    /// What the agent starts ends with it (Windows: a `.cmd` shim's `node`).
    _tree: crate::process::Tree,
    /// The diagnostics log, for what this side answers and notifies.
    log: Arc<dyn Fn(&str, &Value) + Send + Sync>,
}

impl RpcClient {
    /// Spawns `program args...` and starts reading its stdout. Every received
    /// message, and every answer and notification sent, is passed to `log`
    /// (direction, message) for diagnostics. Requests sent are not: they
    /// carry prompts and sign-in details.
    pub fn spawn(
        program: &str,
        args: &[&str],
        cwd: Option<&str>,
        log: impl Fn(&str, &Value) + Send + Sync + 'static,
    ) -> Result<(Arc<Self>, mpsc::UnboundedReceiver<Incoming>), RpcError> {
        Self::spawn_with(program, args, cwd, &[], false, log)
    }

    /// [`RpcClient::spawn`] with environment variables for the child, and
    /// JSON-RPC 2.0 framing when `jsonrpc2`.
    pub fn spawn_with(
        program: &str,
        args: &[&str],
        cwd: Option<&str>,
        env: &[(String, String)],
        jsonrpc2: bool,
        log: impl Fn(&str, &Value) + Send + Sync + 'static,
    ) -> Result<(Arc<Self>, mpsc::UnboundedReceiver<Incoming>), RpcError> {
        Self::spawn_opts(program, args, cwd, SpawnOptions { env: env.to_vec(), jsonrpc2, ..SpawnOptions::default() }, log)
    }

    /// [`RpcClient::spawn_with`] with everything [`SpawnOptions`] can say.
    pub fn spawn_opts(
        program: &str,
        args: &[&str],
        cwd: Option<&str>,
        options: SpawnOptions,
        log: impl Fn(&str, &Value) + Send + Sync + 'static,
    ) -> Result<(Arc<Self>, mpsc::UnboundedReceiver<Incoming>), RpcError> {
        let SpawnOptions { env, remove_env, jsonrpc2, stderr, stdout_line } = options;
        let mut cmd = crate::process::command(program);
        cmd.args(args).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(if stderr.is_some() { Stdio::piped() } else { Stdio::null() }).kill_on_drop(true);
        // Inherited names compare as the platform does (Windows ignores case).
        for (name, _) in std::env::vars_os() {
            let shown = name.to_string_lossy().to_uppercase();
            if remove_env.iter().any(|r| r.eq_ignore_ascii_case(&shown)) {
                cmd.env_remove(&name);
            }
        }
        cmd.envs(env.iter().map(|(k, v)| (k, v)));
        if let Some(c) = cwd {
            cmd.current_dir(c);
        }
        let mut child = cmd.spawn()?;
        let stdin = child.stdin.take().expect("piped stdin");
        let stdout = child.stdout.take().expect("piped stdout");
        if let (Some(on_line), Some(err)) = (stderr, child.stderr.take()) {
            tokio::spawn(async move {
                let mut lines = BufReader::new(err).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    on_line(&line);
                }
            });
        }
        #[expect(clippy::disallowed_methods, reason = "the reader also delivers the answers callers wait for: made to wait for room here, it could never read the answer that frees them")]
        let (tx, rx) = mpsc::unbounded_channel();
        let pending: Pending = Arc::default();
        let log: Arc<dyn Fn(&str, &Value) + Send + Sync> = Arc::new(log);
        let tree = crate::process::Tree::of(child.id());
        let client = Arc::new(RpcClient { stdin: Mutex::new(stdin), jsonrpc2, next_id: AtomicI64::new(1), pending: pending.clone(), child: Mutex::new(child), _tree: tree, log: log.clone() });
        let log_in = log;
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if stdout_line.as_ref().is_some_and(|keep| !keep(&line)) {
                    continue;
                }
                let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
                log_in("incoming", &msg);
                let id = msg.get("id").cloned();
                let method = msg.get("method").and_then(Value::as_str).map(str::to_owned);
                match (id, method) {
                    (Some(id), Some(method)) => {
                        let _ = tx.send(Incoming::Request { id, method, params: msg.get("params").cloned().unwrap_or(Value::Null) });
                    }
                    (None, Some(method)) => {
                        let _ = tx.send(Incoming::Notification { method, params: msg.get("params").cloned().unwrap_or(Value::Null) });
                    }
                    (Some(id), None) => {
                        if let Some(n) = id.as_i64() {
                            if let Some(waiter) = pending.lock().await.remove(&n) {
                                let res = match msg.get("error") {
                                    Some(e) => Err(e.clone()),
                                    None => Ok(msg.get("result").cloned().unwrap_or(Value::Null)),
                                };
                                let _ = waiter.send(res);
                            }
                        }
                    }
                    (None, None) => {}
                }
            }
            pending.lock().await.clear();
            let _ = tx.send(Incoming::Closed);
        });
        Ok((client, rx))
    }

    async fn send(&self, msg: &Value) -> Result<(), RpcError> {
        let mut msg = msg.clone();
        if self.jsonrpc2 {
            msg["jsonrpc"] = json!("2.0");
        }
        let mut line = serde_json::to_vec(&msg).expect("serializable");
        line.push(b'\n');
        let mut stdin = self.stdin.lock().await;
        stdin.write_all(&line).await?;
        stdin.flush().await?;
        Ok(())
    }

    pub async fn request(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        self.send(&json!({ "id": id, "method": method, "params": params })).await?;
        match rx.await {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => Err(RpcError::Remote(e)),
            Err(_) => Err(RpcError::Closed),
        }
    }

    pub async fn notify(&self, method: &str, params: Option<Value>) -> Result<(), RpcError> {
        let mut msg = json!({ "method": method });
        if let Some(p) = params {
            msg["params"] = p;
        }
        self.send_logged(&msg).await
    }

    pub async fn respond(&self, id: Value, result: Value) -> Result<(), RpcError> {
        self.send_logged(&json!({ "id": id, "result": result })).await
    }

    /// [`Self::send`], logged as outgoing.
    async fn send_logged(&self, msg: &Value) -> Result<(), RpcError> {
        (self.log)("outgoing", msg);
        self.send(msg).await
    }

    /// Refuses a server request (one this client can't answer), so the peer
    /// doesn't wait on it for ever.
    pub async fn respond_error(&self, id: Value, code: i64, message: &str) -> Result<(), RpcError> {
        self.send_logged(&json!({ "id": id, "error": { "code": code, "message": message } })).await
    }

    pub async fn kill(&self) {
        let _ = self.child.lock().await.kill().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny echo server in Python standing in for an app-server: answers
    /// requests, emits a notification, and issues one server request.
    const FAKE: &str = r#"
import json, sys
for line in sys.stdin:
    m = json.loads(line)
    if m.get("method") == "ping":
        print(json.dumps({"method": "note", "params": {"n": 1}}), flush=True)
        print(json.dumps({"id": 99, "method": "ask", "params": {}}), flush=True)
        print(json.dumps({"id": m["id"], "result": {"pong": m["params"]}}), flush=True)
    elif m.get("id") == 99:
        print(json.dumps({"method": "answered", "params": m["result"]}), flush=True)
    elif m.get("method") == "fail":
        print(json.dumps({"id": m["id"], "error": {"code": 1, "message": "nope"}}), flush=True)
"#;

    #[tokio::test]
    async fn request_response_notifications_and_server_requests() {
        let (c, mut rx) = RpcClient::spawn(if cfg!(windows) { "python" } else { "python3" }, &["-c", FAKE], None, |_, _| {}).unwrap();
        let res = c.request("ping", json!(7)).await.unwrap();
        assert_eq!(res, json!({ "pong": 7 }));
        let mut got_note = false;
        loop {
            match rx.recv().await.unwrap() {
                Incoming::Notification { method, .. } if method == "note" => got_note = true,
                Incoming::Request { id, method, .. } => {
                    assert_eq!(method, "ask");
                    c.respond(id, json!({ "decision": "accept" })).await.unwrap();
                }
                Incoming::Notification { method, params } if method == "answered" => {
                    assert_eq!(params, json!({ "decision": "accept" }));
                    break;
                }
                other => panic!("unexpected {other:?}"),
            }
        }
        assert!(got_note);
        assert!(matches!(c.request("fail", Value::Null).await, Err(RpcError::Remote(_))));
        c.kill().await;
    }
}
