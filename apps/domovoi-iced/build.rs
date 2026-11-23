//! Embeds the app icon in the Windows executable (Explorer, taskbar, Alt-Tab).
//! The window and macOS Dock icons are set at runtime (`src/main.rs`).

const ICON: &str = "assets/app-icon.ico";

fn main() {
    println!("cargo:rerun-if-changed={ICON}");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    // The icon is kept in Git LFS: a checkout without `git lfs pull` holds a
    // pointer file, not an icon.
    let is_ico = std::fs::read(ICON).is_ok_and(|bytes| bytes.starts_with(&[0, 0, 1, 0]));
    if !is_ico {
        println!("cargo:warning={ICON} is not an icon (run `git lfs pull`); building without it");
        return;
    }
    let mut res = winresource::WindowsResource::new();
    res.set_icon(ICON);
    res.set("ProductName", "Domovoi");
    res.set("FileDescription", "Domovoi Desktop");
    if let Err(e) = res.compile() {
        println!("cargo:warning=Domovoi Desktop gets no icon: the resource compiler failed ({e})");
    }
}
