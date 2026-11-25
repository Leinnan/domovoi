//! Per-connection protocol loop, mirroring `RpcServer.make` + the socket
//! protocol in `effect/rpc/RpcServer.ts`:
//!
//! - every `Request` runs concurrently and ends with exactly one `Exit`
//!   (unless it is a notification);
//! - streams send `Chunk` batches and, when acks are supported, wait for the
//!   client's `Ack` before sending the next batch;
//! - `Interrupt` cancels the request and replies with an interrupt `Exit`;
//! - `Ping` is answered with `Pong`;
//! - a closed transport interrupts everything still running.
//!
//! Each call runs in a root `tracing` span `ws.rpc` named `ws.rpc.<tag>`
//! (`otel.name`; t3code's `RpcInstrumentation`), with `rpc.method`,
//! `rpc.transport` and `rpc.system`; a failed exit records `otel.status_code`
//! `ERROR` and the cause, an interrupted one `otel.interrupted`.

use crate::frame_parts::{SOCKET_FRAME_PART_BYTES, split_socket_frame_owned};
use crate::message::{FromClient, FromServer, RequestEncoded, RequestId, decode_client_frame, encode_server_message};
use crate::message::{CauseEncoded, ExitEncoded};
use tracing::Instrument;
use crate::router::{Failure, Reply, Router};
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
    /// Wait for client `Ack`s between stream chunks (Effect's default).
    pub supports_ack: bool,
    /// Split large outgoing frames (`?frameParts=1`).
    pub frame_parts: bool,
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
            frame_parts: false,
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

fn exit_for(result: Result<Value, Failure>) -> ExitEncoded {
    match result {
        Ok(v) => ExitEncoded::success(v),
        Err(Failure::Fail(e)) => ExitEncoded::fail(e),
        Err(Failure::Die(d)) => ExitEncoded::die(d),
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
    let (tx, mut rx) = mpsc::channel::<FromServer>(OUTBOUND_QUEUE);
    let frame_parts = options.frame_parts;
    let writer = tokio::spawn(async move {
        while let Some(message) = rx.recv().await {
            let frame = encode_server_message(&message);
            let parts = if frame_parts {
                split_socket_frame_owned(frame, SOCKET_FRAME_PART_BYTES)
            } else {
                vec![frame]
            };
            for part in parts {
                if outgoing.send(part).await.is_err() {
                    return;
                }
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
                    .send(FromServer::Defect { defect: Value::String(format!("Failed to decode client message: {e}")) })
                    .await;
                continue;
            }
        };
        let mut eof = false;
        for message in messages {
            match message {
                FromClient::Ping => {
                    // Never waits: this loop must keep reading acks. With the
                    // queue full the client is not reading pongs anyway.
                    let _ = tx.try_send(FromServer::Pong);
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
                FromClient::Eof => eof = true,
                FromClient::Request(request) => {
                    start_request(&router, &ctx, request, &tx, &inflight, &tasks, &options);
                }
            }
        }
        if eof {
            // Let in-flight requests finish, then end the connection.
            tasks.close();
            tasks.wait().await;
            break;
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
    mut request: RequestEncoded,
    tx: &mpsc::Sender<FromServer>,
    inflight: &InflightMap,
    tasks: &TaskTracker,
    options: &ConnectionOptions,
) {
    let id = request.id.clone();
    let silent = request.is_notification();
    let discard = request.is_discard();
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
            "rpc.system" = "effect-rpc",
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
    let supports_ack = options.supports_ack && !silent && !discard;
    let max_chunk = options.max_chunk_values.max(1);

    tasks.spawn(async move {
        let send_exit = async |exit: ExitEncoded| {
            if !silent {
                let _ = tx.send(FromServer::Exit { request_id: id.clone(), exit }).await;
            }
        };
        if discard {
            // Acknowledged once the handler starts; nothing more is sent.
            send_exit(ExitEncoded::void()).await;
        }
        let exit = match reply {
            Err(failure) => Some(exit_for(Err(failure))),
            Ok(Reply::Unary(fut)) => {
                tokio::select! {
                    _ = cancel.cancelled() => Some(ExitEncoded::interrupt()),
                    result = AssertUnwindSafe(fut).catch_unwind() => {
                        Some(exit_for(result.unwrap_or_else(|p| Err(panic_message(p)))))
                    }
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
                        if !values.is_empty() && !silent && !discard {
                            let _ = tx.send(FromServer::Chunk { request_id: id.clone(), values }).await;
                            if supports_ack {
                                ack.notified().await;
                            }
                        }
                        if let Some(f) = failure {
                            return exit_for(Err(f));
                        }
                    }
                    ExitEncoded::void()
                };
                tokio::select! {
                    _ = cancel.cancelled() => Some(ExitEncoded::interrupt()),
                    result = AssertUnwindSafe(run).catch_unwind() => {
                        Some(result.unwrap_or_else(|p| exit_for(Err(panic_message(p)))))
                    }
                }
            }
        };
        inflight.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(&id);
        if let Some(ExitEncoded::Failure { cause }) = &exit {
            record_failure(&tracing::Span::current(), cause);
        }
        if let Some(exit) = exit
            && !discard
        {
            send_exit(exit).await;
        }
    }.instrument(span));
}

/// Marks the call's span failed (with the cause's tag and message) or interrupted.
fn record_failure(span: &tracing::Span, cause: &[CauseEncoded]) {
    if span.is_disabled() {
        return;
    }
    if cause.iter().all(|c| matches!(c, CauseEncoded::Interrupt { .. })) {
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
            CauseEncoded::Interrupt { .. } => None,
        })
        .collect();
    span.record("otel.status_code", "ERROR");
    span.record("otel.status_message", described.join("\n").as_str());
}
