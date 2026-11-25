//! `domovoi://` links, and one running instance per client.
//!
//! A link reaches the client on its command line (Linux, Windows), as an
//! Apple Event (macOS, which the client handles and passes to [`deliver`]),
//! or from a later launch. A later launch hands its link to the instance
//! already running and exits. The hand-off is a socket in the client's data
//! directory, or a named pipe on Windows.

use std::io::Read;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use tokio::sync::mpsc;

use crate::Identity;

/// Longer than any pairing link; more is not one.
const MAX_LINK: usize = 4096;
const READ_TIMEOUT: Duration = Duration::from_secs(2);
/// Links come one click at a time; more than this waiting are dropped.
const INBOX: usize = 16;

type Inbox = (mpsc::Sender<String>, Mutex<Option<mpsc::Receiver<String>>>);

/// Links that came while the app runs (or before its window was up).
fn inbox() -> &'static Inbox {
    static INBOX_CELL: OnceLock<Inbox> = OnceLock::new();
    INBOX_CELL.get_or_init(|| {
        let (tx, rx) = mpsc::channel(INBOX);
        (tx, Mutex::new(Some(rx)))
    })
}

fn is_link(s: &str) -> bool {
    s.len() <= MAX_LINK && s.starts_with("domovoi://")
}

/// Queue a link for [`take_receiver`]'s reader. Anything else is ignored.
pub fn deliver(link: &str) {
    let link = link.trim();
    if is_link(link) {
        let _ = inbox().0.try_send(link.to_owned());
    }
}

/// The receiving end of the links, once: the client's UI reads it.
pub fn take_receiver() -> Option<mpsc::Receiver<String>> {
    inbox().1.lock().ok().and_then(|mut rx| rx.take())
}

/// The link this process was started with.
pub fn from_args() -> Option<String> {
    std::env::args().skip(1).find(|a| is_link(a))
}

/// Run before the event loop. If an instance is already running, this hands
/// it `link` and returns true: this launch has nothing left to do. Otherwise
/// it starts listening for later launches, if this is the first instance.
/// A second launch without a link runs on its own and leaves the listening
/// to the first.
pub fn start(identity: Identity, link: Option<&str>) -> bool {
    match platform::connect(identity) {
        Some(mut to) => link.is_some_and(|link| {
            use std::io::Write;
            to.write_all(format!("{link}\n").as_bytes()).and_then(|()| to.flush()).is_ok()
        }),
        None => {
            platform::listen(identity);
            false
        }
    }
}

/// The first line a later launch sent.
fn read_link(stream: impl Read) {
    let mut buf = Vec::new();
    if stream.take(MAX_LINK as u64 + 1).read_to_end(&mut buf).is_ok() || !buf.is_empty() {
        let text = String::from_utf8_lossy(&buf);
        deliver(text.lines().next().unwrap_or_default());
    }
}

#[cfg(unix)]
mod platform {
    use std::os::unix::net::{UnixListener, UnixStream};
    use std::path::PathBuf;

    use crate::Identity;

    fn path(identity: Identity) -> PathBuf {
        crate::default_data_dir(identity).join("desktop.sock")
    }

    pub fn connect(identity: Identity) -> Option<UnixStream> {
        let stream = UnixStream::connect(path(identity)).ok()?;
        let _ = stream.set_write_timeout(Some(super::READ_TIMEOUT));
        Some(stream)
    }

    pub fn listen(identity: Identity) {
        let path = path(identity);
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        // Nothing answered on it, so it is left from a crash.
        let _ = std::fs::remove_file(&path);
        let listener = match UnixListener::bind(&path) {
            Ok(l) => l,
            Err(e) => {
                tracing::warn!("links from later launches won't reach this one: {e}");
                return;
            }
        };
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
        }
        let spawned = std::thread::Builder::new().name("domovoi-links".into()).spawn(move || {
            for stream in listener.incoming().flatten() {
                let _ = stream.set_read_timeout(Some(super::READ_TIMEOUT));
                super::read_link(stream);
            }
        });
        if let Err(e) = spawned {
            tracing::warn!("links from later launches won't reach this one: {e}");
        }
    }
}

#[cfg(windows)]
mod platform {
    use std::fs::File;

    use crate::Identity;

    /// One pipe per client and data directory (and so per user and profile).
    fn name(identity: Identity) -> String {
        let dir = crate::default_data_dir(identity);
        // FNV-1a: the same in every build, unlike `DefaultHasher`.
        let hash = dir.to_string_lossy().bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3));
        format!(r"\\.\pipe\{}-{hash:016x}", identity.dir_name)
    }

    pub fn connect(identity: Identity) -> Option<File> {
        std::fs::OpenOptions::new().write(true).open(name(identity)).ok()
    }

    pub fn listen(identity: Identity) {
        let name = name(identity);
        let spawned = std::thread::Builder::new().name("domovoi-links".into()).spawn(move || {
            let Ok(rt) = tokio::runtime::Builder::new_current_thread().enable_all().build() else { return };
            rt.block_on(async move {
                use tokio::io::AsyncReadExt;
                use tokio::net::windows::named_pipe::ServerOptions;
                let mut first = true;
                loop {
                    let server = match ServerOptions::new().first_pipe_instance(first).reject_remote_clients(true).create(&name) {
                        Ok(s) => s,
                        Err(e) => {
                            tracing::warn!("links from later launches won't reach this one: {e}");
                            return;
                        }
                    };
                    first = false;
                    if server.connect().await.is_err() {
                        continue;
                    }
                    let mut buf = Vec::new();
                    let mut server = server;
                    let _ = tokio::time::timeout(super::READ_TIMEOUT, (&mut server).take(super::MAX_LINK as u64 + 1).read_to_end(&mut buf)).await;
                    super::read_link(buf.as_slice());
                }
            });
        });
        if let Err(e) = spawned {
            tracing::warn!("links from later launches won't reach this one: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_later_launch_sends_one_line() {
        super::read_link(&b"domovoi://connect?ticket=abc&code=1\nextra"[..]);
        super::read_link(&b"https://example.com\n"[..]);
        let mut rx = super::take_receiver().expect("the inbox's receiver");
        assert_eq!(rx.try_recv().ok().as_deref(), Some("domovoi://connect?ticket=abc&code=1"));
        assert!(rx.try_recv().is_err(), "only domovoi:// links get through");
    }
}
