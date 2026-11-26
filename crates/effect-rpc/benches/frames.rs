//! Wire codec over a real session: every WebSocket frame of the recording
//! in `fixtures/native/session1/wire.ndjson` (a thread's snapshots of some
//! hundred kilobytes are the worst case).
//!
//! `cargo bench -p effect-rpc`

use effect_rpc::message::{FromServer, decode_client_frame, encode_server_message};
use serde_json::Value;
use std::sync::LazyLock;

/// The recorded session. `DOMOVOI_FIXTURES` names another `fixtures` folder: a
/// build kept by `tools/perf.py` reads the recording it was saved with.
fn recording() -> std::path::PathBuf {
    let fixtures = std::env::var_os("DOMOVOI_FIXTURES").map_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures"), Into::into);
    fixtures.join("native/session1/wire.ndjson")
}

struct Frames {
    /// Client → server frames as sent on the wire.
    client: Vec<String>,
    /// Server → client messages, decoded once.
    server: Vec<FromServer>,
    /// The same server messages encoded, as the connection writes them.
    server_text: Vec<String>,
    /// And as MessagePack with named fields, the candidate binary encoding.
    server_msgpack: Vec<Vec<u8>>,
}

/// A message as MessagePack: structs as maps with their field names, and the
/// spellings a human-readable format gets, so every type that encodes to JSON
/// encodes to this unchanged.
fn msgpack(message: &FromServer) -> Vec<u8> {
    use serde::Serialize;
    let mut out = Vec::new();
    message.serialize(&mut rmp_serde::Serializer::new(&mut out).with_struct_map().with_human_readable()).expect("a message is plain data");
    out
}

fn from_msgpack(bytes: &[u8]) -> Option<FromServer> {
    use serde::Deserialize;
    FromServer::deserialize(&mut rmp_serde::Deserializer::new(bytes).with_human_readable()).ok()
}

/// Compressed as one frame would be: zstd at its fastest ordinary level.
fn compressed(bytes: &[u8]) -> Vec<u8> {
    zstd::bulk::compress(bytes, 1).expect("compresses")
}

static FRAMES: LazyLock<Frames> = LazyLock::new(|| {
    let path = recording();
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
    let server_msgpack = server.iter().map(msgpack).collect();
    Frames { client, server, server_text, server_msgpack }
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
    // What each candidate encoding puts on the wire for the whole session
    // (`tools/perf.py` reads these lines). Small frames are sent as they are:
    // compressing them costs more than it saves.
    let whole = |frames: &mut dyn Iterator<Item = &[u8]>, compress: bool| -> usize {
        frames.map(|frame| if compress && frame.len() >= 1024 { compressed(frame).len().min(frame.len()) } else { frame.len() }).sum()
    };
    let json = || f.server_text.iter().map(String::as_bytes);
    let binary = || f.server_msgpack.iter().map(Vec::as_slice);
    eprintln!("PERF frames.session_bytes.json {} bytes", whole(&mut json(), false));
    eprintln!("PERF frames.session_bytes.json_zstd {} bytes", whole(&mut json(), true));
    eprintln!("PERF frames.session_bytes.msgpack {} bytes", whole(&mut binary(), false));
    eprintln!("PERF frames.session_bytes.msgpack_zstd {} bytes", whole(&mut binary(), true));
    assert!(f.server_msgpack.iter().zip(&f.server).all(|(bytes, message)| from_msgpack(bytes).as_ref() == Some(message)), "every message reads back from MessagePack as it was");
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

/// The largest server message alone (a config chunk), encoded.
#[divan::bench]
fn largest_server_message() -> usize {
    let (i, _) = FRAMES.server_text.iter().enumerate().max_by_key(|(_, t)| t.len()).expect("frames");
    encode_server_message(divan::black_box(&FRAMES.server[i])).len()
}

/// Every server message of the session read back from its JSON, as a client
/// reads a frame into the message it is.
#[divan::bench]
fn decode_server_session() -> usize {
    FRAMES.server_text.iter().filter(|t| serde_json::from_str::<FromServer>(divan::black_box(t)).is_ok()).count()
}

/// Every server message of the session, encoded as MessagePack.
#[divan::bench]
fn encode_server_session_msgpack() -> usize {
    FRAMES.server.iter().map(|m| msgpack(divan::black_box(m)).len()).sum()
}

/// Every server message of the session read back from MessagePack.
#[divan::bench]
fn decode_server_session_msgpack() -> usize {
    FRAMES.server_msgpack.iter().filter(|b| from_msgpack(divan::black_box(b)).is_some()).count()
}

/// Every server frame of a kilobyte or more compressed, from its JSON and
/// from its MessagePack: what compression would add to each message's cost.
#[divan::bench]
fn compress_server_session_json() -> usize {
    FRAMES.server_text.iter().filter(|t| t.len() >= 1024).map(|t| compressed(divan::black_box(t.as_bytes())).len()).sum()
}

#[divan::bench]
fn compress_server_session_msgpack() -> usize {
    FRAMES.server_msgpack.iter().filter(|b| b.len() >= 1024).map(|b| compressed(divan::black_box(b)).len()).sum()
}
