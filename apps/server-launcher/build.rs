//! Gives the Windows executable the server's icon (it is what Task Manager
//! and the startup apps list show for a server started at sign-in).

const ICON: &str = "../server/icon/domovoi-server.ico";
/// Long paths and no elevation, as the server has.
const MANIFEST: &str = "../server/windows.manifest";

fn main() {
    println!("cargo:rerun-if-changed={ICON}");
    println!("cargo:rerun-if-changed={MANIFEST}");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    // Kept in Git LFS: a checkout without `git lfs pull` holds a pointer file.
    if !std::fs::read(ICON).is_ok_and(|bytes| bytes.starts_with(&[0, 0, 1, 0])) {
        println!("cargo:warning={ICON} is not an icon (run `git lfs pull`); the launcher gets no icon");
        return;
    }
    let mut resource = winresource::WindowsResource::new();
    resource.set_icon(ICON).set("FileDescription", "Domovoi Server").set("ProductName", "Domovoi");
    resource.set_manifest_file(MANIFEST);
    if let Err(e) = resource.compile() {
        println!("cargo:warning=the launcher gets no icon: the resource compiler failed ({e})");
    }
}
