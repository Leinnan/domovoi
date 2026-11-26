use sha2::{Digest, Sha256};

pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).expect("system randomness");
    b
}

pub fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

pub fn new_uuid() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// 12-character pairing code from an unambiguous alphabet, like t3code's
/// one-time tokens (rejection sampling keeps the distribution uniform).
pub fn pairing_code() -> String {
    const ALPHABET: &[u8] = b"23456789ABCDEFGHJKLMNPQRSTUVWXYZ";
    const LEN: usize = 12;
    let limit = 256 - (256 % ALPHABET.len());
    let mut out = String::with_capacity(LEN);
    while out.len() < LEN {
        for b in random_bytes::<32>() {
            if (b as usize) < limit && out.len() < LEN {
                out.push(ALPHABET[b as usize % ALPHABET.len()] as char);
            }
        }
    }
    out
}

/// Replaces the user's home directory placeholder used in bundled defaults.
pub fn expand_home(s: &str) -> String {
    match dirs::home_dir() {
        Some(h) => s.replace("/HOME", &h.to_string_lossy()),
        None => s.to_owned(),
    }
}

/// Makes `path` this user's alone: a folder mode 0700 and a file 0600 on
/// Unix; on Windows an ACL of its own granting only this user (a folder's
/// new contents inherit it).
pub fn make_private(path: &std::path::Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = if path.is_dir() { 0o700 } else { 0o600 };
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
    }
    #[cfg(windows)]
    {
        crate::win::restrict_to_current_user(path)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        Ok(())
    }
}

/// `path` without the `\\?\` Windows puts before a canonical path
/// (`\\?\UNC\host\share` becomes `\\host\share`): `cmd.exe`, batch files and
/// some CLIs refuse it, and it takes no `/`. Other paths are as given.
pub fn plain_path(path: &std::path::Path) -> std::path::PathBuf {
    #[cfg(windows)]
    if let Some(rest) = path.to_str().and_then(|s| s.strip_prefix(r"\\?\")) {
        return match rest.strip_prefix(r"UNC\") {
            Some(share) => format!(r"\\{share}").into(),
            None => rest.into(),
        };
    }
    path.to_path_buf()
}

/// Opens `path` for writing, readable by this user only from its creation
/// (mode `0600`, or on Windows an owner-only ACL). `new_only` refuses an
/// existing file; otherwise one is truncated.
pub fn create_private(path: &std::path::Path, new_only: bool) -> std::io::Result<std::fs::File> {
    #[cfg(windows)]
    {
        crate::win::create_private(path, new_only)
    }
    #[cfg(not(windows))]
    {
        let mut options = std::fs::OpenOptions::new();
        options.write(true);
        if new_only {
            options.create_new(true);
        } else {
            options.create(true).truncate(true);
        }
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        options.open(path)
    }
}

/// Writes `value` to `path` through a temp file beside it, both readable by
/// this user only from the start, in `dir`, made private too.
pub fn write_secret(dir: &std::path::Path, path: &std::path::Path, value: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    make_private(dir)?;
    let tmp = path.with_extension("tmp");
    create_private(&tmp, false)?.write_all(value)?;
    std::fs::rename(&tmp, path)
}

/// Whether only this user can read `path` (what [`make_private`] leaves).
/// `None` when that can't be told.
pub fn is_private(path: &std::path::Path) -> Option<bool> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(path).ok().map(|m| m.permissions().mode() & 0o077 == 0)
    }
    #[cfg(windows)]
    {
        crate::win::only_current_user_can_access(path)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = path;
        None
    }
}

/// Whether `path` is a symlink (or on Windows any reparse point: a junction
/// too), which the no-follow opens refuse.
pub fn is_link(path: &std::path::Path) -> bool {
    #[cfg(windows)]
    {
        crate::win::is_reparse_point(path)
    }
    #[cfg(not(windows))]
    {
        std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink())
    }
}

pub fn home_dir() -> std::path::PathBuf {
    dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("/"))
}

/// JavaScript's `encodeURIComponent`.
pub fn encode_uri_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// Whole seconds from now until an ISO timestamp (0 if past or unparsable).
pub fn seconds_until(iso: &str) -> i64 {
    use time::format_description::well_known::Rfc3339;
    let Ok(at) = time::OffsetDateTime::parse(iso, &Rfc3339) else { return 0 };
    let ms = (at.unix_timestamp_nanos() / 1_000_000) as i64 - domovoi_contracts::time_fmt::now_millis();
    (ms / 1000).max(0)
}

/// Locks a mutex, carrying on after another thread panicked while holding
/// it: the data here is plain state, and one panic must not turn every
/// later request into another.
pub fn locked<T>(mutex: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}
