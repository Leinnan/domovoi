//! The messages of the RPC socket, as JSON.
//!
//! A frame is one message or an array of them. The client sends
//! [`FromClient`]: a request under an id of its choosing, the acknowledgement
//! of a stream's chunk, an interrupt, a ping. The server answers a request
//! with any number of `Chunk`s (a stream's items) and then exactly one
//! `Exit`, which says how it ended.
//!
//! These are the messages as a reader takes them apart. The server writes
//! its own with the answers in them already written
//! ([`crate::connection::Outgoing`]).

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A request's id: unique among the requests open on its socket.
pub type RequestId = String;

/// A call of the method `tag`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Request {
    pub id: RequestId,
    pub tag: String,
    #[serde(default)]
    pub payload: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "_tag")]
pub enum FromClient {
    Request(Request),
    /// The last chunk of a stream was taken: the next may be sent.
    Ack {
        #[serde(rename = "requestId")]
        request_id: RequestId,
    },
    /// Stop the request; it ends with an interrupted `Exit`.
    Interrupt {
        #[serde(rename = "requestId")]
        request_id: RequestId,
    },
    Ping,
}

/// One reason a request did not succeed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "_tag")]
pub enum CauseEncoded {
    /// A failure the method declares, as its own error record.
    Fail { error: Value },
    /// A failure it does not: a bug, or a request it could not read.
    Die { defect: Value },
    /// The client interrupted it, or its connection is closing.
    Interrupt,
}

/// How a request ended.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "_tag")]
pub enum ExitEncoded {
    /// A method that answers nothing, and a stream that ended, succeed with `null`.
    Success {
        #[serde(default)]
        value: Value,
    },
    Failure { cause: Vec<CauseEncoded> },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "_tag")]
pub enum FromServer {
    Chunk {
        #[serde(rename = "requestId")]
        request_id: RequestId,
        values: Vec<Value>,
    },
    Exit {
        #[serde(rename = "requestId")]
        request_id: RequestId,
        exit: ExitEncoded,
    },
    /// Something went wrong that belongs to no request: a frame the server
    /// could not read, or a client it will not serve.
    Defect {
        defect: Value,
    },
    Pong,
}

/// The messages of one frame from a client.
pub fn decode_client_frame(frame: &str) -> Result<Vec<FromClient>, serde_json::Error> {
    if frame.trim_start().starts_with('[') {
        serde_json::from_str::<Vec<FromClient>>(frame)
    } else {
        serde_json::from_str::<FromClient>(frame).map(|m| vec![m])
    }
}

pub fn encode_server_message(message: &FromServer) -> String {
    serde_json::to_string(message).expect("FromServer is always serializable")
}
