//! Domovoi Desktop for Linux, Windows and macOS (Iced 0.14).

mod app;
mod host;
mod image_actions;
mod links;
mod scroll;
mod store;
mod ui;
mod views;
#[cfg(feature = "webview")]
mod webview;

#[cfg(target_os = "macos")]
const MACOS_DOCK_ICON_PNG: &[u8] = include_bytes!("../assets/app-icon-macos.png");
pub(crate) const WINDOW_ICON_PNG: &[u8] = include_bytes!("../assets/app-icon-256.png");

/// PNG files start with these bytes. A checkout without `git lfs pull` embeds
/// a pointer file instead, and the app goes on without that icon.
#[cfg(any(target_os = "macos", test))]
const PNG_MAGIC: &[u8] = b"\x89PNG\r\n\x1a\n";

pub(crate) fn window_icon() -> Option<iced::window::Icon> {
    match iced::window::icon::from_file_data(WINDOW_ICON_PNG, None) {
        Ok(icon) => Some(icon),
        Err(e) => {
            tracing::warn!("window icon unavailable ({e}): run `git lfs pull`");
            None
        }
    }
}

/// Winit ignores `window::Settings::icon` on macOS because Dock and app-switcher
/// icons belong to `NSApplication`, not individual windows.
#[cfg(target_os = "macos")]
fn set_macos_app_icon() {
    use objc2::{AnyThread, MainThreadMarker};
    use objc2_app_kit::{NSApplication, NSImage};
    use objc2_foundation::NSData;

    let Some(mtm) = MainThreadMarker::new() else {
        return;
    };
    if !MACOS_DOCK_ICON_PNG.starts_with(PNG_MAGIC) {
        tracing::warn!("Dock icon unavailable: run `git lfs pull`");
        return;
    }
    let data = NSData::with_bytes(MACOS_DOCK_ICON_PNG);
    if let Some(image) = NSImage::initWithData(NSImage::alloc(), &data) {
        let app = NSApplication::sharedApplication(mtm);
        // SAFETY: `image` is a valid, non-nil `NSImage`.
        unsafe { app.setApplicationIconImage(Some(&image)) };
    }
}

fn main() -> iced::Result {
    // Started as an artifact's preview window (`webview::open`): that window only.
    #[cfg(feature = "webview")]
    if let Some((url, title)) = webview::window_args() {
        webview::run(url, title);
    }
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn,domovoi_iced=info".into()))
        .init();
    // A link opened while the app runs goes to it, and this launch ends.
    if links::start(links::from_args().as_deref()) {
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    set_macos_app_icon();
    #[cfg(target_os = "linux")]
    let platform_specific = iced::window::settings::PlatformSpecific { application_id: "domovoi".into(), ..Default::default() };
    #[cfg(not(target_os = "linux"))]
    let platform_specific = Default::default();
    iced::application(app::App::new, app::App::update, views::view)
        .settings(ui::fonts::settings())
        .title(app::App::title)
        .theme(app::App::theme)
        .subscription(app::App::subscription)
        .window(iced::window::Settings {
            min_size: Some(iced::Size::new(840.0, 560.0)),
            size: iced::Size::new(1440.0, 900.0),
            icon: window_icon(),
            platform_specific,
            ..Default::default()
        })
        .exit_on_close_request(false)
        .run()
}

#[cfg(test)]
mod ui_tests;

#[cfg(test)]
mod tests {
    /// The icons are kept in Git LFS: a checkout without their contents
    /// builds, but this says why its icons are missing.
    #[test]
    fn embedded_icons_are_images_not_lfs_pointers() {
        assert!(super::WINDOW_ICON_PNG.starts_with(super::PNG_MAGIC), "assets/app-icon-256.png should be a PNG; run `git lfs pull`");
        super::window_icon().expect("assets/app-icon-256.png decodes");
        #[cfg(target_os = "macos")]
        assert!(super::MACOS_DOCK_ICON_PNG.starts_with(super::PNG_MAGIC), "assets/app-icon-macos.png should be a PNG; run `git lfs pull`");
    }
}
