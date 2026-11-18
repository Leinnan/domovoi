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
