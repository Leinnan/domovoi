//! What a client keeps of a server besides its threads, record by record:
//! every config, settings, provider, keybinding, environment, project and
//! thread shell the TypeScript server sent in the recorded sessions must
//! decode into its typed record and encode back to exactly the JSON it came
//! from, without landing in a fallback variant.
//!
//! The stream items and replies that carry these records are checked whole by
//! the tests of their families (`typed_server_round_trip.rs`,
//! `typed_settings_round_trip.rs`, `typed_workspace_round_trip.rs`); this one
//! counts the records themselves.

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use std::collections::BTreeMap;
use std::fmt::Debug;
use domovoi_model::typed::{
    ExecutionEnvironmentDescriptor, ProjectShell, ResolvedKeybindingRule, ServerConfig, ServerProvider, ServerSettings, ThreadShell,
};

const FIXTURES: [&str; 1] = ["session1"];

/// Every value the server sent over the socket in the fixtures, with the RPC it answered.
fn sent() -> Vec<(String, Value)> {
    let mut out = Vec::new();
    for name in FIXTURES {
        let path = format!("{}/../../fixtures/native/{name}/wire.ndjson", env!("CARGO_MANIFEST_DIR"));
        let mut requests: BTreeMap<(i64, String), String> = BTreeMap::new();
        for line in std::fs::read_to_string(&path).unwrap().lines() {
            let record: Value = serde_json::from_str(line).unwrap();
            let frame = &record["frame"];
            if record["kind"] != "ws" || !frame.is_object() {
                continue;
            }
            let connection = record["conn"].as_i64().unwrap_or(0);
            if frame["_tag"] == "Request" {
                requests.insert((connection, frame["id"].to_string()), frame["tag"].as_str().unwrap_or_default().to_owned());
                continue;
            }
            let Some(rpc) = requests.get(&(connection, frame["requestId"].to_string())) else { continue };
            if frame["_tag"] == "Chunk" {
                out.extend(frame["values"].as_array().unwrap().iter().map(|value| (rpc.clone(), value.clone())));
            } else if frame["_tag"] == "Exit" && frame["exit"]["_tag"] == "Success" {
                out.push((rpc.clone(), frame["exit"]["value"].clone()));
            }
        }
    }
    out
}

/// Whether a decoded record holds a fallback variant (`Other(…)`): found in
/// its `Debug` output outside string literals.
fn holds_fallback(record: &impl Debug) -> bool {
    let text = format!("{record:?}");
    let (mut in_string, mut escaped) = (false, false);
    for (at, character) in text.char_indices() {
        if in_string {
            match character {
                _ if escaped => escaped = false,
                '\\' => escaped = true,
                '"' => in_string = false,
                _ => {}
            }
        } else if character == '"' {
            in_string = true;
        } else if text[at..].starts_with("Other(") {
            return true;
        }
    }
    false
}

/// Counts of records checked, and how each kind's first failure read.
#[derive(Default)]
struct Checked {
    counts: BTreeMap<&'static str, usize>,
    failures: BTreeMap<&'static str, String>,
}

impl Checked {
    fn record<T: DeserializeOwned + Serialize + Debug>(&mut self, kind: &'static str, sent: &Value) {
        *self.counts.entry(kind).or_default() += 1;
        let outcome = match serde_json::from_value::<T>(sent.clone()) {
            Err(why) => Err(format!("does not decode: {why}")),
            Ok(typed) if holds_fallback(&typed) => Err("decodes into a fallback variant".to_owned()),
            Ok(typed) if serde_json::to_value(&typed).unwrap() != *sent => Err("encodes differently".to_owned()),
            Ok(_) => Ok(()),
        };
        if let Err(why) = outcome {
            self.failures.entry(kind).or_insert(why);
        }
    }

    fn each<T: DeserializeOwned + Serialize + Debug>(&mut self, kind: &'static str, list: &Value) {
        for sent in list.as_array().into_iter().flatten() {
            self.record::<T>(kind, sent);
        }
    }
}

#[test]
fn recorded_client_records_round_trip_one_by_one() {
    let mut checked = Checked::default();
    for (rpc, value) in sent() {
        match (rpc.as_str(), value["type"].as_str().or(value["kind"].as_str())) {
            ("subscribeServerConfig", Some("snapshot")) => {
                let config = &value["config"];
                checked.record::<ServerConfig>("config", config);
                checked.record::<ServerSettings>("settings", &config["settings"]);
                checked.record::<ExecutionEnvironmentDescriptor>("environment", &config["environment"]);
                checked.each::<ServerProvider>("provider", &config["providers"]);
                checked.each::<ResolvedKeybindingRule>("keybinding", &config["keybindings"]);
            }
            ("subscribeServerConfig", Some("providerStatuses")) => checked.each::<ServerProvider>("provider", &value["payload"]["providers"]),
            ("subscribeServerConfig", Some("settingsUpdated")) => checked.record::<ServerSettings>("settings", &value["payload"]["settings"]),
            ("server.updateSettings", _) => checked.record::<ServerSettings>("settings", &value),
            ("server.refreshProviders", _) => checked.each::<ServerProvider>("provider", &value["providers"]),
            ("subscribeServerLifecycle", _) => checked.record::<ExecutionEnvironmentDescriptor>("environment", &value["payload"]["environment"]),
            ("orchestration.subscribeShell", Some("snapshot")) => {
                checked.each::<ProjectShell>("project", &value["snapshot"]["projects"]);
                checked.each::<ThreadShell>("thread shell", &value["snapshot"]["threads"]);
                checked.each::<ThreadShell>("thread shell", &value["snapshot"]["archivedThreads"]);
            }
            ("orchestration.subscribeShell", Some("project.updated")) => checked.record::<ProjectShell>("project", &value["project"]),
            ("orchestration.subscribeShell", Some("thread.updated")) => checked.record::<ThreadShell>("thread shell", &value["thread"]),
            _ => {}
        }
    }
    println!("records checked, by kind:");
    for (kind, count) in &checked.counts {
        println!("  {kind:<14} {count}");
    }
    assert!(checked.failures.is_empty(), "records that do not round-trip: {:?}", checked.failures);
    for kind in ["config", "settings", "provider", "keybinding", "environment", "project", "thread shell"] {
        assert!(checked.counts.get(kind).copied().unwrap_or(0) > 0, "the fixtures contain `{kind}` records, but none was checked");
    }
}
