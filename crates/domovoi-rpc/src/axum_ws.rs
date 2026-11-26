//! axum WebSocket transport for [`serve_connection`].

use crate::connection::{ConnectionOptions, serve_connection};
use crate::router::Router;
use axum::extract::ws::{Message, WebSocket};
use futures::{SinkExt, StreamExt, future};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// Drives an upgraded socket. Text frames carry the messages; control frames
/// are handled by axum; binary frames are ignored.
pub async fn serve_websocket<C: Send + Sync + 'static>(
    socket: WebSocket,
    router: Arc<Router<C>>,
    ctx: Arc<C>,
    options: ConnectionOptions,
) {
    serve_websocket_until(socket, router, ctx, options, future::pending()).await;
}

/// As [`serve_websocket`], ending once `stop` resolves (the session was
/// revoked, the server is stopping): running requests are interrupted and
/// the client gets a Close frame, so it knows at once instead of waiting
/// for its connection to time out. With `options.heartbeat`, it also ends
/// when the client stops answering pings.
pub async fn serve_websocket_until<C: Send + Sync + 'static>(
    socket: WebSocket,
    router: Arc<Router<C>>,
    ctx: Arc<C>,
    options: ConnectionOptions,
    stop: impl std::future::Future<Output = ()> + Send,
) {
    let (sink, stream) = socket.split();
    // One writer owns the socket's sink: the connection's text frames and
    // the heartbeat's pings both go through it.
    let (frames, mut queued) = futures::channel::mpsc::channel::<Message>(64);
    let writer = tokio::spawn(async move {
        let mut sink = sink;
        while let Some(frame) = queued.next().await {
            if sink.send(frame).await.is_err() {
                break;
            }
        }
        let _ = sink.close().await;
    });
    let started = Instant::now();
    // Milliseconds after `started` when the client was last heard from.
    let heard = Arc::new(AtomicU64::new(0));
    let dead = CancellationToken::new();
    let heartbeat = options.heartbeat.map(|every| {
        let (mut pings, heard, dead) = (frames.clone(), heard.clone(), dead.clone());
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(every);
            tick.tick().await;
            loop {
                tick.tick().await;
                let silent = started.elapsed().saturating_sub(Duration::from_millis(heard.load(Ordering::Relaxed)));
                if silent > every * 5 / 2 {
                    tracing::info!("websocket: no answer for {}s, closing", silent.as_secs());
                    dead.cancel();
                    return;
                }
                if pings.send(Message::Ping(Vec::new().into())).await.is_err() {
                    return;
                }
            }
        })
    });
    let seen = heard.clone();
    let incoming = stream
        .inspect(move |_| seen.store(started.elapsed().as_millis() as u64, Ordering::Relaxed))
        .take_while(|m| future::ready(!matches!(m, Ok(Message::Close(_)))))
        .filter_map(|m| {
            future::ready(match m {
                Ok(Message::Text(t)) => Some(Ok(t.to_string())),
                Ok(_) => None,
                Err(e) => Some(Err(e)),
            })
        })
        .take_until(future::select(Box::pin(stop), Box::pin(dead.cancelled_owned())));
    let outgoing = frames.clone().sink_map_err(|_| ()).with(|s: String| future::ready(Ok::<_, ()>(Message::Text(s.into()))));
    serve_connection(router, ctx, Box::pin(incoming), Box::pin(outgoing), options).await;
    if let Some(h) = heartbeat {
        h.abort();
    }
    drop(frames);
    let _ = writer.await;
}
