//! A server and a bridge in one process, on loopback, with no relays or
//! lookup: HTTP, WebSocket upgrades, the peer id, and redialing.

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::routing::get;
use axum::{Extension, Router};
use futures::{SinkExt, StreamExt};
use std::time::Duration;
use domovoi_remote::{Bridge, ClientEndpoint, Network, PathKind, RemotePeer, RemoteServer, SecretKey};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

fn router() -> Router {
    Router::new()
        .route("/peer", get(|Extension(RemotePeer(peer)): Extension<RemotePeer>| async move { peer.to_string() }))
        .route("/ws", get(|ws: WebSocketUpgrade| async move { ws.on_upgrade(echo) }))
}

async fn echo(mut socket: WebSocket) {
    while let Some(Ok(msg)) = socket.recv().await {
        if let Message::Text(t) = msg
            && socket.send(Message::Text(t)).await.is_err()
        {
            return;
        }
    }
}

/// One HTTP/1.1 GET through the bridge; returns the body.
async fn get_body(port: u16, path: &str) -> String {
    let mut tcp = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    tcp.write_all(format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n").as_bytes()).await.unwrap();
    let mut response = String::new();
    tcp.read_to_string(&mut response).await.unwrap();
    assert!(response.starts_with("HTTP/1.1 200"), "{response}");
    response.split("\r\n\r\n").nth(1).unwrap_or_default().to_owned()
}

#[tokio::test]
async fn closing_one_server_bridge_keeps_the_other_server_reachable() {
    let host_a = RemoteServer::start(SecretKey::generate(), Network::Direct { port: 0 }, router()).await.unwrap();
    let host_b = RemoteServer::start(SecretKey::generate(), Network::Direct { port: 0 }, router()).await.unwrap();
    let key = SecretKey::generate();
    let expected_peer = key.public().to_string();
    let endpoint = ClientEndpoint::bind(key, Network::Direct { port: 0 }).await.unwrap();
    let active = Bridge::start_on(&endpoint, host_a.loopback_ticket(), 0).await.unwrap();
    let observer = Bridge::start_on(&endpoint, host_b.loopback_ticket(), 0).await.unwrap();

    assert_eq!(get_body(observer.port(), "/peer").await, expected_peer);
    observer.stop().await;
    assert_eq!(get_body(active.port(), "/peer").await, expected_peer);

    active.stop().await;
    host_a.shutdown().await;
    host_b.shutdown().await;
}

#[tokio::test]
async fn http_and_websocket_cross_the_bridge() {
    let server = RemoteServer::start(SecretKey::generate(), Network::Direct { port: 0 }, router()).await.unwrap();
    let client_key = SecretKey::generate();
    let client_id = client_key.public();
    let bridge = Bridge::start(client_key, Network::Direct { port: 0 }, server.loopback_ticket(), 0).await.unwrap();

    // The router sees which key dialed.
    assert_eq!(get_body(bridge.port(), "/peer").await, client_id.to_string());

    // Several concurrent HTTP connections share the one iroh connection.
    let bodies = futures::future::join_all((0..8).map(|_| get_body(bridge.port(), "/peer"))).await;
    assert!(bodies.iter().all(|b| *b == client_id.to_string()));

    // WebSocket upgrade and a few round trips.
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{}/ws", bridge.port())).await.unwrap();
    for i in 0..5 {
        ws.send(tokio_tungstenite::tungstenite::Message::text(format!("hello {i}"))).await.unwrap();
        let reply = ws.next().await.unwrap().unwrap();
        assert_eq!(reply.into_text().unwrap().as_str(), format!("hello {i}"));
    }

    let status = bridge.status();
    assert!(status.connected);
    assert_eq!(status.path, Some(PathKind::Direct));
    assert!(status.error.is_none());

    bridge.stop().await;
    server.shutdown().await;
}

#[tokio::test]
async fn a_bad_ticket_fails_at_start() {
    let server = RemoteServer::start(SecretKey::generate(), Network::Direct { port: 0 }, router()).await.unwrap();
    let ticket = server.loopback_ticket();
    server.shutdown().await;
    let started = tokio::time::timeout(Duration::from_secs(40), Bridge::start(SecretKey::generate(), Network::Direct { port: 0 }, ticket, 0)).await;
    assert!(matches!(started, Ok(Err(_))), "dialing a stopped server must fail");
}

#[tokio::test]
async fn the_bridge_redials_after_the_server_restarts() {
    let key = SecretKey::generate();
    let port = free_udp_port();
    let server = RemoteServer::start(key.clone(), Network::Direct { port }, router()).await.unwrap();
    let bridge = Bridge::start(SecretKey::generate(), Network::Direct { port: 0 }, server.loopback_ticket(), 0).await.unwrap();
    get_body(bridge.port(), "/peer").await;

    server.shutdown().await;
    let server = RemoteServer::start(key, Network::Direct { port }, router()).await.unwrap();
    // The old connection is closed by the server; the next request dials again.
    tokio::time::sleep(Duration::from_millis(200)).await;
    let body = tokio::time::timeout(Duration::from_secs(30), get_body(bridge.port(), "/peer")).await.unwrap();
    assert!(!body.is_empty());
    assert!(bridge.status().connected);

    bridge.stop().await;
    server.shutdown().await;
}

/// A GET that may fail: `None` when the bridge closes the connection unanswered.
async fn try_get(port: u16, path: &str) -> Option<String> {
    let mut tcp = TcpStream::connect(("127.0.0.1", port)).await.ok()?;
    tcp.write_all(format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n").as_bytes()).await.ok()?;
    let mut response = String::new();
    tcp.read_to_string(&mut response).await.ok()?;
    response.starts_with("HTTP/1.1 200").then_some(response)
}

#[tokio::test]
async fn requests_fail_fast_while_the_server_is_away() {
    let key = SecretKey::generate();
    let port = free_udp_port();
    let server = RemoteServer::start(key.clone(), Network::Direct { port }, router()).await.unwrap();
    let bridge = Bridge::start(SecretKey::generate(), Network::Direct { port: 0 }, server.loopback_ticket(), 0).await.unwrap();
    get_body(bridge.port(), "/peer").await;
    server.shutdown().await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    // The first request after the server went away waits out one dial.
    assert!(tokio::time::timeout(Duration::from_secs(40), try_get(bridge.port(), "/peer")).await.unwrap().is_none());
    let status = bridge.status();
    assert!(!status.connected && status.error.is_some() && status.listening, "{status:?}");
    // The ones right behind it fail at once instead of dialing one by one.
    let started = std::time::Instant::now();
    let answers = futures::future::join_all((0..8).map(|_| try_get(bridge.port(), "/peer"))).await;
    assert!(answers.iter().all(Option::is_none));
    assert!(started.elapsed() < Duration::from_secs(5), "eight requests took {:?}", started.elapsed());

    // Once the server is back the bridge dials again by itself.
    let server = RemoteServer::start(key, Network::Direct { port }, router()).await.unwrap();
    let recovered = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            if try_get(bridge.port(), "/peer").await.is_some() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    })
    .await;
    assert!(recovered.is_ok(), "the bridge never reconnected");
    assert!(bridge.status().connected && bridge.status().error.is_none());
    bridge.stop().await;
    server.shutdown().await;
}

#[tokio::test]
async fn a_dropped_bridge_frees_its_port() {
    let server = RemoteServer::start(SecretKey::generate(), Network::Direct { port: 0 }, router()).await.unwrap();
    let bridge = Bridge::start(SecretKey::generate(), Network::Direct { port: 0 }, server.loopback_ticket(), 0).await.unwrap();
    let port = bridge.port();
    get_body(port, "/peer").await;
    drop(bridge);
    let freed = tokio::time::timeout(Duration::from_secs(5), async {
        while TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await;
    assert!(freed.is_ok(), "the loopback port still accepts connections after the bridge was dropped");
    server.shutdown().await;
}

/// A peer that opens a stream and never sends a request does not hold it forever.
#[tokio::test]
async fn a_stream_that_sends_nothing_is_closed() {
    let server = RemoteServer::start(SecretKey::generate(), Network::Direct { port: 0 }, router()).await.unwrap();
    let bridge = Bridge::start(SecretKey::generate(), Network::Direct { port: 0 }, server.loopback_ticket(), 0).await.unwrap();
    let mut idle = TcpStream::connect(("127.0.0.1", bridge.port())).await.unwrap();
    // One byte, so the bridge opens the stream; then silence.
    idle.write_all(b"G").await.unwrap();
    let mut rest = Vec::new();
    let closed = tokio::time::timeout(Duration::from_secs(45), idle.read_to_end(&mut rest)).await;
    assert!(closed.is_ok(), "the idle stream was still open after 45 s");
    // Other requests were served meanwhile.
    get_body(bridge.port(), "/peer").await;
    bridge.stop().await;
    server.shutdown().await;
}

fn free_udp_port() -> u16 {
    std::net::UdpSocket::bind(("0.0.0.0", 0)).unwrap().local_addr().unwrap().port()
}

/// Over n0's public relays and lookup: the client dials by endpoint id alone.
/// Needs the internet; run with `cargo test -p domovoi-remote -- --ignored`.
#[tokio::test]
#[ignore = "uses the internet (n0 relays and DNS)"]
async fn dials_by_id_over_n0() {
    let server = RemoteServer::start(SecretKey::generate(), Network::n0(), router()).await.unwrap();
    assert!(server.online(Duration::from_secs(20)).await, "server never reached a relay");
    let status = server.status();
    assert!(status.relay.is_some(), "{status:?}");
    let ticket = domovoi_remote::parse_ticket(&server.id().to_string()).unwrap();
    let started = std::time::Instant::now();
    let bridge = Bridge::start(SecretKey::generate(), Network::n0(), ticket, 0).await.unwrap();
    let body = get_body(bridge.port(), "/peer").await;
    assert!(!body.is_empty());
    eprintln!("connected in {:?}, status {:?}, server {:?}", started.elapsed(), bridge.status(), status);
    bridge.stop().await;
    server.shutdown().await;
}


/// With no direct paths on the client, traffic must cross n0's relay.
/// Needs the internet; run with `cargo test -p domovoi-remote -- --ignored`.
#[tokio::test]
#[ignore = "uses the internet (n0 relays and DNS)"]
async fn falls_back_to_the_relay() {
    let server = RemoteServer::start(SecretKey::generate(), Network::n0(), router()).await.unwrap();
    assert!(server.online(Duration::from_secs(20)).await, "server never reached a relay");
    let bridge = Bridge::start(SecretKey::generate(), Network::RelayOnly, server.ticket(), 0).await.unwrap();
    let body = get_body(bridge.port(), "/peer").await;
    assert!(!body.is_empty());
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{}/ws", bridge.port())).await.unwrap();
    ws.send(tokio_tungstenite::tungstenite::Message::text("over the relay")).await.unwrap();
    assert_eq!(ws.next().await.unwrap().unwrap().into_text().unwrap().as_str(), "over the relay");
    let status = bridge.status();
    eprintln!("relay status: {status:?}");
    assert_eq!(status.path, Some(PathKind::Relay));
    bridge.stop().await;
    server.shutdown().await;
}

/// An RPC socket goes over a connection of its own: frames as they are, of
/// any size, in both directions, and the stream's end is read as its end.
#[tokio::test]
async fn rpc_frames_cross_without_http() {
    let echo: domovoi_remote::RpcHandler = std::sync::Arc::new(|RemotePeer(peer), mut reader, mut writer| {
        Box::pin(async move {
            writer.send(peer.to_string().as_bytes()).await.unwrap();
            while let Ok(Some(frame)) = reader.next().await {
                writer.send(&frame).await.unwrap();
            }
            writer.finish().await;
        })
    });
    let server = RemoteServer::start_with_rpc(SecretKey::generate(), Network::Direct { port: 0 }, router(), Some(echo)).await.unwrap();
    let key = SecretKey::generate();
    let me = key.public().to_string();
    let bridge = Bridge::start(key, Network::Direct { port: 0 }, server.loopback_ticket(), 0).await.unwrap();

    // The server sees a stream once something is sent on it.
    let (mut reader, mut writer) = bridge.rpc().await.unwrap();
    writer.send(b"hello").await.unwrap();
    assert_eq!(reader.next().await.unwrap().as_deref(), Some(me.as_bytes()), "the handler is told which device it serves");
    assert_eq!(reader.next().await.unwrap().as_deref(), Some(&b"hello"[..]));
    writer.send(b"").await.unwrap();
    assert_eq!(reader.next().await.unwrap().as_deref(), Some(&b""[..]), "an empty frame is a frame");
    let large: Vec<u8> = (0..3_000_000u32).map(|i| (i % 251) as u8).collect();
    writer.send(&large).await.unwrap();
    assert_eq!(reader.next().await.unwrap(), Some(large));
    // HTTP still crosses the bridge beside it.
    assert_eq!(get_body(bridge.port(), "/peer").await, me);
    writer.finish().await;
    assert_eq!(reader.next().await.unwrap(), None, "the handler ended its half after ours");

    bridge.stop().await;
    server.shutdown().await;
}

/// A server that does not serve RPC this way refuses the connection: the
/// client learns it at once and takes its socket through HTTP.
#[tokio::test]
async fn a_server_without_rpc_streams_refuses_them() {
    let server = RemoteServer::start(SecretKey::generate(), Network::Direct { port: 0 }, router()).await.unwrap();
    let bridge = Bridge::start(SecretKey::generate(), Network::Direct { port: 0 }, server.loopback_ticket(), 0).await.unwrap();
    let refused = tokio::time::timeout(Duration::from_secs(10), bridge.rpc()).await.expect("refused without waiting out the dial");
    assert!(refused.is_err());
    assert!(!get_body(bridge.port(), "/peer").await.is_empty(), "HTTP is served as before");
    bridge.stop().await;
    server.shutdown().await;
}
