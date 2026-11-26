//! The loop that serves one connection:
//!
//! - every `Request` runs concurrently and ends with exactly one `Exit`;
//! - a stream sends `Chunk` batches and waits for the client's `Ack` before
//!   it sends the next, so a client is never sent more than it has taken;
//! - `Interrupt` cancels the request, which ends with an interrupted `Exit`;
//! - `Ping` is answered with `Pong`;
//! - a closed transport interrupts everything still running.
//!
//! Each call runs in a root `tracing` span `ws.rpc` named `ws.rpc.<tag>`
//! (`otel.name`), with `rpc.method`, `rpc.transport` and `rpc.system`; a
//! failed exit records `otel.status_code` `ERROR` and the cause, an
//! interrupted one `otel.interrupted`.

use crate::message::{CauseEncoded, FromClient, Request, RequestId, decode_client_frame};
use tracing::Instrument;
use crate::router::{Encoded, Failure, Reply, Router};
use serde::Serialize;
use futures::{FutureExt, Sink, SinkExt, Stream, StreamExt};
use serde_json::Value;
use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex};
use tokio::sync::{Notify, mpsc};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

/// Messages queued for one client's socket before requests wait for it.
const OUTBOUND_QUEUE: usize = 256;

#[derive(Debug, Clone)]
pub struct ConnectionOptions {
    /// Wait for the client's `Ack` between a stream's chunks.
    pub supports_ack: bool,
    /// Send large frames packed ([`crate::packed`]): the client asked for it.
    pub pack: bool,
    /// Maximum values per `Chunk` message.
    pub max_chunk_values: usize,
    /// WebSocket transports: send a Ping control frame this often, and close
    /// the connection when nothing (no Pong, no message) has come back for
    /// 2.5 intervals. A client that vanished without a Close frame (Wi-Fi
    /// gone) otherwise looks connected until TCP gives up, if ever.
    pub heartbeat: Option<std::time::Duration>,
}

impl Default for ConnectionOptions {
    fn default() -> Self {
        Self {
            supports_ack: true,
            pack: false,
            max_chunk_values: 256,
            heartbeat: None,
        }
    }
}

struct Inflight {
    cancel: CancellationToken,
    ack: Arc<Notify>,
}

type InflightMap = Arc<Mutex<HashMap<RequestId, Inflight>>>;

/// A message to the client as the server writes it: what
/// [`crate::message::FromServer`] reads, with the answers in it as text
/// that is already written ([`Encoded`]).
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "_tag")]
pub enum Outgoing {
    Chunk {
        #[serde(rename = "requestId")]
        request_id: RequestId,
        values: Vec<Encoded>,
    },
    Exit {
        #[serde(rename = "requestId")]
        request_id: RequestId,
        exit: Exit,
    },
    Defect {
        defect: Value,
    },
    Pong,
}

/// Stream chunks waiting for the transport can be discarded after an interrupt.
struct Queued {
    message: Outgoing,
    cancel: Option<CancellationToken>,
}

impl From<Outgoing> for Queued {
    fn from(message: Outgoing) -> Self {
        Self { message, cancel: None }
    }
}

/// How a request ended ([`crate::message::ExitEncoded`] as it is written).
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "_tag")]
pub enum Exit {
    Success { value: Encoded },
    Failure { cause: Vec<CauseEncoded> },
}

impl Exit {
    /// The end of a request that answers nothing, and of a stream.
    pub fn void() -> Self {
        static NULL: std::sync::LazyLock<Encoded> = std::sync::LazyLock::new(|| crate::router::encoded(&()).expect("null is JSON"));
        Exit::Success { value: NULL.clone() }
    }
    fn interrupt() -> Self {
        Exit::Failure { cause: vec![CauseEncoded::Interrupt] }
    }
}

impl Outgoing {
    /// The frame that carries the message.
    pub fn frame(&self) -> String {
        serde_json::to_string(self).expect("a message to the client is plain data")
    }
}

fn exit_for(result: Result<Encoded, Failure>) -> Exit {
    match result {
        Ok(value) => Exit::Success { value },
        Err(Failure::Fail(error)) => Exit::Failure { cause: vec![CauseEncoded::Fail { error }] },
        Err(Failure::Die(defect)) => Exit::Failure { cause: vec![CauseEncoded::Die { defect }] },
    }
}

fn panic_message(panic: Box<dyn std::any::Any + Send>) -> Failure {
    let msg = panic
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "handler panicked".into());
    Failure::die(msg)
}

/// Serves one client until `incoming` ends. Generic over the transport so it
/// can be driven by axum WebSockets or by in-memory channels in tests.
pub async fn serve_connection<C, R, W, RE>(
    router: Arc<Router<C>>,
    ctx: Arc<C>,
    mut incoming: R,
    mut outgoing: W,
    options: ConnectionOptions,
) where
    C: Send + Sync + 'static,
    R: Stream<Item = Result<String, RE>> + Unpin,
    W: Sink<String> + Unpin + Send + 'static,
    RE: std::fmt::Display,
{
    // Bounded: a client that reads slower than the server writes holds up the
    // requests feeding it (a stream stops pulling, and a live stream that
    // falls behind its broadcast resyncs from the log) instead of queueing
    // without limit in this process.
    let (tx, mut rx) = mpsc::channel::<Queued>(OUTBOUND_QUEUE);
    let pack = options.pack;
    let writer = tokio::spawn(async move {
        while let Some(Queued { message, cancel }) = rx.recv().await {
            if cancel.as_ref().is_some_and(CancellationToken::is_cancelled) {
                continue;
            }
            let frame = message.frame();
            let frame = match pack {
                false => frame,
                true if frame.len() < crate::packed::PACK_APART_FROM => crate::packed::pack(frame),
                true => match tokio::task::spawn_blocking(move || crate::packed::pack(frame)).await {
                    Ok(packed) => packed,
                    Err(_) => return,
                },
            };
            if cancel.as_ref().is_some_and(CancellationToken::is_cancelled) {
                continue;
            }
            if outgoing.send(frame).await.is_err() {
                return;
            }
        }
        let _ = outgoing.close().await;
    });

    let inflight: InflightMap = Arc::default();
    let tasks = TaskTracker::new();

    while let Some(frame) = incoming.next().await {
        let frame = match frame {
            Ok(f) => f,
            Err(e) => {
                tracing::debug!("transport error: {e}");
                break;
            }
        };
        let messages = match decode_client_frame(&frame) {
            Ok(m) => m,
            Err(e) => {
                let _ = tx
                    .send(Outgoing::Defect { defect: Value::String(format!("Failed to decode client message: {e}")) }.into())
                    .await;
                continue;
            }
        };
        for message in messages {
            match message {
                FromClient::Ping => {
                    // Never waits: this loop must keep reading acks. With the
                    // queue full the client is not reading pongs anyway.
                    let _ = tx.try_send(Outgoing::Pong.into());
                }
                FromClient::Ack { request_id } => {
                    if let Some(entry) = inflight.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(&request_id) {
                        entry.ack.notify_one();
                    }
                }
                FromClient::Interrupt { request_id } => {
                    if let Some(entry) = inflight.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(&request_id) {
                        entry.cancel.cancel();
                    }
                }
                FromClient::Request(request) => {
                    start_request(&router, &ctx, request, &tx, &inflight, &tasks, &options);
                }
            }
        }
    }

    // Transport closed: interrupt whatever is still running.
    for entry in inflight.lock().unwrap_or_else(std::sync::PoisonError::into_inner).values() {
        entry.cancel.cancel();
    }
    tasks.close();
    tasks.wait().await;
    drop(tx);
    let _ = writer.await;
}

fn start_request<C: Send + Sync + 'static>(
    router: &Arc<Router<C>>,
    ctx: &Arc<C>,
    mut request: Request,
    tx: &mpsc::Sender<Queued>,
    inflight: &InflightMap,
    tasks: &TaskTracker,
    options: &ConnectionOptions,
) {
    let id = request.id.clone();
    let cancel = CancellationToken::new();
    let ack = Arc::new(Notify::new());
    {
        let mut map = inflight.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if map.contains_key(&id) {
            tracing::warn!(request_id = %id, "duplicate request id ignored");
            return;
        }
        map.insert(id.clone(), Inflight { cancel: cancel.clone(), ack: ack.clone() });
    }

    let span = if router.traced(&request.tag) {
        tracing::info_span!(
            parent: None,
            "ws.rpc",
            "otel.name" = %format!("ws.rpc.{}", request.tag),
            "rpc.method" = %request.tag,
            "rpc.transport" = "websocket",
            "rpc.system" = "domovoi-rpc",
            "otel.status_code" = tracing::field::Empty,
            "otel.status_message" = tracing::field::Empty,
            "otel.interrupted" = tracing::field::Empty,
        )
    } else {
        tracing::Span::none()
    };
    let reply = span.in_scope(|| router.dispatch(ctx.clone(), &mut request));
    let tx = tx.clone();
    let inflight = inflight.clone();
    let supports_ack = options.supports_ack;
    let max_chunk = options.max_chunk_values.max(1);

    tasks.spawn(async move {
        let exit = match reply {
            Err(failure) => exit_for(Err(failure)),
            Ok(Reply::Unary(fut)) => {
                tokio::select! {
                    _ = cancel.cancelled() => Exit::interrupt(),
                    result = AssertUnwindSafe(fut).catch_unwind() => exit_for(result.unwrap_or_else(|p| Err(panic_message(p)))),
                }
            }
            Ok(Reply::Stream(stream)) => {
                let run = async {
                    let mut batches = stream.ready_chunks(max_chunk);
                    while let Some(batch) = batches.next().await {
                        let mut values = Vec::with_capacity(batch.len());
                        let mut failure = None;
                        for item in batch {
                            match item {
                                Ok(v) => values.push(v),
                                Err(f) => {
                                    failure = Some(f);
                                    break;
                                }
                            }
                        }
                        if !values.is_empty() {
                            let _ = tx.send(Queued {
                                message: Outgoing::Chunk { request_id: id.clone(), values },
                                cancel: Some(cancel.clone()),
                            }).await;
                            if supports_ack {
                                ack.notified().await;
                            }
                        }
                        if let Some(f) = failure {
                            return exit_for(Err(f));
                        }
                    }
                    Exit::void()
                };
                tokio::select! {
                    _ = cancel.cancelled() => Exit::interrupt(),
                    result = AssertUnwindSafe(run).catch_unwind() => result.unwrap_or_else(|p| exit_for(Err(panic_message(p)))),
                }
            }
        };
        inflight.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&id);
        if let Exit::Failure { cause } = &exit {
            record_failure(&tracing::Span::current(), cause);
        }
        let _ = tx.send(Outgoing::Exit { request_id: id.clone(), exit }.into()).await;
    }.instrument(span));
}

/// Marks the call's span failed (with the cause's tag and message) or interrupted.
fn record_failure(span: &tracing::Span, cause: &[CauseEncoded]) {
    if span.is_disabled() {
        return;
    }
    if cause.iter().all(|c| matches!(c, CauseEncoded::Interrupt)) {
        span.record("otel.interrupted", true);
        span.record("otel.status_message", "Interrupted");
        return;
    }
    let text = |v: &Value| v.as_str().map(str::to_owned);
    let described: Vec<String> = cause
        .iter()
        .filter_map(|c| match c {
            CauseEncoded::Fail { error } => {
                let tag = error.get("_tag").and_then(Value::as_str).unwrap_or("Error");
                let message = error.get("message").or_else(|| error.get("detail")).and_then(text);
                Some(match message {
                    Some(m) => format!("{tag}: {m}"),
                    None => tag.to_owned(),
                })
            }
            CauseEncoded::Die { defect } => Some(text(defect).unwrap_or_else(|| defect.to_string())),
            CauseEncoded::Interrupt => None,
        })
        .collect();
    span.record("otel.status_code", "ERROR");
    span.record("otel.status_message", described.join("\n").as_str());
}
