//! Server settings, persisted to `<userdata>/settings.json` (t3code
//! `serverSettings.ts` and `applyServerSettingsPatch`). Updates are broadcast
//! to `subscribeServerConfig` as `settingsUpdated`.
//!
//! The server reads its settings as the record [`ServerSettings`]. The file
//! is a JSON document that may hold more than the record reads: keys of a
//! newer or older build, at any depth, and values of a type this build does
//! not expect. [`Settings`] keeps those beside the record, so a load and a
//! save lose nothing and clients are sent the document as it is. (t3code
//! decodes the file with its schema and writes back the settings that differ
//! from their defaults, so it drops such keys on the next save.)
//!
//! Patch semantics follow t3code: the model-preference operations
//! (`setModelFavorites`, `setModelsHidden`, `setProviderModelOrder`), keyed maps
//! whose `null` entries remove keys, whole-value replacements for selections,
//! scripts and provider instances, and a deep merge for everything else.
//! A patch is merged into the document, not into the record, so that it may
//! carry keys this build does not know.
//! Not ported: background-activity profile derivation and the legacy
//! per-project override keys.

#![expect(clippy::disallowed_types, reason = "the user's settings file: keys of another build and values the record cannot read are kept as written, so a load and save loses nothing")]

use crate::config_failure::{Failure, SettingsOperation};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use serde_json::{Map, Value};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use domovoi_model::typed::{ModelFavorite, ProviderModelPreference, ServerSettings};
use tokio::sync::broadcast;

/// Keys whose patch value replaces the current value wholesale.
const REPLACE: [&str; 6] = [
    "defaultModelSelection",
    "fallbackModelSelection",
    "speechPostProcessingModelSelection",
    "defaultProjectScripts",
    "textGenerationModelSelection",
    "providerInstances",
];
/// Keyed maps: each entry merges into the saved one; a `null` entry removes it.
const ENTRY_MAPS: [&str; 7] =
    ["usageLimitSources", "textGenerators", "secretProviders", "sourceControlHosts", "usagePriceOverrides", "usageModelAliases", "projectSettingsOverrides"];
/// Operations and legacy spellings that are not settings themselves.
const NOT_MERGED: [&str; 10] = [
    "setModelFavorites",
    "setModelsHidden",
    "setProviderModelOrder",
    "migrateModelPreferences",
    "projectAgentBrowserAccessOverrides",
    "projectAutoPullOverrides",
    "projectScriptOverrides",
    "worktreeCleanup",
    "favorites",
    "providerModelPreferences",
];

/// The server's settings: the record this build reads (it derefs to it), and
/// what the settings document holds besides.
///
/// Written as JSON it is the document: the record with `unknown` laid over
/// it, which is exactly what was loaded or patched.
#[derive(Clone, Debug, PartialEq)]
pub struct Settings {
    record: ServerSettings,
    /// Keys of another build: what the settings document holds that `record`
    /// does not write back. A key this build does not know, at any depth, and
    /// a known key whose value the record could not read (it then holds the
    /// default, and the value stays here as it was). The one place the
    /// settings are kept as JSON.
    unknown: Map<String, Value>,
}

impl Default for Settings {
    /// Settings of which nothing is set, as a server that has not loaded any reads them.
    fn default() -> Self {
        Settings { record: ServerSettings::default(), unknown: Map::new() }
    }
}

impl std::ops::Deref for Settings {
    type Target = ServerSettings;

    fn deref(&self) -> &ServerSettings {
        &self.record
    }
}

impl Serialize for Settings {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.fields().serialize(serializer)
    }
}

fn encoded(record: &ServerSettings) -> Map<String, Value> {
    match serde_json::to_value(record) {
        Ok(Value::Object(fields)) => fields,
        _ => Map::new(),
    }
}

/// What `document` holds that `written` (a record written back) does not, so
/// that `written` with the result merged over it is `document` again. `None`
/// when `written` holds a key the document lacks: no merge would remove it.
fn beyond(document: &Map<String, Value>, written: &Map<String, Value>) -> Option<Map<String, Value>> {
    if written.keys().any(|key| !document.contains_key(key)) {
        return None;
    }
    let mut out = Map::new();
    for (key, held) in document {
        match (held, written.get(key)) {
            (Value::Object(held), Some(Value::Object(written))) => {
                let rest = beyond(held, written)?;
                if !rest.is_empty() {
                    out.insert(key.clone(), Value::Object(rest));
                }
            }
            (held, Some(written)) if held == written => {}
            (held, _) => {
                out.insert(key.clone(), held.clone());
            }
        }
    }
    Some(out)
}

/// Removes from `document` the value an undecodable part of it was found at:
/// the innermost object entry on `path` (an element of a list takes the whole
/// list with it). `false` when there is nothing to remove.
fn remove_at(document: &mut Map<String, Value>, path: &serde_path_to_error::Path) -> bool {
    let keys: Vec<&str> = path
        .iter()
        .map_while(|segment| match segment {
            serde_path_to_error::Segment::Map { key } => Some(key.as_str()),
            _ => None,
        })
        .collect();
    let Some((last, parents)) = keys.split_last() else { return false };
    let mut fields = &mut *document;
    for key in parents {
        match fields.get_mut(*key) {
            Some(Value::Object(inner)) => fields = inner,
            _ => return false,
        }
    }
    fields.remove(*last).is_some()
}

impl Settings {
    /// Reads an optional boolean flag stored in unknown/extension settings keys.
    pub fn unknown_bool(&self, key: &str) -> bool {
        self.unknown.get(key).and_then(Value::as_bool).unwrap_or(false)
    }

    /// The settings a document holds. The record reads as much of it as fits:
    /// a value that does not, however deep, is left to `unknown` (with the
    /// list or entry it makes unreadable), and the record has the default there.
    pub fn from_document(document: Map<String, Value>) -> Settings {
        let mut readable = document.clone();
        // Each round removes one value; a document has fewer values than bytes.
        for _ in 0..100_000 {
            let record: ServerSettings = match serde_path_to_error::deserialize(&Value::Object(readable.clone())) {
                Ok(record) => record,
                Err(error) => {
                    let top = error.path().iter().next().and_then(|segment| match segment {
                        serde_path_to_error::Segment::Map { key } => Some(key.clone()),
                        _ => None,
                    });
                    if remove_at(&mut readable, error.path()) || top.is_some_and(|key| readable.remove(&key).is_some()) {
                        continue;
                    }
                    break;
                }
            };
            let written = encoded(&record);
            if let Some(unknown) = beyond(&document, &written) {
                return Settings { record, unknown };
            }
            // The record wrote back more than it was given: read without the setting that did.
            let grew = written.iter().find(|(key, value)| match (document.get(*key), value) {
                (Some(Value::Object(held)), Value::Object(written)) => beyond(held, written).is_none(),
                (Some(_), _) => false,
                (None, _) => true,
            });
            match grew.map(|(key, _)| key.clone()) {
                Some(key) if readable.remove(&key).is_some() => continue,
                _ => break,
            }
        }
        // Nothing of it reads as settings: all of it is kept as it is.
        Settings { record: ServerSettings::default(), unknown: document }
    }

    /// The settings document: the record with the keys of another build laid over it.
    fn fields(&self) -> Map<String, Value> {
        let mut fields = encoded(&self.record);
        merge(&mut fields, &self.unknown);
        fields
    }

    /// The settings as the file holds them and clients are sent them.
    pub fn document(&self) -> Value {
        Value::Object(self.fields())
    }

    /// The settings as a reader's own view of the document: for a reader that
    /// takes less of a setting than the record requires (a provider instance
    /// that names no driver), so that it reads of a hand-edited file what it
    /// read before the settings were a record. `None` when the document does
    /// not fit the view at all.
    pub fn view<T: serde::de::DeserializeOwned>(&self) -> Option<T> {
        serde_json::from_value(self.document()).ok()
    }

    /// One setting as a reader's own view of it, as `view` is of them all:
    /// the object the document holds under `path` (a setting's key and, for
    /// one entry of a keyed setting, the entry's key after it). `None` when
    /// nothing is there, when it is no object, or when it does not fit the view.
    pub fn view_at<T: serde::de::DeserializeOwned>(&self, path: &[&str]) -> Option<T> {
        let mut fields = self.fields();
        let (last, parents) = path.split_last()?;
        for key in parents {
            match fields.remove(*key) {
                Some(Value::Object(inner)) => fields = inner,
                _ => return None,
            }
        }
        match fields.remove(*last) {
            Some(held @ Value::Object(_)) => serde_json::from_value(held).ok(),
            _ => None,
        }
    }

    /// The patch as it was sent, for the managed policy's vetting.
    pub fn patch_fields(patch: &SettingsPatch) -> &Map<String, Value> {
        &patch.0
    }

    /// The settings document, for the managed policy's vetting.
    pub fn document_fields(&self) -> Map<String, Value> {
        self.fields()
    }

    /// These settings with the managed policy's locks laid over them again.
    pub fn with_locks(self) -> Settings {
        let mut fields = self.fields();
        crate::policy::current().apply_locks(&mut fields);
        Settings::from_document(fields)
    }

    /// `applyServerSettingsPatch`: these settings with `patch` applied.
    pub fn patched(&self, patch: &SettingsPatch) -> Settings {
        Settings::from_document(apply_patch(&self.fields(), &patch.0))
    }

    /// `updateProviderInstance`: these settings with one provider instance created, replaced or removed.
    pub fn with_instance(&self, mutation: &InstanceMutation) -> Settings {
        let mut fields = self.fields();
        apply_instance_mutation(&mut fields, mutation);
        Settings::from_document(fields)
    }

}

/// What `server.updateSettings` is asked to change (`ClientServerSettingsPatch`).
///
/// Kept as the JSON object it arrives as: it is merged into the settings
/// document key by key, whatever its keys, so that a client of another build
/// can set what this build does not know. The operations and the keys with
/// rules of their own are read from it where they are applied. A patch that
/// is not an object changes nothing.
///
/// The theme defaults are left out as it is read, as t3code's
/// `ClientServerSettingsPatch` omits them: only the server sets those, once
/// it has validated the theme.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SettingsPatch(Map<String, Value>);

/// Keys of `ServerSettingsPatch` a client's patch never sets.
const SERVER_ONLY_PATCH_KEYS: [&str; 4] = ["defaultTheme", "defaultThemeSetAt", "defaultAppearance", "defaultAppearanceSetAt"];

impl<'de> Deserialize<'de> for SettingsPatch {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Ok(match Value::deserialize(deserializer)? {
            Value::Object(mut fields) => {
                for key in SERVER_ONLY_PATCH_KEYS {
                    fields.remove(key);
                }
                SettingsPatch(fields)
            }
            _ => SettingsPatch::default(),
        })
    }
}

/// One provider instance to create, replace or remove (`ProviderInstanceMutation`).
#[derive(Clone, Debug, PartialEq)]
pub struct InstanceMutation {
    /// `create` (kept if the instance exists), `remove`, or anything else to
    /// put the instance in place; without one nothing is done.
    operation: Option<String>,
    instance_id: String,
    /// The instance (`ProviderInstanceConfig`) as it was sent: it becomes
    /// part of the settings document, keys of another build included.
    instance: Value,
}

impl InstanceMutation {
    /// The mutation a request names; anything but an object names none.
    pub fn optional<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<Self>, D::Error> {
        let Value::Object(mut fields) = Value::deserialize(deserializer)? else { return Ok(None) };
        let text = |value: Option<&Value>| value.and_then(Value::as_str).map(str::to_owned);
        Ok(Some(InstanceMutation {
            operation: text(fields.get("operation")),
            instance_id: text(fields.get("instanceId")).unwrap_or_default(),
            instance: fields.remove("instance").unwrap_or(Value::Null),
        }))
    }
}

fn updates() -> &'static broadcast::Sender<Settings> {
    static TX: OnceLock<broadcast::Sender<Settings>> = OnceLock::new();
    TX.get_or_init(|| broadcast::channel(64).0)
}

/// The settings after each save.
pub fn subscribe() -> broadcast::Receiver<Settings> {
    updates().subscribe()
}

pub fn path(userdata: &Path) -> PathBuf {
    userdata.join("settings.json")
}

/// `deepMerge` of two objects: objects merge key by key; anything else replaces.
fn merge(current: &mut Map<String, Value>, patch: &Map<String, Value>) {
    for (key, value) in patch {
        match (current.get_mut(key), value) {
            (Some(Value::Object(existing)), Value::Object(value)) => merge(existing, value),
            _ => {
                current.insert(key.clone(), value.clone());
            }
        }
    }
}

/// `deepMerge`: objects merge key by key; anything else replaces.
fn deep_merge(current: &mut Value, patch: &Value) {
    match (current, patch) {
        (Value::Object(current), Value::Object(patch)) => merge(current, patch),
        (current, patch) => *current = patch.clone(),
    }
}

/// The built-in defaults as a settings document.
pub fn default_document() -> Map<String, Value> {
    encoded(&crate::defaults::settings())
}

/// The settings file as saved, if it holds an object.
fn saved(userdata: &Path) -> Option<Map<String, Value>> {
    match std::fs::read_to_string(path(userdata)).ok().and_then(|text| serde_json::from_str::<Value>(&text).ok()) {
        Some(Value::Object(saved)) => Some(saved),
        _ => None,
    }
}

/// The defaults with the saved settings merged over them, and the managed
/// policy's defaults and locks (`crate::policy`) laid in.
pub fn load(userdata: &Path) -> Settings {
    let fields = crate::policy::current().loaded(&default_document(), saved(userdata).as_ref());
    let settings = Settings::from_document(fields);
    crate::providers::remember_instances(&settings);
    settings
}

/// Writes settings atomically and announces them. The managed policy's
/// values are not written as the user's: each locked path keeps what the user
/// had stored there.
pub fn save(userdata: &Path, settings: &Settings) -> Result<(), Failure> {
    let p = path(userdata);
    let failed = |e: std::io::Error| Failure::Settings { settings_path: p.to_string_lossy().into_owned(), operation: SettingsOperation::WriteFile, cause: e.to_string() };
    let tmp = p.with_extension("json.tmp");
    let mut fields = settings.fields();
    let policy = crate::policy::current();
    if policy.is_managed() {
        let builtin = default_document();
        let mut stored = builtin.clone();
        if let Some(saved) = saved(userdata) {
            merge(&mut stored, &saved);
        }
        policy.persisted(&mut fields, &stored, &builtin);
    }
    std::fs::write(&tmp, serde_json::to_string_pretty(&fields).expect("a JSON object serializes")).map_err(failed)?;
    std::fs::rename(&tmp, &p).map_err(failed)?;
    crate::providers::remember_instances(settings);
    let _ = updates().send(settings.clone());
    Ok(())
}

fn text_of<'a>(value: &'a Value, key: &str) -> &'a str {
    value.get(key).and_then(Value::as_str).unwrap_or_default()
}

fn model_key(v: &Value) -> (String, String) {
    (text_of(v, "provider").to_owned(), text_of(v, "model").to_owned())
}

fn is_true(value: &Value, key: &str) -> bool {
    value.get(key) == Some(&Value::Bool(true))
}

/// The preferences of `provider` in `preferences`, made if there are none
/// (or something that is no record is in their place).
fn preference(preferences: &mut Map<String, Value>, provider: String) -> &mut Map<String, Value> {
    let entry = preferences.entry(provider).or_insert(Value::Null);
    if !entry.is_object() {
        let blank = ProviderModelPreference { hidden_models: Some(vec![]), model_order: Some(vec![]) };
        *entry = serde_json::to_value(blank).expect("a record serializes");
    }
    entry.as_object_mut().expect("made an object above")
}

/// `applyServerSettingsPatch`, on the settings document.
fn apply_patch(current: &Map<String, Value>, p: &Map<String, Value>) -> Map<String, Value> {
    let migrated = |key: &str| p.get("migrateModelPreferences").and_then(|m| m.get(key));

    // Favorites: an explicit list, else the current one, then the toggles.
    let mut favorites: Option<Vec<Value>> = p.get("favorites").or(current.get("favorites")).or(migrated("favorites")).and_then(Value::as_array).cloned();
    if let Some(changes) = p.get("setModelFavorites").and_then(Value::as_array) {
        let mut list = favorites.take().unwrap_or_default();
        for c in changes {
            let key = model_key(c);
            let exists = list.iter().any(|f| model_key(f) == key);
            if is_true(c, "favorite") {
                if !exists {
                    let favorite = ModelFavorite { provider: key.0.as_str().into(), model: key.1 };
                    list.push(serde_json::to_value(favorite).expect("a record serializes"));
                }
            } else {
                list.retain(|f| model_key(f) != key);
            }
        }
        favorites = Some(list);
    }

    let mut prefs: Option<Map<String, Value>> =
        p.get("providerModelPreferences").or(current.get("providerModelPreferences")).or(migrated("providerModelPreferences")).and_then(Value::as_object).cloned();
    if let Some(changes) = p.get("setModelsHidden").and_then(Value::as_array).filter(|a| !a.is_empty()) {
        let mut updated = prefs.take().unwrap_or_default();
        for c in changes {
            let (provider, model) = model_key(c);
            let entry = preference(&mut updated, provider);
            let mut hidden: Vec<Value> =
                entry.get("hiddenModels").and_then(Value::as_array).cloned().unwrap_or_default().into_iter().filter(|m| m.as_str() != Some(model.as_str())).collect();
            if is_true(c, "hidden") {
                hidden.push(Value::String(model));
            }
            entry.insert("hiddenModels".into(), Value::Array(hidden));
            if !entry.contains_key("modelOrder") {
                entry.insert("modelOrder".into(), Value::Array(vec![]));
            }
        }
        prefs = Some(updated);
    }
    if let Some(order) = p.get("setProviderModelOrder").filter(|v| v.is_object()) {
        let mut updated = prefs.take().unwrap_or_default();
        let entry = preference(&mut updated, text_of(order, "provider").to_owned());
        entry.insert("modelOrder".into(), order.get("modelOrder").cloned().unwrap_or(Value::Null));
        if !entry.contains_key("hiddenModels") {
            entry.insert("hiddenModels".into(), Value::Array(vec![]));
        }
        prefs = Some(updated);
    }

    let mut n = current.clone();
    let mergeable: Map<String, Value> = p
        .iter()
        .filter(|(k, v)| !v.is_null() || !REPLACE.contains(&k.as_str()))
        .filter(|(k, _)| !NOT_MERGED.contains(&k.as_str()) && !REPLACE.contains(&k.as_str()) && !ENTRY_MAPS.contains(&k.as_str()))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect();
    merge(&mut n, &mergeable);
    n.insert("favorites".into(), favorites.map(Value::Array).unwrap_or(Value::Null));
    n.insert("providerModelPreferences".into(), prefs.map(Value::Object).unwrap_or(Value::Null));
    if let Some(wc) = p.get("worktreeCleanup") {
        let merged = if wc.get("mode").and_then(Value::as_str) == Some("custom") {
            let mut rules = current.get("worktreeCleanup").and_then(|c| c.get("rules")).filter(|r| r.is_object()).cloned().unwrap_or_else(|| Value::Object(Map::new()));
            deep_merge(&mut rules, wc.get("rules").unwrap_or(&Value::Null));
            Value::Object(Map::from_iter([("mode".to_owned(), Value::String("custom".into())), ("rules".to_owned(), rules)]))
        } else {
            wc.clone()
        };
        n.insert("worktreeCleanup".into(), merged);
    }
    for key in REPLACE {
        if let Some(v) = p.get(key) {
            n.insert(key.into(), v.clone());
        }
    }
    for key in ENTRY_MAPS {
        let Some(entries) = p.get(key).and_then(Value::as_object) else { continue };
        let mut map = n.get(key).and_then(Value::as_object).cloned().unwrap_or_default();
        for (k, v) in entries {
            if v.is_null() {
                map.remove(k);
            } else if let Some(existing) = map.get_mut(k).filter(|e| e.is_object() && v.is_object()) {
                deep_merge(existing, v);
            } else {
                map.insert(k.clone(), v.clone());
            }
        }
        if key == "projectSettingsOverrides" {
            map.retain(|_, v| v.as_object().is_none_or(|o| !o.is_empty()));
        }
        n.insert(key.into(), Value::Object(map));
    }
    if let Some(dir) = p.get("worktreesDirectory").and_then(Value::as_str) {
        let old = current.get("worktreesDirectory").and_then(Value::as_str).unwrap_or_default();
        if dir != old {
            let mut previous: Vec<Value> = current.get("previousWorktreesDirectories").and_then(Value::as_array).cloned().unwrap_or_default();
            previous.retain(|d| d.as_str() != Some(dir));
            if !old.is_empty() && !previous.iter().any(|d| d.as_str() == Some(old)) {
                previous.push(Value::String(old.to_owned()));
            }
            n.insert("previousWorktreesDirectories".into(), Value::Array(previous));
        }
    }
    n
}

/// `updateProviderInstance`, on the settings document.
fn apply_instance_mutation(settings: &mut Map<String, Value>, mutation: &InstanceMutation) {
    let instances = settings.entry("providerInstances").or_insert(Value::Null);
    if !instances.is_object() {
        *instances = Value::Object(Map::new());
    }
    let instances = instances.as_object_mut().expect("made an object above");
    match mutation.operation.as_deref() {
        Some("remove") => {
            instances.remove(&mutation.instance_id);
        }
        Some("create") if instances.contains_key(&mutation.instance_id) => {}
        Some(_) => {
            instances.insert(mutation.instance_id.clone(), mutation.instance.clone());
        }
        None => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_reader_s_own_view_reads_the_document_as_it_is() {
        #[derive(Deserialize, Default, Debug, PartialEq)]
        #[serde(default)]
        struct Provider {
            label: Option<String>,
            account: Option<String>,
        }
        #[derive(Deserialize, Default, Debug, PartialEq)]
        #[serde(default, rename_all = "camelCase")]
        struct All {
            worktrees_directory: Option<String>,
        }
        // An entry the record cannot read (no `kind`), with a key it does not have.
        let document = json!({ "worktreesDirectory": "/wt", "secretProviders": { "op": { "label": "Vault", "account": "a-1" }, "odd": 7 } });
        let settings = Settings::from_document(document.as_object().unwrap().clone());
        assert_eq!(settings.secret_providers.as_ref().map_or(0, |providers| providers.len()), 0, "the record has no such provider");
        assert_eq!(settings.view_at::<Provider>(&["secretProviders", "op"]), Some(Provider { label: Some("Vault".into()), account: Some("a-1".into()) }));
        assert_eq!(settings.view_at::<Provider>(&["secretProviders", "odd"]), None, "not an object");
        assert_eq!(settings.view_at::<Provider>(&["secretProviders", "none"]), None);
        assert_eq!(settings.view_at::<Provider>(&["nothing", "op"]), None);
        assert_eq!(settings.view_at::<Provider>(&[]), None);
        assert_eq!(settings.view::<All>(), Some(All { worktrees_directory: Some("/wt".into()) }));
    }

    fn fields(value: Value) -> Map<String, Value> {
        value.as_object().expect("an object").clone()
    }

    fn mutation(value: Value) -> InstanceMutation {
        InstanceMutation::optional(value).unwrap().expect("a mutation")
    }

    #[test]
    fn patch_operations_follow_t3code() {
        let current = fields(json!({ "enableAssistantStreaming": false, "favorites": [{ "provider": "codex", "model": "a" }],
            "secretProviders": { "x": { "kind": "env" }, "y": { "kind": "env" } }, "nested": { "a": 1, "b": 2 },
            "worktreesDirectory": "/old", "previousWorktreesDirectories": [] }));
        let next = Value::Object(apply_patch(&current, &fields(json!({
            "enableAssistantStreaming": true,
            "setModelFavorites": [{ "provider": "codex", "model": "a", "favorite": false }, { "provider": "claudeAgent", "model": "b", "favorite": true }],
            "setModelsHidden": [{ "provider": "codex", "model": "m", "hidden": true }],
            "setProviderModelOrder": { "provider": "codex", "modelOrder": ["m", "n"] },
            "secretProviders": { "x": null, "z": { "kind": "file" } },
            "nested": { "b": 3 },
            "defaultModelSelection": { "instanceId": "codex", "model": "m" },
            "worktreesDirectory": "/new",
        }))));
        assert_eq!(next["enableAssistantStreaming"], true);
        assert_eq!(next["favorites"], json!([{ "provider": "claudeAgent", "model": "b" }]));
        assert_eq!(next["providerModelPreferences"], json!({ "codex": { "hiddenModels": ["m"], "modelOrder": ["m", "n"] } }));
        assert_eq!(next["secretProviders"], json!({ "y": { "kind": "env" }, "z": { "kind": "file" } }));
        assert_eq!(next["nested"], json!({ "a": 1, "b": 3 }));
        assert_eq!(next["defaultModelSelection"]["model"], "m");
        assert_eq!(next["previousWorktreesDirectories"], json!(["/old"]));
        assert!(next.get("setModelFavorites").is_none());
    }

    #[test]
    fn a_client_patch_never_sets_the_theme_defaults() {
        let patch: SettingsPatch = serde_json::from_value(json!({
            "defaultTheme": "x", "defaultThemeSetAt": "1", "defaultAppearance": "dark", "defaultAppearanceSetAt": "2", "enablePreviewTunnel": false,
        }))
        .unwrap();
        assert_eq!(patch.0, fields(json!({ "enablePreviewTunnel": false })));
    }

    #[test]
    fn a_preference_that_is_no_record_is_started_afresh() {
        let current = fields(json!({ "providerModelPreferences": { "codex": 5 } }));
        let next = apply_patch(&current, &fields(json!({ "setModelsHidden": [{ "provider": "codex", "model": "m", "hidden": true }] })));
        assert_eq!(next["providerModelPreferences"], json!({ "codex": { "hiddenModels": ["m"], "modelOrder": [] } }));
    }

    #[test]
    fn instances_and_persistence() {
        let mut s = Map::new();
        apply_instance_mutation(&mut s, &mutation(json!({ "operation": "create", "instanceId": "codex-2", "instance": { "driver": "codex" } })));
        apply_instance_mutation(&mut s, &mutation(json!({ "operation": "create", "instanceId": "codex-2", "instance": { "driver": "other" } })));
        assert_eq!(s["providerInstances"]["codex-2"]["driver"], "codex", "create keeps an existing instance");
        apply_instance_mutation(&mut s, &mutation(json!({ "operation": "remove", "instanceId": "codex-2" })));
        assert_eq!(s["providerInstances"], json!({}));
        assert_eq!(InstanceMutation::optional(json!("remove")).unwrap(), None, "anything but an object names no mutation");

        let dir = std::env::temp_dir().join(format!("domovoi-settings-{}", crate::util::new_uuid()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut rx = subscribe();
        let mut settings = load(&dir);
        assert_eq!(settings.keep_awake_while_agents_work, Some(true), "the defaults are read");
        settings = settings.patched(&serde_json::from_value(json!({ "keepAwakeWhileAgentsWork": false })).unwrap());
        save(&dir, &settings).unwrap();
        assert_eq!(load(&dir).keep_awake_while_agents_work, Some(false));
        // Other tests save settings too: ours is among what was announced.
        let mut announced = vec![];
        while let Ok(sent) = rx.try_recv() {
            announced.push(sent);
        }
        assert!(announced.contains(&settings));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// What a settings file of another build may hold.
    fn odd() -> Value {
        json!({
            "defaultAutoPull": "yes",
            "branchNamePrefix": "odd",
            "sidebarAutoSettleAfterDays": 7.0,
            "futureSetting": { "level": 3, "deep": { "deeper": [1, 2.0, { "x": null }] } },
            "storageCleanup": { "futureRule": 1, "logsAfterDays": "soon", "worktreeOnMerge": true },
            "providerInstances": { "codex-ok": { "driver": "codex", "config": { "setupMode": "managed" }, "futureField": 1 }, "bad": 5, "no-driver": { "displayName": "Nameless" } },
            "textGenerators": { "local": { "kind": "openaiCompatible", "baseUrl": "http://llm.test/v1" }, "broken": { "kind": 7 } },
            "favorites": [{ "provider": "codex", "model": "a" }, { "model": "no-provider" }],
            "hiddenThreadTags": ["wip", 7, "later"],
            "worktreesDirectory": 5,
            "projectSettingsOverrides": { "p1": { "defaultAutoPull": true, "futureOverride": 1 }, "p2": { "defaultAutoPull": "yes" } },
        })
    }

    #[test]
    fn a_document_is_read_as_far_as_it_fits_and_written_back_whole() {
        let settings = Settings::from_document(fields(odd()));
        assert_eq!(settings.document(), odd(), "nothing is lost or reworded");
        assert_eq!(json!(settings), odd(), "and clients are sent the document");

        // What fits is read.
        assert_eq!(settings.branch_name_prefix.as_deref(), Some("odd"));
        assert_eq!(settings.storage_cleanup.as_ref().and_then(|rules| rules.worktree_on_merge), Some(true), "a record is read beside its value that does not fit");
        let instances = settings.provider_instances.as_ref().expect("the instances that fit are read");
        assert_eq!(instances.keys().map(|id| id.as_str()).collect::<Vec<_>>(), ["codex-ok"]);
        assert_eq!(settings.text_generators.as_ref().map(|generators| generators.len()), Some(1));
        assert_eq!(settings.project_settings_overrides.as_ref().map(|projects| projects.keys().map(|id| id.as_str()).collect::<Vec<_>>()), Some(vec!["p1"]));
        assert_eq!(settings.hidden_thread_tags.as_deref(), Some(&["wip".to_owned(), "later".to_owned()][..]));
        // What does not is the default, and stays in the document.
        assert_eq!((settings.default_auto_pull, settings.worktrees_directory.clone(), settings.favorites.clone()), (None, None, None));
        assert_eq!(settings.storage_cleanup.as_ref().and_then(|rules| rules.logs_after_days), None);
        assert_eq!(settings.unknown["defaultAutoPull"], "yes");
        assert_eq!(settings.unknown["storageCleanup"], json!({ "futureRule": 1, "logsAfterDays": "soon" }));
        assert_eq!(settings.unknown["providerInstances"], json!({ "codex-ok": { "futureField": 1 }, "bad": 5, "no-driver": { "displayName": "Nameless" } }));
        assert!(!settings.unknown.contains_key("branchNamePrefix"), "what the record reads is not kept twice");
    }

    #[test]
    fn a_load_and_a_save_leave_the_file_as_it_was() {
        let dir = std::env::temp_dir().join(format!("domovoi-settings-{}", crate::util::new_uuid()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(path(&dir), odd().to_string()).unwrap();
        let loaded = load(&dir);
        let mut expected = Value::Object(encoded(&crate::defaults::settings()));
        deep_merge(&mut expected, &odd());
        assert_eq!(loaded.document(), expected, "the file over the defaults");
        save(&dir, &loaded).unwrap();
        let saved: Value = serde_json::from_str(&std::fs::read_to_string(path(&dir)).unwrap()).unwrap();
        assert_eq!(saved, expected);
        assert_eq!(load(&dir), loaded);

        // A file that is no object is no settings file.
        std::fs::write(path(&dir), "[1, 2]").unwrap();
        assert_eq!(load(&dir).document(), Value::Object(encoded(&crate::defaults::settings())));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn patches_and_updates_keep_the_keys_of_another_build() {
        let settings = Settings::from_document(fields(odd()));
        let patch: SettingsPatch = serde_json::from_value(json!({ "defaultAutoPull": true, "futureSetting": { "level": 4 }, "storageCleanup": { "logsAfterDays": 3, "newerRule": "x" } })).unwrap();
        let patched = settings.patched(&patch);
        assert_eq!(patched.default_auto_pull, Some(true), "a patch puts a wrong value right");
        assert_eq!(patched.storage_cleanup.as_ref().and_then(|rules| rules.logs_after_days), Some(Some(3)));
        let document = patched.document();
        assert_eq!(document["futureSetting"], json!({ "level": 4, "deep": { "deeper": [1, 2.0, { "x": null }] } }));
        assert_eq!(document["storageCleanup"], json!({ "futureRule": 1, "logsAfterDays": 3, "worktreeOnMerge": true, "newerRule": "x" }));
        assert_eq!(serde_json::from_value::<SettingsPatch>(json!([1])).unwrap(), SettingsPatch::default(), "a patch that is no object changes nothing");

        let with = settings.with_instance(&mutation(json!({ "operation": "upsert", "instanceId": "codex-9", "instance": { "driver": "codex", "futureField": 2 } })));
        assert!(with.provider_instances.as_ref().unwrap().contains_key("codex-9"));
        assert_eq!(with.document()["providerInstances"]["codex-9"], json!({ "driver": "codex", "futureField": 2 }));
    }

    #[test]
    fn the_default_settings_are_all_read() {
        let settings = Settings::from_document(encoded(&crate::defaults::settings()));
        assert!(settings.unknown.is_empty());
        assert_eq!(*settings, crate::defaults::settings());
    }
}
