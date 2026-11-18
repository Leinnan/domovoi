//! Every `ServerSettings` the TypeScript server sent in the recorded sessions,
//! and every settings sample written from what the Domovoi server sends, must decode
//! into the typed record and encode back to exactly the JSON it came from.

use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Debug;
use domovoi_model::typed::{ModelFavorite, RuntimeMode, ServerSettings};

const FIXTURES: [&str; 2] = ["ts-session1", "ts-session2-boot"];

/// The places a recording carries settings.
const SNAPSHOT: &str = "subscribeServerConfig snapshot (config.settings)";
const SETTINGS_UPDATED: &str = "subscribeServerConfig settingsUpdated (payload.settings)";
const UPDATE_EXIT: &str = "server.updateSettings exit";
const KINDS: [&str; 3] = [SNAPSHOT, SETTINGS_UPDATED, UPDATE_EXIT];

/// The settings samples, written from what the Domovoi server sends.
const DEFAULTS_SAMPLE: &str = "settings.domovoi-server.json";
const PATCHED_SAMPLE: &str = "settings.domovoi-server-patched.json";
const FULL_SAMPLE: &str = "server.updateSettings.json";

/// How many settings `ServerSettings` defines.
const SETTINGS_IN_SCHEMA: usize = 82;

fn fixture(name: &str) -> Vec<Value> {
    let path = format!("{}/../../fixtures/golden/{name}/wire.ndjson", env!("CARGO_MANIFEST_DIR"));
    std::fs::read_to_string(&path).unwrap().lines().map(|line| serde_json::from_str(line).unwrap()).collect()
}

fn json_file(path: &str) -> Value {
    let path = format!("{}/{path}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{path}: {error}"))).unwrap()
}

fn sample(name: &str) -> Value {
    json_file(&format!("tests/samples/{name}"))
}

/// Every settings object in the fixtures, with the place it was sent in.
fn recorded() -> Vec<(&'static str, Value)> {
    let mut settings = Vec::new();
    for name in FIXTURES {
        let mut requests: BTreeMap<(i64, String), String> = BTreeMap::new();
        for line in fixture(name) {
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
            match (rpc.as_str(), frame["_tag"].as_str()) {
                ("subscribeServerConfig", Some("Chunk")) => {
                    for item in frame["values"].as_array().unwrap() {
                        match item["type"].as_str() {
                            Some("snapshot") => settings.push((SNAPSHOT, item["config"]["settings"].clone())),
                            Some("settingsUpdated") => settings.push((SETTINGS_UPDATED, item["payload"]["settings"].clone())),
                            _ => {}
                        }
                    }
                }
                ("server.updateSettings", Some("Exit")) => {
                    assert_eq!(frame["exit"]["_tag"], "Success", "the recorded settings update succeeded");
                    settings.push((UPDATE_EXIT, frame["exit"]["value"].clone()));
                }
                _ => {}
            }
        }
    }
    settings
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

/// The fallback variants (`Other(…)`) a decoded record holds, each with the
/// text leading up to it. Found in the record's `Debug` output outside string
/// literals, where the derived `Debug` of a variant is the only source of `Other(`.
fn fallbacks(record: &impl Debug) -> Vec<String> {
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
        } else if text[at..].starts_with("Other(") {
            let from = text[..at].char_indices().rev().nth(40).map_or(0, |(start, _)| start);
            let to = text[at..].char_indices().nth(60).map_or(text.len(), |(end, _)| at + end);
            found.push(format!("…{}…", &text[from..to]));
        }
    }
    found
}

/// Decodes `sent` as settings and encodes them again; `Err` says how the round trip failed.
fn round_trip(sent: &Value) -> Result<ServerSettings, String> {
    let settings: ServerSettings = serde_json::from_value(sent.clone()).map_err(|error| format!("does not decode: {error}"))?;
    let unknown = fallbacks(&settings);
    if !unknown.is_empty() {
        return Err(format!("decodes into a fallback variant:\n    {}", unknown.join("\n    ")));
    }
    let encoded = serde_json::to_value(&settings).map_err(|error| format!("does not encode: {error}"))?;
    if encoded != *sent {
        let mut lines = Vec::new();
        differences("", sent, &encoded, &mut lines);
        return Err(format!("encodes differently:\n    {}", lines.join("\n    ")));
    }
    Ok(settings)
}

#[test]
fn recorded_settings_round_trip_through_their_type() {
    let mut checked: BTreeMap<&str, usize> = BTreeMap::new();
    let mut sent_keys: BTreeSet<String> = BTreeSet::new();
    // Settings sent alike fail alike: one entry per distinct failure.
    let mut failures: BTreeMap<String, usize> = BTreeMap::new();
    for (kind, sent) in recorded() {
        *checked.entry(kind).or_default() += 1;
        sent_keys.extend(sent.as_object().expect("settings are an object").keys().cloned());
        if let Err(why) = round_trip(&sent) {
            *failures.entry(format!("{kind} {why}")).or_default() += 1;
        }
    }

    println!("settings checked, by where they were sent:");
    for kind in KINDS {
        println!("  {kind:<58} {}", checked.get(kind).copied().unwrap_or(0));
    }
    // The full sample holds every setting of the schema, so what it has and no recording does was typed from the schema alone.
    let full = sample(FULL_SAMPLE);
    let never_sent: Vec<&String> = full.as_object().unwrap().keys().filter(|key| !sent_keys.contains(*key)).collect();
    println!("settings the recordings sent: {} of {SETTINGS_IN_SCHEMA}; never sent (verified against the schema only): {never_sent:?}", sent_keys.len());

    let report: Vec<String> = failures.iter().map(|(failure, count)| format!("{failure}\n    ({count} times)")).collect();
    assert!(failures.is_empty(), "settings that do not round-trip:\n{}", report.join("\n"));
    for kind in KINDS {
        assert!(checked.get(kind).copied().unwrap_or(0) > 0, "the fixtures contain settings in `{kind}`, but none was checked");
    }
    for key in &sent_keys {
        assert!(full.get(key).is_some(), "the recordings send a setting the full sample lacks: {key}");
    }
}

#[test]
fn settings_samples_round_trip_through_their_type() {
    for name in [DEFAULTS_SAMPLE, PATCHED_SAMPLE, FULL_SAMPLE] {
        if let Err(why) = round_trip(&sample(name)) {
            panic!("tests/samples/{name} {why}");
        }
        println!("  {name:<34} round-trips");
    }
}

/// The defaults sample is what the Domovoi server loads its settings from
/// (`settings::load` over `domovoi_contracts::defaults::settings`), byte for byte in meaning.
#[test]
fn the_defaults_sample_is_what_the_domovoi_server_starts_with() {
    let bundled = json_file("../domovoi-contracts/defaults/settings.json");
    assert_eq!(sample(DEFAULTS_SAMPLE), bundled);
    round_trip(&bundled).unwrap();
}

/// The full sample sets every setting of the schema, so a setting the type
/// lacked or misnamed would show up as a difference in its round trip.
#[test]
fn the_full_sample_sets_every_setting() {
    let sent = sample(FULL_SAMPLE);
    assert_eq!(sent.as_object().unwrap().len(), SETTINGS_IN_SCHEMA);
    round_trip(&sent).unwrap();
}

/// What the client core reads from settings today reads directly off the type.
#[test]
fn the_settings_a_client_reads_are_at_hand() {
    let settings = round_trip(&sample(FULL_SAMPLE)).unwrap();

    let favorites: Vec<(&str, &str)> =
        settings.favorites.iter().flatten().flatten().map(|ModelFavorite { provider, model }| (provider.as_str(), model.as_str())).collect();
    assert_eq!(favorites, [("codex", "gpt-6-luna"), ("claudeAgent", "claude-fable-5"), ("claude-work", "claude-opus-5")]);

    let preferences = settings.provider_model_preferences.as_ref().unwrap().as_ref().unwrap();
    let codex = &preferences["codex"];
    assert_eq!(codex.hidden_models.as_deref(), Some(&["gpt-5-mini".to_owned(), "gpt-5-nano".to_owned()][..]));
    assert_eq!(codex.model_order.as_ref().map(Vec::len), Some(3));

    let selection = settings.default_model_selection.as_ref().unwrap().as_ref().unwrap();
    assert_eq!((selection.instance_id.as_str(), selection.model.as_str()), ("codex", "gpt-6"));
    assert_eq!(settings.default_runtime_mode, Some(RuntimeMode::AutoAcceptEdits));

    assert_eq!(settings.new_worktrees_start_from_origin, Some(false));
    assert_eq!(settings.fast_worktree_checkout, Some(true));
    assert_eq!(settings.worktrees_directory.as_deref(), Some("/Users/sam/worktrees"));
    assert_eq!(settings.branch_naming_mode.as_ref().map(|mode| mode.as_str()), Some("semantic"));
    assert_eq!(settings.branch_name_prefix.as_deref(), Some("sam"));
    assert_eq!(settings.sidebar_auto_settle_after_days.as_ref().unwrap().as_ref().and_then(|days| days.as_u64()), Some(3));
    assert_eq!(settings.sidebar_auto_settle_on_merge, Some(false));
    assert_eq!(settings.sidebar_auto_settle_keep_pull_requests, Some(true));
    assert_eq!(settings.add_project_base_directory.as_deref(), Some("/Users/sam/src"));
    assert_eq!(settings.default_auto_pull, Some(true));
    assert_eq!(settings.ghq_clone_layout, Some(true));
    assert_eq!(settings.keep_awake_while_agents_work, Some(false));
    assert_eq!(settings.enable_agent_browser_access, Some(false));
    assert_eq!(settings.enable_provider_update_checks, Some(false));
    assert_eq!(settings.continue_threads_after_server_update, Some(false));
    assert_eq!(settings.remove_agent_credits_on_merge, Some(true));
    assert_eq!(settings.review_guides_enabled, Some(true));

    // The Domovoi server's own defaults: never-set favorites are `null`, and auto-settle is a number of days.
    let defaults = round_trip(&sample(DEFAULTS_SAMPLE)).unwrap();
    assert_eq!((defaults.favorites, defaults.provider_model_preferences, defaults.default_model_selection), (Some(None), Some(None), Some(None)));
    assert_eq!(defaults.sidebar_auto_settle_after_days.unwrap().unwrap().as_u64(), Some(7));
    assert_eq!(defaults.default_runtime_mode, Some(RuntimeMode::FullAccess));

    // After patches, the Domovoi server keeps a `null` the TypeScript server would leave out, and partial worktree rules.
    let patched = round_trip(&sample(PATCHED_SAMPLE)).unwrap();
    assert_eq!(patched.default_thread_env_mode, Some(None));
    assert_eq!(patched.sidebar_auto_settle_after_days, Some(None));
}
