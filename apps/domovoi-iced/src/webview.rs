//! In-app preview window for interactive artifacts (HTML, multi-file
//! bundles, React), behind the `webview` cargo feature: the artifact's signed
//! preview URL in a window of its own with the system webview (WKWebView on
//! macOS, WebView2 on Windows, WebKitGTK on Linux) through `wry` and `tao`.
//!
//! The window runs in a helper process: `domovoi-desktop --artifact-window
//! <url> <title>`, started by [`open`]. It is not embedded in the Iced window
//! because iced 0.14 owns the only event loop (winit allows one per process,
//! on the main thread, and `wry`'s child webview must be made and kept on
//! that thread, which iced exposes only through `window::run` closures whose
//! results must be `Send`, which a `WebView` is not). A native child view would
//! also sit above iced's wgpu surface, so the palette, menus and toasts would
//! fall behind it, and it would have to follow the panel's layout by hand. On
//! Linux, `wry` needs a GTK window (X11 only for child views) and a GTK main
//! loop pumped from winit's. A process of its own avoids all of that: the
//! app's window and event loop stay as they are.
//!
//! The helper reads its stdin until it closes, so it ends with the app. It
//! loads only `http(s)` URLs; a link to another origin opens in the system
//! browser. If the webview cannot start (no WebKitGTK at run time, say), the
//! helper opens the URL in the system browser instead; if the helper cannot
//! be started, the caller falls back to the browser.
//!
//! Limits: no console or errors from the page back into the app ("Ask about
//! this error"); the window does not follow the app's light/dark switch after
//! it opens (the URL carries the theme it was signed for); one window per
//! click, not reused; Linux builds with the feature need the WebKitGTK 4.1
//! and GTK 3 development packages; the feature was built (and its tests run)
//! on macOS only, not on Windows or Linux, and not tried by hand.

use std::io::Read;
use std::process::{Child, Stdio};
use std::sync::Mutex;

/// The helper's first argument.
const FLAG: &str = "--artifact-window";

/// Preview windows open now; their stdin closes with the app.
static WINDOWS: Mutex<Vec<Child>> = Mutex::new(Vec::new());

/// Whether a URL may be shown in the preview window.
fn allowed(url: &str) -> bool {
    url.starts_with("http://") || url.starts_with("https://")
}

/// `scheme://host:port` of a URL.
fn origin(url: &str) -> &str {
    let rest = url.find("://").map_or(0, |at| at + 3);
    url[rest..].find('/').map_or(url, |end| &url[..rest + end])
}

/// Opens `url` in a preview window of its own, titled `title`.
pub fn open(url: &str, title: &str) -> std::io::Result<()> {
    if !allowed(url) {
        return Err(std::io::Error::other("only http(s) previews open in the app"));
    }
    let exe = std::env::current_exe()?;
    #[expect(clippy::disallowed_methods, reason = "the preview window is this same binary, detached from the app's tasks; it ends when its stdin (held here) closes")]
    let child = std::process::Command::new(exe).arg(FLAG).arg(url).arg(title).stdin(Stdio::piped()).stdout(Stdio::null()).spawn()?;
    let mut windows = WINDOWS.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    windows.retain_mut(|w| matches!(w.try_wait(), Ok(None)));
    windows.push(child);
    Ok(())
}

/// The URL and title when this process was started as a preview window.
pub fn window_args() -> Option<(String, String)> {
    let mut args = std::env::args().skip(1);
    (args.next()? == FLAG).then_some(())?;
    let url = args.next()?;
    let title = args.next().unwrap_or_else(|| "Artifact preview".into());
    Some((url, title))
}

/// Runs the preview window until it is closed or the app goes; never returns.
pub fn run(url: String, title: String) -> ! {
    use tao::event::{Event, WindowEvent};
    use tao::event_loop::{ControlFlow, EventLoopBuilder};
    use tao::window::WindowBuilder;

    if !allowed(&url) {
        std::process::exit(2);
    }
    let event_loop = EventLoopBuilder::<()>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    std::thread::spawn(move || {
        let mut stdin = std::io::stdin();
        let mut buf = [0u8; 64];
        while matches!(stdin.read(&mut buf), Ok(n) if n > 0) {}
        let _ = proxy.send_event(());
    });
    let webview = WindowBuilder::new()
        .with_title(&title)
        .with_inner_size(tao::dpi::LogicalSize::new(1100.0, 800.0))
        .build(&event_loop)
        .map_err(|e| e.to_string())
        .and_then(|window| {
            let home = origin(&url).to_owned();
            let builder = wry::WebViewBuilder::new().with_url(&url).with_navigation_handler(move |to: String| {
                if origin(&to) == home || to.starts_with("about:") || to.starts_with("blob:") || to.starts_with("data:") {
                    return true;
                }
                if allowed(&to) {
                    let _ = open::that(&to);
                }
                false
            });
            #[cfg(target_os = "linux")]
            let built = {
                use tao::platform::unix::WindowExtUnix;
                use wry::WebViewBuilderExtUnix;
                match window.default_vbox() {
                    Some(vbox) => builder.build_gtk(vbox),
                    None => return Err("no GTK container".to_owned()),
                }
            };
            #[cfg(not(target_os = "linux"))]
            let built = builder.build(&window);
            built.map(|webview| (window, webview)).map_err(|e| e.to_string())
        });
    let held = match webview {
        Ok(held) => held,
        Err(why) => {
            eprintln!("domovoi-desktop: no webview ({why}); opening the preview in the browser");
            let _ = open::that(&url);
            std::process::exit(1);
        }
    };
    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        let _ = &held;
        match event {
            Event::WindowEvent { event: WindowEvent::CloseRequested, .. } | Event::UserEvent(()) => *control_flow = ControlFlow::Exit,
            _ => {}
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn previews_stay_on_their_origin_and_on_http() {
        assert_eq!(origin("http://127.0.0.1:4321/api/assets/x?sig=1"), "http://127.0.0.1:4321");
        assert_eq!(origin("https://example.com"), "https://example.com");
        assert!(allowed("https://example.com/a") && !allowed("file:///etc/passwd") && !allowed("javascript:alert(1)"));
        assert!(open("file:///etc/passwd", "x").is_err());
    }
}
