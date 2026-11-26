//! Stages the self-contained Windows App SDK runtime beside the executable
//! (no runtime installer, no package identity) and embeds the app icon.

#[cfg(windows)]
const ICON: &str = "../domovoi-iced/assets/app-icon.ico";

fn main() {
    #[cfg(windows)]
    {
        windows_reactor_setup::as_self_contained();

        println!("cargo:rerun-if-changed={ICON}");
        // The icon is kept in Git LFS: a checkout without `git lfs pull`
        // holds a pointer file, not an icon.
        if !std::fs::read(ICON).is_ok_and(|bytes| bytes.starts_with(&[0, 0, 1, 0])) {
            println!("cargo:warning={ICON} is not an icon (run `git lfs pull`); Domovoi.exe gets no icon");
            return;
        }
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon(ICON).set("ProductName", "Domovoi").set("FileDescription", "Domovoi");
        if let Err(e) = resource.compile() {
            println!("cargo:warning=Domovoi.exe gets no icon: the resource compiler failed ({e})");
        }
    }
}
