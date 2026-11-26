//! Signing in to the server on the same machine, without a pairing code.
//!
//! A server listening on loopback writes a **descriptor** to
//! `<home>/local/server.json`: where it listens and which server it is. It
//! holds no secret. A client that finds it proves it runs as the server's
//! user by **writing a file**: it makes `<home>/local/requests/<id>` with a
//! random secret in it and sends the server the id. The server reads the
//! file, removes it, and answers with a session and the secret it read.
//!
//! What that proves, each way:
//!
//! - To the server: the client can write the server's home, which only the
//!   server's user can. That is what `domovoi-server pair` needs too, so no
//!   one gains anything they could not already take.
//! - To the client: whoever answered could read the home. Another program
//!   that took the server's port cannot echo the secret, and the client
//!   drops its answer.
//!
//! Nothing reusable is on disk: a request works once and for a minute.
//!
//! This crate is the part both sides share (and the client side): the
//! server's side is `apps/server/src/local.rs`. It depends on neither.
//!
//! Limits: a server with a home other than `~/.domovoi`, or listening beyond
//! loopback, is not found (`DOMOVOI_LOCAL_SERVER` names another descriptor).
//! On Windows the owner of the files is not checked here; the folder has an
//! ACL for the server's user only.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// The descriptor format this build writes and reads.
pub const VERSION: u32 = 1;
/// The longest descriptor a client reads.
pub const MAX_DESCRIPTOR_BYTES: u64 = 4096;
/// Names the descriptor to use (a path), or `off` for none.
pub const ENV: &str = "DOMOVOI_LOCAL_SERVER";

/// What a server says of itself to clients on its machine.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Descriptor {
    pub version: u32,
    /// `http://127.0.0.1:13773`: the loopback address the server is bound to.
    pub url: String,
    /// The server's id, as `/.well-known/t3/environment` gives it.
    pub environment_id: String,
    /// The machine's name, for the client's "connect" card.
    pub label: String,
}

impl Descriptor {
    pub fn new(address: std::net::SocketAddr, environment_id: &str, label: &str) -> Self {
        Descriptor { version: VERSION, url: format!("http://{address}"), environment_id: environment_id.to_owned(), label: label.to_owned() }
    }

    /// A descriptor of this version whose URL is a loopback address and
    /// nothing else; anything else is `None`.
    pub fn parse(text: &str) -> Option<Descriptor> {
        let descriptor: Descriptor = serde_json::from_str(text).ok()?;
        (descriptor.version == VERSION && descriptor.address().is_some() && !descriptor.environment_id.trim().is_empty()).then_some(descriptor)
    }

    /// The loopback address in `url` (`http://<ip>:<port>`, an IP literal:
    /// a name would be the resolver's to answer).
    pub fn address(&self) -> Option<std::net::SocketAddr> {
        let address: std::net::SocketAddr = self.url.strip_prefix("http://")?.parse().ok()?;
        (address.ip().is_loopback() && address.port() != 0).then_some(address)
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).expect("a descriptor is plain data")
    }
}

/// `<home>/local`: the descriptor and the requests.
pub fn local_dir(home: &Path) -> PathBuf {
    home.join("local")
}

/// `<home>/local/server.json`.
pub fn descriptor_path(home: &Path) -> PathBuf {
    local_dir(home).join("server.json")
}

/// `<home>/local/requests`.
pub fn requests_dir(home: &Path) -> PathBuf {
    local_dir(home).join("requests")
}

/// The requests folder that goes with a descriptor: `requests` beside it.
pub fn requests_beside(descriptor: &Path) -> Option<PathBuf> {
    descriptor.parent().map(|dir| dir.join("requests"))
}

/// The server's default home, `~/.domovoi`.
pub fn default_home() -> Option<PathBuf> {
    let var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(var).filter(|v| !v.is_empty()).map(|home| PathBuf::from(home).join(".domovoi"))
}

/// Where a client looks for the descriptor: the default home's, or what
/// [`ENV`] names; `None` when that is `off` or there is no home.
pub fn locate() -> Option<PathBuf> {
    locate_with(std::env::var_os(ENV).as_deref())
}

fn locate_with(named: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    match named {
        Some(value) if value.is_empty() => default_home().map(|home| descriptor_path(&home)),
        Some(value) if value.eq_ignore_ascii_case("off") => None,
        Some(value) => Some(PathBuf::from(value)),
        None => default_home().map(|home| descriptor_path(&home)),
    }
}

/// The user this process runs as.
#[cfg(unix)]
fn me() -> u32 {
    // SAFETY: `geteuid` has no preconditions and cannot fail.
    unsafe { libc::geteuid() }
}

/// Whether `meta` is of something this user owns that group and others
/// cannot write (Unix; elsewhere the folder's ACL is trusted).
fn mine(meta: &std::fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        meta.uid() == me() && meta.permissions().mode() & 0o022 == 0
    }
    #[cfg(not(unix))]
    {
        let _ = meta;
        true
    }
}

/// The descriptor's text, for [`Descriptor::parse`]: `None` when there is
/// none, it is not a plain file, it is too long, or (Unix) another user owns
/// it or can write it.
pub fn read_descriptor(path: &Path) -> Option<String> {
    use std::io::Read;
    let meta = std::fs::symlink_metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_DESCRIPTOR_BYTES || !mine(&meta) {
        return None;
    }
    let mut text = String::new();
    std::fs::File::open(path).ok()?.take(MAX_DESCRIPTOR_BYTES).read_to_string(&mut text).ok()?;
    Some(text)
}

/// What a client sends (the id) and expects back (the secret).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    pub id: String,
    pub secret: String,
}

/// An id is 32 lowercase hex characters: nothing that could leave the folder.
pub fn valid_id(id: &str) -> bool {
    id.len() == 32 && id.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn random_hex<const N: usize>() -> std::io::Result<String> {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes).map_err(|e| std::io::Error::other(e.to_string()))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// The request this process made last, removed when it makes another: a
/// request the server never read (it was not running) does not pile up.
static LAST: std::sync::Mutex<Option<PathBuf>> = std::sync::Mutex::new(None);

/// Makes a request in `dir` (the server's `local/requests`): a file only
/// this user can read, named by a random id, holding a random secret.
///
/// Refused when `dir` is not a folder of this user's that nobody else can
/// write (Unix): the secret would be anyone's to read back.
pub fn create_request(dir: &Path) -> std::io::Result<Request> {
    use std::io::Write;
    let meta = std::fs::symlink_metadata(dir)?;
    if !meta.is_dir() || !mine(&meta) {
        return Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "the requests folder is not this user's alone"));
    }
    let mut last = LAST.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Some(old) = last.take() {
        let _ = std::fs::remove_file(old);
    }
    let request = Request { id: random_hex::<16>()?, secret: random_hex::<32>()? };
    let path = dir.join(&request.id);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(&path)?.write_all(request.secret.as_bytes())?;
    *last = Some(path);
    Ok(request)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("domovoi-local-{tag}-{}-{}", std::process::id(), random_hex::<4>().unwrap()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[cfg(unix)]
    fn chmod(path: &Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    #[test]
    fn a_descriptor_names_a_loopback_address_and_nothing_else() {
        let made = Descriptor::new("127.0.0.1:13773".parse().unwrap(), "env-1", "Studio");
        assert_eq!(made.url, "http://127.0.0.1:13773");
        assert_eq!(Descriptor::parse(&made.to_json()), Some(made.clone()));
        assert_eq!(Descriptor::new("[::1]:9".parse().unwrap(), "e", "x").address().map(|a| a.port()), Some(9));

        let with = |field: &str, value: serde_json::Value| {
            let mut json = serde_json::to_value(&made).unwrap();
            json[field] = value;
            Descriptor::parse(&json.to_string())
        };
        assert_eq!(with("version", 2.into()), None, "another version");
        assert_eq!(with("environmentId", "".into()), None);
        for url in ["http://localhost:13773", "http://192.168.1.5:13773", "https://127.0.0.1:13773", "http://127.0.0.1:13773/x", "http://127.0.0.1", "http://127.0.0.1:0", "http://evil.example@127.0.0.1:1", "127.0.0.1:1"] {
            assert_eq!(with("url", url.into()), None, "{url}");
        }
        assert_eq!(Descriptor::parse("not json"), None);
        // A newer server's extra fields are no reason to refuse it.
        assert!(with("extra", true.into()).is_some());
    }

    #[test]
    fn ids_are_32_lowercase_hex_characters() {
        assert!(valid_id("0123456789abcdef0123456789abcdef"));
        for bad in ["", "0123456789abcdef0123456789abcde", "0123456789abcdef0123456789abcdef0", "0123456789ABCDEF0123456789abcdef", "../../../../etc/passwd/aaaaaaaaaa", "0123456789abcdef0123456789abcde/"] {
            assert!(!valid_id(bad), "{bad}");
        }
    }

    #[test]
    fn the_descriptor_is_found_where_the_environment_says() {
        let os = |s: &'static str| Some(std::ffi::OsStr::new(s));
        assert_eq!(locate_with(os("off")), None);
        assert_eq!(locate_with(os("OFF")), None);
        assert_eq!(locate_with(os("/tmp/x/server.json")), Some(PathBuf::from("/tmp/x/server.json")));
        assert_eq!(locate_with(None), default_home().map(|home| home.join("local").join("server.json")));
        assert_eq!(requests_beside(Path::new("/h/local/server.json")), Some(PathBuf::from("/h/local/requests")));
        assert_eq!(requests_dir(Path::new("/h")), PathBuf::from("/h/local/requests"));
    }

    #[test]
    fn a_request_is_a_private_file_holding_its_secret_and_replaces_the_last() {
        let dir = temp("request");
        #[cfg(unix)]
        chmod(&dir, 0o700);
        let first = create_request(&dir).unwrap();
        assert!(valid_id(&first.id));
        assert_eq!(first.secret.len(), 64);
        let path = dir.join(&first.id);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), first.secret);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        let second = create_request(&dir).unwrap();
        assert_ne!((&first.id, &first.secret), (&second.id, &second.secret));
        assert!(!path.exists(), "the unread request is gone");
        assert!(dir.join(&second.id).exists());
        assert!(create_request(&dir.join("missing")).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn what_others_can_write_is_not_trusted() {
        let dir = temp("trust");
        let file = dir.join("server.json");
        std::fs::write(&file, "{}").unwrap();
        chmod(&file, 0o600);
        assert_eq!(read_descriptor(&file).as_deref(), Some("{}"));
        chmod(&file, 0o644);
        assert!(read_descriptor(&file).is_some(), "readable by others is the server's business, not a risk to the client");
        chmod(&file, 0o666);
        assert_eq!(read_descriptor(&file), None, "writable by others");
        chmod(&file, 0o600);
        std::os::unix::fs::symlink(&file, dir.join("link.json")).unwrap();
        assert_eq!(read_descriptor(&dir.join("link.json")), None, "a link");
        assert_eq!(read_descriptor(&dir.join("none.json")), None);
        std::fs::write(&file, vec![b' '; MAX_DESCRIPTOR_BYTES as usize + 1]).unwrap();
        assert_eq!(read_descriptor(&file), None, "too long");

        let requests = dir.join("requests");
        std::fs::create_dir(&requests).unwrap();
        chmod(&requests, 0o777);
        assert!(create_request(&requests).is_err(), "a folder others can write");
        chmod(&requests, 0o700);
        assert!(create_request(&requests).is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
