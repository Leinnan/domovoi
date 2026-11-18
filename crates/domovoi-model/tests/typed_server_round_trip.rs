//! Everything a server told its clients about itself in the recorded
//! sessions (its config stream, its lifecycle stream, provider refreshes and
//! its environment descriptor) must decode into its typed record and encode
//! back to exactly the JSON it came from. So must the samples of what the
//! recordings lack: what the Domovoi server sends, and the shapes no session used.

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Debug;
use domovoi_model::typed::{
    EditorId, ExecutionEnvironmentDescriptor, ExecutionEnvironmentPlatformOs, KeybindingCommand, KeybindingWhenNode, ProviderOptionDescriptor,
    ResolvedKeybindingRule, ServerConfigIssue, ServerConfigStreamEvent, ServerInstallation, ServerLifecycleStreamEvent, ServerPackageRunner,
    ServerProvider, ServerProviderDelta, ServerProviderState, ServerProviderUpdatedPayload, ServerUpsertKeybindingResult,
};

const FIXTURES: [&str; 2] = ["ts-session1", "ts-session2-boot"];
const ENVIRONMENT_ROUTE: &str = "/.well-known/t3/environment";

/// Kinds the recorded sessions are known to contain. Checking none of one of
/// these means the walk over the fixtures broke, not that the types are right.
const KINDS_IN_FIXTURES: [&str; 9] = [
    "subscribeServerConfig snapshot",
    "subscribeServerConfig providerStatuses",
    "subscribeServerConfig settingsUpdated",
    "subscribeServerConfig environmentThemesUpdated",
    "subscribeServerConfig usageLimitSourcesUpdated",
    "subscribeServerLifecycle welcome",
    "subscribeServerLifecycle ready",
    "server.refreshProviders",
    "GET /.well-known/t3/environment",
];

fn fixture(name: &str) -> Vec<Value> {
    let path = format!("{}/../../fixtures/golden/{name}/wire.ndjson", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap().lines().map(|line| serde_json::from_str(line).unwrap()).collect()
}

fn sample(name: &str) -> Value {
    let path = format!("{}/tests/samples/{name}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{path}: {error}"))).unwrap()
}

/// What the recorded sessions hold of this family, by kind.
struct Recorded {
    payloads: Vec<(String, Value)>,
    /// Environment descriptors recorded as the bytes of a compressed body,
    /// which hold no JSON to read.
    unreadable_environment_bodies: usize,
}

fn recorded() -> Recorded {
    let mut recorded = Recorded { payloads: Vec::new(), unreadable_environment_bodies: 0 };
    for name in FIXTURES {
        let mut requests: BTreeMap<(i64, String), String> = BTreeMap::new();
        for line in fixture(name) {
            if line["kind"] == "http" {
                if line["path"] == ENVIRONMENT_ROUTE && line["method"] == "GET" {
                    match &line["res"] {
                        body @ Value::Object(_) => recorded.payloads.push((format!("GET {ENVIRONMENT_ROUTE}"), body.clone())),
                        _ => recorded.unreadable_environment_bodies += 1,
                    }
                }
                continue;
            }
            let frame = &line["frame"];
            if line["kind"] != "ws" || !frame.is_object() {
                continue;
            }
            let connection = line["conn"].as_i64().unwrap_or(0);
            if frame["_tag"] == "Request" {
                requests.insert((connection, frame["id"].to_string()), frame["tag"].as_str().unwrap_or_default().to_owned());
                continue;
            }
            let Some(rpc) = requests.get(&(connection, frame["requestId"].to_string())) else { continue };
            match (frame["_tag"].as_str(), rpc.as_str()) {
                (Some("Chunk"), "subscribeServerConfig" | "subscribeServerLifecycle") => {
                    for item in frame["values"].as_array().unwrap() {
                        recorded.payloads.push((format!("{rpc} {}", item["type"].as_str().unwrap_or("<no type>")), item.clone()));
                    }
                }
                (Some("Exit"), "server.refreshProviders" | "server.upsertKeybinding" | "server.removeKeybinding")
                    if frame["exit"]["_tag"] == "Success" =>
                {
                    recorded.payloads.push((rpc.clone(), frame["exit"]["value"].clone()));
                }
                _ => {}
            }
        }
    }
    recorded
}

/// Where `sent` and `encoded` differ, one line per path, so a field the typed
/// record lacks reads as "dropped".
fn differences(path: &str, sent: &Value, encoded: &Value, out: &mut Vec<String>) {
    match (sent, encoded) {
        (Value::Object(sent), Value::Object(encoded)) => {
            let keys: BTreeSet<&String> = sent.keys().chain(encoded.keys()).collect();
            for key in keys {
                let at = format!("{path}.{key}");
                match (sent.get(key), encoded.get(key)) {
                    (Some(sent), Some(encoded)) => differences(&at, sent, encoded, out),
                    (Some(sent), None) => out.push(format!("{at}: dropped (sent {sent})")),
                    (None, Some(encoded)) => out.push(format!("{at}: added (encoded {encoded})")),
                    (None, None) => {}
                }
            }
        }
        (Value::Array(sent), Value::Array(encoded)) if sent.len() == encoded.len() => {
            for (index, (sent, encoded)) in sent.iter().zip(encoded).enumerate() {
                differences(&format!("{path}[{index}]"), sent, encoded, out);
            }
        }
        _ if sent != encoded => out.push(format!("{path}: sent {sent}, encoded {encoded}")),
        _ => {}
    }
}

/// The places in a decoded record's `Debug` output, outside string literals,
/// where `marker` begins, each with the text around it.
fn occurrences(record: &impl Debug, marker: &str) -> Vec<String> {
    let text = format!("{record:?}");
    let (mut found, mut in_string, mut escaped) = (Vec::new(), false, false);
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
        } else if text[at..].starts_with(marker) {
            let from = text[..at].char_indices().rev().nth(40).map_or(0, |(start, _)| start);
            let to = text[at..].char_indices().nth(60).map_or(text.len(), |(end, _)| at + end);
            found.push(format!("…{}…", &text[from..to]));
        }
    }
    found
}

/// What a decoded record holds that this build could not read: fallback
/// variants (`Other(…)`) and list elements left out (`unread: [(…`).
fn unread(record: &impl Debug) -> Vec<String> {
    let mut found = occurrences(record, "Other(");
    found.extend(occurrences(record, "unread: [("));
    found
}

/// Decodes `record` as `T` and encodes it again; `Err` says how the round trip failed.
fn round_trip<T: DeserializeOwned + Serialize + Debug>(record: &Value) -> Result<(), String> {
    let typed: T = serde_json::from_value(record.clone()).map_err(|error| format!("does not decode: {error}"))?;
    let unknown = unread(&typed);
    if !unknown.is_empty() {
        return Err(format!("holds what this build could not read:\n    {}", unknown.join("\n    ")));
    }
    let encoded = serde_json::to_value(&typed).map_err(|error| format!("does not encode: {error}"))?;
    if encoded != *record {
        let mut lines = Vec::new();
        differences("", record, &encoded, &mut lines);
        return Err(format!("encodes differently:\n    {}", lines.join("\n    ")));
    }
    Ok(())
}

fn round_trip_as(kind: &str, payload: &Value) -> Result<(), String> {
    match kind.split(' ').next().unwrap_or_default() {
        "subscribeServerConfig" => round_trip::<ServerConfigStreamEvent>(payload),
        "subscribeServerLifecycle" => round_trip::<ServerLifecycleStreamEvent>(payload),
        "server.refreshProviders" => round_trip::<ServerProviderUpdatedPayload>(payload),
        "server.upsertKeybinding" | "server.removeKeybinding" => round_trip::<ServerUpsertKeybindingResult>(payload),
        "GET" => round_trip::<ExecutionEnvironmentDescriptor>(payload),
        other => Err(format!("no typed record for `{other}`")),
    }
}

#[test]
fn recorded_server_traffic_round_trips_through_its_types() {
    let recorded = recorded();
    let mut checked: BTreeMap<&str, usize> = BTreeMap::new();
    // Payloads of one kind usually fail alike: one entry per distinct failure.
    let mut failures: BTreeMap<String, usize> = BTreeMap::new();
    for (kind, payload) in &recorded.payloads {
        *checked.entry(kind).or_default() += 1;
        if let Err(why) = round_trip_as(kind, payload) {
            *failures.entry(format!("{kind} {why}")).or_default() += 1;
        }
    }

    for (kind, count) in &checked {
        println!("{kind}: {count}");
    }
    println!("GET {ENVIRONMENT_ROUTE} recorded as a compressed body (not checked): {}", recorded.unreadable_environment_bodies);
    for kind in KINDS_IN_FIXTURES {
        assert!(checked.contains_key(kind), "the fixtures hold no `{kind}`: the walk over them is broken");
    }
    let report: Vec<String> = failures.iter().map(|(failure, count)| format!("{count}× {failure}")).collect();
    assert!(failures.is_empty(), "{} kinds of failure:\n{}", failures.len(), report.join("\n"));
}

/// The samples of this family: each file, the type its items decode as, and
/// whether it holds a list of stream items or one document.
const SAMPLES: [(&str, &str, bool); 8] = [
    ("subscribeServerConfig.domovoi-server.json", "subscribeServerConfig", true),
    ("subscribeServerConfig.json", "subscribeServerConfig", true),
    ("subscribeServerLifecycle.domovoi-server.json", "subscribeServerLifecycle", true),
    ("subscribeServerLifecycle.json", "subscribeServerLifecycle", true),
    ("server.refreshProviders.domovoi-server.json", "server.refreshProviders", false),
    ("server.upsertKeybinding.json", "server.upsertKeybinding", false),
    ("server.removeKeybinding.json", "server.removeKeybinding", false),
    ("environment.domovoi-server.json", "GET", false),
];

#[test]
fn samples_round_trip_through_their_types() {
    let mut failures = Vec::new();
    let mut event_types: BTreeSet<String> = BTreeSet::new();
    for (file, kind, is_stream) in SAMPLES {
        let document = sample(file);
        let items: Vec<Value> = if is_stream { document.as_array().unwrap_or_else(|| panic!("{file} is not a list")).clone() } else { vec![document] };
        assert!(!items.is_empty(), "{file} is empty");
        for (index, item) in items.iter().enumerate() {
            if let Some(event_type) = item["type"].as_str().filter(|_| is_stream) {
                event_types.insert(format!("{kind} {event_type}"));
            }
            if let Err(why) = round_trip_as(kind, item) {
                failures.push(format!("{file}[{index}] {why}"));
            }
        }
        println!("{file}: {}", items.len());
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));

    // Between them the samples hold every event type of both streams.
    for event_type in [
        "subscribeServerConfig snapshot",
        "subscribeServerConfig keybindingsUpdated",
        "subscribeServerConfig providerStatuses",
        "subscribeServerConfig providerStatusDeltas",
        "subscribeServerConfig settingsUpdated",
        "subscribeServerConfig environmentThemesUpdated",
        "subscribeServerConfig usageLimitSourcesUpdated",
        "subscribeServerLifecycle welcome",
        "subscribeServerLifecycle ready",
        "subscribeServerLifecycle legacyThreadMigration",
    ] {
        assert!(event_types.contains(event_type), "no sample holds a `{event_type}`");
    }
}

#[test]
fn what_the_client_reads_is_there_to_read() {
    let events: Vec<ServerConfigStreamEvent> = serde_json::from_value(sample("subscribeServerConfig.domovoi-server.json")).unwrap();
    let ServerConfigStreamEvent::Snapshot(snapshot) = &events[0] else { panic!("the stream starts with a snapshot") };
    let config = &snapshot.config;
    assert_eq!(config.environment.environment_id.as_str(), "5f0c1c57-6f0e-4d0b-9a57-0e3f6f0a2b11");
    assert_eq!((config.environment.platform.os.as_str(), config.environment.platform.arch.as_str()), ("darwin", "arm64"));
    assert_eq!(config.environment.server_version, "0.1.9");

    let claude = config.providers.iter().find(|provider| provider.instance_id.as_str() == "claudeAgent").unwrap();
    assert_eq!((claude.driver.as_str(), claude.display_name.as_deref(), &claude.status), ("claudeAgent", Some("Claude"), &ServerProviderState::Ready));
    assert_eq!(claude.auth.label.as_deref(), Some("Claude Max Subscription"));
    assert_eq!(claude.slash_commands.as_deref().unwrap()[0].name, "review");
    let model = claude.models.iter().find(|model| model.is_default == Some(true)).unwrap();
    let descriptors = model.capabilities.as_ref().unwrap().option_descriptors.as_deref().unwrap();
    assert_eq!(descriptors[0].id(), Some("effort"));
    assert!(descriptors[0].options().iter().any(|choice| choice.id == "medium" && choice.is_default == Some(true)));
    assert_eq!((descriptors[1].id(), descriptors[1].options().len()), (Some("ultracode"), 0));

    let ServerConfigStreamEvent::KeybindingsUpdated(updated) = events.last().unwrap() else { panic!("the stream ends with the keybindings") };
    let rule = updated.payload.keybindings.iter().find(|rule| rule.command.script_id() == Some("dev")).unwrap();
    assert_eq!((rule.command.as_str(), rule.shortcut.key.as_str(), rule.shortcut.mod_key, rule.shortcut.shift_key), ("script.dev.run", "d", true, true));
    assert!(matches!(&rule.when_ast, Some(KeybindingWhenNode::Or { left, .. }) if **left == KeybindingWhenNode::Identifier { name: "terminalFocus".into() }));
    assert!(matches!(updated.payload.issues[0], ServerConfigIssue::KeybindingsInvalidEntry(_)));
}

#[test]
fn an_unknown_enum_value_survives_a_round_trip() {
    // The Domovoi server on Windows names the system as Rust does.
    let mut descriptor = sample("environment.domovoi-server.json");
    descriptor["platform"] = json!({ "os": "win32", "arch": "riscv64", "machine": "mainframe" });
    descriptor["capabilities"]["serverSelfUpdate"] = json!("teleport");
    let typed: ExecutionEnvironmentDescriptor = serde_json::from_value(descriptor.clone()).unwrap();
    assert_eq!(typed.platform.os, ExecutionEnvironmentPlatformOs::Other("win32".into()));
    assert_eq!(typed.platform.arch.as_str(), "riscv64");
    assert_eq!(serde_json::to_value(&typed).unwrap(), descriptor);

    let mut provider = sample("server.refreshProviders.domovoi-server.json")["providers"][0].clone();
    provider["status"] = json!("hibernating");
    provider["auth"]["status"] = json!("expired");
    let typed: ServerProvider = serde_json::from_value(provider.clone()).unwrap();
    assert_eq!(typed.status, ServerProviderState::Other("hibernating".into()));
    assert_eq!(serde_json::to_value(&typed).unwrap(), provider);

    let editor: EditorId = serde_json::from_value(json!("emacs")).unwrap();
    assert_eq!((editor.as_str(), serde_json::to_value(&editor).unwrap()), ("emacs", json!("emacs")));
}

#[test]
fn a_keybinding_command_is_a_listed_one_a_script_run_or_kept_as_sent() {
    for (sent, expected) in [
        ("chat.new", KeybindingCommand::ChatNew),
        ("thread.jump.9", KeybindingCommand::ThreadJump9),
        ("script.dev-server.run", KeybindingCommand::ScriptRun("script.dev-server.run".into())),
        // Not a script id the schema allows: upper case, empty, and too long.
        ("script.Dev.run", KeybindingCommand::Other("script.Dev.run".into())),
        ("script..run", KeybindingCommand::Other("script..run".into())),
        ("script.abcdefghijklmnopqrstuvwxy.run", KeybindingCommand::Other("script.abcdefghijklmnopqrstuvwxy.run".into())),
        ("time.travel", KeybindingCommand::Other("time.travel".into())),
    ] {
        let command: KeybindingCommand = serde_json::from_value(json!(sent)).unwrap();
        assert_eq!(command, expected);
        assert_eq!(serde_json::to_value(&command).unwrap(), json!(sent));
    }
    assert_eq!(KeybindingCommand::ScriptRun("script.dev-server.run".into()).script_id(), Some("dev-server"));
    assert_eq!(KeybindingCommand::ChatNew.script_id(), None);
}

#[test]
fn an_unknown_union_tag_survives_a_round_trip() {
    let event = json!({ "version": 2, "type": "pluginsUpdated", "payload": { "plugins": ["a", "b"] } });
    let typed: ServerConfigStreamEvent = serde_json::from_value(event.clone()).unwrap();
    assert!(matches!(typed, ServerConfigStreamEvent::Other(_)));
    assert_eq!(serde_json::to_value(&typed).unwrap(), event);

    let event = json!({ "version": 1, "sequence": 7, "type": "shuttingDown", "payload": { "at": "2026-11-17T09:00:00.000Z" } });
    let typed: ServerLifecycleStreamEvent = serde_json::from_value(event.clone()).unwrap();
    assert!(matches!(typed, ServerLifecycleStreamEvent::Other(_)));
    assert_eq!(serde_json::to_value(&typed).unwrap(), event);

    // An unknown node anywhere in a `when` tree, an unknown option and an
    // unknown issue leave the rest of what holds them readable.
    let rule = json!({
        "command": "chat.new",
        "shortcut": { "key": "n", "metaKey": false, "ctrlKey": false, "shiftKey": false, "altKey": false, "modKey": true },
        "whenAst": { "type": "and", "left": { "type": "identifier", "name": "composerFocus" }, "right": { "type": "xor", "nodes": [1, 2] } },
    });
    let typed: ResolvedKeybindingRule = serde_json::from_value(rule.clone()).unwrap();
    assert!(matches!(&typed.when_ast, Some(KeybindingWhenNode::And { right, .. }) if matches!(**right, KeybindingWhenNode::Other(_))));
    assert_eq!(serde_json::to_value(&typed).unwrap(), rule);

    let option = json!({ "id": "temperature", "label": "Temperature", "type": "number", "min": 0, "max": 2 });
    let typed: ProviderOptionDescriptor = serde_json::from_value(option.clone()).unwrap();
    assert_eq!((typed.id(), typed.label(), typed.options().len()), (Some("temperature"), Some("Temperature"), 0));
    assert_eq!(serde_json::to_value(&typed).unwrap(), option);

    let issue = json!({ "kind": "settings.malformed-config", "message": "settings.json is not valid JSON" });
    let typed: ServerConfigIssue = serde_json::from_value(issue.clone()).unwrap();
    assert!(matches!(typed, ServerConfigIssue::Other(_)));
    assert_eq!(serde_json::to_value(&typed).unwrap(), issue);
}

#[test]
fn a_server_installation_this_build_cannot_read_is_kept_as_sent() {
    for (sent, known) in [
        (json!({ "kind": "npx" }), true),
        (json!({ "kind": "pnpm-dlx" }), true),
        (json!({ "kind": "npm-global", "prefix": "/opt/node" }), true),
        (json!({ "kind": "t3-launcher", "launcher": "/opt/bin/t3 ", "baseDir": " /data/t3" }), true),
        // As the schema's own tests have it: these read as no installation at all.
        (json!({ "kind": "future-manager" }), false),
        (json!({ "kind": "npm-global" }), false),
        (json!({ "kind": "t3-launcher", "launcher": "t3" }), false),
        (json!("npx"), false),
    ] {
        let typed: ServerInstallation = serde_json::from_value(sent.clone()).unwrap();
        assert_eq!(!matches!(typed, ServerInstallation::Other(_)), known, "{sent}");
        assert_eq!(serde_json::to_value(&typed).unwrap(), sent);
    }
    let runner: ServerInstallation = serde_json::from_value(json!({ "kind": "bunx" })).unwrap();
    assert_eq!(runner, ServerInstallation::PackageRunner { kind: ServerPackageRunner::Bunx });
}

#[test]
fn a_record_missing_a_required_field_is_an_error() {
    let without = |mut record: Value, pointer: &str, field: &str| {
        record.pointer_mut(pointer).and_then(Value::as_object_mut).unwrap().remove(field).unwrap();
        record
    };
    let error = |result: Result<(), serde_json::Error>| result.unwrap_err().to_string();

    let descriptor = sample("environment.domovoi-server.json");
    assert!(error(serde_json::from_value::<ExecutionEnvironmentDescriptor>(without(descriptor.clone(), "", "serverVersion")).map(drop)).contains("serverVersion"));
    assert!(error(serde_json::from_value::<ExecutionEnvironmentDescriptor>(without(descriptor, "/platform", "arch")).map(drop)).contains("arch"));

    let provider = sample("server.refreshProviders.domovoi-server.json")["providers"][0].clone();
    assert!(error(serde_json::from_value::<ServerProvider>(without(provider.clone(), "", "models")).map(drop)).contains("models"));
    assert!(error(serde_json::from_value::<ServerProvider>(without(provider.clone(), "/auth", "status")).map(drop)).contains("status"));
    assert!(error(serde_json::from_value::<ServerProvider>(without(provider.clone(), "/models/0", "isCustom")).map(drop)).contains("isCustom"));
    // A delta may leave its lists out, and nothing else.
    let delta = without(provider.clone(), "", "models");
    assert!(serde_json::from_value::<ServerProviderDelta>(delta.clone()).unwrap().models.is_none());
    assert!(error(serde_json::from_value::<ServerProviderDelta>(without(delta, "", "checkedAt")).map(drop)).contains("checkedAt"));

    // A stream event of a known type must decode in full: it does not become `Other`.
    let events = sample("subscribeServerConfig.domovoi-server.json");
    assert!(error(serde_json::from_value::<ServerConfigStreamEvent>(without(events[0].clone(), "/config", "cwd")).map(drop)).contains("cwd"));
    assert!(error(serde_json::from_value::<ServerConfigStreamEvent>(without(events[3].clone(), "/payload", "issues")).map(drop)).contains("issues"));
    assert!(error(serde_json::from_value::<ServerConfigStreamEvent>(without(events[1].clone(), "", "type")).map(drop)).contains("type"));
    let lifecycle = sample("subscribeServerLifecycle.domovoi-server.json");
    assert!(error(serde_json::from_value::<ServerLifecycleStreamEvent>(without(lifecycle[0].clone(), "", "sequence")).map(drop)).contains("sequence"));
    assert!(error(serde_json::from_value::<ServerLifecycleStreamEvent>(without(lifecycle[1].clone(), "/payload", "at")).map(drop)).contains("at"));

    // So must a node of a known type, an option and an issue.
    assert!(error(serde_json::from_value::<KeybindingWhenNode>(json!({ "type": "and", "left": { "type": "identifier", "name": "a" } })).map(drop)).contains("right"));
    assert!(error(serde_json::from_value::<ProviderOptionDescriptor>(json!({ "id": "effort", "label": "Reasoning", "type": "select" })).map(drop)).contains("options"));
    assert!(error(serde_json::from_value::<ServerConfigIssue>(json!({ "kind": "keybindings.invalid-entry", "message": "bad rule" })).map(drop)).contains("index"));
}

#[test]
fn a_keybinding_this_build_cannot_read_is_left_out_of_the_list_and_kept() {
    let sent = json!({
        "keybindings": [
            { "command": "chat.new", "shortcut": { "key": "n", "metaKey": false, "ctrlKey": false, "shiftKey": false, "altKey": false, "modKey": true } },
            { "command": "chat.new", "shortcut": "mod+n" },
        ],
        "issues": [],
    });
    let typed: ServerUpsertKeybindingResult = serde_json::from_value(sent.clone()).unwrap();
    assert_eq!((typed.keybindings.len(), typed.keybindings.unread()), (1, 1));
    assert_eq!(serde_json::to_value(&typed).unwrap(), sent);
}
