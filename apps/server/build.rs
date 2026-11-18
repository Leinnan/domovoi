//! Gives the Windows executable its icon (apps/server/icon/domovoi-server.ico,
//! made by tools/icons.py). Needs the resource compiler (rc.exe with MSVC,
//! windres with MinGW); without one the build goes on without the icon.

fn main() {
    println!("cargo:rerun-if-changed=icon/domovoi-server.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let mut resource = winresource::WindowsResource::new();
    resource.set_icon("icon/domovoi-server.ico").set("FileDescription", "Domovoi Server").set("ProductName", "Domovoi");
    if let Err(e) = resource.compile() {
        println!("cargo:warning=domovoi-server.exe gets no icon: the resource compiler failed ({e})");
    }
}
