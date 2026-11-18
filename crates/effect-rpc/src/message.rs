//! Transport-encoded RPC envelopes, mirroring `effect/rpc/RpcMessage.ts`
//! (`FromClientEncoded` / `FromServerEncoded`) under the JSON serialization.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;

/// Request ids are `string | number` on the wire; Effect's socket client uses
/// numeric ids rendered as strings, but both forms must round-trip verbatim.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RequestId {
    Num(i64),
    Str(String),
}

impl fmt::Display for RequestId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RequestId::Num(n) => write!(f, "{n}"),
            RequestId::Str(s) => f.write_str(s),
        }
    }
}

/// `RequestEncoded`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RequestEncoded {
    pub id: RequestId,
    pub tag: String,
    #[serde(default)]
    pub payload: Value,
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_notification: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discard: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sampled: Option<bool>,
}

impl RequestEncoded {
    pub fn is_notification(&self) -> bool {
        self.is_notification == Some(true)
    }
    pub fn is_discard(&self) -> bool {
        self.discard == Some(true)
    }
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// `FromClientEncoded`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "_tag")]
pub enum FromClient {
    Request(RequestEncoded),
    Ack {
        #[serde(rename = "requestId")]
        request_id: RequestId,
    },
    Interrupt {
        #[serde(rename = "requestId")]
        request_id: RequestId,
    },
    Ping,
    Eof,
}

/// One element of an encoded `Cause` (`ExitEncoded.cause`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "_tag")]
pub enum CauseEncoded {
    Fail {
        error: Value,
    },
    Die {
        defect: Value,
    },
    Interrupt {
        #[serde(rename = "fiberId")]
        fiber_id: Option<i64>,
    },
}

/// `ExitEncoded<unknown, unknown>`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "_tag")]
pub enum ExitEncoded {
    /// `void` successes (unit RPCs, stream completion) encode as `"value": null`
    /// under `Schema.toCodecJson` (checked with conformance/probe-exit.ts).
    Success {
        #[serde(default)]
        value: Value,
    },
    Failure { cause: Vec<CauseEncoded> },
}

impl ExitEncoded {
    pub fn success(value: Value) -> Self {
        ExitEncoded::Success { value }
    }
    pub fn void() -> Self {
        ExitEncoded::Success { value: Value::Null }
    }
    pub fn fail(error: Value) -> Self {
        ExitEncoded::Failure {
            cause: vec![CauseEncoded::Fail { error }],
        }
    }
    pub fn die(defect: Value) -> Self {
        ExitEncoded::Failure {
            cause: vec![CauseEncoded::Die { defect }],
        }
    }
    pub fn interrupt() -> Self {
        ExitEncoded::Failure {
            cause: vec![CauseEncoded::Interrupt { fiber_id: None }],
        }
    }
}

/// `FromServerEncoded` (minus server-originated `Request`, which t3code does not use).
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
    Defect {
        defect: Value,
    },
    Pong,
    ClientProtocolError {
        error: Value,
        #[serde(rename = "requestIds", default, skip_serializing_if = "Option::is_none")]
        request_ids: Option<Vec<RequestId>>,
    },
}

/// Mirrors `RpcSerialization.json.makeUnsafe().decode`: a frame holds either one
/// message or an array of messages.
pub fn decode_client_frame(frame: &str) -> Result<Vec<FromClient>, serde_json::Error> {
    // Straight into the messages; a frame that does not decode that way goes
    // through `Value` as before, so its error reads the same.
    let direct = if frame.trim_start().starts_with('[') {
        serde_json::from_str::<Vec<FromClient>>(frame)
    } else {
        serde_json::from_str::<FromClient>(frame).map(|m| vec![m])
    };
    if let Ok(messages) = direct {
        return Ok(messages);
    }
    let value: Value = serde_json::from_str(frame)?;
    match value {
        Value::Array(items) => items.into_iter().map(serde_json::from_value).collect(),
        other => Ok(vec![serde_json::from_value(other)?]),
    }
}

pub fn encode_server_message(message: &FromServer) -> String {
    serde_json::to_string(message).expect("FromServer is always serializable")
}
