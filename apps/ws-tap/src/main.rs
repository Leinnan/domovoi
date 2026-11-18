//! ws-tap: transparent reverse proxy that records traffic between a t3code web
//! client and a server (TS reference or domovoi-server) as NDJSON:
//!
//! {"t":ms,"conn":n,"kind":"ws","dir":"c2s"|"s2c","frame":<parsed JSON or string>}
//! {"t":ms,"kind":"http","method":..,"path":..,"status":..,"req":..,"res":..}

#![allow(clippy::disallowed_types, clippy::disallowed_macros, reason = "a recorder of traffic: frames are kept as the JSON they were")]

use axum::{
    Router,
    body::{Body, Bytes},
    extract::{FromRequestParts, Request, State, ws::{Message as AxMsg, WebSocketUpgrade}},
    http::{HeaderMap, HeaderName, Response, StatusCode},
};
use clap::Parser;
use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{
    sync::{Arc, Mutex, atomic::{AtomicU64, Ordering}},
    time::Instant,
};
use tokio::io::AsyncWriteExt;
use tokio_tungstenite::tungstenite::{self, client::IntoClientRequest};

#[derive(Parser)]
struct Args {
    /// Address to listen on.
    #[arg(long, default_value = "127.0.0.1:13999")]
    listen: String,
    /// Upstream server origin, e.g. http://127.0.0.1:13773
    #[arg(long)]
    upstream: String,
    /// NDJSON output file.
    #[arg(long)]
    out: String,
}

struct Tap {
    upstream: String,
    http: reqwest::Client,
    out: Mutex<std::fs::File>,
    start: Instant,
    conns: AtomicU64,
}

impl Tap {
    fn log(&self, mut v: Value) {
        v["t"] = json!(self.start.elapsed().as_millis() as u64);
        use std::io::Write;
        let mut f = self.out.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let _ = writeln!(f, "{v}");
    }
}

fn parse_frame(s: &str) -> Value {
    serde_json::from_str(s).unwrap_or_else(|_| Value::String(s.to_owned()))
}

/// Rejoins t3code `frameParts` (U+001E-prefixed parts) so a split message is
/// logged once, whole. Returns None while parts are still pending.
fn join_part(pending: &mut String, frame: &str) -> Option<String> {
    if let Some(part) = frame.strip_prefix('\u{1e}') {
        pending.push_str(part);
        return None;
    }
    if pending.is_empty() {
        return Some(frame.to_owned());
    }
    let mut whole = std::mem::take(pending);
    whole.push_str(frame);
    Some(whole)
}

// accept-encoding is dropped so upstream replies uncompressed and bodies are
// recorded as readable JSON (the client still gets a valid identity response).
const HOP: &[&str] = &["connection", "keep-alive", "transfer-encoding", "upgrade", "te", "trailer",
    "proxy-connection", "sec-websocket-key", "sec-websocket-version", "sec-websocket-extensions",
    "sec-websocket-accept", "content-length", "accept-encoding"];

fn forward_headers(src: &HeaderMap) -> Vec<(HeaderName, axum::http::HeaderValue)> {
    src.iter()
        .filter(|(k, _)| !HOP.contains(&k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

fn body_json(b: &Bytes) -> Value {
    if b.is_empty() { return Value::Null; }
    serde_json::from_slice(b).unwrap_or_else(|_| {
        if b.len() <= 4096 { Value::String(String::from_utf8_lossy(b).into()) } else { json!(format!("<{} bytes>", b.len())) }
    })
}

async fn proxy(State(tap): State<Arc<Tap>>, req: Request) -> Response<Body> {
    let (mut parts, body) = req.into_parts();
    let path_q = parts.uri.path_and_query().map(|p| p.as_str().to_owned()).unwrap_or_else(|| "/".into());
    if let Ok(ws) = WebSocketUpgrade::from_request_parts(&mut parts, &()).await {
        return ws_proxy(tap, ws, path_q, parts.headers).await;
    }
    let bytes = axum::body::to_bytes(body, 64 << 20).await.unwrap_or_default();
    let mut rb = tap.http.request(parts.method.clone(), format!("{}{}", tap.upstream, path_q)).body(bytes.clone());
    for (k, v) in forward_headers(&parts.headers) { rb = rb.header(k, v); }
    let res = match rb.send().await {
        Ok(r) => r,
        Err(e) => return Response::builder().status(StatusCode::BAD_GATEWAY).body(Body::from(e.to_string())).expect("a status and a body make a response"),
    };
    let status = res.status();
    let headers = res.headers().clone();
    let streaming = headers.get("content-type").is_some_and(|v| v.as_bytes().starts_with(b"text/event-stream"));
    let mut out = Response::builder().status(status);
    for (k, v) in headers.iter().filter(|(k, _)| !HOP.contains(&k.as_str())) { out = out.header(k, v); }
    if streaming {
        tap.log(json!({"kind":"http","method":parts.method.as_str(),"path":path_q,"status":status.as_u16(),"req":body_json(&bytes),"res":"<event-stream>"}));
        return out.body(Body::from_stream(res.bytes_stream())).expect("copied headers make a response");
    }
    let rbytes = res.bytes().await.unwrap_or_default();
    let set_cookie: Vec<_> = headers.get_all("set-cookie").iter().filter_map(|v| v.to_str().ok()).collect();
    tap.log(json!({"kind":"http","method":parts.method.as_str(),"path":path_q,"status":status.as_u16(),
        "req":body_json(&bytes),"res":body_json(&rbytes),"setCookie":set_cookie,
        "contentType": headers.get("content-type").and_then(|v| v.to_str().ok())}));
    out.body(Body::from(rbytes)).expect("copied headers make a response")
}

async fn ws_proxy(tap: Arc<Tap>, ws: WebSocketUpgrade, path_q: String, headers: HeaderMap) -> Response<Body> {
    let conn = tap.conns.fetch_add(1, Ordering::Relaxed);
    let url = format!("{}{}", tap.upstream.replacen("http", "ws", 1), path_q);
    let mut upstream_req = url.as_str().into_client_request().expect("the upstream URL is a ws URL");
    for (k, v) in forward_headers(&headers) {
        if k == "host" { continue; }
        upstream_req.headers_mut().insert(k, v);
    }
    if let Some(h) = headers.get("host") { upstream_req.headers_mut().insert("host", h.clone()); }
    let (up, resp) = match tokio_tungstenite::connect_async(upstream_req).await {
        Ok(x) => x,
        Err(tungstenite::Error::Http(r)) => {
            let status = r.status();
            let body = r.body().clone().unwrap_or_default();
            tap.log(json!({"kind":"ws-open","conn":conn,"path":path_q,"status":status.as_u16(),"res":body_json(&Bytes::from(body.clone()))}));
            return Response::builder().status(status).body(Body::from(body)).expect("a status and a body make a response");
        }
        Err(e) => {
            tap.log(json!({"kind":"ws-open","conn":conn,"path":path_q,"error":e.to_string()}));
            return Response::builder().status(StatusCode::BAD_GATEWAY).body(Body::from(e.to_string())).expect("a status and a body make a response");
        }
    };
    tap.log(json!({"kind":"ws-open","conn":conn,"path":path_q,"status":resp.status().as_u16()}));
    ws.on_upgrade(move |client| async move {
        let (mut cs, mut cr) = client.split();
        let (mut us, mut ur) = up.split();
        let t1 = tap.clone();
        let c2s = async move {
            while let Some(Ok(m)) = cr.next().await {
                let fwd = match m {
                    AxMsg::Text(t) => { t1.log(json!({"kind":"ws","conn":conn,"dir":"c2s","frame":parse_frame(&t)})); tungstenite::Message::Text(t.as_str().into()) }
                    AxMsg::Binary(b) => tungstenite::Message::Binary(b),
                    AxMsg::Ping(p) => tungstenite::Message::Ping(p),
                    AxMsg::Pong(p) => tungstenite::Message::Pong(p),
                    AxMsg::Close(_) => break,
                };
                if us.send(fwd).await.is_err() { break; }
            }
            let _ = us.close().await;
        };
        let t2 = tap.clone();
        let s2c = async move {
            let mut pending = String::new();
            while let Some(Ok(m)) = ur.next().await {
                let fwd = match m {
                    tungstenite::Message::Text(t) => {
                        if let Some(whole) = join_part(&mut pending, t.as_str()) {
                            t2.log(json!({"kind":"ws","conn":conn,"dir":"s2c","frame":parse_frame(&whole)}));
                        }
                        AxMsg::Text(t.as_str().into())
                    }
                    tungstenite::Message::Binary(b) => AxMsg::Binary(b),
                    tungstenite::Message::Ping(p) => AxMsg::Ping(p),
                    tungstenite::Message::Pong(p) => AxMsg::Pong(p),
                    tungstenite::Message::Close(_) => break,
                    tungstenite::Message::Frame(_) => continue,
                };
                if cs.send(fwd).await.is_err() { break; }
            }
            let _ = cs.close().await;
        };
        tokio::select! { _ = c2s => {}, _ = s2c => {} }
        tap.log(json!({"kind":"ws-close","conn":conn}));
    })
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();
    let _ = rustls::crypto::ring::default_provider().install_default();
    let args = Args::parse();
    let file = std::fs::OpenOptions::new().create(true).append(true).open(&args.out).expect("open out");
    let tap = Arc::new(Tap {
        upstream: args.upstream.trim_end_matches('/').to_owned(),
        http: reqwest::Client::builder().redirect(reqwest::redirect::Policy::none()).build().expect("an HTTP client with default TLS"),
        out: Mutex::new(file),
        start: Instant::now(),
        conns: AtomicU64::new(0),
    });
    let app = Router::new().fallback(proxy).with_state(tap);
    let listener = tokio::net::TcpListener::bind(&args.listen).await.expect("bind");
    let mut stdout = tokio::io::stdout();
    let _ = stdout.write_all(format!("TAP LISTENING {}\n", listener.local_addr().expect("a bound listener has an address")).as_bytes()).await;
    let _ = stdout.flush().await;
    axum::serve(listener, app).await.expect("serving the tap");
}
