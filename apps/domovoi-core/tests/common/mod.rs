//! Shared by the test crates: what domovoi-server makes of a request the core sent.

use serde_json::Value;
use domovoi_model::typed::{Command, ThreadLaunchInput};

/// Whether the server reads the request `frame` as the core wrote it. The
/// server decodes what `orchestration.dispatchCommand` carries as a
/// [`Command`] and what `orchestration.launchThread` carries as a
/// [`ThreadLaunchInput`], and refuses what does not decode. So such a request
/// must decode, as a command of a type this build knows, and must encode
/// back to what was sent: nothing the core wrote is dropped or guessed.
/// Requests of other methods are not looked at.
pub fn read_by_the_server(frame: &Value) -> Result<(), String> {
    let sent = &frame["payload"];
    let back = match frame["tag"].as_str() {
        Some("orchestration.dispatchCommand") => match Command::from_value(sent.clone()) {
            Ok(Command::Other(other)) => return Err(format!("`{}` is not a command type the server knows", other.type_name())),
            Ok(command) => serde_json::to_value(&command),
            Err(why) => return Err(format!("the server refuses {sent}: {why}")),
        },
        Some("orchestration.launchThread") => match serde_json::from_value::<ThreadLaunchInput>(sent.clone()) {
            Ok(launch) => serde_json::to_value(&launch),
            Err(why) => return Err(format!("the server refuses the launch {sent}: {why}")),
        },
        _ => return Ok(()),
    };
    match back {
        Ok(back) if back == *sent => Ok(()),
        Ok(back) => Err(format!("sent as {sent}, read as {back}")),
        Err(why) => Err(format!("{sent} does not encode again: {why}")),
    }
}

/// Where the UUIDs in `text` start.
pub fn uuids(text: &str) -> Vec<usize> {
    let bytes = text.as_bytes();
    let mut found = vec![];
    let mut at = 0;
    while at + 36 <= bytes.len() {
        let is = bytes[at..at + 36].iter().enumerate().all(|(n, byte)| if matches!(n, 8 | 13 | 18 | 23) { *byte == b'-' } else { byte.is_ascii_hexdigit() });
        if is {
            found.push(at);
            at += 36;
        } else {
            at += 1;
        }
    }
    found
}

/// JSON with its objects' keys in order, so two encodings of one value read the same.
pub fn canonical(value: &Value) -> String {
    match value {
        Value::Object(fields) => {
            let sorted: std::collections::BTreeMap<&String, &Value> = fields.iter().collect();
            format!("{{{}}}", sorted.iter().map(|(key, value)| format!("{}:{}", Value::String((*key).clone()), canonical(value))).collect::<Vec<_>>().join(","))
        }
        Value::Array(items) => format!("[{}]", items.iter().map(canonical).collect::<Vec<_>>().join(",")),
        other => other.to_string(),
    }
}
