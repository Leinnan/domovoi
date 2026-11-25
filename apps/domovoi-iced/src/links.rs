//! `domovoi://` links from the OS: on the command line (Linux, Windows), as
//! an Apple Event (macOS), or from a later launch, which hands its link to
//! the instance already running over a socket in the data directory (a named
//! pipe on Windows) and exits.

use std::io::Read;
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use iced::Subscription;
use tokio::sync::mpsc;

/// Longer than any pairing link; more is not one.
const MAX_LINK: usize = 4096;
const READ_TIMEOUT: Duration = Duration::from_secs(2);

type Inbox = (mpsc::Sender<String>, Mutex<Option<mpsc::Receiver<String>>>);

/// Links that came while the app runs (or before its window was up).
fn inbox() -> &'static Inbox {
    static INBOX: OnceLock<Inbox> = OnceLock::new();
    INBOX.get_or_init(|| {
        // Links come one click at a time; more than this waiting are dropped.
        let (tx, rx) = mpsc::channel(16);
        (tx, Mutex::new(Some(rx)))
    })
}

fn is_link(s: &str) -> bool {
    s.len() <= MAX_LINK && s.starts_with("domovoi://")
}

fn deliver(link: &str) {
    let link = link.trim();
    if is_link(link) {
        let _ = inbox().0.try_send(link.to_owned());
    }
}

/// The link this process was started with.
pub fn from_args() -> Option<String> {
    std::env::args().skip(1).find(|a| is_link(a))
}

/// Run before the event loop: hands `link` to an instance already running
/// (true: this one has nothing left to do), else starts listening for later
/// launches when this is the first. A second launch without a link runs on
/// its own and leaves the listening to the first.
pub fn start(link: Option<&str>) -> bool {
    #[cfg(target_os = "macos")]
    mac::install();
    #[cfg(windows)]
    windows::register_scheme();
    match platform::connect() {
        Some(mut to) => link.is_some_and(|link| {
            use std::io::Write;
            to.write_all(format!("{link}\n").as_bytes()).and_then(|()| to.flush()).is_ok()
        }),
        None => {
            platform::listen();
            false
        }
    }
}

/// Links as they arrive.
pub fn subscription() -> Subscription<String> {
    Subscription::run(incoming)
}

fn incoming() -> impl iced::futures::Stream<Item = String> {
    iced::stream::channel(16, async |mut out| {
        use iced::futures::SinkExt;
        let taken = inbox().1.lock().ok().and_then(|mut rx| rx.take());
        let Some(mut rx) = taken else {
            return;
        };
        while let Some(link) = rx.recv().await {
            if out.send(link).await.is_err() {
                break;
            }
        }
    })
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

    fn path() -> PathBuf {
        crate::store::default_data_dir().join("desktop.sock")
    }

    pub fn connect() -> Option<UnixStream> {
        let stream = UnixStream::connect(path()).ok()?;
        let _ = stream.set_write_timeout(Some(super::READ_TIMEOUT));
        Some(stream)
    }

    pub fn listen() {
        let path = path();
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

    /// One pipe per data directory (and so per user and profile).
    fn name() -> String {
        let dir = crate::store::default_data_dir();
        // FNV-1a: the same in every build, unlike `DefaultHasher`.
        let hash = dir.to_string_lossy().bytes().fold(0xcbf2_9ce4_8422_2325_u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3));
        format!(r"\\.\pipe\domovoi-desktop-{hash:016x}")
    }

    pub fn connect() -> Option<File> {
        std::fs::OpenOptions::new().write(true).open(name()).ok()
    }

    pub fn listen() {
        let name = name();
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

/// Windows has no packaging step that could register the scheme, so each
/// release build points `domovoi://` (for this user) at itself when it
/// doesn't already.
#[cfg(windows)]
mod windows {
    use std::process::Command;

    const KEY: &str = r"HKCU\Software\Classes\domovoi";

    pub fn register_scheme() {
        if cfg!(debug_assertions) {
            return;
        }
        let _ = std::thread::Builder::new().name("domovoi-scheme".into()).spawn(|| {
            let Ok(exe) = std::env::current_exe() else { return };
            let command = format!("\"{}\" \"%1\"", exe.display());
            let open = format!(r"{KEY}\shell\open\command");
            if reg(&["query", &open, "/ve"]).is_some_and(|out| out.contains(&command)) {
                return;
            }
            let icon = format!("\"{}\",0", exe.display());
            let ok = reg(&["add", KEY, "/ve", "/d", "URL:Domovoi", "/f"]).is_some()
                && reg(&["add", KEY, "/v", "URL Protocol", "/d", "", "/f"]).is_some()
                && reg(&["add", &format!(r"{KEY}\DefaultIcon"), "/ve", "/d", &icon, "/f"]).is_some()
                && reg(&["add", &open, "/ve", "/d", &command, "/f"]).is_some();
            if !ok {
                tracing::warn!("couldn't register domovoi:// links with Windows");
            }
        });
    }

    #[allow(clippy::disallowed_methods, reason = "reg.exe exits at once and is waited for; the server's process helpers aren't in this crate")]
    fn reg(args: &[&str]) -> Option<String> {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        let out = Command::new("reg").args(args).creation_flags(CREATE_NO_WINDOW).output().ok()?;
        out.status.success().then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

/// Launch Services delivers a link as a `GetURL` Apple Event, to the running
/// app or to the one it starts; AppKit leaves that event to the app (winit
/// sets no application delegate that would claim it).
#[cfg(target_os = "macos")]
mod mac {
    use objc2::rc::Retained;
    use objc2::runtime::{AnyObject, NSObject};
    use objc2::{ClassType, class, define_class, msg_send, sel};
    use objc2_foundation::NSString;

    /// `'GURL'`, the event class and id of a URL to open.
    const GET_URL: u32 = u32::from_be_bytes(*b"GURL");
    /// `'----'`: the event's direct object (the URL).
    const DIRECT_OBJECT: u32 = u32::from_be_bytes(*b"----");

    define_class!(
        #[unsafe(super(NSObject))]
        #[name = "DomovoiLinkHandler"]
        struct LinkHandler;

        impl LinkHandler {
            #[unsafe(method(handleGetURLEvent:withReplyEvent:))]
            fn handle_get_url(&self, event: &AnyObject, _reply: &AnyObject) {
                // SAFETY: `event` is an `NSAppleEventDescriptor`; both messages are part of it.
                let direct: Option<Retained<AnyObject>> = unsafe { msg_send![event, paramDescriptorForKeyword: DIRECT_OBJECT] };
                let url: Option<Retained<NSString>> = direct.and_then(|d| unsafe { msg_send![&*d, stringValue] });
                if let Some(url) = url {
                    super::deliver(&url.to_string());
                }
            }
        }
    );

    pub fn install() {
        // SAFETY: plain `NSObject` subclass without ivars.
        let handler: Retained<LinkHandler> = unsafe { msg_send![LinkHandler::class(), new] };
        let manager: Retained<AnyObject> = unsafe { msg_send![class!(NSAppleEventManager), sharedAppleEventManager] };
        // SAFETY: the selector is the handler's, with the signature Apple Events expect.
        let () = unsafe {
            msg_send![&*manager, setEventHandler: &*handler, andSelector: sel!(handleGetURLEvent:withReplyEvent:), forEventClass: GET_URL, andEventID: GET_URL]
        };
        // The manager doesn't retain its handlers; this one lives as long as the app.
        std::mem::forget(handler);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_later_launch_sends_one_line() {
        super::read_link(&b"domovoi://connect?ticket=abc&code=1\nextra"[..]);
        super::read_link(&b"https://example.com\n"[..]);
        let rx = super::inbox().1.lock().unwrap().take();
        let mut rx = rx.expect("the inbox's receiver");
        assert_eq!(rx.try_recv().ok().as_deref(), Some("domovoi://connect?ticket=abc&code=1"));
        assert!(rx.try_recv().is_err(), "only domovoi:// links get through");
    }
}
