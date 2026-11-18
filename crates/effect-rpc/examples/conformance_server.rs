//! Test server driven by `conformance/effect-client.ts`, which uses the real
//! `effect/rpc` RpcClient to check wire compatibility.

use axum::{Router as Http, extract::{Query, ws::WebSocketUpgrade}, routing::get};
use effect_rpc::{ConnectionOptions, Router, axum_ws::serve_websocket};
use futures::stream;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::HashMap, sync::Arc, time::Duration};

struct Ctx;

#[derive(Deserialize, Serialize)]
struct EchoPayload { a: f64 }
#[derive(Deserialize)]
struct CountPayload { n: u32 }
#[derive(Serialize)]
struct Nope { _tag: &'static str, message: String }

#[tokio::main]
async fn main() {
    let port: u16 = std::env::args().nth(1).and_then(|p| p.parse().ok()).unwrap_or(0);
    let router = Arc::new(
        Router::<Ctx>::new()
            .unary("echo", |_, p: EchoPayload| async move { Ok::<_, Value>(p) })
            .unary("fail", |_, _: Value| async move {
                Err::<Value, _>(Nope { _tag: "Nope", message: "no".into() })
            })
            .unary("sleep", |_, _: Value| async move {
                tokio::time::sleep(Duration::from_secs(60)).await;
                Ok::<_, Value>(Value::Null)
            })
            .unary("big", |_, _: Value| async move { Ok::<_, Value>(json!("é".repeat(200_000))) })
            .stream("count", |_, p: CountPayload| {
                stream::unfold(0u32, move |i| async move {
                    if i >= p.n { return None; }
                    tokio::time::sleep(Duration::from_millis(2)).await;
                    Some((Ok::<_, Value>(i), i + 1))
                })
            }),
    );
    let app = Http::new().route(
        "/ws",
        get(move |ws: WebSocketUpgrade, Query(q): Query<HashMap<String, String>>| {
            let router = router.clone();
            async move {
                let options = ConnectionOptions {
                    frame_parts: q.contains_key(effect_rpc::frame_parts::SOCKET_FRAME_PARTS_QUERY_PARAM),
                    ..Default::default()
                };
                ws.on_upgrade(move |s| serve_websocket(s, router, Arc::new(Ctx), options))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await.unwrap();
    println!("LISTENING {}", listener.local_addr().unwrap().port());
    axum::serve(listener, app).await.unwrap();
}
