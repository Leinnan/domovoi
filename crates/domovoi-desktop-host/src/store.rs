//! Cross-platform secure key-value storage for `domovoi-core`'s `Store` effect
//! plus this device's persistent iroh identity key.
//!
//! - `DOMOVOI_STORE=memory`: in-memory only.
//! - `cache:*` keys: atomic JSON files in the platform cache directory so large
//!   offline thread caches never hit the OS keyring.
//! - Secret keys (`deviceKey`, `token:*`, `environments`, `activeEnvironment`):
//!   OS keyring in release builds, falling back (and defaulting in debug builds
//!   to avoid repeated ad-hoc macOS Keychain prompts) to a `0600`-permissioned
//!   atomic JSON file under the platform config directory (`DOMOVOI_DATA`
//!   overrides the directory).
//!
//! Each client keeps its own folder and keyring service ([`Identity`]).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use domovoi_remote::SecretKey;

use crate::Identity;

const DEVICE_KEY_NAME: &str = "deviceKey";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Backend {
    Memory,
    File,
    KeyringWithFileFallback,
}

pub struct SecureStore {
    backend: Backend,
    keyring_service: &'static str,
    data_dir: PathBuf,
    cache_dir: PathBuf,
    memory: Mutex<BTreeMap<String, String>>,
    file_lock: Mutex<()>,
}

impl SecureStore {
    pub fn from_env(identity: Identity) -> Self {
        let memory_only = cfg!(test) || std::env::var("DOMOVOI_STORE").ok().as_deref() == Some("memory");
        let data_dir = default_data_dir(identity);
        let cache_dir = default_cache_dir(identity, &data_dir);
        let backend = if memory_only {
            Backend::Memory
        } else if cfg!(debug_assertions) || std::env::var("DOMOVOI_STORE").ok().as_deref() == Some("file") {
            Backend::File
        } else {
            Backend::KeyringWithFileFallback
        };
        Self {
            backend,
            keyring_service: identity.keyring_service,
            data_dir,
            cache_dir,
            memory: Mutex::new(BTreeMap::new()),
            file_lock: Mutex::new(()),
        }
    }

    /// Nothing written anywhere (tests).
    pub fn in_memory() -> Self {
        Self {
            backend: Backend::Memory,
            keyring_service: "",
            data_dir: PathBuf::new(),
            cache_dir: PathBuf::new(),
            memory: Mutex::new(BTreeMap::new()),
            file_lock: Mutex::new(()),
        }
    }

    /// Files in `data_dir` only, no keyring (tests).
    pub fn in_dir(data_dir: PathBuf) -> Self {
        let cache_dir = data_dir.join("cache");
        Self {
            backend: Backend::File,
            keyring_service: "",
            data_dir,
            cache_dir,
            memory: Mutex::new(BTreeMap::new()),
            file_lock: Mutex::new(()),
        }
    }

    pub fn load(&self, key: &str) -> Option<String> {
        if self.backend == Backend::Memory {
            return self.memory.lock().unwrap_or_else(std::sync::PoisonError::into_inner).get(key).cloned();
        }
        if let Some(path) = self.cache_file(key) {
            return std::fs::read_to_string(path).ok();
        }
        if self.backend == Backend::KeyringWithFileFallback
            && let Ok(entry) = keyring::Entry::new(self.keyring_service, key) {
                match entry.get_password() {
                    Ok(value) => return Some(value),
                    Err(keyring::Error::NoEntry) => {
                        // Fall through to file store in case a value was written before migration.
                    }
                    Err(err) => {
                        tracing::debug!(%key, %err, "keyring read failed, falling back to file store");
                    }
                }
            }
        let _guard = self.file_lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        self.read_file().ok().and_then(|map| map.get(key).cloned())
    }

    pub fn save(&self, key: &str, value: &str) -> bool {
        if self.backend == Backend::Memory {
            self.memory.lock().unwrap_or_else(std::sync::PoisonError::into_inner).insert(key.to_owned(), value.to_owned());
            return true;
        }
        if let Some(path) = self.cache_file(key) {
            return write_atomic(&path, value.as_bytes(), false);
        }
        if self.backend == Backend::KeyringWithFileFallback
            && let Ok(entry) = keyring::Entry::new(self.keyring_service, key)
                && entry.set_password(value).is_ok() {
                    return true;
                }
        let _guard = self.file_lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let Ok(mut map) = self.read_file() else {
            tracing::warn!("store file could not be read; refusing to overwrite");
            return false;
        };
        map.insert(key.to_owned(), value.to_owned());
        self.write_file(&map)
    }

    pub fn delete(&self, key: &str) -> bool {
        if self.backend == Backend::Memory {
            self.memory.lock().unwrap_or_else(std::sync::PoisonError::into_inner).remove(key);
            return true;
        }
        if let Some(path) = self.cache_file(key) {
            let _ = std::fs::remove_file(path);
            return true;
        }
        if self.backend == Backend::KeyringWithFileFallback
            && let Ok(entry) = keyring::Entry::new(self.keyring_service, key) {
                let _ = entry.delete_credential();
            }
        let _guard = self.file_lock.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let Ok(mut map) = self.read_file() else {
            return false;
        };
        if map.remove(key).is_some() {
            return self.write_file(&map);
        }
        true
    }

    /// Persistent 64-hex-char iroh device identity key, created on first use.
    pub fn device_key(&self) -> Result<SecretKey, String> {
        if let Some(existing) = self.load(DEVICE_KEY_NAME) {
            return parse_secret_key(&existing);
        }
        let generated = SecretKey::generate();
        let hex = encode_hex(&generated.to_bytes());
        if !self.save(DEVICE_KEY_NAME, &hex) {
            return Err("could not persist device key".into());
        }
        Ok(generated)
    }

    fn store_file(&self) -> PathBuf {
        self.data_dir.join("store.json")
    }

    fn cache_file(&self, key: &str) -> Option<PathBuf> {
        let rest = key.strip_prefix("cache:")?;
        let sanitized: String = rest.chars().map(|c| if c.is_ascii_alphanumeric() || c == '-' { c } else { '_' }).collect();
        Some(self.cache_dir.join(format!("cache_{sanitized}.json")))
    }

    fn read_file(&self) -> Result<BTreeMap<String, String>, String> {
        let path = self.store_file();
        match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text).map_err(|e| e.to_string()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(err) => Err(err.to_string()),
        }
    }

    fn write_file(&self, values: &BTreeMap<String, String>) -> bool {
        let Ok(bytes) = serde_json::to_vec_pretty(values) else {
            return false;
        };
        write_atomic(&self.store_file(), &bytes, true)
    }
}

/// The client's settings folder: `DOMOVOI_DATA`, else its folder in the
/// platform config directory.
pub fn default_data_dir(identity: Identity) -> PathBuf {
    if let Some(dir) = std::env::var_os("DOMOVOI_DATA") {
        return PathBuf::from(dir);
    }
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join(identity.dir_name)
}

fn default_cache_dir(identity: Identity, data_dir: &Path) -> PathBuf {
    if std::env::var_os("DOMOVOI_DATA").is_some() {
        return data_dir.join("cache");
    }
    dirs::cache_dir().unwrap_or_else(|| data_dir.to_path_buf()).join(identity.dir_name)
}

fn write_atomic(path: &Path, bytes: &[u8], secret: bool) -> bool {
    let Some(dir) = path.parent() else {
        return false;
    };
    if std::fs::create_dir_all(dir).is_err() {
        return false;
    }
    // Windows: the file inherits the ACL of the user's profile folder (the
    // user, SYSTEM and Administrators), so it is set no tighter.
    #[cfg(not(unix))]
    let _ = secret;
    #[cfg(unix)]
    if secret {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    }
    let tmp = dir.join(format!(".tmp-{}-{}", std::process::id(), now_nanos()));
    if std::fs::write(&tmp, bytes).is_err() {
        let _ = std::fs::remove_file(&tmp);
        return false;
    }
    #[cfg(unix)]
    if secret {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    if std::fs::rename(&tmp, path).is_err() {
        // On Windows `rename` can fail if the target exists; fall back to replace.
        let _ = std::fs::remove_file(path);
        if std::fs::rename(&tmp, path).is_err() {
            let _ = std::fs::remove_file(&tmp);
            return false;
        }
    }
    true
}

fn now_nanos() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or(0)
}

pub fn parse_secret_key(s: &str) -> Result<SecretKey, String> {
    let s = s.trim();
    if s.len() != 64 || !s.is_ascii() {
        return Err("the device key must be 64 hex characters".into());
    }
    let mut bytes = [0u8; 32];
    for (i, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).map_err(|_| "the device key must be hex".to_owned())?;
    }
    Ok(SecretKey::from_bytes(&bytes))
}

fn encode_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        use std::fmt::Write;
        let _ = write!(&mut out, "{b:02x}");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_and_file_store_round_trip() {
        let mem = SecureStore::in_memory();
        assert_eq!(mem.load("k"), None);
        assert!(mem.save("k", "v"));
        assert_eq!(mem.load("k").as_deref(), Some("v"));
        assert!(mem.delete("k"));
        assert_eq!(mem.load("k"), None);

        let dir = tempfile::tempdir().unwrap();
        let file = SecureStore::in_dir(dir.path().to_path_buf());
        assert!(file.save("environments", "[1,2]"));
        assert!(file.save("cache:env1", "{\"rows\":[]}"));
        assert_eq!(file.load("environments").as_deref(), Some("[1,2]"));
        assert_eq!(file.load("cache:env1").as_deref(), Some("{\"rows\":[]}"));
        let key1 = file.device_key().unwrap();
        let key2 = file.device_key().unwrap();
        assert_eq!(key1.to_bytes(), key2.to_bytes());
        assert!(file.delete("environments"));
        assert_eq!(file.load("environments"), None);
    }
}
