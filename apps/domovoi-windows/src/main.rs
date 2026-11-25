//! Domovoi for Windows: a WinUI 3 shell through `windows-reactor`, on the
//! client core (`domovoi_core`) and the Rust effect host it shares with the
//! Iced app (`domovoi-desktop-host`). The core is linked directly: events and
//! the view model are plain Rust values. The shell only draws and collects
//! input. `AGENTS.md` beside this crate maps the modules and the reactor's
//! rules.
#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
mod core_link;
#[cfg(windows)]
mod launch;
#[cfg(windows)]
mod store;
#[cfg(windows)]
mod theme;
#[cfg(windows)]
mod ui_thread;
#[cfg(windows)]
mod views;
#[cfg(windows)]
mod window;

#[cfg(windows)]
fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn,domovoi_windows=info".into()))
        .with_writer(std::io::stderr)
        .init();
    let launch = launch::Launch::from_args(std::env::args().skip(1));
    // A link opened while the app runs goes to it, and this launch ends.
    if domovoi_desktop_host::links::start(core_link::IDENTITY, launch.link.as_deref()) {
        return;
    }
    domovoi_desktop_host::windows::register_scheme(domovoi_desktop_host::windows::SchemeOwner::Primary);
    domovoi_desktop_host::windows::register_app_id(core_link::IDENTITY, None);
    if let Err(error) = windows_reactor::App::run_component::<window::MainWindow>(launch) {
        tracing::error!("Domovoi could not start: {error:?}");
        std::process::exit(1);
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("This is Domovoi's Windows app; it only runs on Windows. Elsewhere run the Iced app (`just desktop`).");
}
