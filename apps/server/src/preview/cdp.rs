//! A minimal Chrome DevTools Protocol client: JSON commands and events over
//! the browser's one WebSocket, with flattened target sessions (t3code drives
//! Chromium through Playwright, which speaks the same protocol).
//!
//! Commands answer by id; events are routed by session to bounded queues.
//! An event that finds its queue full is dropped (a screencast frame among
//! them is acknowledged by nobody, so Chrome sends no more to that session
//! until its viewer reattaches).
//!
//! Limits: no typed protocol: callers read the fields they need.

#![expect(clippy::disallowed_types, clippy::disallowed_macros, reason = "the DevTools protocol: JSON commands, results and events")]

use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, oneshot};
use cdp_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;

/// How long a command may take before its caller stops waiting.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
/// Events a session may have queued before more are dropped.
const SESSION_QUEUE: usize = 1024;
/// Commands waiting to be written to the socket.
const OUTGOING_QUEUE: usize = 256;

#[derive(Debug, Clone, thiserror::Error)]
pub enum CdpError {
    #[error("the browser connection closed")]
    Closed,
    #[error("the browser did not answer {0} in time")]
    Timeout(String),
    #[error("{0}")]
    Protocol(String),
    #[error("could not connect to the browser: {0}")]
    Connect(String),
}

/// An event of a session (or of the browser, with no session).
#[derive(Debug, Clone)]
pub struct Event {
    pub method: String,
    pub params: Value,
}

type Pending = Mutex<HashMap<u64, oneshot::Sender<Result<Value, CdpError>>>>;

pub struct Cdp {
    out: mpsc::Sender<String>,
    pending: Arc<Pending>,
    next_id: AtomicU64,
    sessions: Arc<Mutex<HashMap<String, mpsc::Sender<Event>>>>,
    /// Cancelled once the socket is gone.
    pub closed: CancellationToken,
}

impl Cdp {
    /// Connects to `ws://127.0.0.1:<port><path>`, the browser's DevTools endpoint.
    pub async fn connect(port: u16, path: &str) -> Result<Arc<Cdp>, CdpError> {
        let stream = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.map_err(|e| CdpError::Connect(e.to_string()))?;
        let url = format!("ws://127.0.0.1:{port}{path}");
        let mut config = cdp_tungstenite::tungstenite::protocol::WebSocketConfig::default();
        // Full-page screenshots of a large page arrive as one message.
        config.max_message_size = Some(256 << 20);
        config.max_frame_size = Some(256 << 20);
        let (socket, _) = cdp_tungstenite::client_async_with_config(url, stream, Some(config)).await.map_err(|e| CdpError::Connect(e.to_string()))?;
        let (mut sink, mut source) = socket.split();
        let (out, mut outgoing) = mpsc::channel::<String>(OUTGOING_QUEUE);
        let cdp = Arc::new(Cdp {
            out,
            pending: Arc::default(),
            next_id: AtomicU64::new(1),
            sessions: Arc::default(),
            closed: CancellationToken::new(),
        });
        let closed = cdp.closed.clone();
        tokio::spawn({
            let closed = closed.clone();
            async move {
                loop {
                    tokio::select! {
                        () = closed.cancelled() => break,
                        next = outgoing.recv() => match next {
                            Some(text) => if sink.send(Message::Text(text.into())).await.is_err() { break },
                            None => break,
                        },
                    }
                }
                let _ = sink.close().await;
                closed.cancel();
            }
        });
        let (pending, sessions) = (cdp.pending.clone(), cdp.sessions.clone());
        tokio::spawn(async move {
            loop {
                let message = tokio::select! {
                    () = closed.cancelled() => break,
                    message = source.next() => message,
                };
                let text = match message {
                    Some(Ok(Message::Text(text))) => text.to_string(),
                    Some(Ok(Message::Binary(bytes))) => String::from_utf8_lossy(&bytes).into_owned(),
                    Some(Ok(_)) => continue,
                    Some(Err(_)) | None => break,
                };
                let Ok(mut message) = serde_json::from_str::<Value>(&text) else { continue };
                if let Some(id) = message.get("id").and_then(Value::as_u64) {
                    let answer = match message.get_mut("error") {
                        Some(error) => Err(CdpError::Protocol(error["message"].as_str().unwrap_or("protocol error").to_owned())),
                        None => Ok(message.get_mut("result").map(Value::take).unwrap_or(Value::Null)),
                    };
                    if let Some(waiting) = crate::util::locked(&pending).remove(&id) {
                        let _ = waiting.send(answer);
                    }
                    continue;
                }
                let Some(method) = message.get("method").and_then(Value::as_str).map(str::to_owned) else { continue };
                let session = message.get("sessionId").and_then(Value::as_str).unwrap_or("").to_owned();
                let params = message.get_mut("params").map(Value::take).unwrap_or(Value::Null);
                let queue = crate::util::locked(&sessions).get(&session).cloned();
                if let Some(queue) = queue {
                    // Full: dropped (see the module header).
                    let _ = queue.try_send(Event { method, params });
                }
            }
            closed.cancel();
            // Whoever still waits learns the browser is gone.
            for (_, waiting) in crate::util::locked(&pending).drain() {
                let _ = waiting.send(Err(CdpError::Closed));
            }
            crate::util::locked(&sessions).clear();
        });
        Ok(cdp)
    }

    /// Sends `method` to the browser (`session` `None`) or to a target session.
    pub async fn call(&self, session: Option<&str>, method: &str, params: Value) -> Result<Value, CdpError> {
        if self.closed.is_cancelled() {
            return Err(CdpError::Closed);
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let mut message = json!({ "id": id, "method": method, "params": params });
        if let Some(session) = session {
            message["sessionId"] = json!(session);
        }
        let (tx, rx) = oneshot::channel();
        crate::util::locked(&self.pending).insert(id, tx);
        if self.out.send(message.to_string()).await.is_err() {
            crate::util::locked(&self.pending).remove(&id);
            return Err(CdpError::Closed);
        }
        match tokio::time::timeout(COMMAND_TIMEOUT, rx).await {
            Ok(Ok(answer)) => answer,
            Ok(Err(_)) => Err(CdpError::Closed),
            Err(_) => {
                crate::util::locked(&self.pending).remove(&id);
                Err(CdpError::Timeout(method.to_owned()))
            }
        }
    }

    /// The events of `session` (`""`: the browser's own) from now on.
    pub fn subscribe(&self, session: &str) -> mpsc::Receiver<Event> {
        let (tx, rx) = mpsc::channel(SESSION_QUEUE);
        crate::util::locked(&self.sessions).insert(session.to_owned(), tx);
        rx
    }

    pub fn unsubscribe(&self, session: &str) {
        crate::util::locked(&self.sessions).remove(session);
    }

    /// Attaches to a page target: a flattened session for it, and its events.
    pub async fn attach(self: &Arc<Self>, target_id: &str) -> Result<(Session, mpsc::Receiver<Event>), CdpError> {
        let attached = self.call(None, "Target.attachToTarget", json!({ "targetId": target_id, "flatten": true })).await?;
        let id = attached["sessionId"].as_str().ok_or_else(|| CdpError::Protocol("no session id".into()))?.to_owned();
        let events = self.subscribe(&id);
        Ok((Session { cdp: self.clone(), id }, events))
    }
}

/// A target session.
#[derive(Clone)]
pub struct Session {
    pub cdp: Arc<Cdp>,
    pub id: String,
}

impl Session {
    pub async fn call(&self, method: &str, params: Value) -> Result<Value, CdpError> {
        self.cdp.call(Some(&self.id), method, params).await
    }

    /// Detaches; its events stop.
    pub async fn detach(&self) {
        self.cdp.unsubscribe(&self.id);
        let _ = self.cdp.call(None, "Target.detachFromTarget", json!({ "sessionId": self.id })).await;
    }
}
