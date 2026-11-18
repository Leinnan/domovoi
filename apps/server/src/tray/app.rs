//! The menu and what its items do. Lives on the main thread; anything slow
//! runs on the server's runtime and comes back as a [`TrayMessage`].

use super::icon::{self, Style};
use super::model::{IconKind, MenuModel, model};
use super::{Notify, TrayMessage};
use crate::control::{Control, ServerStatus};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tray_icon::menu::{CheckMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{Icon, TrayIcon, TrayIconBuilder};

/// How long a confirmation ("Pairing link copied") replaces the status line.
const FLASH: Duration = Duration::from_secs(4);
/// A QR code image holds a pairing code: it is deleted after this long.
const QR_LIFETIME: Duration = Duration::from_secs(600);

#[cfg(target_os = "macos")]
const ICON_SIZE: u32 = 44;
#[cfg(not(target_os = "macos"))]
const ICON_SIZE: u32 = 64;

struct Items {
    menu: Menu,
    status: MenuItem,
    error: MenuItem,
    error_shown: bool,
    remote: CheckMenuItem,
    copy_link: MenuItem,
    show_qr: MenuItem,
    devices: Submenu,
    /// The rows currently in `devices`.
    device_rows: Vec<Submenu>,
    no_devices: MenuItem,
    login: CheckMenuItem,
    endpoint: MenuItem,
    copy_endpoint: MenuItem,
    open_logs: MenuItem,
    open_data: MenuItem,
}

pub struct TrayApp {
    handle: tokio::runtime::Handle,
    notify: Notify,
    control: Option<Control>,
    status: Option<ServerStatus>,
    /// A change the user asked for is under way.
    busy: bool,
    /// "Launch at Login" is on.
    login: bool,
    flash: Option<(String, Instant)>,
    tray: Option<TrayIcon>,
    items: Option<Items>,
    shown: Option<MenuModel>,
    shown_icon: Option<(IconKind, Style)>,
    icons: HashMap<(IconKind, Style), Icon>,
    /// Kept alive: on X11 the clipboard's content goes with its owner.
    clipboard: Option<arboard::Clipboard>,
    qr_files: Vec<(PathBuf, Instant)>,
    exit: bool,
    /// Test seam (`DOMOVOI_TRAY_TEST_DIR`): the menu's state is written to
    /// `state.json` there, and ids listed in a `click` file are clicked.
    test_dir: Option<PathBuf>,
    #[cfg(windows)]
    _menu_hook: Option<super::windows::MenuZOrderHook>,
}

impl TrayApp {
    pub fn new(handle: tokio::runtime::Handle, notify: Notify) -> Self {
        TrayApp {
            handle,
            notify,
            control: None,
            status: None,
            busy: false,
            login: false,
            flash: None,
            tray: None,
            items: None,
            shown: None,
            shown_icon: None,
            icons: HashMap::new(),
            clipboard: None,
            qr_files: Vec::new(),
            exit: false,
            test_dir: std::env::var_os("DOMOVOI_TRAY_TEST_DIR").map(PathBuf::from),
            #[cfg(windows)]
            _menu_hook: None,
        }
    }

    /// The loop should end: the server stopped, or the icon could not be made.
    pub fn should_exit(&self) -> bool {
        self.exit
    }

    /// Creates the menu and the icon. Called once the event loop runs (and,
    /// on Linux, on the GTK thread).
    pub fn build(&mut self) {
        match self.try_build() {
            Ok(()) => self.refresh(),
            Err(e) => {
                tracing::warn!("No status bar icon: {e}; the server keeps running (stop it with Ctrl-C or `domovoi-server stop`)");
            }
        }
    }

    fn try_build(&mut self) -> Result<(), String> {
        let menu = Menu::new();
        let item = |id: &str, text: &str, enabled: bool| MenuItem::with_id(id, text, enabled, None);
        let items = Items {
            status: item("status", "Domovoi Server is starting…", false),
            error: item("error", "", false),
            error_shown: false,
            remote: CheckMenuItem::with_id("remote", "Remote Access", false, false, None),
            copy_link: item("copy-link", "Copy Pairing Link", false),
            show_qr: item("show-qr", "Show Pairing QR Code", false),
            devices: Submenu::with_id("devices", "Devices", true),
            device_rows: Vec::new(),
            no_devices: item("no-devices", "No devices paired", false),
            login: CheckMenuItem::with_id("login", "Launch at Login", false, false, None),
            endpoint: item("endpoint", "Endpoint: …", false),
            copy_endpoint: item("copy-endpoint", "Copy Endpoint ID", false),
            open_logs: item("open-logs", "Open Logs", false),
            open_data: item("open-data", "Open Data Folder", false),
            menu,
        };
        let advanced = Submenu::with_id("advanced", "Advanced", true);
        let version = item("version", &format!("Version {}", env!("CARGO_PKG_VERSION")), false);
        let quit = item("quit", "Quit Domovoi Server", true);
        let e = |e: tray_icon::menu::Error| e.to_string();
        advanced.append_items(&[&version, &items.endpoint, &items.copy_endpoint, &PredefinedMenuItem::separator(), &items.open_logs, &items.open_data]).map_err(e)?;
        items.devices.append(&items.no_devices).map_err(e)?;
        items
            .menu
            .append_items(&[
                &items.status,
                &PredefinedMenuItem::separator(),
                &items.remote,
                &items.copy_link,
                &items.show_qr,
                &items.devices,
                &PredefinedMenuItem::separator(),
                &items.login,
                &advanced,
                &PredefinedMenuItem::separator(),
                &quit,
            ])
            .map_err(e)?;
        let style = current_style(IconKind::Off);
        let builder = TrayIconBuilder::new().with_menu(Box::new(items.menu.clone())).with_tooltip("Domovoi Server");
        let first = self.icon(IconKind::Off, style)?;
        // A template image is tinted by macOS for light and dark menu bars.
        #[cfg(target_os = "macos")]
        let builder = builder.with_icon_templated(first);
        #[cfg(not(target_os = "macos"))]
        let builder = builder.with_icon(first);
        #[cfg(windows)]
        let builder = match std::env::current_exe() {
            Ok(exe) => builder.with_guid(super::windows::tray_guid_for_exe(&exe)),
            Err(_) => builder,
        };
        self.tray = Some(builder.build().map_err(|e| e.to_string())?);
        self.shown_icon = Some((IconKind::Off, style));
        self.items = Some(items);
        #[cfg(windows)]
        {
            self._menu_hook = super::windows::MenuZOrderHook::install();
        }
        Ok(())
    }

    fn icon(&mut self, kind: IconKind, style: Style) -> Result<Icon, String> {
        if let Some(icon) = self.icons.get(&(kind, style)) {
            return Ok(icon.clone());
        }
        let icon = Icon::from_rgba(icon::render(kind, style, ICON_SIZE), ICON_SIZE, ICON_SIZE).map_err(|e| e.to_string())?;
        self.icons.insert((kind, style), icon.clone());
        Ok(icon)
    }

    pub fn on_message(&mut self, message: TrayMessage) {
        match message {
            TrayMessage::Ready(control) => {
                self.login = control.launch_at_login();
                self.control = Some(control);
            }
            TrayMessage::Status(status) => {
                // The change the user asked for has landed (or failed).
                self.busy = false;
                self.status = Some(status);
            }
            TrayMessage::Menu(id) => self.on_menu(&id),
            TrayMessage::RemoteSet(result) => {
                self.busy = false;
                if let Err(e) = result {
                    self.flash(format!("Remote access: {e}"));
                }
            }
            TrayMessage::Link { result, qr } => match result {
                Ok(link) if qr => match self.show_qr(&link.link) {
                    Ok(()) => self.flash("QR code opened (valid 10 minutes)".into()),
                    Err(e) => self.flash(format!("QR code: {e}")),
                },
                Ok(link) => match self.copy(&link.link) {
                    Ok(()) => self.flash("Pairing link copied (valid 10 minutes)".into()),
                    Err(e) => self.flash(format!("Copying failed: {e}")),
                },
                Err(e) => self.flash(format!("Pairing link: {e}")),
            },
            TrayMessage::Revoked(result) => match result {
                Ok(true) => self.flash("Device signed out".into()),
                Ok(false) => self.flash("That device was already signed out".into()),
                Err(e) => self.flash(format!("Signing out failed: {e}")),
            },
            TrayMessage::ServerExited(error) => {
                if let Some(e) = error {
                    tracing::error!("The server stopped: {e}");
                }
                self.cleanup();
                self.exit = true;
            }
        }
        self.refresh();
    }

    fn flash(&mut self, text: String) {
        self.flash = Some((text, Instant::now()));
    }

    /// Runs `work` on the server's runtime; its message comes back to this thread.
    fn spawn<Fut>(&self, work: impl FnOnce(Control) -> Fut + Send + 'static)
    where
        Fut: Future<Output = TrayMessage> + Send,
    {
        let Some(control) = self.control.clone() else { return };
        let notify = self.notify.clone();
        self.handle.spawn(async move { notify(work(control).await) });
    }

    fn on_menu(&mut self, id: &str) {
        match id {
            "remote" => {
                let on = self.shown.as_ref().is_some_and(|m| m.remote_checked);
                self.busy = true;
                self.spawn(move |control| async move { TrayMessage::RemoteSet(control.set_remote(!on).await) });
            }
            "copy-link" => self.spawn(|control| async move { TrayMessage::Link { result: control.pairing_link().await, qr: false } }),
            "show-qr" => self.spawn(|control| async move { TrayMessage::Link { result: control.pairing_link().await, qr: true } }),
            "copy-endpoint" => {
                if let Some(id) = self.status.as_ref().map(|s| s.endpoint_id.clone()) {
                    match self.copy(&id) {
                        Ok(()) => self.flash("Endpoint ID copied".into()),
                        Err(e) => self.flash(format!("Copying failed: {e}")),
                    }
                }
            }
            "login" => {
                // Writing one small file: done here, no round trip needed.
                if let Some(control) = &self.control {
                    let wanted = !self.login;
                    let result = control.set_launch_at_login(wanted);
                    self.login = control.launch_at_login();
                    match result {
                        Ok(Some(note)) => self.flash(format!("Launch at login: {note}")),
                        Ok(None) => self.flash(if wanted { "Domovoi Server will start when you sign in".into() } else { "Domovoi Server will no longer start when you sign in".into() }),
                        Err(e) => self.flash(format!("Launch at login: {e}")),
                    }
                }
            }
            "open-logs" => self.open_folder(Control::logs_dir),
            "open-data" => self.open_folder(Control::home_dir),
            "quit" => match &self.control {
                Some(control) => {
                    control.shutdown();
                    self.flash("Stopping…".into());
                }
                // The server never came up: nothing to wait for.
                None => self.exit = true,
            },
            other => {
                if let Some(device) = other.strip_prefix("revoke:") {
                    let device = device.to_owned();
                    self.spawn(move |control| async move { TrayMessage::Revoked(control.revoke(&device)) });
                }
            }
        }
    }

    fn open_folder(&mut self, which: fn(&Control) -> PathBuf) {
        if let Some(path) = self.control.as_ref().map(which)
            && let Err(e) = open::that(path)
        {
            self.flash(format!("Opening the folder failed: {e}"));
        }
    }

    fn copy(&mut self, text: &str) -> Result<(), String> {
        if self.clipboard.is_none() {
            self.clipboard = Some(arboard::Clipboard::new().map_err(|e| e.to_string())?);
        }
        self.clipboard.as_mut().ok_or("no clipboard")?.set_text(text.to_owned()).map_err(|e| e.to_string())
    }

    /// Writes the link as a QR image only the user can read and opens it.
    fn show_qr(&mut self, link: &str) -> Result<(), String> {
        let png = icon::qr_png(link).ok_or("the link is too long for a QR code")?;
        let path = std::env::temp_dir().join(format!("domovoi-pairing-{}.png", crate::util::new_uuid()));
        crate::remote::write_private(&path, &png).map_err(|e| e.to_string())?;
        self.qr_files.push((path.clone(), Instant::now()));
        open::that(&path).map_err(|e| e.to_string())
    }

    /// Once a second: expire the confirmation line and old QR images, and
    /// follow the bar's light or dark look.
    pub fn tick(&mut self) {
        if self.flash.as_ref().is_some_and(|(_, at)| at.elapsed() > FLASH) {
            self.flash = None;
        }
        self.qr_files.retain(|(path, at)| {
            let keep = at.elapsed() < QR_LIFETIME;
            if !keep {
                let _ = std::fs::remove_file(path);
            }
            keep
        });
        if let Some(dir) = &self.test_dir {
            let clicks = dir.join("click");
            if let Ok(ids) = std::fs::read_to_string(&clicks) {
                let _ = std::fs::remove_file(&clicks);
                for id in ids.lines().map(str::trim).filter(|l| !l.is_empty()) {
                    self.on_menu(id);
                }
            }
        }
        self.refresh();
    }

    /// Writes what the menu shows, for tests that cannot look at the screen.
    fn report(&self) {
        let (Some(dir), Some(shown)) = (&self.test_dir, &self.shown) else { return };
        let rect = self.tray.as_ref().and_then(TrayIcon::rect);
        /// What `state.json` says of the menu.
        #[derive(serde::Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Report<'a> {
            status_line: &'a str,
            error_line: &'a Option<String>,
            remote_checked: bool,
            remote_enabled: bool,
            launch_at_login: bool,
            pairing_enabled: bool,
            devices: Vec<ReportedDevice<'a>>,
            icon: String,
            tooltip: &'a str,
            icon_shown: bool,
            icon_x: Option<f64>,
            icon_width: Option<u32>,
            qr_files: Vec<String>,
        }
        #[derive(serde::Serialize)]
        struct ReportedDevice<'a> {
            id: &'a str,
            label: &'a str,
        }
        let state = Report {
            status_line: &shown.status_line,
            error_line: &shown.error_line,
            remote_checked: shown.remote_checked,
            remote_enabled: shown.remote_enabled,
            launch_at_login: shown.launch_at_login,
            pairing_enabled: shown.pairing_enabled,
            devices: shown.devices.iter().map(|d| ReportedDevice { id: &d.id, label: &d.label }).collect(),
            icon: format!("{:?}", self.shown_icon.map(|(kind, _)| kind)),
            tooltip: &shown.tooltip,
            icon_shown: self.tray.is_some(),
            icon_x: rect.map(|r| r.position.x),
            icon_width: rect.map(|r| r.size.width),
            qr_files: self.qr_files.iter().map(|(p, _)| p.to_string_lossy().into_owned()).collect(),
        };
        let _ = std::fs::write(dir.join("state.json"), serde_json::to_string(&state).unwrap_or_default());
    }

    fn cleanup(&mut self) {
        for (path, _) in self.qr_files.drain(..) {
            let _ = std::fs::remove_file(path);
        }
    }

    /// Brings the menu and the icon in line with the current state.
    fn refresh(&mut self) {
        let next = model(self.status.as_ref(), self.busy, self.flash.as_ref().map(|(text, _)| text.as_str()), self.login);
        let style = current_style(next.icon);
        if self.shown_icon != Some((next.icon, style))
            && let Ok(icon) = self.icon(next.icon, style)
            && let Some(tray) = &self.tray
        {
            // A coloured mark needs a plain image: macOS would tint a template's mark too.
            #[cfg(target_os = "macos")]
            let set = if style == Style::Template { tray.set_icon_templated(Some(icon)) } else { tray.set_icon(Some(icon)) };
            #[cfg(not(target_os = "macos"))]
            let set = tray.set_icon(Some(icon));
            if set.is_ok() {
                self.shown_icon = Some((next.icon, style));
            }
        }
        if self.shown.as_ref() == Some(&next) {
            self.report();
            return;
        }
        let Some(items) = &mut self.items else { return };
        let previous = self.shown.as_ref();
        items.status.set_text(&next.status_line);
        match (&next.error_line, items.error_shown) {
            (Some(text), shown) => {
                items.error.set_text(text);
                if !shown && items.menu.insert(&items.error, 1).is_ok() {
                    items.error_shown = true;
                }
            }
            (None, true) => {
                let _ = items.menu.remove(&items.error);
                items.error_shown = false;
            }
            (None, false) => {}
        }
        items.remote.set_checked(next.remote_checked);
        items.remote.set_enabled(next.remote_enabled);
        items.copy_link.set_enabled(next.pairing_enabled);
        items.show_qr.set_enabled(next.pairing_enabled);
        for ready_only in [&items.copy_endpoint, &items.open_logs, &items.open_data] {
            ready_only.set_enabled(next.ready);
        }
        items.login.set_enabled(next.ready);
        items.login.set_checked(next.launch_at_login);
        if let Some(status) = &self.status {
            let id = &status.endpoint_id;
            items.endpoint.set_text(format!("Endpoint: {}…", &id[..id.len().min(16)]));
        }
        if previous.map(|p| &p.devices) != Some(&next.devices) {
            for row in items.device_rows.drain(..) {
                let _ = items.devices.remove(&row);
            }
            let _ = items.devices.remove(&items.no_devices);
            if next.devices.is_empty() {
                let _ = items.devices.append(&items.no_devices);
            }
            for device in &next.devices {
                let row = Submenu::new(&device.label, true);
                let _ = row.append(&MenuItem::with_id(format!("revoke:{}", device.id), "Sign Out", true, None));
                if items.devices.append(&row).is_ok() {
                    items.device_rows.push(row);
                }
            }
        }
        if previous.map(|p| &p.tooltip) != Some(&next.tooltip)
            && let Some(tray) = &self.tray
        {
            let _ = tray.set_tooltip(Some(&next.tooltip));
        }
        self.shown = Some(next);
        self.report();
    }
}

impl Drop for TrayApp {
    fn drop(&mut self) {
        self.cleanup();
    }
}

/// The glyph is black or white to stand out on the bar, so the state's mark
/// can be green (connected) or red (error). macOS tints a template image
/// itself, which suits the plain glyph; a mark in colour needs a plain image,
/// matched to the menu bar's appearance.
#[cfg_attr(not(target_os = "macos"), allow(unused_variables))] // the mark decides only on macOS
fn current_style(kind: IconKind) -> Style {
    #[cfg(target_os = "macos")]
    {
        if !matches!(kind, IconKind::Connected | IconKind::Error) {
            Style::Template
        } else if matches!(dark_light::detect(), Ok(dark_light::Mode::Light)) {
            Style::OnLight
        } else {
            Style::OnDark
        }
    }
    #[cfg(windows)]
    {
        if super::windows::taskbar_is_dark() { Style::OnDark } else { Style::OnLight }
    }
    #[cfg(not(any(target_os = "macos", windows)))]
    {
        // GNOME's top bar is dark whatever the theme; other desktops' panels follow it.
        let gnome = std::env::var("XDG_CURRENT_DESKTOP").is_ok_and(|d| d.to_ascii_uppercase().contains("GNOME"));
        if gnome || !matches!(dark_light::detect(), Ok(dark_light::Mode::Light)) { Style::OnDark } else { Style::OnLight }
    }
}
