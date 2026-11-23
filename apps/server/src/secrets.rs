//! Credentials in provider settings (t3code `SettingsSecretValue` and
//! `settingsSecrets.ts`): a literal is kept out of `settings.json` and away
//! from clients. Settings hold [`REDACTED`] in its place, and the value
//! lives in `<userdata>/secrets/<sha256(instance id)>/<field>`, readable by
//! the user only. A secret reference (`{ provider, reference }`) is kept as
//! written; Domovoi has no secret providers to read one from yet.

#![expect(clippy::disallowed_types, reason = "a provider instance's `config`, an open record whose credential fields are moved here")]

use serde_json::Value;
use std::path::{Path, PathBuf};

/// What settings show for a saved credential (t3code's `SECRET_REDACTED`).
pub const REDACTED: &str = "\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}\u{2022}";

/// Credential fields of each driver's `config`.
fn fields(driver: &str) -> &'static [&'static str] {
    match driver {
        crate::antigravity::DRIVER => &["apiKey"],
        _ => &[],
    }
}

fn dir_in(userdata: &Path, instance: &str) -> PathBuf {
    userdata.join("secrets").join(crate::util::sha256_hex(instance.as_bytes()))
}

fn file_in(userdata: &Path, instance: &str, field: &str) -> PathBuf {
    dir_in(userdata, instance).join(field)
}

fn write(userdata: &Path, instance: &str, field: &str, value: &str) -> std::io::Result<()> {
    let dir = dir_in(userdata, instance);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(field);
    let tmp = path.with_extension("tmp");
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
        let mut f = std::fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
        f.write_all(value.as_bytes())?;
    }
    #[cfg(not(unix))]
    std::fs::write(&tmp, value)?;
    std::fs::rename(&tmp, &path)
}

/// The credential a setting holds for `instance`: the saved secret for
/// [`REDACTED`], the literal of a setting not moved yet, nothing for none.
/// A secret reference can't be read: `Err` names its provider.
pub fn resolve(instance: &str, field: &str, value: &Value) -> Result<String, String> {
    resolve_in(&crate::antigravity::state_dir(), instance, field, value)
}

pub fn resolve_in(userdata: &Path, instance: &str, field: &str, value: &Value) -> Result<String, String> {
    match value {
        Value::String(s) if s == REDACTED => Ok(std::fs::read_to_string(file_in(userdata, instance, field)).unwrap_or_default()),
        Value::String(s) => Ok(s.trim().to_owned()),
        Value::Object(reference) if reference.contains_key("provider") => Err(reference["provider"].as_str().unwrap_or("secret provider").to_owned()),
        _ => Ok(String::new()),
    }
}

/// Settings as they are saved and sent: each credential literal of a
/// provider instance moved to the secret store and [`REDACTED`] in its
/// place; an emptied one removed. Instances no longer in the settings
/// leave the store too.
pub fn stash(userdata: &Path, settings: crate::settings::Settings) -> crate::settings::Settings {
    let mut document = settings.document();
    let Some(instances) = document.get_mut("providerInstances").and_then(Value::as_object_mut) else { return settings };
    let mut changed = false;
    for (id, instance) in instances.iter_mut() {
        let driver = instance["driver"].as_str().unwrap_or_default().to_owned();
        let Some(config) = instance.get_mut("config").and_then(Value::as_object_mut) else { continue };
        for field in fields(&driver) {
            match config.get(*field) {
                Some(Value::String(s)) if s == REDACTED => {}
                Some(Value::String(s)) if s.trim().is_empty() => {
                    let _ = std::fs::remove_file(file_in(userdata, id, field));
                }
                Some(Value::String(s)) => {
                    let value = s.trim().to_owned();
                    match write(userdata, id, field, &value) {
                        Ok(()) => {
                            config.insert((*field).to_owned(), Value::String(REDACTED.to_owned()));
                            changed = true;
                        }
                        Err(e) => tracing::error!(instance = %id, "could not save a provider credential: {e}"),
                    }
                }
                _ => {}
            }
        }
    }
    // A removed instance's credentials go with it.
    if let Ok(entries) = std::fs::read_dir(userdata.join("secrets")) {
        let kept: std::collections::HashSet<String> = instances.keys().map(|id| crate::util::sha256_hex(id.as_bytes())).collect();
        for entry in entries.flatten() {
            if !kept.contains(&*entry.file_name().to_string_lossy()) {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
    }
    if !changed {
        return settings;
    }
    match document {
        Value::Object(fields) => crate::settings::Settings::from_document(fields),
        _ => settings,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn credentials_leave_the_settings_and_come_back_for_the_agent() {
        let userdata = std::env::temp_dir().join(format!("domovoi-secrets-{}", crate::util::new_uuid()));
        std::fs::create_dir_all(&userdata).unwrap();
        let doc = json!({ "providerInstances": { "ag": { "driver": "antigravity", "config": { "authMethod": "gemini-api-key", "apiKey": " k-123 " } } } });
        let Value::Object(doc) = doc else { unreachable!() };
        let saved = stash(&userdata, crate::settings::Settings::from_document(doc));
        let config = saved.document()["providerInstances"]["ag"]["config"].clone();
        assert_eq!(config["apiKey"], REDACTED, "{config}");
        assert_eq!(resolve_in(&userdata, "ag", "apiKey", &config["apiKey"]).unwrap(), "k-123");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(file_in(&userdata, "ag", "apiKey")).unwrap().permissions().mode() & 0o777, 0o600);
        }
        // Sent back unchanged: still the saved key. Emptied: gone.
        let again = stash(&userdata, saved.clone());
        assert_eq!(resolve_in(&userdata, "ag", "apiKey", &again.document()["providerInstances"]["ag"]["config"]["apiKey"]).unwrap(), "k-123");
        let Value::Object(cleared) = json!({ "providerInstances": { "ag": { "driver": "antigravity", "config": { "apiKey": "" } } } }) else { unreachable!() };
        stash(&userdata, crate::settings::Settings::from_document(cleared));
        assert_eq!(resolve_in(&userdata, "ag", "apiKey", &json!(REDACTED)).unwrap(), "");
        assert_eq!(resolve_in(&userdata, "ag", "apiKey", &json!({ "provider": "op", "reference": "x" })), Err("op".into()));
        let _ = std::fs::remove_dir_all(&userdata);
    }
}
