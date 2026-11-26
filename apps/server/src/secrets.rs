//! Credentials in provider settings (t3code `SettingsSecretValue` and
//! `settingsSecrets.ts`): a literal is kept out of `settings.json` and away
//! from clients. Settings hold [`REDACTED`] in its place, and the value
//! lives in `<userdata>/secrets/<sha256(instance id)>/<field>`, readable by
//! the user only. A secret reference (`{ provider, reference }`) is kept as
//! written; Domovoi has no secret providers to read one from yet.
//!
//! [`requests`] keeps what the user answers an agent's `request_secret`
//! card with, apart from the settings' secrets.

#![expect(clippy::disallowed_types, reason = "a provider instance's `config`, an open record whose credential fields are moved here")]

use serde_json::Value;
use std::path::{Path, PathBuf};

/// What settings show for a saved credential.
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
    crate::util::write_secret(&dir, &path, value.as_bytes())
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

/// Answers to `request_secret` cards. A saved
/// value lives in `<userdata>/secret-requests/<hex>.bin` (owner-only), named
/// by the one-use `secret-ref:<hex>` an HMAC of the thread and card ids
/// gives, so only this server can name it. Orchestration records only the
/// card's status: the value never reaches events, projections, logs or tool
/// results. A `file` card's value moves to `<userdata>/secret-requests/files/<hex>`
/// for the agent's own shell commands; so does a secret the user pasted in
/// chat. Values and files go after 24 hours.
pub mod requests {
    use hmac::{KeyInit, Mac};
    use serde::{Deserialize, Serialize};
    use std::path::{Path, PathBuf};

    const REF_PREFIX: &str = "secret-ref:";
    const TTL_MS: i64 = 24 * 60 * 60 * 1000;

    /// Why an answer, a ref or a file was refused.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Refused {
        StoreFailed,
        InvalidRef,
        ReadFailed,
        RefUnavailable,
        RefExpired,
        ConsumeFailed,
    }

    impl Refused {
        /// t3code's text for it.
        pub fn message(self) -> &'static str {
            match self {
                Refused::StoreFailed => "Could not store the secret.",
                Refused::InvalidRef => "That secretRef is not valid.",
                Refused::ReadFailed => "Could not read the secret.",
                Refused::RefUnavailable => "That secretRef was already used or does not exist. Ask the user again with request_secret.",
                Refused::RefExpired => "That secretRef expired. Ask the user again with request_secret.",
                Refused::ConsumeFailed => "Could not use that secretRef. Try again.",
            }
        }
    }

    #[derive(Serialize, Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Stored {
        project_id: String,
        value: String,
        saved_at: i64,
    }

    /// One reader or writer at a time, so no two consumers read one ref.
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn dir(userdata: &Path) -> PathBuf {
        userdata.join("secret-requests")
    }

    fn files_dir(userdata: &Path) -> PathBuf {
        dir(userdata).join("files")
    }

    fn owner_only_dir(path: &Path) -> std::io::Result<()> {
        std::fs::create_dir_all(path)?;
        crate::util::make_private(path)
    }

    /// Writes `bytes` to a new owner-only file; an existing one is left as it is (`Ok(false)`).
    fn create_new(path: &Path, bytes: &[u8]) -> std::io::Result<bool> {
        use std::io::Write;
        let partial = path.with_extension("partial");
        let _ = std::fs::remove_file(&partial);
        if path.exists() {
            return Ok(false);
        }
        let mut file = crate::util::create_private(&partial, true)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&partial, path)?;
        Ok(true)
    }

    fn salt(userdata: &Path) -> std::io::Result<Vec<u8>> {
        let path = dir(userdata).join("salt");
        match std::fs::read(&path) {
            Ok(salt) if salt.len() >= 32 => Ok(salt),
            _ => {
                owner_only_dir(&dir(userdata))?;
                let _ = std::fs::remove_file(&path);
                create_new(&path, &crate::util::random_bytes::<32>())?;
                std::fs::read(&path)
            }
        }
    }

    fn tag(userdata: &Path, message: &str) -> std::io::Result<String> {
        let key = salt(userdata)?;
        let mut mac = <hmac::Hmac<sha2::Sha256> as KeyInit>::new_from_slice(&key).map_err(std::io::Error::other)?;
        mac.update(message.as_bytes());
        Ok(hex::encode(mac.finalize().into_bytes())[..32].to_owned())
    }

    /// The ref of a card: only this server can make it, from the thread and card ids.
    pub fn ref_for(userdata: &Path, thread_id: &str, turn_item_id: &str) -> std::io::Result<String> {
        Ok(format!("{REF_PREFIX}{}", tag(userdata, &format!("{thread_id}\u{0}{turn_item_id}"))?))
    }

    fn hex_of(secret_ref: &str) -> Option<&str> {
        secret_ref.strip_prefix(REF_PREFIX).filter(|h| h.len() == 32 && h.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
    }

    fn value_file(userdata: &Path, secret_ref: &str) -> Option<PathBuf> {
        Some(dir(userdata).join(format!("{}.bin", hex_of(secret_ref)?)))
    }

    fn now_ms() -> i64 {
        crate::time_fmt::now_millis()
    }

    /// Keeps a card's answer. A value already saved for the card (an earlier
    /// answer whose recording failed) is kept, so racing answers never overwrite one.
    pub fn save(userdata: &Path, thread_id: &str, turn_item_id: &str, project_id: &str, value: &str) -> Result<(), Refused> {
        let _guard = crate::util::locked(&LOCK);
        sweep_locked(userdata);
        let secret_ref = ref_for(userdata, thread_id, turn_item_id).map_err(|_| Refused::StoreFailed)?;
        let file = value_file(userdata, &secret_ref).ok_or(Refused::StoreFailed)?;
        let stored = serde_json::to_vec(&Stored { project_id: project_id.to_owned(), value: value.to_owned(), saved_at: now_ms() }).map_err(|_| Refused::StoreFailed)?;
        owner_only_dir(&dir(userdata)).map_err(|_| Refused::StoreFailed)?;
        create_new(&file, &stored).map(|_| ()).map_err(|_| Refused::StoreFailed)
    }

    /// Forgets a card's answer (its recording failed, or no one can receive it).
    pub fn forget(userdata: &Path, thread_id: &str, turn_item_id: &str) {
        let _guard = crate::util::locked(&LOCK);
        if let Some(file) = ref_for(userdata, thread_id, turn_item_id).ok().and_then(|r| value_file(userdata, &r)) {
            let _ = std::fs::remove_file(file);
        }
    }

    /// The card's ref, while its answer is kept.
    pub fn saved_ref(userdata: &Path, thread_id: &str, turn_item_id: &str) -> Option<String> {
        let secret_ref = ref_for(userdata, thread_id, turn_item_id).ok()?;
        value_file(userdata, &secret_ref)?.is_file().then_some(secret_ref)
    }

    fn read_locked(userdata: &Path, secret_ref: &str, project_id: &str) -> Result<(PathBuf, String), Refused> {
        let file = value_file(userdata, secret_ref).ok_or(Refused::InvalidRef)?;
        let bytes = match std::fs::read(&file) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(Refused::RefUnavailable),
            Err(_) => return Err(Refused::ReadFailed),
        };
        let stored: Stored = serde_json::from_slice(&bytes).map_err(|_| Refused::RefUnavailable)?;
        if stored.project_id != project_id {
            return Err(Refused::RefUnavailable);
        }
        if now_ms() - stored.saved_at > TTL_MS {
            let _ = std::fs::remove_file(&file);
            return Err(Refused::RefExpired);
        }
        Ok((file, stored.value))
    }

    /// Spends a ref: its value, which leaves the store, so no ref is used twice.
    /// Only a tool of the ref's own project may spend it.
    pub fn consume(userdata: &Path, secret_ref: &str, project_id: &str) -> Result<String, Refused> {
        let _guard = crate::util::locked(&LOCK);
        let (file, value) = read_locked(userdata, secret_ref, project_id)?;
        std::fs::remove_file(&file).map_err(|_| Refused::ConsumeFailed)?;
        Ok(value)
    }

    /// Moves a saved `file` card's value to an owner-only file and returns
    /// its path. A retry gets the file the first call wrote.
    pub fn save_to_file(userdata: &Path, thread_id: &str, turn_item_id: &str, project_id: &str) -> Result<PathBuf, Refused> {
        let _guard = crate::util::locked(&LOCK);
        let secret_ref = ref_for(userdata, thread_id, turn_item_id).map_err(|_| Refused::StoreFailed)?;
        let hex = hex_of(&secret_ref).ok_or(Refused::StoreFailed)?.to_owned();
        let target = files_dir(userdata).join(&hex);
        if !target.is_file() {
            // The stored value goes only once the file is complete, so a failed write can retry.
            let (_, value) = read_locked(userdata, &secret_ref, project_id)?;
            owner_only_dir(&files_dir(userdata)).map_err(|_| Refused::StoreFailed)?;
            create_new(&target, value.as_bytes()).map_err(|_| Refused::StoreFailed)?;
        }
        match value_file(userdata, &secret_ref).map(std::fs::remove_file) {
            Some(Err(e)) if e.kind() != std::io::ErrorKind::NotFound => Err(Refused::ConsumeFailed),
            _ => Ok(target),
        }
    }

    /// Writes a secret the user pasted in chat to an owner-only file and
    /// returns its path; a retry with the same key gets the same file, unchanged.
    pub fn save_pasted(userdata: &Path, thread_id: &str, request_key: &str, value: &str) -> Result<PathBuf, Refused> {
        let _guard = crate::util::locked(&LOCK);
        sweep_locked(userdata);
        let hex = tag(userdata, &format!("{thread_id}\u{0}pasted\u{0}{request_key}")).map_err(|_| Refused::StoreFailed)?;
        let target = files_dir(userdata).join(hex);
        owner_only_dir(&files_dir(userdata)).map_err(|_| Refused::StoreFailed)?;
        create_new(&target, value.as_bytes()).map_err(|_| Refused::StoreFailed)?;
        Ok(target)
    }

    /// Removes answers and files older than a day.
    fn sweep_locked(userdata: &Path) {
        let now = now_ms();
        let old = |path: &Path| {
            std::fs::metadata(path).and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok()).is_some_and(|age| age.as_millis() as i64 > TTL_MS)
        };
        for folder in [dir(userdata), files_dir(userdata)] {
            let Ok(entries) = std::fs::read_dir(&folder) else { continue };
            for entry in entries.flatten() {
                let path = entry.path();
                let name = entry.file_name().to_string_lossy().into_owned();
                let expired = if name.ends_with(".bin") {
                    std::fs::read(&path).ok().and_then(|b| serde_json::from_slice::<Stored>(&b).ok()).is_none_or(|s| now - s.saved_at > TTL_MS)
                } else {
                    path.is_file() && name != "salt" && old(&path)
                };
                if expired {
                    let _ = std::fs::remove_file(&path);
                }
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn a_ref_is_spent_once_and_only_by_its_project() {
            let userdata = std::env::temp_dir().join(format!("domovoi-secret-requests-{}", crate::util::new_uuid()));
            assert_eq!(saved_ref(&userdata, "t1", "i1"), None);
            save(&userdata, "t1", "i1", "p1", "hunter2").unwrap();
            // A second answer keeps the first value.
            save(&userdata, "t1", "i1", "p1", "other").unwrap();
            let secret_ref = saved_ref(&userdata, "t1", "i1").unwrap();
            assert!(secret_ref.starts_with("secret-ref:") && secret_ref.len() == 11 + 32, "{secret_ref}");
            assert_ne!(Some(secret_ref.clone()), ref_for(&userdata, "t1", "i2").ok());
            assert_eq!(consume(&userdata, "secret-ref:nothex", "p1"), Err(Refused::InvalidRef));
            assert_eq!(consume(&userdata, &secret_ref, "p2"), Err(Refused::RefUnavailable));
            assert_eq!(consume(&userdata, &secret_ref, "p1").unwrap(), "hunter2");
            assert_eq!(consume(&userdata, &secret_ref, "p1"), Err(Refused::RefUnavailable));
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(std::fs::metadata(dir(&userdata)).unwrap().permissions().mode() & 0o777, 0o700);
            }
            let _ = std::fs::remove_dir_all(&userdata);
        }

        #[test]
        fn a_file_card_moves_its_value_to_a_file_once() {
            let userdata = std::env::temp_dir().join(format!("domovoi-secret-requests-{}", crate::util::new_uuid()));
            save(&userdata, "t1", "i1", "p1", "pin 1234").unwrap();
            let file = save_to_file(&userdata, "t1", "i1", "p1").unwrap();
            assert_eq!(std::fs::read_to_string(&file).unwrap(), "pin 1234");
            assert_eq!(saved_ref(&userdata, "t1", "i1"), None, "the stored copy is gone");
            assert_eq!(save_to_file(&userdata, "t1", "i1", "p1").unwrap(), file, "a retry gets the same file");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(std::fs::metadata(&file).unwrap().permissions().mode() & 0o777, 0o600);
            }
            let pasted = save_pasted(&userdata, "t1", "k", "pw").unwrap();
            assert_eq!(save_pasted(&userdata, "t1", "k", "changed").unwrap(), pasted);
            assert_eq!(std::fs::read_to_string(&pasted).unwrap(), "pw");
            let _ = std::fs::remove_dir_all(&userdata);
        }
    }
}

/// Where the server's own credentials are kept:
/// apart from `secrets/`, whose entries follow the provider instances.
fn server_secrets(userdata: &Path) -> PathBuf {
    userdata.join("server-secrets")
}

/// A standalone issue tracker's secret name,
/// stable across releases so a saved key survives upgrades.
fn tracker_secret(tracker: &str) -> String {
    use base64::Engine;
    format!("issue-tracker-{}-token", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(tracker))
}

/// A server secret by its name, if one is stored.
pub fn server_secret(userdata: &Path, name: &str) -> std::io::Result<Option<String>> {
    match std::fs::read_to_string(server_secrets(userdata).join(name)) {
        Ok(value) => Ok(Some(value.trim().to_owned()).filter(|v| !v.is_empty())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Stores a server secret under `name` (`ServerSecretStore.set`), readable by the user only.
pub fn set_server_secret(userdata: &Path, name: &str, value: &str) -> std::io::Result<()> {
    let dir = server_secrets(userdata);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(name);
    crate::util::write_secret(&dir, &path, value.as_bytes())
}

/// Forgets a server secret (`ServerSecretStore.remove`); none stored is no failure.
pub fn remove_server_secret(userdata: &Path, name: &str) -> std::io::Result<()> {
    match std::fs::remove_file(server_secrets(userdata).join(name)) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// The credential stored for a standalone issue tracker, if one is.
pub fn tracker_token(userdata: &Path, tracker: &str) -> std::io::Result<Option<String>> {
    match std::fs::read_to_string(server_secrets(userdata).join(tracker_secret(tracker))) {
        Ok(token) => Ok(Some(token.trim().to_owned()).filter(|t| !t.is_empty())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

/// Stores a standalone issue tracker's credential, readable by the user only.
pub fn set_tracker_token(userdata: &Path, tracker: &str, token: &str) -> std::io::Result<()> {
    let dir = server_secrets(userdata);
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(tracker_secret(tracker));
    crate::util::write_secret(&dir, &path, token.as_bytes())
}

/// Forgets a standalone issue tracker's credential; none stored is no failure.
pub fn remove_tracker_token(userdata: &Path, tracker: &str) -> std::io::Result<()> {
    match std::fs::remove_file(server_secrets(userdata).join(tracker_secret(tracker))) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_tracker_credential_is_kept_apart_and_can_be_forgotten() {
        let userdata = std::env::temp_dir().join(format!("domovoi-tracker-secrets-{}", crate::util::new_uuid()));
        assert_eq!(tracker_secret("linear"), "issue-tracker-bGluZWFy-token");
        assert_eq!(tracker_token(&userdata, "linear").unwrap(), None);
        set_tracker_token(&userdata, "linear", "lin_api_1").unwrap();
        assert_eq!(tracker_token(&userdata, "linear").unwrap().as_deref(), Some("lin_api_1"));
        // Saving provider settings prunes `secrets/`, not the server's own.
        let Value::Object(doc) = json!({ "providerInstances": {} }) else { unreachable!() };
        std::fs::create_dir_all(userdata.join("secrets").join("gone")).unwrap();
        stash(&userdata, crate::settings::Settings::from_document(doc));
        assert!(!userdata.join("secrets").join("gone").exists());
        assert_eq!(tracker_token(&userdata, "linear").unwrap().as_deref(), Some("lin_api_1"));
        remove_tracker_token(&userdata, "linear").unwrap();
        remove_tracker_token(&userdata, "linear").unwrap();
        assert_eq!(tracker_token(&userdata, "linear").unwrap(), None);
        let _ = std::fs::remove_dir_all(&userdata);
    }

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
