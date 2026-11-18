//! Wire codec over a real session: every WebSocket frame recorded from the
//! TypeScript server in `fixtures/golden/ts-session1/wire.ndjson` (1742
//! frames; config chunks of about 190 KB are the worst case).
//!
//! `cargo bench -p effect-rpc`

use effect_rpc::frame_parts::{SOCKET_FRAME_PART_BYTES, split_socket_frame};
use effect_rpc::message::{FromServer, decode_client_frame, encode_server_message};
use serde_json::Value;
use std::sync::LazyLock;

struct Frames {
    /// Client → server frames as sent on the wire.
    client: Vec<String>,
    /// Server → client messages, decoded once.
    server: Vec<FromServer>,
    /// The same server messages encoded, as the connection writes them.
    server_text: Vec<String>,
}

static FRAMES: LazyLock<Frames> = LazyLock::new(|| {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../fixtures/golden/ts-session1/wire.ndjson");
    let text = std::fs::read_to_string(path).expect("fixture");
    let mut client = Vec::new();
    let mut server = Vec::new();
    for line in text.lines() {
        let rec: Value = serde_json::from_str(line).expect("ndjson");
        if rec["kind"] != "ws" {
            continue;
        }
        let frame = serde_json::to_string(&rec["frame"]).expect("frame");
        match rec["dir"].as_str() {
            Some("c2s") => client.push(frame),
            Some("s2c") => {
                if let Ok(m) = serde_json::from_str::<FromServer>(&frame) {
                    server.push(m);
                }
            }
            _ => {}
        }
    }
    let server_text = server.iter().map(encode_server_message).collect();
    Frames { client, server, server_text }
});

fn main() {
    let f = &*FRAMES;
    eprintln!(
        "{} client frames ({} B), {} server messages ({} B)",
        f.client.len(),
        f.client.iter().map(String::len).sum::<usize>(),
        f.server.len(),
        f.server_text.iter().map(String::len).sum::<usize>(),
    );
    divan::main();
}

/// Every client frame of the session, decoded.
#[divan::bench]
fn decode_client_session() -> usize {
    FRAMES.client.iter().map(|f| decode_client_frame(divan::black_box(f)).map_or(0, |m| m.len())).sum()
}

/// Every server message of the session, encoded.
#[divan::bench]
fn encode_server_session() -> usize {
    FRAMES.server.iter().map(|m| encode_server_message(divan::black_box(m)).len()).sum()
}

/// Every encoded server message through the frame splitter (`?frameParts=1`).
#[divan::bench]
fn split_server_session() -> usize {
    FRAMES.server_text.iter().map(|t| split_socket_frame(divan::black_box(t), SOCKET_FRAME_PART_BYTES).len()).sum()
}

/// The largest server message alone (a config chunk), encoded and split.
#[divan::bench]
fn largest_server_message() -> usize {
    let (i, _) = FRAMES.server_text.iter().enumerate().max_by_key(|(_, t)| t.len()).expect("frames");
    let text = encode_server_message(divan::black_box(&FRAMES.server[i]));
    split_socket_frame(&text, SOCKET_FRAME_PART_BYTES).len()
}
