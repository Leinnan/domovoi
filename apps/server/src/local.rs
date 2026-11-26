//! The server's side of signing in from the same machine without a pairing
//! code (`domovoi_local` has the protocol and the client's side).
//!
//! A server bound to a loopback address writes `<home>/local/server.json`
//! once it listens, and removes it when it stops. A client proves it runs as
//! this user by writing `<home>/local/requests/<id>`; [`Local::redeem`] reads
//! that file once and hands back the secret in it, which the route answers
//! with beside the new session (`http.rs`, `POST /api/domovoi/local/session`).
//!
//! Not published, so nothing to find: a server listening beyond loopback, one
//! started with `--no-local-sign-in`, and one whose managed policy leaves out
//! the desktop client.
//!
//! The trust is the home directory's: whoever can write it can already run
//! `domovoi-server pair`. Signing a same-machine client out therefore lasts
//! only until it signs in again.

use crate::util::locked;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

/// How long a request file is good for after it is written.
const REQUEST_LIFE: Duration = Duration::from_secs(60);
/// The longest secret read from a request file (a client writes 64 characters).
const MAX_SECRET_BYTES: u64 = 128;

pub struct Local {
    home: PathBuf,
    /// The descriptor this server wrote, while it is published.
    published: Mutex<Option<String>>,
}

impl Local {
    pub fn new(home: &Path) -> Self {
        Local { home: home.to_owned(), published: Mutex::new(None) }
    }

    fn requests(&self) -> PathBuf {
        domovoi_local::requests_dir(&self.home)
    }

    pub fn is_published(&self) -> bool {
        locked(&self.published).is_some()
    }

    /// Writes the descriptor for a server listening on `address`. Call it
    /// once the listener is bound: a second server on the same port never
    /// gets here, so it never replaces the first one's.
    ///
    /// `Ok(false)` when `address` is not loopback: a server reachable from
    /// other machines is not one to find by a file.
    pub fn publish(&self, address: std::net::SocketAddr, environment_id: &str, label: &str) -> std::io::Result<bool> {
        if !address.ip().is_loopback() {
            return Ok(false);
        }
        let dir = domovoi_local::local_dir(&self.home);
        let requests = self.requests();
        for folder in [&dir, &requests] {
            std::fs::create_dir_all(folder)?;
            // Not through a link: the folder is where clients write secrets.
            if crate::util::is_link(folder) {
                return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("{} is a link", folder.display())));
            }
            crate::util::make_private(folder)?;
        }
        clear(&requests);
        let descriptor = domovoi_local::Descriptor::new(address, environment_id, label).to_json();
        crate::util::write_secret(&dir, &domovoi_local::descriptor_path(&self.home), descriptor.as_bytes())?;
        *locked(&self.published) = Some(descriptor);
        Ok(true)
    }

    /// Removes the descriptor and any unread requests, at shutdown. A
    /// descriptor another server has written over this one's since is left.
    pub fn retract(&self) {
        let Some(mine) = locked(&self.published).take() else { return };
        let path = domovoi_local::descriptor_path(&self.home);
        if std::fs::read_to_string(&path).is_ok_and(|found| found == mine) {
            let _ = std::fs::remove_file(&path);
            clear(&self.requests());
        }
    }

    /// The secret in the request file `id`, which is removed: `None` unless
    /// the descriptor is published and the file is a plain one this user
    /// wrote within the last minute, read and removed here and now. A file
    /// that cannot be removed is refused, so each works once.
    pub fn redeem(&self, id: &str) -> Option<String> {
        if !self.is_published() || !domovoi_local::valid_id(id) {
            return None;
        }
        let path = self.requests().join(id);
        let secret = read_request(&path);
        // Removed whatever it held: a refused request does not get a second try.
        let removed = std::fs::remove_file(&path).is_ok();
        secret.filter(|_| removed)
    }
}

/// Removes every plain file in `dir`.
fn clear(dir: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        if entry.file_type().is_ok_and(|kind| !kind.is_dir()) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// A request file's secret, when the file is what a client's request is.
fn read_request(path: &Path) -> Option<String> {
    use std::io::Read;
    // Checked on the path (no link is followed to another user's file), then
    // again on the open file, which is what is read.
    if crate::util::is_link(path) || !std::fs::symlink_metadata(path).ok()?.is_file() {
        return None;
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::custom_flags(&mut options, libc::O_NOFOLLOW | libc::O_NONBLOCK);
    let file = options.open(path).ok()?;
    let meta = file.metadata().ok()?;
    if !meta.is_file() || meta.len() == 0 || meta.len() > MAX_SECRET_BYTES {
        return None;
    }
    #[cfg(unix)]
    if !crate::util::owned_by_this_user(&meta) {
        return None;
    }
    #[cfg(windows)]
    if !crate::win::owned_by_current_user(path) {
        return None;
    }
    // Written within the last minute (or, with a clock that stepped back, not long after now).
    let written = meta.modified().ok()?;
    let age = match std::time::SystemTime::now().duration_since(written) {
        Ok(age) => age,
        Err(ahead) => ahead.duration(),
    };
    if age >= REQUEST_LIFE {
        return None;
    }
    let mut secret = String::new();
    file.take(MAX_SECRET_BYTES).read_to_string(&mut secret).ok()?;
    let secret = secret.trim();
    (!secret.is_empty() && secret.bytes().all(|b| b.is_ascii_alphanumeric())).then(|| secret.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home(tag: &str) -> PathBuf {
        let home = std::env::temp_dir().join(format!("domovoi-local-server-{tag}-{}", crate::util::new_uuid()));
        std::fs::create_dir_all(&home).unwrap();
        home
    }

    fn loopback() -> std::net::SocketAddr {
        "127.0.0.1:13773".parse().unwrap()
    }

    fn published(tag: &str) -> (Local, PathBuf) {
        let home = home(tag);
        let local = Local::new(&home);
        assert!(local.publish(loopback(), "env-1", "Studio").unwrap());
        (local, home)
    }

    #[test]
    fn the_descriptor_is_published_on_loopback_only_and_taken_back() {
        let home = home("publish");
        let local = Local::new(&home);
        let path = domovoi_local::descriptor_path(&home);
        assert!(!local.publish("192.168.1.5:13773".parse().unwrap(), "env-1", "Studio").unwrap());
        assert!(!local.publish("0.0.0.0:13773".parse().unwrap(), "env-1", "Studio").unwrap());
        assert!(!path.exists() && !local.is_published());

        // A request left by a client of an earlier run is cleared.
        std::fs::create_dir_all(domovoi_local::requests_dir(&home)).unwrap();
        let stale = domovoi_local::requests_dir(&home).join("0".repeat(32));
        std::fs::write(&stale, "secret").unwrap();
        assert!(local.publish(loopback(), "env-1", "Studio").unwrap());
        assert!(!stale.exists());
        let text = domovoi_local::read_descriptor(&path).expect("a descriptor a client accepts");
        let descriptor = domovoi_local::Descriptor::parse(&text).unwrap();
        assert_eq!((descriptor.url.as_str(), descriptor.environment_id.as_str(), descriptor.label.as_str()), ("http://127.0.0.1:13773", "env-1", "Studio"));
        for private in [domovoi_local::local_dir(&home), domovoi_local::requests_dir(&home), path.clone()] {
            assert_eq!(crate::util::is_private(&private), Some(true), "{}", private.display());
        }

        // Another server's descriptor, written since, is not this one's to remove.
        std::fs::write(&path, "{}").unwrap();
        local.retract();
        assert!(path.exists());
        assert!(local.publish(loopback(), "env-1", "Studio").unwrap());
        local.retract();
        assert!(!path.exists() && !local.is_published());
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn a_request_is_redeemed_once() {
        let (local, home) = published("redeem");
        let request = domovoi_local::create_request(&domovoi_local::requests_dir(&home)).unwrap();
        assert_eq!(local.redeem(&request.id).as_deref(), Some(request.secret.as_str()));
        assert_eq!(local.redeem(&request.id), None, "used");
        assert!(!domovoi_local::requests_dir(&home).join(&request.id).exists());
        assert_eq!(local.redeem("../server.json"), None);
        assert_eq!(local.redeem(&"f".repeat(32)), None, "no such request");

        // Not while nothing is published.
        let request = domovoi_local::create_request(&domovoi_local::requests_dir(&home)).unwrap();
        local.retract();
        assert_eq!(local.redeem(&request.id), None);
        let _ = std::fs::remove_dir_all(&home);
    }

    #[cfg(unix)]
    #[test]
    fn links_stale_and_odd_requests_are_refused_and_removed() {
        let (local, home) = published("refuse");
        let requests = domovoi_local::requests_dir(&home);
        let id = |c: &str| c.repeat(32);

        // A link to a file with a fine secret in it.
        let target = home.join("target");
        std::fs::write(&target, "a".repeat(64)).unwrap();
        std::os::unix::fs::symlink(&target, requests.join(id("a"))).unwrap();
        assert_eq!(local.redeem(&id("a")), None);
        assert!(target.exists(), "what a link points at is left alone");
        assert!(!requests.join(id("a")).exists(), "the link itself is removed");

        // Written more than a minute ago.
        let old = requests.join(id("b"));
        std::fs::write(&old, "b".repeat(64)).unwrap();
        let earlier = std::time::SystemTime::now() - Duration::from_secs(120);
        std::fs::File::options().write(true).open(&old).unwrap().set_modified(earlier).unwrap();
        assert_eq!(local.redeem(&id("b")), None);
        assert!(!old.exists());

        // Empty, too long, not a token, a folder.
        std::fs::write(requests.join(id("c")), "").unwrap();
        assert_eq!(local.redeem(&id("c")), None);
        std::fs::write(requests.join(id("d")), "d".repeat(500)).unwrap();
        assert_eq!(local.redeem(&id("d")), None);
        std::fs::write(requests.join(id("e")), "not a token\n../x").unwrap();
        assert_eq!(local.redeem(&id("e")), None);
        std::fs::create_dir(requests.join(id("f"))).unwrap();
        assert_eq!(local.redeem(&id("f")), None);
        let _ = std::fs::remove_dir_all(&home);
    }
}
