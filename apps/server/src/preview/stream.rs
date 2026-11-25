//! `/api/preview-stream/*` (t3code `ServerBrowserStream.ts`): a server tab's
//! live view.
//!
//! `GET /api/preview-stream/ws?threadId&tabId[&maxWidth=1280][&maxHeight=800][&quality=70][&interactive=false][&wsTicket]`
//! upgrades to a WebSocket (authenticated like a media request with
//! `orchestration:read`; input needs `preview:operate` too and no
//! `interactive=false`). The server sends JPEG frames as binary messages and
//! JSON as text (`{"type":"viewport"|"control"|"probe", ...}`); the client
//! answers each frame with `{"type":"ack"}`, and only one frame is on the
//! wire with only the newest waiting behind it. Anything else the client
//! sends is input (`mouse`, `wheel`, `key`, `text`, `resize`, `viewport`,
//! `navigate`, `history`, `stopLoading`, `reload`, `probe`, `takeControl`,
//! `releaseControl`, `dialog`). A tab that does not exist or closes ends the
//! socket with close code 4404 (`tab closed`); a browser that cannot start
//! answers 503 before the upgrade. The socket closes with its session.
//!
//! Limits: `download` answers 404 (the browser refuses downloads) and
//! `upload` 409 (no file chooser is intercepted); no `clipboard`,
//! `pointer`, `fileChooser`, `popup` or `download` messages; no host setup
//! close code (4503: Linux sandbox and library diagnosis are not done).

#![expect(clippy::disallowed_types, reason = "viewer messages are JSON")]

use super::host::{self, AttachError, AttachInput, Output};
use super::{Peer, authenticate_media};
use crate::state::SharedState;
use axum::extract::ws::{CloseFrame, Message, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use futures::{SinkExt, StreamExt};
use serde_json::Value;
use std::collections::HashMap;
use std::sync::Arc;

pub const ROUTE: &str = "/api/preview-stream/{*rest}";
const TAB_GONE_CODE: u16 = 4404;
const DEFAULT_QUALITY: u32 = 70;
/// Input messages waiting for the tab (they run one at a time, in order).
const INPUT_QUEUE: usize = 256;

fn int_param(q: &HashMap<String, String>, name: &str, fallback: u32, max: u32) -> u32 {
    match q.get(name).and_then(|v| v.parse::<f64>().ok()) {
        Some(v) if v.is_finite() && v > 0.0 => (v.round() as u32).min(max),
        _ => fallback,
    }
}

fn not_found() -> Response {
    (StatusCode::NOT_FOUND, "Not Found").into_response()
}

pub async fn handler(
    State(state): State<SharedState>,
    headers: HeaderMap,
    peer: Peer,
    Path(rest): Path<String>,
    Query(q): Query<HashMap<String, String>>,
    upgrade: Result<WebSocketUpgrade, axum::extract::ws::rejection::WebSocketUpgradeRejection>,
) -> Response {
    let ticket = q.get("wsTicket").map(String::as_str);
    match rest.as_str() {
        "download" => {
            if let Err(r) = authenticate_media(&state, &headers, &peer, ticket, "orchestration:read") {
                return r;
            }
            return not_found();
        }
        "upload" => return (StatusCode::METHOD_NOT_ALLOWED, "Method Not Allowed").into_response(),
        "ws" => {}
        _ => return not_found(),
    }
    let Ok(upgrade) = upgrade else { return not_found() };
    let session = match authenticate_media(&state, &headers, &peer, ticket, "orchestration:read") {
        Ok(s) => s,
        Err(r) => return r,
    };
    let can_operate = session.permissions.iter().any(|p| p == "preview:operate") && q.get("interactive").map(String::as_str) != Some("false");
    let (thread_id, tab_id) = (q.get("threadId").cloned().unwrap_or_default(), q.get("tabId").cloned().unwrap_or_default());
    if thread_id.is_empty() || tab_id.is_empty() {
        return not_found();
    }
    host::start(&state);
    let input = AttachInput {
        thread_id,
        tab_id,
        max_width: int_param(&q, "maxWidth", 1280, 7680),
        max_height: int_param(&q, "maxHeight", 800, 4320),
        quality: int_param(&q, "quality", DEFAULT_QUALITY, 100),
        can_operate,
    };
    let viewer = match host::attach(&state, input).await {
        Ok(v) => Some(v),
        Err(AttachError::Gone) => None,
        Err(AttachError::Launch(cause)) => {
            tracing::warn!("server preview browser failed to start: {cause}");
            return (StatusCode::SERVICE_UNAVAILABLE, "Service Unavailable").into_response();
        }
    };
    let ended = super::session_ended(state.clone(), session.token_hash.clone());
    upgrade.on_upgrade(move |socket| async move {
        let (mut tx, mut rx) = socket.split();
        let gone = || Message::Close(Some(CloseFrame { code: TAB_GONE_CODE, reason: "tab closed".into() }));
        // A refused upgrade reads as an auth failure to ticket clients, so a missing tab is a close code.
        let Some(mut viewer) = viewer else {
            let _ = tx.send(gone()).await;
            return;
        };
        let mut output = viewer.take_output();
        let viewer = Arc::new(viewer);
        let (inputs, mut queued) = tokio::sync::mpsc::channel::<Value>(INPUT_QUEUE);
        let worker = {
            let viewer = viewer.clone();
            tokio::spawn(async move {
                while let Some(message) = queued.recv().await {
                    viewer.input(message).await;
                }
            })
        };
        let mut in_flight: Option<host::Ack> = None;
        let mut pending: Option<(Vec<u8>, Option<host::Ack>)> = None;
        let mut waiting_ack = false;
        tokio::pin!(ended);
        loop {
            tokio::select! {
                () = &mut ended => break,
                out = output.recv() => match out {
                    Some(Output::Frame { data, ack }) => {
                        if !waiting_ack {
                            waiting_ack = true;
                            in_flight = ack;
                            if tx.send(Message::Binary(data.into())).await.is_err() { break }
                        } else if let Some((_, Some(superseded))) = pending.replace((data, ack)) {
                            // Never sent, but the browser still waits for its ack.
                            tokio::spawn(superseded.send());
                        }
                    }
                    Some(Output::Text(text)) => if tx.send(Message::Text(text.into())).await.is_err() { break },
                    Some(Output::Gone) | None => {
                        let _ = tx.send(gone()).await;
                        break;
                    }
                },
                incoming = rx.next() => {
                    let text = match incoming {
                        Some(Ok(Message::Text(t))) => t.to_string(),
                        Some(Ok(Message::Binary(b))) => String::from_utf8_lossy(&b).into_owned(),
                        Some(Ok(Message::Ping(_) | Message::Pong(_))) => continue,
                        Some(Ok(Message::Close(_)) | Err(_)) | None => break,
                    };
                    let Ok(message) = serde_json::from_str::<Value>(&text) else { continue };
                    if message["type"] == "ack" {
                        if let Some(ack) = in_flight.take() {
                            tokio::spawn(ack.send());
                        }
                        waiting_ack = false;
                        if let Some((data, ack)) = pending.take() {
                            waiting_ack = true;
                            in_flight = ack;
                            if tx.send(Message::Binary(data.into())).await.is_err() { break }
                        }
                        continue;
                    }
                    if !can_operate {
                        continue;
                    }
                    // Control and dialog replies must reach a tab whose action waits on a dialog.
                    if matches!(message["type"].as_str(), Some("takeControl" | "releaseControl" | "dialog")) {
                        let viewer = viewer.clone();
                        tokio::spawn(async move { viewer.input(message).await });
                    } else if inputs.try_send(message).is_err() {
                        tracing::debug!("server preview input dropped: the queue is full");
                    }
                }
            }
        }
        worker.abort();
        for ack in in_flight.into_iter().chain(pending.and_then(|(_, a)| a)) {
            tokio::spawn(ack.send());
        }
        viewer.close().await;
    })
}

/// `POST /api/preview-stream/upload`: no file chooser is ever intercepted, so none is open.
pub async fn upload(State(state): State<SharedState>, headers: HeaderMap, peer: Peer, Path(rest): Path<String>, Query(q): Query<HashMap<String, String>>) -> Response {
    if rest != "upload" {
        return not_found();
    }
    if let Err(r) = authenticate_media(&state, &headers, &peer, q.get("wsTicket").map(String::as_str), "preview:operate") {
        return r;
    }
    (StatusCode::CONFLICT, "The page's file picker is no longer open.").into_response()
}
